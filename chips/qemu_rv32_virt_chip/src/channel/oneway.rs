//! One-way inter-kernel communication channel.

use core::cell::{Cell, OnceCell, UnsafeCell};

use kernel::deferred_call::{DeferredCall, DeferredCallClient};
use kernel::threadlocal::{ThreadLocal, ThreadLocalAccess, DynThreadId, ThreadLocalDyn};
use kernel::utilities::registers::interfaces::Readable;
use kernel::utilities::cells::OptionalCell;
use kernel::utilities::cells::TakeCell;
use kernel::smp::shared_channel::SharedChannel;
use kernel::smp::portal::Portalable;
use kernel::smp::mutex::Mutex;
use kernel::collections::queue::Queue;
use kernel::collections::ring_buffer::RingBuffer;
use kernel::hil::time::{Alarm, AlarmClient};

use rv32i::csr::CSR;

use crate::MAX_THREADS;
use crate::portal::{NUM_PORTALS, PORTALS, QemuRv32VirtPortal};
use crate::portal_cell::QemuRv32VirtPortalCell;
use crate::channel::{QemuRv32VirtChannel, QemuRv32VirtMessage, QemuRv32VirtMessageBody};

// pub struct QemuRv32VirtChannelOneWay<'a> {
//     shared_buffer: &'a [QemuRv32VirtMessage],
// }

// impl QemuRv32VirtChannelOneWay {
//     pub fn new(
//         shared_buffer: &'a [QemuRv32VirtMessage]
//     ) -> (QemuRv32VirtChannelOneWaySender, QemuRv32VirtChannelOneWayReceiver) {
//         (
//             QemuRv32VirtChannelOneWaySender::new(shared_buffer),
//             QemuRv32VirtChannelOneWayReceiver::new(shared_buffer),
//         )
//     }
// }

pub struct OneWayQemuRv32VirtChannelSender<'a, A: Alarm<'a>> {
    channel: UnsafeCell<RingBuffer<'a, Option<QemuRv32VirtMessage>>>,
    notified: Cell<usize>,
    alarm: &'a A,
}

static mut SHARED_BUFFER: &'static mut [Option<QemuRv32VirtMessage>] = &mut [None; 1];

impl<'a, A: Alarm<'a>> OneWayQemuRv32VirtChannelSender<'a, A> {
    pub fn new(
        local_buffer: &'a mut [Option<QemuRv32VirtMessage>],
        alarm: &'a A,
    ) -> OneWayQemuRv32VirtChannelSender<'a, A> {
        OneWayQemuRv32VirtChannelSender {
            alarm,
            channel: UnsafeCell::new(RingBuffer::new(local_buffer)),
            notified: Cell::new(0),
        }
    }

    pub fn do_service(&self) {
        use QemuRv32VirtMessageBody as M;

        let channel = unsafe { &mut *self.channel.get() };

        if let Some(Some(message)) = channel.dequeue() {
            match message.body {
                M::Response(res) => {
                    kernel::debug!("QemuRv32VirtChannelOneWaySender received response: {:?}", res);
                    // self.receive()
                }
                _ => panic!("Invalid message type for QemuRv32VirtChannelOneWaySender"),
            }
        }
    }

    pub fn send(&self, message: QemuRv32VirtMessage, duration: u32) {
        // TODO: safety
        let channel = unsafe { &mut *self.channel.get() };
        let shared_buffer = unsafe {
            &mut *core::ptr::addr_of_mut!(SHARED_BUFFER)
        };

        // Flush the shared buffer
        shared_buffer.fill(None);

        // Copy from the local buffer to the shared buffer
        for item in shared_buffer.iter_mut() {
            if let Some(copy) = channel.dequeue() {
                *item = copy;
            } else {
                break;
            }
        }

        // Empty the local buffer
        channel.empty();

        // Set alarm
        // TODO: change it to a reasonable amount of whatever
        self.alarm.set_alarm(self.alarm.now(), duration.into());

        // Notify the receiver
        let receiver = 1;
        let closure = move |c: &mut crate::chip::QemuRv32VirtClint| c.set_soft_interrupt(receiver);
        unsafe {
            crate::clint::with_clic_panic(closure);
        }
    }

    // Procedures when received a signal from the receiver
    fn retrieve_from_shared_buffer(&self) {
        // This is safe because the local channel is only exclusively used here.
        let channel = unsafe { &mut *self.channel.get() };
        // We guarantee that there's only one mutable reference of the shared buffer
        // at any given time.
        let shared_buffer = unsafe {
            &mut *core::ptr::addr_of_mut!(SHARED_BUFFER)
        };

        // Successfully received the message, disarm first
        if self.alarm.is_armed() {
            self.alarm
                .disarm()
                .unwrap()
        }

        // Empty the local buffer
        channel.empty();

        // Copy from the shared buffer to the local_buffer
        for item in shared_buffer.iter() {
            channel.enqueue(*item);
        }

        // Flush the shared buffer
        shared_buffer.fill(None);
    }
}

