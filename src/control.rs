use super::{
    ActiveQueue,
    Events,
    OpenClient,
    OpenServer,
    ReadHalf,
    ShmController,
    ShmControllerRing,
    ShmQueue,
};
use std::collections::HashMap;

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
// TODO: may want to change Debug to something struct-like.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct ControlMessage {
    /// The operand of the message. For requests the bit use is as follows (high-to-low)
    ///
    /// ```text
    /// |   3    |   2    |   1    |   0    |
    /// | First Argument  |        |
    /// |        |        |Command |
    /// |        |        |        |  Tag 
    /// ```
    ///
    /// For responses on the other hand:
    ///
    /// ```text
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

/// Request a new pipe as a 'server' (queue head A).
///
/// The `public_id` is used as the first argument.
pub struct PipeRequest {
    /// An identifier with which other clients can request this ring.
    /// The method of discovering which id to use is out-of-scope for the shm-controller itself. A
    /// useful mechanism for statically determined networks may be unique IDs.
    pub public_id: u32,
    /// The tag identifying the request.
    pub tag: Tag,
}

/// Join a pipe with a public id as a client (queue head B).
pub struct PipeJoin {
    /// The identifier that was used when creating the server.
    /// When this matches multiple servers then any of these is chosen (Internally, the first.. But
    /// this detail is not yet stable. Don't rely on the order yet).
    pub public_id: u32,
    /// The tag identifying the request.
    pub tag: Tag,
}

/// Purposefully leave a pipe.
pub struct PipeLeave {
    /// The queue ID that you want to leave.
    pub queue: u32,
    /// The tag identifying the request.
    pub tag: Tag,
}

/// Send a parameterless ping.
pub struct Ping {
    pub payload: Tag,
}

pub trait Request {
    type Response;
    /// Set the tag that the poll structure has chosen.
    fn set_tag(&mut self, _: Tag);
    /// Get the tag of the request.
    fn tag(&self) -> Tag;
    /// Interpret the returned message as a response.
    fn response(&self, _: ControlResponse<'_>) -> Self::Response;
}

/// A structure with which to poll a control queue.
///
/// The message structure works the same in both directions.
pub struct Poll {
    /// The ring we encapsulate.
    ring: ShmControllerRing,
    /// A buffer of recent responses.
    responses: HashMap<Tag, ControlMessage>,
}

impl Cmd {
    pub const BAD: Self = Cmd(!0);
    pub const UNIMPLEMENTED: Self = Cmd(!0 - 1);
    pub const OUT_OF_QUEUES: Self = Cmd(!0 - 2);

    /// Create a new ring as a server.
    pub const REQUEST_PIPE: Self = Cmd(1);
    /// A ping will immediately be answered by a ping back.
    pub const REQUEST_PING: Self = Cmd(2);
    /// A client connection to an existing server.
    pub const PIPE_JOIN: Self = Cmd(3);
    /// Advertise a server host, but do not yet create a queue.
    /// More or less a listening socket.
    pub const SERVER_HOST: Self = Cmd(4);
    /// Remove ourselves from a channel, giving the spot up.
    pub const PIPE_LEAVE: Self = Cmd(5);
}

impl ControlMessage {
    /// Create a new message.
    ///
    /// Exists for document purposes, see the list of implementors of `Into` and `From`.
    pub fn new(msg: impl Into<Self>) -> Self {
        msg.into()
    }

