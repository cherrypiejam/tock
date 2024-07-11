//! Inter-processor communication channel.

use core::cell::Cell;

use kernel::deferred_call::{DeferredCallThread, DeferredCallClient};
use kernel::threadlocal::{ThreadLocal, ThreadLocalAccess, DynThreadId, ThreadId};
use kernel::utilities::registers::interfaces::Readable;

use kernel::smp::channel::{InterThreadChannel, InterThreadMessage};
use kernel::smp::rwlock::RwLock;
use kernel::smp::mutex::Mutex;
use kernel::smp::ipc::IPCInterThreadMessage as IpcM;
use kernel::collections::ring_buffer::{RingBuffer, Queue};
use kernel::thread_local_static_access;

use rv32i::csr::CSR;

use crate::MAX_THREADS;
use crate::clint::CLIC;

type Buffer = [u8; 1024];

// pub static mut SHARED_CHANNEL_BUFFER: Buffer = [0; BUFFER_SIZE];

pub static mut SHARED_CHANNEL: RwLock<QemuRv32VirtChannel> = RwLock::new(QemuRv32VirtChannel::new())
static mut SHARED_CHANNEL_BUFFER: [InterThreadMessage<IpcM>; 1024] = [InterThreadMessage::<IpcM>::empty(); 1024];

// pub static mut CHANNEL_BUFFER: ThreadLocal<MAX_THREADS, Buffer> = ThreadLocal::init([0; 1024]);

#[derive(Copy, Clone)]
enum QemuRv32VirtChannelState {
    Init,
    Process,
    End,
}

pub struct QemuRv32VirtChannel {
    state: Cell<QemuRv32VirtChannelState>,
    buffer: RingBuffer<u8>,
    ctr: usize,
}

impl QemuRv32VirtChannel {
    pub const fn new() -> Self {
        QemuRv32VirtChannel {
            state: Cell::new(QemuRv32VirtChannelState::Init),
            buffer: RingBuffer::new(&mut SHARED_CHANNEL_BUFFER),
            ctr: 0,
        }
    }

    pub fn service(&self) {
        use QemuRv32VirtChannelState as S;

        match self.state.get() {
            S::Init => {
                let hart_id = CSR.mhartid.extract().get();
                let closure = |buf: &mut Buffer| -> usize {
                    buf.iter().fold(0, |acc, x| acc + *x as usize)
                };
                let res = unsafe {
                    CHANNEL_BUFFER.get_mut(DynThreadId::new(hart_id))
                        .expect("This hart does not have access to the QemuRv32VirtChannel")
                        .enter_nonreentrant(closure)
                };
                self.flush_local_buffer();
                unsafe {
                    crate::chip::MACHINE_SOFT_FIRED_COUNT.fetch_add(res, core::sync::atomic::Ordering::Relaxed);
                }
                self.state.replace(S::End);
            }
            S::Process => todo!(),
            S::End => {
                // TODO: Safety
                DeferredCallThread::unset();
                self.state.replace(S::Init);
            }
        }
    }

    fn flush_local_buffer(&self) {
        let hart_id = CSR.mhartid.extract().get();
        unsafe {
            CHANNEL_BUFFER.get_mut(DynThreadId::new(hart_id))
                .expect("This hart does not have access to the QemuRv32VertChannel")
                .enter_nonreentrant(|buf: &mut Buffer| {
                    *buf = core::mem::zeroed()
                })
        };
    }

}


impl DeferredCallClient for QemuRv32VirtChannel {
    fn handle_deferred_call(&self) {
        self.service()
    }

    fn register(&'static self) {
        DeferredCallThread::register(self)
    }
}


impl InterThreadChannel for Mutex<QemuRv32VirtChannel> {
    type Message   = IpcM;
    type MessageId = usize;

    fn send(&self, target: &dyn ThreadId, msg: Message) -> MessageId {
        let channel = self.lock();

        let id = CSR.mhartid.extract().get();
        if let Some(clic) =
            thread_local_static_access!(CLIC, DynThreadId::new(id))
        {
            let target_id = target.get_id();
            let uid = channel.ctr + 1;
            let message = InterThreadMessage::new(
                id as u8,
                target_id as u8,
                uid,
                msg
            );
            channel.ctr = uid;
            let _ = channel.enqueue(message).expect("QemuRv32VirtChannel overflowed");
            clic.set_soft_interrupt(target.get_id());

            uid
        } else {
            panic!("This hart does not have access to its CLIC");
        }

    }

    fn try_recv(&self, msg_id: MessageId) -> Option<(DynThreadId, Message)> {
        let channel = self.lock();

        let mut head = None;

        while Some(msg) = channel.dequeue() {
            if let Some(head_msg_id) = head {
                if head_msg_id == msg.uid {
                    break;
                }
            } else {
                head = Some(msg.uid)
            }

            let hart_id = CSR.mhartid.extract().get();
            let bitmask = 0b1 << hart_id;
            if msg_id == msg.id()
                && msg.is_recipient(hart_id as u8)
            {
                let receivers = msg.mark_recipient(hart_id as u8);
                if receivers != 0 {
                    channel.enqueue(msg);
                }
                return Some((
                    unsafe { DynThreadId::new(msg.sender as usize) },
                    msg.extract()
                ))
            } else {
                channel.enqueue(msg);
            }
        }

        None
    }

    fn recv(&self, msg_id: MessageId) -> Option<(DynThreadId, Message)> {
        while let Some(msg) = self.try_recv() {
            return Some(msg)
        }
    }

}
