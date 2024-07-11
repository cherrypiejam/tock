// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2022.

//! Inter-process communication mechanism for Tock.
//!
//! This is a special syscall driver that allows userspace applications to
//! share memory.

use crate::capabilities::MemoryAllocationCapability;
use crate::grant::{AllowRoCount, AllowRwCount, Grant, UpcallCount};
use crate::kernel::Kernel;
use crate::process;
use crate::process::ProcessId;
use crate::processbuffer::ReadableProcessBuffer;
use crate::syscall_driver::{CommandReturn, SyscallDriver};
use crate::ErrorCode;

use crate::platform::platform::KernelResources;
use crate::platform::chip::{Chip, ChipAtomic};
use crate::kernel::smp::{InterThreadChannel, InterThreadMessage};
use crate::deferred_call::{DeferredCall, DeferredCallClient};
// use crate::errorcode::ErrorCode;

/// Syscall number
pub const DRIVER_NUM: usize = 0x10001;

/// Ids for read-only allow buffers
mod ro_allow {
    pub(super) const SEARCH: usize = 0;
    /// The number of allow buffers the kernel stores for this grant.
    pub(super) const COUNT: u8 = 1;
}

#[derive(Copy, Clone, Debug)]
pub enum IPCInterThreadMessage {
    Invalid,
    Discover([u8; 10]), // FIXME: Size of a ro grant?
    DiscoverSuccess(u32),
    DiscoverFail,
}

impl core::convert::TryFrom<InterThreadMessage> for IPCInterThreadMessage {
    type Error = ();

    fn try_from(value: T) -> Result<Self, Self::Error> {
        match value {
            InterThreadMessage::IPC(m) => Ok(m),
            _ => Err(())
        }
    }
}

impl core::convert::Into<InterThreadMessage> for IPCInterThreadMessage {
    fn into(self) -> InterThreadMessage {
        InterThreadMessage::Ipc(self)
    }
}

/// Enum to mark which type of upcall is scheduled for the IPC mechanism.
#[derive(Copy, Clone, Debug)]
pub enum IPCUpcallType {
    /// Indicates that the upcall is for the service upcall handler this
    /// process has setup.
    Service,
    /// Indicates that the upcall is from a different service app and will
    /// call one of the client upcalls setup by this process.
    Client,
}

/// State that is stored in each process's grant region to support IPC.
#[derive(Default)]
struct IPCData;

/// The IPC mechanism struct.
pub struct IPC<const NUM_PROCS: u8, KR: KernelResources<C>, C: Chip + ChipAtomic> {
    /// The grant regions for each process that holds the per-process IPC data.
    data: Grant<
        IPCData,
        UpcallCount<NUM_PROCS>,
        AllowRoCount<{ ro_allow::COUNT }>,
        AllowRwCount<NUM_PROCS>,
    >,
    /// To enable inter-thread communication
    platform: &KR,
    chip: &C,
    deferred_call: DeferredCall,
}

impl<const NUM_PROCS: u8> IPC<NUM_PROCS, KR: KernelResources<C>, C: Chip + ChipAtomic> {
    pub fn new(
        kernel: &'static Kernel,
        driver_num: usize,
        capability: &dyn MemoryAllocationCapability,
        resources: &KR,
        chip: &C,
    ) -> Self {
        Self {
            data: kernel.create_grant(driver_num, capability),
            resources,
            chip,
            deferred_call: DeferredCall::new(),
        }
    }

    /// Schedule an IPC upcall for a process. This is called by the main
    /// scheduler loop if an IPC task was queued for the process.
    pub(crate) unsafe fn schedule_upcall(
        &self,
        schedule_on: ProcessId,
        called_from: ProcessId,
        cb_type: IPCUpcallType,
    ) -> Result<(), process::Error> {
        let schedule_on_id = schedule_on.index().ok_or(process::Error::NoSuchApp)?;
        let called_from_id = called_from.index().ok_or(process::Error::NoSuchApp)?;
        self.data.enter(schedule_on, |_, schedule_on_data| {
            self.data.enter(called_from, |_, called_from_data| {
                // If the other app shared a buffer with us, make
                // sure we have access to that slice and then call
                // the upcall. If no slice was shared then just
                // call the upcall.
                let (len, ptr) = match called_from_data.get_readwrite_processbuffer(schedule_on_id)
                {
                    Ok(slice) => {
                        // Ensure receiving app has MPU access to sending app's buffer
                        self.data
                            .kernel
                            .process_map_or(None, schedule_on, |process| {
                                process.add_mpu_region(slice.ptr(), slice.len(), slice.len())
                            });
                        (slice.len(), slice.ptr() as usize)
                    }
                    Err(_) => (0, 0),
                };
                let to_schedule: usize = match cb_type {
                    IPCUpcallType::Service => schedule_on_id,
                    IPCUpcallType::Client => called_from_id,
                };
                let _ = schedule_on_data.schedule_upcall(to_schedule, (called_from_id, len, ptr));
            })
        })?
    }

