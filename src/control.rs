#[repr(C)]
pub struct ControlMessage {
    pub op: u64,
    pub param: u64,
}

/// Request a new ring.
pub struct RequestNewRing {}

/// Answer to a requested ring.
pub struct ProvideNewRing {
    /// Offset of the head control structure (hint: the requester is partner A).
    pub head: u64,
    /// Flags of the result (unused).
    pub flags: u64,
}

impl ControlMessage {
    /// Create a new message.
    ///
    /// Exists for document purposes, see the list of implementors of `Into` and `From`.
    pub fn new(msg: impl Into<Self>) -> Self {
        msg.into()
    }
}

impl From<RequestNewRing> for ControlMessage {
    fn from(msg: RequestNewRing) -> Self {
        ControlMessage {
            op: 0,
            param: 0,
        }
    }
}
