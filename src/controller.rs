use std::collections::HashMap;

use crate::control::{
    Cmd,
    ControlMessage,
};

use super::{
    Events,
    ShmQueue,
};

pub struct ControllerState {
    pub(crate) free_queues: Vec<u32>,
    pub(crate) open_server: Vec<OpenServer>,
    pub(crate) open_client: Vec<OpenClient>,
    pub(crate) active: Vec<ActiveQueue>,
    pub(crate) sockets: Vec<Socket>,
    pub(crate) binds: HashMap<u32, Socket>,
}

/// A queue opened only on the server (A) side.
pub struct OpenServer {
    pub(crate) queue: u32,
    pub(crate) user_tag: u32,
    pub(crate) a: u32,
}

/// A queue opened only on the client (A) side, i.e. pending request.
pub struct OpenClient {
    pub(crate) queue: u32,
    pub(crate) user_tag: u32,
    pub(crate) b: u32,
}

/// A queue with both sides alive.
pub struct ActiveQueue {
    pub(crate) queue: u32,
    pub(crate) user_tag: u32,
    pub(crate) a: u32,
    pub(crate) b: u32,
}

pub struct Socket {
}

impl ControllerState {
    pub(crate) fn execute_control(
        &mut self,
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

                    let queue = self.free_queues.pop();

                    if let Some(queue) = queue {
                        self.open_server.push(OpenServer {
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

                    let matching = self.open_server.iter().position(|open| {
                        open.user_tag == msg.value0()
                    });

                    if let Some(matching) = matching {
                        let open = self.open_server.remove(matching);
                        self.active.push(ActiveQueue {
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

                    let matching = self.active.iter().position(|active| {
                        active.queue == msg.value0()
                    });

                    if let Some(matching) = matching {
                        if self.active[matching].a == queue_owner {
                            let active = self.active.remove(matching);
                            self.open_client.push(OpenClient {
                                queue: active.queue,
                                user_tag: active.user_tag,
                                b: active.b,
                            });

                            op = Cmd::PIPE_LEAVE;
                        } else if self.active[matching].b == queue_owner {
                            let active = self.active.remove(matching);
                            self.open_server.push(OpenServer {
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
                Cmd::SOCKET_CREATE => {
                    todo!()
                }
                Cmd::SOCKET_CONNECT => {
                    todo!()
                }
                _ => {
                    extra_space = 0;
                    op = match cmd {
                        Cmd::SOCKET_CREATE | Cmd::SOCKET_CONNECT | Cmd::SOCKET_BIND => Cmd::UNIMPLEMENTED,
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