    fn discover(&self, processid: ProcessId) -> CommandReturn {
        self.data
            .enter(processid, |_, kernel_data| {
                kernel_data
                    .get_readonly_processbuffer(ro_allow::SEARCH)
                    .and_then(|search| {
                        search.enter(|slice| {
                            self.data
                                .kernel
                                .process_until(|p| {
                                    let s = p.get_process_name().as_bytes();
                                    // are slices equal?
                                    if s.len() == slice.len()
                                        && s.iter()
                                        .zip(slice.iter())
                                        .all(|(c1, c2)| *c1 == c2.get())
                                    {
                                        // Return the index of the process which is used for
                                        // subscribe number
                                        p.processid()
                                            .index()
                                            .map(|i| CommandReturn::success_u32(i as u32))
                                    } else {
                                        None
                                    }
                                })
                                .unwrap_or(CommandReturn::failure(ErrorCode::NODEVICE))
                        })
                    })
                    .unwrap_or(CommandReturn::failure(ErrorCode::INVAL))
            })
            .unwrap_or(CommandReturn::failure(ErrorCode::NOMEM))
    }

    fn discover_local(&self, slice: &ReadableProcessSlice) -> Option<u32> {
        self.data
            .kernel
            .process_until(|p| {
                let s = p.get_process_name().as_bytes();
                // are slices equal?
                if s.len() == slice.len()
                    && s.iter()
                    .zip(slice.iter())
                    .all(|(c1, c2)| *c1 == c2.get())
                {
                    // Return the index of the process which is used for
                    // subscribe number
                    p.processid()
                        .index()
                        .map(|i| i as u32)
                        // .map(|i| CommandReturn::success_u32(i as u32))
                } else {
                    None
                }
            })
    }

    fn discover_global(&self, slice: &ReadableProcessSlice) -> Option<(DynThreadId, u32)> {
        use InterThreadMessage as M;
        use IPCInterThreadMessage as IpcM;

        let channel = self.platform.shared_channel();

        let mut buf = [0u8, 10];
        buf.copy_from_slice(slice[0..10]);
        let message = M::Discover(buf).into();

        for target_id in (0..1).filter(|&id| id != self.chip.id()) {
            let message_id = channel.write(&target, &message);
            chip.notify(&target);
            match channel.read(message_id) {
                Some((r, M::Ipc(IpcM::DiscoverSuccess(i)))) => return Some((unsafe { DynThreadId::new(r) }, i)),
                _ => {}
            }
        }

        None
    }

    pub fn service(&self) {
        use IPCInterThreadMessage as M;

        let (id, message) = self.platform
            .shared_channel()
            .read()
            .expect("IPC service failed to receive message");

        match message.into() {
            M::Discover(target) => {
                assert!(id.get_id() != self.chip.id().get_id());
                let response = self.discover_local.map(|i| M::DiscoverSuccess(i)).unwrap_or(M::DiscoverFail).into();
                self.platform.shared_channel()
                    .write(&id, &response)
            },
            _ => panic!("Unknown IPC message received"),
        }
    }
}

impl<const NUM_PROCS: u8> SyscallDriver for IPC<NUM_PROCS> {
    /// command is how notify() is implemented.
    /// Notifying an IPC service is done by setting client_or_svc to 0,
    /// and notifying an IPC client is done by setting client_or_svc to 1.
    /// In either case, the target_id is the same number as provided in a notify
    /// upcall or as returned by allow.
    ///
    /// Returns INVAL if the other process doesn't exist.

