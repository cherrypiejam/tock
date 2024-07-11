/// Inter-thread channel

use crate::threadlocal::{ThreadId, DynThreadId};

pub trait InterThreadChannel {
    type MessageId;

    fn write(&self, target: &dyn ThreadId, msg: &InterThreadMessage) -> MessageId {}

    fn try_read(&self, msg_id: &MessageId) -> Option<(DynThreadId, InterThreadMessage)> { None }

    fn read(&self, msg_id: MessageId) -> Option<(DynThreadId, InterThreadMessage)> { None }

    // fn request(&self, target: &dyn ThreadId, msg: &InterThreadMessage) -> Option<InterThreadMessage> {
    //     let msg_id = self.send(target, msg);
    //     self.recv(msg_id).map(|(_, m)| m)
    // }
}

use crate::ipc::IPCInterThreadMessage;

pub struct InterThreadMessageHeader {
    pub sender: u8,
    pub receivers: usize,
    pub uid: usize,
}

pub enum InterThreadMessageBody {
    Invalid,
    Ipc(IPCInterThreadMessage),
}

pub struct InterThreadMessage(InterThreadMessageHeader, InterThreadMessageBody);

impl InterThreadMessage {
    pub const fn empty() -> Self {
        Self(InterThreadMessageHeader::empty(), InterThreadMessageBody::Invalid)
    }

    pub fn header(&self) -> &InterThreadMessageHeader {
        &self.0
    }

    pub fn body(&self) -> &InterThreadMessageBody {
        &self.1
    }

    pub fn to_response(&self, sender: &dyn ThreadId, body: InterThreadMessageBody) -> Self {
        let receiver = unsafe { DynThreadId::new(self.0.sender) }
        let header = InterThreadMessageHeader::new(sender, )
        Self(body)
    }
}

impl InterThreadMessageHeader {
    pub const fn empty() -> Self {
        Self {
            sender: 0,
            receivers: 0,
            uid: 0,
        }
    }

    pub fn new(sender: &dyn ThreadId, receiver: &dyn ThreadId, uid: usize) -> Self {
        Self {
            sender: sender.get_id() as u8,
            receivers: 0b1 << receiver.get_id(),
            uid,
        }
    }
}

impl InterThreadMessageWrapper {
    pub const fn empty() -> Self {
        Self {
            sender: 0,
            receivers: 0,
            uid: 0,
            body: InterThreadMessage::Invalid,
        }
    }

    pub fn new(sender: u8, receiver: u8, uid: usize, body: T) -> Self {
        Self {
            sender,
            receivers: 0b1 << receiver,
            uid,
            body,
        }
    }

    pub fn id(&self) -> usize {
        self.uid
    }

    pub fn is_recipient(&self, receiver: u8) -> bool {
        let bitmask = 0b1 << receiver;
        self.msg.receivers & bitmask
    }

    pub fn mark_recipient(&mut self, receiver: u8) -> usize {
        let bitmask = 0b1 << receiver;
        self.receivers &= !bitmask;
        self.receivers
    }

    pub fn extract(&self) -> InterThreadMessage {
        self.message
    }
}

// pub enum InterThreadMessage {
//     Invalid,
//     RawData([u8; 8]),
//     IpcDiscover,
//     IpcDiscoverResponse(usize),
// }

impl InterThreadChannel for () {}

