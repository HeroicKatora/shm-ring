Shm PBX—The shared memory private branch exchange.

Operate a shared memory file that provides clients with the basic structures
for joining point-to-point message ring queue, that each work similar to the
kernel's io-uring and XDP's structures. The goal is to make enable programs
that feel like libraries (operate entirely in-memory), but are actually
implemented as IPC to some user-wide or task specific server.

### Motivation

See [motivation.md](doc/motivation.md). The library is competing on problem
spaces that lead to 'micro-service' architectures but where interests in
zero-copy performance, flexibility, the stability of process isolation, and an
outlook towards eventual hardware implementations come together.