impl<'a, A: Alarm<'a>> AlarmClient for OneWayQemuRv32VirtChannelSender<'a, A> {
    fn alarm(&self) {
        panic!("Failed to meet the deadline, kill everything");
    }
}

impl<'a, A: Alarm<'a>> QemuRv32VirtChannel for OneWayQemuRv32VirtChannelSender<'a, A> {
    fn service(&self) {
        self.retrieve_from_shared_buffer();
        self.do_service();
    }

    fn service_async(&self) {
        let old_val = self.notified.get();
        self.notified.set(old_val + 1);
    }

    fn has_pending_requests(&self) -> bool {
        self.notified.get() != 0
    }

    fn complete(&self) {
        let old_val = self.notified.get();
        self.notified.set(old_val - 1);
    }

    fn write(&self, message: QemuRv32VirtMessage) -> bool {
        let channel = unsafe { &mut *self.channel.get() };
        if channel.enqueue(Some(message)) {
            self.send(message, u32::MAX.into());
        }
        true
    }
}

pub struct OneWayQemuRv32VirtChannelReceiver<'a, A: Alarm<'a>> {
    channel: UnsafeCell<RingBuffer<'a, Option<QemuRv32VirtMessage>>>,
    notified: Cell<usize>,
    alarm: &'a A,
}

impl<'a, A: Alarm<'a>> OneWayQemuRv32VirtChannelReceiver<'a, A> {
    pub fn new(
        local_buffer: &'a mut [Option<QemuRv32VirtMessage>],
        alarm: &'a A,
    ) -> OneWayQemuRv32VirtChannelReceiver<'a, A> {
        OneWayQemuRv32VirtChannelReceiver {
            alarm,
            channel: UnsafeCell::new(RingBuffer::new(local_buffer)),
            notified: Cell::new(0),
        }
    }

    pub fn do_service(&self) {
        use QemuRv32VirtMessageBody as M;
        let channel = unsafe { &mut *self.channel.get() };

        if let Some(Some(message)) = channel.dequeue() {
            match message.body {
                M::Request(res) => {
                    assert!(channel.enqueue(Some(QemuRv32VirtMessage {
                        src: 1,
                        dst: 0,
                        body: M::Response(res),
                    })));
                }
                _ => panic!("Invalid message type for QemuRv32VirtChannelOneWayReceiver"),
            }
        }
    }

    // Procedures to return responses to the sender.
    // This is used when getting alarmed by itself
    pub fn send(&self) {
        let channel = unsafe { &mut *self.channel.get() };
        let shared_buffer = unsafe {
            &mut *core::ptr::addr_of_mut!(SHARED_BUFFER)
        };

        // Flush the shared buffer
        shared_buffer.fill(None);

        // Copy from the local buffer to the shared buffer
        for item in shared_buffer.iter_mut() {
            if let Some(copy) = channel.dequeue() {
                *item = copy;
            } else {
                break;
            }
        }

        // Empty the local buffer
        channel.empty();

        // Notify the receiver
        let receiver = 0;
        let closure = move |c: &mut crate::chip::QemuRv32VirtClint| c.set_soft_interrupt(receiver);
        unsafe {
            crate::clint::with_clic_panic(closure);
        }
    }

    // This is called when getting a signal from the sender.
    // This copies contents in the shared buffer to its local buffer.
    fn receive(&self, duration: u32) {
        let channel = unsafe { &mut *self.channel.get() };
        let shared_buffer = unsafe {
            &mut *core::ptr::addr_of_mut!(SHARED_BUFFER)
        };

        // Set alarm to respond on time
        if self.alarm.is_armed() {
            self.alarm
                .disarm()
                .unwrap()
        }
        self.alarm.set_alarm(self.alarm.now(), duration.into());

        // Empty the local buffer
        channel.empty();

        // Copy from the shared buffer to the local_buffer
        for item in shared_buffer.iter() {
            channel.enqueue(*item);
        }

        // Flush the shared buffer
        shared_buffer.fill(None);
    }
}

