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

    // Timing safety: not depending on the receiver side
    pub fn do_service(&self) {
        use QemuRv32VirtMessageBody as M;

        let channel = unsafe { &mut *self.channel.get() };

        if let Some(Some(message)) = channel.dequeue() {
            match message.body {
                M::Response(res) => {
                    kernel::debug!("QemuRv32VirtChannelOneWaySender received response: {:?}", res);
                    // TODO: forward the response to the sender-local IPC mechanism
                }
                _ => panic!("Invalid message type for QemuRv32VirtChannelOneWaySender"),
            }
        }
    }

    // Timing safety: its timing doesn't not depend on the receiver
    // as long as we guarantee no concurrent access of the shared buffer
    // between the sender and the receiver.
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

    // This is called to retrieve responses from the receiver for later processing.
    fn service_prepare(&self) {
        // This is safe because the local channel is only exclusively used here.
        let channel = unsafe { &mut *self.channel.get() };

        // Timing safety: the timing of accessing shared_buffer does not depend on
        // the other side. This protocol _must_ guarantee that there's no concurrent
        // access to the shared buffer in parallel at any given time.
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
        shared_buffer.iter().for_each(|item| {
            let _ = channel.push(*item).ok_or(()).unwrap_err();
        });

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
    // Timing safety: not depending on the receiver.
    // The timing of this method may result in only 1-bit leak when it misses
    // the deadline to retrieve the message.
    fn service(&self) {
        self.service_prepare();
        self.do_service();
    }

    // Top half when getting a response from the receiver.
    // Timing safety: not depending on the receiver
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

    // Must be writable, timing doesn't matter as its sender-dependent
    fn write(&self, message: QemuRv32VirtMessage) -> bool {
        let channel = unsafe { &mut *self.channel.get() };
        let success = channel.enqueue(Some(message));
        if success {
            self.send(message, u32::MAX.into());
        }
        success
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

    // This method is invoked to return the actual response to the sender.
    pub fn do_service_finalize(&self) {
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

    // This is called when receiving a signal from the sender
    fn service_prepare(&self, duration: u32) {
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
        self.do_service_finalize()
    }
}

impl<'a, A: Alarm<'a>> QemuRv32VirtChannel for OneWayQemuRv32VirtChannelReceiver<'a, A> {
    fn service(&self) {
        self.do_service();
    }

    // This is called when the receiver receives a signal from the sender and served
    // as the channel-specific code of the blocking top half. For timing safety, we
    // need to save the received message, set an alarm for response, and immediately
    // encrypt the shared buffer with a nonce to fake a non-distinguishable response.
    fn service_async(&self) {
        // Saved the signal for the bottom half.
        let old_val = self.notified.get();
        self.notified.set(old_val + 1);

        // Top half
        use kernel::hil::time::{ConvertTicks, Ticks};
        let ticks_1sec = unsafe {
            crate::clint::with_clic_panic(|c| c.ticks_from_seconds(1).into_u32())
        };

        self.service_prepare(ticks_1sec);
    }

    fn has_pending_requests(&self) -> bool {
        self.notified.get() != 0
    }

    // TODO: this does not need to be in a critical section because no futher
    // interrupt that modifies its local state will be fired and handled. This
    // must be guaranteed by the sender's TCB.
    fn complete(&self) {
        let old_val = self.notified.get();
        self.notified.set(old_val - 1);
    }

    // This is called when the receiver writes a response locally. Similarly,
    // this function doesn't not need to sit in a critical section because no
    // `service_async` will be triggered. Is this true?
    // FIXME: timer interrupts are not handled immediately
    fn write(&self, message: QemuRv32VirtMessage) -> bool {
        let channel = unsafe { &mut *self.channel.get() };
        channel
            .enqueue(Some(message))
    }
}
