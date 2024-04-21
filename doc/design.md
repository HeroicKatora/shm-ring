The problem with user-space communication is not the feasibility. Indeed, spin
locking can work, perfectly fine, and atomics can be used to 'acquire'
resources such as locks. The problem is efficiency. You want to wait until your
communication partner, or in the case of a server select which partner, has
provided the next message—often with an ("async") runtime system on your end to
perform additional background tasks.

But *blocking* on a `futex` is fraught with peril, if the other process dies
unexpectedly all sorts of problems arise. In single process lock (`Mutex`)
abstractions this is not at all critical, as usually all your threads form a
single availability group and die together. They are unblocked by being killed.
Obviously this is undesirable and indeed even part of the reason for forking
tasks to other processes. If the OS is involved, a similar thing occurs but now
the availability group is the whole computer.

We tackle, not fully solve, this by involving a central authority tracking the
processes by their PID and step in by taking over the role of a side. This does
not solve any priority-inversion and trust problems but if you're sharing a
memory region, we're only targetting mutually trusting processes anyways? The
project will provide a bridge between trust regions and separate shared
memories instead.

  TODO: a testing strategy for this is actually critical.

The problem arises concretely when trying to futex-wait on changes to the
head-index of either ring side. Even if we check the ID of our partner to be
valid before entering such wait, it races with any leave or crash events (that
would prompt the ring authority to forcibly de-initialize the slot). Then the
wake-up might never come! So: *never* under any circumstance use futex waits
without reasonably small timeouts. (There's an in-the-air proposal for Linux to
have processes register a resource list of futexes, which the kernel will reap
together with file descriptors to mitigate this, but it's not clear what will
be necessary and how the interface will look like).

  TODO: Looking into `FUTEX_WAKE_OP` to modify the head state.
