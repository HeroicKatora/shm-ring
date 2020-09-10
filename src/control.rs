use super::{ReadHalf, ShmController, ShmQueues};

/// A tag is a payload that will be returned as part of the response.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Tag(pub u32);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ControlMessage {
    pub op: u64,
}

pub struct ControlResponse<'ring> {
    pub msg: ControlMessage,
    ring: &'ring ReadHalf<'ring>,
}

/// Request a new ring.
pub struct RequestNewRing {
    pub payload: Tag,
}

/// Answer to a requested ring.
pub struct ProvideNewRing {
    /// Offset of the head control structure (hint: the requester is partner A).
    pub head: u32,
    /// The payload sent.
    pub payload: Tag,
}

/// Send a parameterless ping.
pub struct Ping {
    pub payload: Tag,
}

impl ControlMessage {
    pub const BAD: u32 = !0;
    pub const UNIMPLEMENTED: u32 = !0 - 1;
    pub const REQUEST_NEW_RING: u32 = 1;
    pub const REQUEST_PING: u32 = 2;

    /// Create a new message.
    ///
    /// Exists for document purposes, see the list of implementors of `Into` and `From`.
    pub fn new(msg: impl Into<Self>) -> Self {
        msg.into()
    }

    fn with_raw(op: u32, Tag(payload): Tag) -> Self {
        ControlMessage {
            op: u64::from(op) << 32 | u64::from(payload)
        }
    }

    fn payload(self) -> Tag {
        Tag(self.op as u32)
    }

    fn operation(self) -> u32 {
        (self.op >> 32) as u32
    }
}

impl From<RequestNewRing> for ControlMessage {
    fn from(RequestNewRing { payload }: RequestNewRing) -> Self {
        ControlMessage::with_raw(ControlMessage::REQUEST_NEW_RING, payload)
    }
}

impl From<Ping> for ControlMessage {
    fn from(Ping { payload }: Ping) -> Self {
        ControlMessage::with_raw(ControlMessage::REQUEST_PING, payload)
    }
}

/// Private implementation of commands.
impl ControlMessage {
    pub(crate) fn execute(controller: &ShmController, mut queue: ShmQueues) {
        if queue.half_to_write_to().prepare_write(8).is_none() {
            // No space for response. Don't.
            return;
        }

        let mut reader = queue.half_to_read_from();
        if let Some(available) = reader.available(8) {
            let op = available.controller_u64();
            let msg = ControlMessage { op };
            let tag = msg.payload();
            let cmd = msg.operation();
            let (mut op, extra_space);
            match cmd {
                ControlMessage::REQUEST_PING => {
                    op = ControlMessage::REQUEST_PING;
                    extra_space = 0;
                }
                _ => {
                    op = ControlMessage::UNIMPLEMENTED;
                    extra_space = 0;
                }
            };
            if extra_space > 0 {
                op = ControlMessage::UNIMPLEMENTED;
            }
            // At this point, commit to writing a result.
            available.commit();

            let answer = ControlMessage::with_raw(op, tag);
            let mut writer = queue.half_to_write_to();
            let mut write = writer.prepare_write(8).unwrap();
            write.controller_u64(answer.op);
            write.commit();
        }
    }
}

impl<'ring> ControlResponse<'ring> {
    pub fn new(ring: &'ring ReadHalf, op: [u8; 8]) -> Self {
        ControlResponse {
            ring: ring,
            msg: ControlMessage {
                op: u64::from_be_bytes(op),
            },
        }
    }

    pub fn payload(&self) -> Tag {
        self.msg.payload()
    }

    pub fn operation(&self) -> u32 {
        self.msg.operation()
    }
}