    /// Add an argument to the control message. See message encoding.
    pub fn with_argument(self, val: u32) -> Self {
        // Same encoding..
        self.with_result(val)
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

    fn value0(self) -> u32 {
        (self.op >> 32) as u32
    }
}

impl From<PipeRequest> for ControlMessage {
    fn from(PipeRequest { tag, public_id }: PipeRequest) -> Self {
        ControlMessage::with_raw(Cmd::REQUEST_PIPE, tag).with_argument(public_id)
    }
}

impl From<Ping> for ControlMessage {
    fn from(Ping { payload }: Ping) -> Self {
        ControlMessage::with_raw(Cmd::REQUEST_PING, payload)
    }
}

impl From<PipeJoin> for ControlMessage {
    fn from(PipeJoin { tag, public_id }: PipeJoin) -> Self {
        ControlMessage::with_raw(Cmd::PIPE_JOIN, tag).with_argument(public_id)
    }
}

impl From<PipeLeave> for ControlMessage {
    fn from(PipeLeave { tag, queue }: PipeLeave) -> Self {
        ControlMessage::with_raw(Cmd::PIPE_LEAVE, tag).with_argument(queue)
    }
}

/// Private implementation of commands.
impl ControlMessage {
    pub(crate) fn execute(
        controller: &mut ShmController,
        queue_owner: u32,
        mut queue: ShmQueue<'_>,
        mut events: Option<&mut Events>,
    ) {
        let queues = &mut queue.queues;
        if queues.half_to_write_to().prepare_write(8).is_none() {
            // No space for response. Don't.
            return;
        }

        let mut reader = queues.half_to_read_from();
        if let Some(available) = reader.available(8) {
            let op = available.controller_u64();
            let msg = ControlMessage { op };
            let tag = msg.tag();
            let cmd = msg.cmd();

            let mut result = 0;
            let (mut op, extra_space);
            match cmd {
                Cmd::REQUEST_PING => {
                    extra_space = 0;
                    op = Cmd::REQUEST_PING;
                }
                Cmd::REQUEST_PIPE => {
                    extra_space = 0;

                    let queue = controller.state.free_queues.pop();

                    if let Some(queue) = queue {
                        controller.state.open_server.push(OpenServer {
                            queue,
                            user_tag: msg.value0(),
                            a: queue_owner,
                        });
                        op = Cmd::REQUEST_PIPE;
                        result = queue;
                    } else {
                        op = Cmd::OUT_OF_QUEUES;
                    }
                }
                Cmd::PIPE_JOIN => {
                    extra_space = 0;

                    let matching = controller.state.open_server.iter().position(|open| {
                        open.user_tag == msg.value0()
                    });

                    if let Some(matching) = matching {
                        let open = controller.state.open_server.remove(matching);
                        controller.state.active.push(ActiveQueue {
                            queue: open.queue,
                            user_tag: open.user_tag,
                            a: open.a,
                            b: queue_owner,
                        });

                        op = Cmd::REQUEST_PIPE;
                        result = open.queue;
                    } else {
                        op = Cmd::OUT_OF_QUEUES;
                    }
                }
                Cmd::PIPE_LEAVE => {
                    extra_space = 0;

                    let matching = controller.state.active.iter().position(|active| {
                        active.queue == msg.value0()
                    });

                    if let Some(matching) = matching {
                        if controller.state.active[matching].a == queue_owner {
                            let active = controller.state.active.remove(matching);
                            controller.state.open_client.push(OpenClient {
                                queue: active.queue,
                                user_tag: active.user_tag,
                                b: active.b,
                            });

                            op = Cmd::PIPE_LEAVE;
                        } else if controller.state.active[matching].b == queue_owner {
                            let active = controller.state.active.remove(matching);
                            controller.state.open_server.push(OpenServer {
                                queue: active.queue,
                                user_tag: active.user_tag,
                                a: active.a,
                            });

                            op = Cmd::PIPE_LEAVE;
                        } else {
                            // This message from the wrong person. Just don't do anything, but
                            // answer something to make them aware.
                            op = Cmd::OUT_OF_QUEUES;
                        }
                    } else {
                        op = Cmd::OUT_OF_QUEUES;
                    }
                }
                _ => {
                    extra_space = 0;
                    op = match cmd {
                        Cmd::SERVER_HOST => Cmd::UNIMPLEMENTED,
                        _ => Cmd::BAD,
                    };
                }
            };
            if extra_space > 0 {
                op = Cmd::UNIMPLEMENTED;
            }
            // At this point, commit to writing a result.
            available.commit();

            let answer = ControlMessage::with_raw(op, tag).with_result(result);
            let mut writer = queues.half_to_write_to();
            let mut write = writer.prepare_write(8).unwrap();
            write.controller_u64(answer.op);
            write.commit();

            if let Some(ref mut events) = &mut events {
                // TODO: provide info on who or what queue?
                events.request(msg, answer);
            }
        }
    }
}

impl Poll {
    /// Wrap a ring for communication via control messages.
    pub fn new(ring: ShmControllerRing) -> Self {
        Poll {  ring, responses: HashMap::default() }
    }

    /// Unwrap the raw ring within.
    pub fn into_ring(self) -> ShmControllerRing {
        self.ring
    }

    pub fn send(&mut self, _: &mut impl Request) {
        todo!()
    }

    pub fn retrieve<R: Request>(&mut self, _: &R) -> Option<R::Response> {
        todo!()
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
        self.msg.value0()
    }
}
