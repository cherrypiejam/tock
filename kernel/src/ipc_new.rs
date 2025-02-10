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
use crate::processbuffer::{ReadableProcessBuffer, WriteableProcessBuffer, ProcessSliceBuffer};
use crate::syscall_driver::{CommandReturn, SyscallDriver};
use crate::ErrorCode;

/// Syscall number
pub const DRIVER_NUM: usize = 0x10000;

/// Ids for read-only allow buffers
mod ro_allow {
    pub(super) const SEARCH: usize = 0;
    /// The number of allow buffers the kernel stores for this grant.
    pub(super) const COUNT: u8 = 1;
}

/// Enum to mark which type of upcall is scheduled for the IPC mechanism.
#[derive(Copy, Clone, Debug)]
pub enum IPCUpcallType {
    Sender,
    ClientReceiver,
    ServiceReceiver,
    // ReceiveContinue,
    // ReceiveFinished,

    // ----------- Depreciated ------------

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

// Streaming protocol
// Process A: 1. |put all data in grant buffer| 2. |transmit data, send queue to receive queue|
// Process B: 3. |get receive queue| 4. process receive queue. 5. return data
// IPC Driver (per process): (receive queue (empty), send queue (one buffer))
// --- process A sends a message to process B


// Current Status (0 copy, sharing buffer)
// IPC (per process): discover -> ro buffer x 1;
// writable callback buffer (rw buffer x N);
// Upcall count x N;

// New IPC (double buffering)
// IPC (per process): discover -> ro buffer x 1;
// writable callback buffer (rw buffer x N), not shared;
// upcall count x N;
// [[(ReceivePort, SendPort); N]; N];

// TODO:
// Do we still want to keep the current IPC interface (callbacks)?
// similar interfaces (callbacks) -> but with explicit `send` button to transmit
// data from one process to another process.


// Assume 2 upcalls for each process
// 1. Callback when getting a message
// 2. Callback when a message is send

/// The IPC mechanism struct.
pub struct IPC<const NUM_PROCS: u8>
{
    /// The grant regions for each process that holds the per-process IPC data.
    data: Grant<
        IPCData,
        // TODO: fix upcall #
        UpcallCount<NUM_PROCS>,
        AllowRoCount<{ ro_allow::COUNT }>,
        AllowRwCount<16>, // receiver and sender buffer // fix me with a generic const
    >,
}


impl<const NUM_PROCS: u8> IPC<NUM_PROCS>
{
    pub fn new(
        kernel: &'static Kernel,
        driver_num: usize,
        capability: &dyn MemoryAllocationCapability,
    ) -> Self {
        Self {
            data: kernel.create_grant(driver_num, capability),
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
            match cb_type {
                IPCUpcallType::Sender => {
                    Ok(()) // FIXME: do nothing for now.
                }
                _ => {
                    self.data.enter(called_from, |_, called_from_data| {
                        let to_schedule: usize = match cb_type {
                            IPCUpcallType::ServiceReceiver => schedule_on_id,
                            IPCUpcallType::ClientReceiver => called_from_id,
                            _ => unreachable!(),
                        };

                        let (len, ptr) = match schedule_on_data.get_readwrite_processbuffer((to_schedule << 1) | 1)
                        {
                            Ok(slice) => (slice.len(), slice.ptr() as usize),
                            Err(_) => (0, 0),
                        };

                        let _ = schedule_on_data.schedule_upcall(to_schedule, (called_from_id, len, ptr));
                    })
                }
            }
        })?
    }
}







// Bigger problem
// server-client semantics or peer-peer semantics

// Usersapce interface
//  - Send
//    - SendServerCB
//    - SendClientCB
//      why server-client semantics????
//      should we just use port semantics instead?
//  - Receive
//    - ReceiveServerCB
//    - ReceiveClientCB
// Capsule interface
//
//