impl<'a, A: Alarm<'a>> AlarmClient for OneWayQemuRv32VirtChannelReceiver<'a, A> {
    fn alarm(&self) {
        self.send()
    }
}

impl<'a, A: Alarm<'a>> QemuRv32VirtChannel for OneWayQemuRv32VirtChannelReceiver<'a, A> {
    fn service(&self) {
        self.do_service();
    }

    fn service_async(&self) {
        use kernel::hil::time::{ConvertTicks, Ticks};
        let ticks_1sec = unsafe {
            crate::clint::with_clic_panic(|c| c.ticks_from_seconds(1).into_u32())
        };

        let old_val = self.notified.get();
        self.notified.set(old_val + 1);
        self.receive(ticks_1sec);
    }

    fn has_pending_requests(&self) -> bool {
        self.notified.get() != 0
    }

    fn complete(&self) {
        let old_val = self.notified.get();
        self.notified.set(old_val - 1);
    }

    fn write(&self, message: QemuRv32VirtMessage) -> bool {
        let channel = unsafe { &mut *self.channel.get() };
        channel
            .enqueue(Some(message))
    }
}

// -----------------------------------------------------------------------------------------

// pub struct QemuRv32VirtChannelOneWay<'a> {
//     shared_buffer: &'a UnsafeCell<[Option<QemuRv32VirtMessage>]>,
//     channel: &'a RingBuffer<'a, Option<QemuRv32VirtMessage>>,
//     notified: Cell<usize>,
//     role: OnceCell<QemuRv32VirtChannelOneWayRole>,
//     alarm: 
// }


// // pub struct QemuRv32VirtChannelOneWaySender<'a> {
// //     shared_buffer: &'a UnsafeCell<[Option<QemuRv32VirtMessage>]>,
// //     channel: &'a RingBuffer<'a, Option<QemuRv32VirtMessage>>,
// //     notified: Cell<usize>,
// //     role: OnceCell<QemuRv32VirtChannelOneWayRole>,
// // }

// // pub struct QemuRv32VirtChannelOneWayReceiver<'a> {
// //     shared_buffer: &'a UnsafeCell<[Option<QemuRv32VirtMessage>]>,
// //     channel: &'a RingBuffer<'a, Option<QemuRv32VirtMessage>>,
// //     notified: Cell<usize>,
// //     role: OnceCell<QemuRv32VirtChannelOneWayRole>,
// // }

// pub struct QemuRv32VirtChannelOneWay<'a> {
//     shared_buffer: &'a UnsafeCell<[Option<QemuRv32VirtMessage>]>,
//     channel: RingBuffer<'a, Option<QemuRv32VirtMessage>>,
//     notified: Cell<usize>,
//     role: OnceCell<QemuRv32VirtChannelOneWayRole>,
//     alarm: 
// }

// impl<'a> QemuRv32VirtChannelOneWay<'a> {
//     pub fn new(
//         shared_buffer: &'a [Option<QemuRv32VirtMessage>],
//         local_buffer: &'a [Option<QemuRv32VirtMessage>],
//         role: QemuRv32VirtChannelOneWayRole,
//     ) -> Self {
//         let locked_role = oncecell::new();
//         locked_role.set(role);
//         QemuRv32VirtChannelOneWay {
//             shared_buffer,
//             channel: RingBuffer::new(local_buffer),
//             notified: Cell::new(0),
//             role: locked_role,
//         }
//     }

