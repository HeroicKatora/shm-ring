use super::{
    Events,
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

/// A message passed on a socket-control queue.
pub struct SocketMessage(ControlMessage);

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


/// Request a new server.
///
/// Send this on the controller queue.
///
/// Establishes a separate queue with the controller, in which notifications about accepted
/// connections arrive and with which modifications to the socket can be performed.
pub struct SocketCreate {
    /// reserved, ID of a configured descriptor. Must be `0`.
    pub public_id: u32,
    /// The tag identifying the request.
    pub tag: Tag,
}

/// Open a socket on a port.
///
/// Send this on a socket queue.
pub struct SocketBind {
    /// The port under which to bind the socket.
    pub port: u32,
    /// The tag identifying the request.
    pub tag: Tag,
}

/// Designate an area for accepted connections.
pub struct SocketAccept {
    /// The page where to put accepted sockets.
    pub location: u32,
    /// The tag identifying the request.
    pub tag: Tag,
}

/// Connect to a socket under a port.
///
/// Send this on the controller queue.
pub struct SocketConnect {
    /// The port under which the target socket is bound.
    pub port: u32,
    /// The tag identifying the request.
    pub tag: Tag,
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
    /// A client connection to an existing server.
    pub const PIPE_JOIN: Self = Cmd(3);

    /// A ping will immediately be answered by a ping back.
    pub const REQUEST_PING: Self = Cmd(2);

    /// Create a queue for a server host, but do not advertise it yet.
    /// More or less a socket.
    pub const SOCKET_CREATE: Self = Cmd(4);
    /// Advertise a socket on a particular 'port'.
    pub const SOCKET_BIND: Self = Cmd(5);
    /// Advertise a socket on a particular 'port'.
    pub const SOCKET_CONNECT: Self = Cmd(6);

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

    pub(crate) fn with_raw(Cmd(op): Cmd, Tag(tag): Tag) -> Self {
        ControlMessage {
            op: u64::from(op) << 16 | u64::from(tag)
        }
    }

    pub(crate) fn with_result(self, val: u32) -> Self {
        ControlMessage { op: self.op | u64::from(val) << 32 }
    }

    pub(crate) fn tag(self) -> Tag {
        Tag(self.op as u16)
    }

    pub(crate) fn cmd(self) -> Cmd {
        Cmd((self.op >> 16) as u16)
    }

    pub(crate) fn value0(self) -> u32 {
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

impl From<SocketCreate> for ControlMessage {
    fn from(SocketCreate { public_id, tag }: SocketCreate) -> Self {
        ControlMessage::with_raw(Cmd::SOCKET_CREATE, tag).with_argument(public_id)
    }
}

impl From<SocketBind> for SocketMessage {
    fn from(SocketBind { port, tag }: SocketBind) -> Self {
        let ctrl = ControlMessage::with_raw(Cmd::SOCKET_BIND, tag).with_argument(port);
        SocketMessage(ctrl)
    }
}

impl From<SocketConnect> for ControlMessage {
    fn from(SocketConnect { port, tag }: SocketConnect) -> Self {
        ControlMessage::with_raw(Cmd::SOCKET_CONNECT, tag).with_argument(port)
    }
}

/// Private implementation of commands.
impl ControlMessage {
    pub(crate) fn execute(
        controller: &mut ShmController,
        queue_owner: u32,
        queue: ShmQueue<'_>,
        events: Option<&mut Events>,
    ) {
        controller.state.execute_control(queue_owner, queue, events)
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
            ring,
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
