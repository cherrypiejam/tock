use kernel::threadlocal::{ThreadLocal, ThreadLocalDyn};

pub mod shared;
pub mod oneway;


static NO_CHANNEL: ThreadLocal<0, Option<&'static dyn QemuRv32VirtChannel>> = ThreadLocal::new([]);

static mut SHARED_CHANNEL: &'static dyn ThreadLocalDyn<Option<&'static dyn QemuRv32VirtChannel>> = &NO_CHANNEL;

pub unsafe fn set_shared_channel(
    shared_channel: &'static dyn ThreadLocalDyn<Option<&'static dyn QemuRv32VirtChannel>>
) {
    *core::ptr::addr_of_mut!(SHARED_CHANNEL) = shared_channel;
}

unsafe fn with_shared_channel<R, F>(f: F) -> Option<R>
where
    F: FnOnce(&mut Option<&'static dyn QemuRv32VirtChannel>) -> R
{
    let threadlocal: &'static dyn ThreadLocalDyn<_> = *core::ptr::addr_of_mut!(SHARED_CHANNEL);
    threadlocal
        .get_mut().map(|c| c.enter_nonreentrant(f))
}

pub unsafe fn with_shared_channel_panic<R, F>(f: F) -> R
where
    F: FnOnce(&mut Option<&'static dyn QemuRv32VirtChannel>) -> R
{
    with_shared_channel(f).expect("Current thread does not have access to its shared channel")
}

pub trait QemuRv32VirtChannel {
    fn service(&self);

    fn service_async(&self);

    fn complete(&self);

    fn has_pending_requests(&self) -> bool;

    fn write(&self, message: QemuRv32VirtMessage) -> bool;
}

#[derive(Copy, Clone, Debug)]
pub struct QemuRv32VirtMessage {
    pub src: usize,
    pub dst: usize,
    pub body: QemuRv32VirtMessageBody,
}

#[derive(Copy, Clone, Debug)]
pub enum QemuRv32VirtMessageBody {
    PortalRequest(usize),
    PortalResponse(usize, *const ()),
    // For ony-way channel
    Request(u32),
    Response(u32),
}
