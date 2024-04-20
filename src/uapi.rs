use crate::data;

type ClientIdInt = u64;

const _: () = {
    // This pre-condition is required so that all PID values can be utilized as ring identifiers.
    // The assertion here serves as a warning: we *can* provide facilities to make construction
    // fallible, but doing so might be much more tedious.
    assert!(
        core::mem::size_of::<uapi::c::pid_t>() < core::mem::size_of::<ClientIdInt>(),
        "The platform you're using can not guarantee that PIDs can atomically replace slots in ring
        registration. Please open a pull-request for fallible interfaces instead."
    );
};

impl data::ClientIdentifier {
    #[cfg(feature = "uapi")]
    pub fn new() -> Self {
        let pid = uapi::getpid();
        assert!(pid > 0, "Unsupported pid {pid} reported from the OS");
        data::ClientIdentifier(pid as ClientIdInt)
    }
}