    /// Initiates a service discovery or notifies a client or service.
    ///
    /// ### `command_num`
    ///
    /// - `0`: Driver existence check, always returns Ok(())
    /// - `1`: Perform discovery on the package name passed to `allow_readonly`. Returns the
    ///        service descriptor if the service is found, otherwise returns an error.
    /// - `2`: Notify a service previously discovered to have the service descriptor in
    ///        `target_id`. Returns an error if `target_id` refers to an invalid service or the
    ///        notify fails to enqueue.
    /// - `3`: Notify a client with descriptor `target_id`, typically in response to a previous
    ///        notify from the client. Returns an error if `target_id` refers to an invalid client
    ///        or the notify fails to enqueue.
    fn command(
        &self,
        command_number: usize,
        target_id: usize,
        _: usize,
        processid: ProcessId,
    ) -> CommandReturn {
        match command_number {
            0 => CommandReturn::success(),
            1 =>
            /* Discover */
            {
                // let ret = self.discover(processid).into_inner();
                // match ret {
                //     SyscallReturn::Failure(ErrorCode::NODEVICE) => {
                //         self.platform
                //             .shared_channel()
                //             .request(IPCInterThreadMessage::IPCDiscover)
                //     }
                //     _ => ret
                // }

                self.data
                    .enter(processid, |_, kernel_data| {
                        kernel_data
                            .get_readonly_processbuffer(ro_allow::SEARCH)
                            .and_then(|search| {
                                search.enter(|slice| {
                                    self.discover_by_name(slice)
                                        .map(|i| CommandReturn::success_u32(i))
                                        .unwrap_or_else(|| {
                                            self.discover_global(slice)
                                                .map(|i| CommandReturn::success_u32(i))
                                                .unwrap_or(CommandReturn::failure(ErrorCode::NODEVICE))
                                        })
                                })
                            })
                            .unwrap_or(CommandReturn::failure(ErrorCode::INVAL))
                    })
                    .unwrap_or(CommandReturn::failure(ErrorCode::NOMEM))
            }
            2 =>
            /* Service notify */
            {
                let cb_type = IPCUpcallType::Service;

                let thread_id_mask = 0xff << (usize::BITS - 8);
                let thread_id = (thread_id & thread_id_mask) >> thread_id_mast.trailing_zeros();
                let target_id = target_id & !thread_id_mask;

                if thread_id == self.chip.id().get_id() {
                    let other_process =
                        self.data
                        .kernel
                        .process_until(|p| match p.processid().index() {
                            Some(i) if i == target_id => Some(p.processid()),
                            _ => None,
                        });

                    other_process.map_or(CommandReturn::failure(ErrorCode::INVAL), |otherapp| {
                        self.data.kernel.process_map_or(
                            CommandReturn::failure(ErrorCode::INVAL),
                            otherapp,
                            |target| {
                                let ret = target.enqueue_task(process::Task::IPC((processid, cb_type)));
                                match ret {
                                    Ok(()) => CommandReturn::success(),
                                    Err(e) => {
                                        // `enqueue_task` does not provide information on whether the
                                        // recipient has set a non-null callback. It only reports
                                        // general failures, such as insufficient memory in the pending
                                        // tasks queue
                                        CommandReturn::failure(e)
                                    }
                                }
                            },
                        )
                    })
                } else {
                    let _ = self.platform
                        .shared_channel()
                        .request(InterThreadMessage::IPCwhatever);
                }
            }
            3 =>
            /* Client notify */
            {
                let cb_type = IPCUpcallType::Client;

                let other_process =
                    self.data
                        .kernel
                        .process_until(|p| match p.processid().index() {
                            Some(i) if i == target_id => Some(p.processid()),
                            _ => None,
                        });

                other_process.map_or(CommandReturn::failure(ErrorCode::INVAL), |otherapp| {
                    self.data.kernel.process_map_or(
                        CommandReturn::failure(ErrorCode::INVAL),
                        otherapp,
                        |target| {
                            let ret = target.enqueue_task(process::Task::IPC((processid, cb_type)));
                            match ret {
                                Ok(()) => CommandReturn::success(),
                                Err(e) => {
                                    // `enqueue_task` does not provide information on whether the
                                    // recipient has set a non-null callback. It only reports
                                    // general failures, such as insufficient memory in the pending
                                    // tasks queue
                                    CommandReturn::failure(e)
                                }
                            }
                        },
                    )
                })
            }
            _ => CommandReturn::failure(ErrorCode::NOSUPPORT),
        }
    }

    fn allocate_grant(&self, processid: ProcessId) -> Result<(), crate::process::Error> {
        self.data.enter(processid, |_, _| {})
    }
}

impl<const NUM_PROCS: u8> DeferredCallClient for IPC<NUM_PROCS> {
    fn handle_deferred_call(&self) {
        self.service();
    }

    fn register(&'static self) {
        self.deferred_call.register(self);
    }
}
