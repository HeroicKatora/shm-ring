use super::{ReadHalf, ShmController, ShmQueues};

/// A tag is an identifier for a pending request.
///
/// The command server has no internal buffer so this should be more than enough. However even for
/// larger buffers this has 65536 entries.. This is also just a strategy, any custom queue may use
/// a different request structure or even dynamically change it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tag(pub u16);

/// The command to invoke, respectively its response if multiple are possible.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cmd(pub u16);

/// A message descriptor on the control queue.
///
/// These must always be passed 8-byte aligned but since there are no other messages this should
/// not present a problem.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ControlMessage {
    /// The operand of the message. For requests the bit use is as follows (high-to-low)
    ///
    /// ```
    /// |   3    |   2    |   1    |   0    |
    /// | First Argument  |        |
    /// |        |        |Command |
    /// |        |        |        |  Tag 
    /// ```
    ///
    /// For responses on the other hand:
    ///
    /// ```
    /// |   3    |   2    |   1    |   0    |
    /// | First Result    |        |
    /// |        |        |Response|
    /// |        |        |        |  Tag 
    /// ```
    ///
    /// However there is no request with more than one argument currently defined. In any case, the
    /// plan is to use the first argument as an offset into the queue memory if that ever becomes
    /// necessary.
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

impl Cmd {
    pub const BAD: Self = Cmd(!0);
    pub const UNIMPLEMENTED: Self = Cmd(!0 - 1);
    pub const OUT_OF_QUEUES: Self = Cmd(!0 - 2);
    pub const REQUEST_NEW_RING: Self = Cmd(1);
    pub const REQUEST_PING: Self = Cmd(2);
}

impl ControlMessage {
    /// Create a new message.
    ///
    /// Exists for document purposes, see the list of implementors of `Into` and `From`.
    pub fn new(msg: impl Into<Self>) -> Self {
        msg.into()
    }

    fn with_raw(Cmd(op): Cmd, Tag(tag): Tag) -> Self {
        ControlMessage {
            op: u64::from(op) << 16 | u64::from(tag)
        }
    }

    fn with_result(self, val: u32) -> Self {
        ControlMessage { op: self.op | u64::from(val) << 32 }
    }

    fn tag(self) -> Tag {
        Tag(self.op as u16)
    }

    fn cmd(self) -> Cmd {
        Cmd((self.op >> 16) as u16)
    }
}

impl From<RequestNewRing> for ControlMessage {
    fn from(RequestNewRing { payload }: RequestNewRing) -> Self {
        ControlMessage::with_raw(Cmd::REQUEST_NEW_RING, payload)
    }
}

impl From<Ping> for ControlMessage {
    fn from(Ping { payload }: Ping) -> Self {
        ControlMessage::with_raw(Cmd::REQUEST_PING, payload)
    }
}

/// Private implementation of commands.
impl ControlMessage {
    pub(crate) fn execute(controller: &mut ShmController, mut queue: ShmQueues) {
        if queue.half_to_write_to().prepare_write(8).is_none() {
            // No space for response. Don't.
            return;
        }

        let mut reader = queue.half_to_read_from();
        if let Some(available) = reader.available(8) {
            let op = available.controller_u64();
            let msg = ControlMessage { op };
            let tag = msg.tag();
            let cmd = msg.cmd();

            let mut result = 0;
            let (mut op, extra_space);
            match cmd {
                Cmd::REQUEST_PING => {
                    op = Cmd::REQUEST_PING;
                    extra_space = 0;
                }
                _ => {
                    let queues = &mut controller.state.free_queues;
                    if let Some(queue) = queues.pop() {
                        op = Cmd::REQUEST_NEW_RING;
                        result = queue;
                    } else {
                        op = Cmd::OUT_OF_QUEUES;
                    }

                    extra_space = 0;
                }
            };
            if extra_space > 0 {
                op = Cmd::UNIMPLEMENTED;
            }
            // At this point, commit to writing a result.
            available.commit();

            let answer = ControlMessage::with_raw(op, tag).with_result(result);
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

    pub fn tag(&self) -> Tag {
        self.msg.tag()
    }

    pub fn response(&self) -> Cmd {
        self.msg.cmd()
    }

    pub fn value0(&self) -> u32 {
        (self.msg.op >> 32) as u32
    }
}