//     fn find<P>(
//         channel: &mut RingBuffer<'a, Option<QemuRv32VirtMessage>>,
//         predicate: P
//     ) -> Option<QemuRv32VirtMessage>
//     where
//         P: Fn(&QemuRv32VirtMessage) -> bool
//     {
//         let mut len = channel.len();
//         while len != 0 {
//             let msg = channel.dequeue()
//                 .expect("Invalid QemuRv32VirtChannelOneWay State")
//                 .expect("Invalid Message Type");
//             if predicate(&msg) {
//                 return Some(msg)
//             }
//             channel.enqueue(Some(msg));
//             len -= 1;
//         }
//         None
//     }

//     pub fn service(&self) {
//         let hart_id = CSR.mhartid.extract().get();

//         // Acquire the mutex for the entire operation to reserve a slot for a potential
//         // response. Invoking teleport inside the scope will result in a deadlock.
//         // TODO: switch to a non-blocking channel
//         let mut channel = self.channel.lock();
//         if let Some(msg) = Self::find(&mut channel, |msg| msg.dst == hart_id) {
//             use QemuRv32VirtMessageBody as M;
//             match msg.body {
//                 M::PortalRequest(portal_id) => {
//                     let closure = |ps: &mut [QemuRv32VirtPortal; NUM_PORTALS]| {
//                         use QemuRv32VirtPortal as P;
//                         let traveler = match ps[portal_id] {
//                             P::Uart16550(val) => {
//                                 let portal = unsafe {
//                                     &*(val as *const QemuRv32VirtPortalCell<crate::uart::Uart16550>)
//                                 };
//                                 assert!(portal.get_id() == portal_id);
//                                 if let Some(uart) = portal.take() {
//                                     uart.save_context();
//                                     unsafe {
//                                         kernel::thread_local_static_access!(crate::plic::PLIC, DynThreadId::new(hart_id))
//                                             .expect("Unable to access thread-local PLIC controller")
//                                             .enter_nonreentrant(|plic| {
//                                                 plic.disable(hart_id * 2,
//                                                              (crate::interrupts::UART0 as u32).try_into().unwrap());
//                                             });
//                                     }
//                                     Some(uart as *mut _ as *const _)
//                                 } else {
//                                     None
//                                 }
//                             }
//                             P::Counter(val) => {
//                                 let portal = unsafe {
//                                     &*(val as *const QemuRv32VirtPortalCell<usize>)
//                                 };
//                                 assert!(portal.get_id() == portal_id);
//                                 portal.take().map(|val| val as *mut _ as *const _)
//                             }
//                             _ => panic!("Invalid Portal"),
//                         };

//                         if let Some(val) = traveler {
//                             assert!(channel.enqueue(Some(QemuRv32VirtMessage {
//                                 src: hart_id,
//                                 dst: msg.src,
//                                 body: QemuRv32VirtMessageBody::PortalResponse(
//                                     portal_id,
//                                     val
//                                 ),
//                             })));

//                             unsafe {
//                                 kernel::thread_local_static_access!(crate::clint::CLIC, DynThreadId::new(hart_id))
//                                     .expect("This thread does not have access to CLIC")
//                                     .enter_nonreentrant(|clic| {
//                                         clic.set_soft_interrupt(msg.src);
//                                     })
//                             };
//                         }
//                     };

//                     unsafe {
//                         (&*core::ptr::addr_of!(PORTALS))
//                             .get_mut()
//                             .expect("This thread doesn't not have access to its local portals")
//                             .enter_nonreentrant(closure);
//                     }
//                 }
//                 M::PortalResponse(portal_id, traveler) => {
//                     let closure = |ps: &mut [QemuRv32VirtPortal; NUM_PORTALS]| {
//                         use QemuRv32VirtPortal as P;
//                         match ps[portal_id] {
//                             P::Uart16550(val) => {
//                                 let portal = unsafe {
//                                     &*(val as *const QemuRv32VirtPortalCell<crate::uart::Uart16550>)
//                                 };
//                                 assert!(portal.get_id() == portal_id);
//                                 portal.link(unsafe { &mut *(traveler as *mut _) })
//                                     .expect("Failed to link the uart portal");