impl<const NUM_PROCS: u8> SyscallDriver for IPC<NUM_PROCS>
{
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
            2 =>
            /* Send message to a service */
            {
                crate::debug!("2222222222222: starting sending message");
                let maybe_other_processid =
                    self.data
                        .kernel
                        .process_until(|p| match p.processid().index() {
                            Some(i) if i == target_id => Some(p.processid()),
                            _ => None,
                        });


                // may just use target_id instead
                maybe_other_processid.map_or(CommandReturn::failure(ErrorCode::INVAL), |other_processid| {

                    crate::debug!("getting other processid");
                    processid.index().map_or(CommandReturn::failure(ErrorCode::INVAL), |self_id| {

                        crate::debug!("getting self p index, entering main logic");
                        // Main logic
                        self.data
                            .enter(processid, |_, sender_data| {

                                let ret = self.data.enter(other_processid, |_, _| ());
                                crate::debug!("getting self data self id {self_id}, target id {target_id}, ret: {:?}", ret);

                                self.data
                                    .enter(other_processid, |_, receiver_data| {
                                        crate::debug!("getting other processes data");

                                        // Convention:
                                        // Even ID is for sending queue
                                        // Odd  ID is for receiving queue
                                        match (sender_data.get_readwrite_processbuffer(target_id << 1),
                                               receiver_data.get_readwrite_processbuffer((target_id << 1) | 1))
                                        {
                                            (Ok(send_slice), Ok(receive_slice)) => {
                                                crate::debug!("getting both buffers");
                                                send_slice
                                                    .enter(|sslice| {
                                                        receive_slice
                                                            .mut_enter(|rslice| {
                                                                let receive_buffer = ProcessSliceBuffer::new(rslice);

                                                                if let Err(e) = receive_buffer.append_process_slice(sslice) {
                                                                    crate::debug!("dropping message from IPC: {:?}", e);
                                                                    // should we propagate error to the sender upcall
                                                                    CommandReturn::failure(ErrorCode::INVAL)
                                                                } else {
                                                                    crate::debug!("appened process slice");
                                                                    self.data.kernel.process_map_or(
                                                                        CommandReturn::failure(ErrorCode::INVAL),
                                                                        processid,
                                                                        |sender| {
                                                                            crate::debug!("append process slice, enter process 1");
                                                                            self.data.kernel.process_map_or(
                                                                                CommandReturn::failure(ErrorCode::INVAL),
                                                                                other_processid,
                                                                                |receiver| {

                                                                                    crate::debug!("append process slice, enter process 2");
                                                                                    let ret = sender
                                                                                        .enqueue_task(process::Task::IPC((processid, IPCUpcallType::Sender)))
                                                                                        .and_then(|_| {
                                                                                            receiver
                                                                                                .enqueue_task(process::Task::IPC((processid, IPCUpcallType::ServiceReceiver)))
                                                                                        });
                                                                                    match ret {
                                                                                        Ok(()) => {
                                                                                            crate::debug!("append process slice, return status OK");
                                                                                            CommandReturn::success()
                                                                                        }
                                                                                        Err(e) => {
                                                                                            // `enqueue_task` does not provide information on whether the
                                                                                            // recipient has set a non-null callback. It only reports
                                                                                            // general failures, such as insufficient memory in the pending
                                                                                            // tasks queue
                                                                                            CommandReturn::failure(e)
                                                                                        }
                                                                                    }
                                                                                }
                                                                            )
                                                                        }
                                                                    )
                                                                }
                                                            })
                                                            .unwrap_or(CommandReturn::failure(ErrorCode::INVAL))
                                                    })
                                                    .unwrap_or(CommandReturn::failure(ErrorCode::INVAL))
                                            }
                                            _ => CommandReturn::failure(ErrorCode::INVAL),
                                        }

                                    })
                                    .unwrap_or(CommandReturn::failure(ErrorCode::INVAL))
                            })
                            .unwrap_or(CommandReturn::failure(ErrorCode::INVAL))
                    })
                })
            }
            3 =>
            /* Send message to a client */
            {
                crate::debug!("3333333333333333: starting sending message");
                let maybe_other_processid =
                    self.data
                        .kernel
                        .process_until(|p| match p.processid().index() {
                            Some(i) if i == target_id => Some(p.processid()),
                            _ => None,
                        });


                // may just use target_id instead
                maybe_other_processid.map_or(CommandReturn::failure(ErrorCode::INVAL), |other_processid| {

                    crate::debug!("getting other processid");
                    processid.index().map_or(CommandReturn::failure(ErrorCode::INVAL), |self_id| {

                        crate::debug!("getting self p index, entering main logic");
                        // Main logic
                        self.data
                            .enter(processid, |_, sender_data| {

                                let ret = self.data.enter(other_processid, |_, _| ());
                                crate::debug!("getting self data self id {self_id}, target id {target_id}, ret: {:?}", ret);

                                self.data
                                    .enter(other_processid, |_, receiver_data| {
                                        crate::debug!("getting other processes data");

                                        // Convention:
                                        // Even ID is for sending queue
                                        // Odd  ID is for receiving queue
                                        match (sender_data.get_readwrite_processbuffer(self_id << 1),
                                               receiver_data.get_readwrite_processbuffer((self_id << 1) | 1))
                                        {
                                            (Ok(send_slice), Ok(receive_slice)) => {
                                                crate::debug!("getting both buffers");
                                                send_slice
                                                    .enter(|sslice| {
                                                        receive_slice
                                                            .mut_enter(|rslice| {
                                                                let receive_buffer = ProcessSliceBuffer::new(rslice);

                                                                if let Err(e) = receive_buffer.append_process_slice(sslice) {
                                                                    crate::debug!("dropping message from IPC: {:?}", e);
                                                                    // should we propagate error to the sender upcall
                                                                    CommandReturn::failure(ErrorCode::INVAL)
                                                                } else {
                                                                    crate::debug!("appened process slice");
                                                                    self.data.kernel.process_map_or(
                                                                        CommandReturn::failure(ErrorCode::INVAL),
                                                                        processid,
                                                                        |sender| {
                                                                            crate::debug!("append process slice, enter process 1");
                                                                            self.data.kernel.process_map_or(
                                                                                CommandReturn::failure(ErrorCode::INVAL),
                                                                                other_processid,
                                                                                |receiver| {

                                                                                    crate::debug!("append process slice, enter process 2");
                                                                                    let ret = sender
                                                                                        .enqueue_task(process::Task::IPC((processid, IPCUpcallType::Sender)))
                                                                                        .and_then(|_| {
                                                                                            receiver
                                                                                                .enqueue_task(process::Task::IPC((processid, IPCUpcallType::ClientReceiver)))
                                                                                        });
                                                                                    match ret {
                                                                                        Ok(()) => {
                                                                                            crate::debug!("append process slice, return status OK");
                                                                                            CommandReturn::success()
                                                                                        }
                                                                                        Err(e) => {
                                                                                            // `enqueue_task` does not provide information on whether the
                                                                                            // recipient has set a non-null callback. It only reports
                                                                                            // general failures, such as insufficient memory in the pending
                                                                                            // tasks queue
                                                                                            CommandReturn::failure(e)
                                                                                        }
                                                                                    }
                                                                                }
                                                                            )
                                                                        }
                                                                    )
                                                                }
                                                            })
                                                            .unwrap_or(CommandReturn::failure(ErrorCode::INVAL))
                                                    })
                                                    .unwrap_or(CommandReturn::failure(ErrorCode::INVAL))
                                            }
                                            _ => CommandReturn::failure(ErrorCode::INVAL),
                                        }

                                    })
                                    .unwrap_or(CommandReturn::failure(ErrorCode::INVAL))
                            })
                            .unwrap_or(CommandReturn::failure(ErrorCode::INVAL))
                    })
                })
            }
            4 =>
            /* Getting my id */
            {
                crate::debug!("Command 3 getting my id");
                processid.index().map_or(CommandReturn::failure(ErrorCode::INVAL), |self_id| {
                    crate::debug!("getting self process id {}", self_id);
                    CommandReturn::success_u32(self_id as u32)
                })
            }
            // 2 =>
            // /* Service notify */
            // {
            //     let cb_type = IPCUpcallType::Service;

