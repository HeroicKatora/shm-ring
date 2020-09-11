# shm-ring

This provides IPC task queues in shared memory and user-space. It's somewhat
inspired by `io-uring` except it is much more principled in doing everything
over such a queue, even control tasks. No other system calls and weird stuff.

The interface and memory layout is language agnostic. The primary controller
implementation is nevertheless in Rust.


## License

Public domain. Alternatively Unlicense or Apache-2.0 if you must have one..