//                                 portal.enter(|uart| {
//                                     uart.restore_context();
//                                     // Enable uart interrupts
//                                     unsafe {
//                                         kernel::thread_local_static_access!(crate::plic::PLIC, DynThreadId::new(hart_id))
//                                             .expect("Unable to access thread-local PLIC controller")
//                                             .enter_nonreentrant(|plic| {
//                                                 plic.enable(hart_id * 2, (crate::interrupts::UART0 as u32).try_into().unwrap());
//                                             });
//                                     }
//                                     // Try continue from the last transmit in case of missing interrupts
//                                     let _ = uart.try_transmit_continue();
//                                 });
//                             }
//                             P::Counter(val) => {
//                                 let portal = unsafe {
//                                     &*(val as *const QemuRv32VirtPortalCell<usize>)
//                                 };
//                                 assert!(portal.get_id() == portal_id);
//                                 portal.link(unsafe { &mut *(traveler as *mut _) })
//                                     .expect("Failed to link the counter portal");
//                             }
//                             _ => panic!("Invalid Portal"),
//                         };
//                     };

//                     unsafe {
//                         (&*core::ptr::addr_of!(PORTALS))
//                             .get_mut()
//                             .expect("This thread doesn't not have access to its local portals")
//                             .enter_nonreentrant(closure);
//                     }
//                 }
//                 _ => panic!("Unsupported message"),
//             }
//         }
//     }

//     pub fn service_async(&self) {
//         let old_val = self.notified.get();
//         self.notified.set(old_val + 1);
//     }

//     pub fn has_pending_requests(&self) -> bool {
//         self.notified.get() != 0
//     }

//     pub fn service_complete(&self) {
//         let old_val = self.notified.get();
//         self.notified.set(old_val - 1);
//     }

//     fn write(&mut self, message: Self::Message) -> bool {
//         use QemuRv32VirtChannelOneWayRole as R;

//         // Flush the shared buffer
//         let shared_buffer = unsafe { &mut *self.shared_buffer.get() };
//         shared_buffer.fill(None);

//         // Copy from the local buffer to the shared buffer
//         for item in shared_buffer.iter_mut() {
//             if let copy @ Some(_) = self.channel.dequeue() {
//                 *item = copy;
//             } else {
//                 break;
//             }
//         }

//         // Empty the local buffer
//         self.channel.empty();

//         let role = self.role.get().expect("No role defined for the one-way channel");
//         match role {
//             R::Sender => {
//                 // set fuser
//                 // notify
//             }
//             R::Receiver => {
//                 // notify
//             }
//         }
//     }

//     fn read(&self) -> Option<Self::Message> {
//         let role = self.role.get().expect("No role defined for the one-way channel");
//         match role {
//             R::Sender => {
//                 // defuse
//             }
//             R::Receiver => {
//                 // set up alarm for response
//             },
//         }

//         // Empty the local buffer
//         self.channel.empty();

//         // Copy from the shared buffer to the shared buffer
//         let shared_buffer = unsafe { &mut *self.shared_buffer.get() };
//         for item in shared_buffer.iter() {
//             self.channel.enqueue(*item);
//         }

//         // Flush the shared buffer
//         shared_buffer.fill(None);



//         self.channel
//             .lock()
//             .dequeue()
//             .map(|val| val.expect("Invalid message"))
//     }
// }

// // --------------------------------------------------------------------------------


// use kernel::hil::time::{self, Alarm, Frequency, Ticks, Ticks32, AlarmClient};

// pub struct ClosureAlarm<'a, A: Alarm<'a>, F: Fn()> {
//     alarm: &'a A,
//     closure: F,
// }

// impl<'a, A: Alarm<'a>, F: Fn()> ClosureAlarm<'a, A, F> {
//     pub const fn new(alarm: &'a A, closure: F) -> ClosureAlarm<'a, A, F> {
//         ClosureAlarm {
//             alarm,
//             closure,
//         }
//     }

//     fn disarm(&self) {
//         if self.alarm.is_armed() {
//             self.alarm
//                 .disarm()
//                 .expect("closurealarm is unable to disarm");
//         }
//     }

//     fn set_alarm(&self, duration: usize) {
//         self.disarm();
//         self.alarm.set_alarm(...);
//     }
// }

// impl<'a, A: Alarm<'a>, F: FnMut()> AlarmClient for ClosureAlarm<'a, A, F> {
//     fn alarm(&self) {
//         (self.closure)();
//     }
// }