            //     let other_process =
            //         self.data
            //             .kernel
            //             .process_until(|p| match p.processid().index() {
            //                 Some(i) if i == target_id => Some(p.processid()),
            //                 _ => None,
            //             });

            //     other_process.map_or(CommandReturn::failure(ErrorCode::INVAL), |otherapp| {
            //         self.data.kernel.process_map_or(
            //             CommandReturn::failure(ErrorCode::INVAL),
            //             otherapp,
            //             |target| {
            //                 let ret = target.enqueue_task(process::Task::IPC((processid, cb_type)));
            //                 match ret {
            //                     Ok(()) => CommandReturn::success(),
            //                     Err(e) => {
            //                         // `enqueue_task` does not provide information on whether the
            //                         // recipient has set a non-null callback. It only reports
            //                         // general failures, such as insufficient memory in the pending
            //                         // tasks queue
            //                         CommandReturn::failure(e)
            //                     }
            //                 }
            //             },
            //         )
            //     })
            // }
            // 3 =>
            // /* Client notify */
            // {
            //     let cb_type = IPCUpcallType::Client;

            //     let other_process =
            //         self.data
            //             .kernel
            //             .process_until(|p| match p.processid().index() {
            //                 Some(i) if i == target_id => Some(p.processid()),
            //                 _ => None,
            //             });

            //     other_process.map_or(CommandReturn::failure(ErrorCode::INVAL), |otherapp| {
            //         self.data.kernel.process_map_or(
            //             CommandReturn::failure(ErrorCode::INVAL),
            //             otherapp,
            //             |target| {
            //                 let ret = target.enqueue_task(process::Task::IPC((processid, cb_type)));
            //                 match ret {
            //                     Ok(()) => CommandReturn::success(),
            //                     Err(e) => {
            //                         // `enqueue_task` does not provide information on whether the
            //                         // recipient has set a non-null callback. It only reports
            //                         // general failures, such as insufficient memory in the pending
            //                         // tasks queue
            //                         CommandReturn::failure(e)
            //                     }
            //                 }
            //             },
            //         )
            //     })
            // }
            _ => CommandReturn::failure(ErrorCode::NOSUPPORT),
        }
    }

    fn allocate_grant(&self, processid: ProcessId) -> Result<(), crate::process::Error> {
        self.data.enter(processid, |_, _| {})
    }
}
