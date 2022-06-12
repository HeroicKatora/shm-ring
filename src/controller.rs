use std::num::NonZeroU32;

use crate::control::{Cmd, ControlMessage, SocketMessage};

use super::{Events, ShmQueue};

pub struct ControllerState {
    pub(crate) free_queues: Vec<QueueId>,
    pub(crate) open_server: Vec<OpenServer>,
    pub(crate) open_client: Vec<OpenClient>,
    pub(crate) active: Vec<ActiveQueue>,
    pub(crate) sockets: Vec<Socket>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueueId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SocketId(pub u32);

/// A queue opened only on the server (A) side.
pub struct OpenServer {
    pub(crate) queue: QueueId,
    pub(crate) user_tag: u32,
    pub(crate) a: ClientId,
}

/// A queue opened only on the client (A) side, i.e. pending request.
pub struct OpenClient {
    pub(crate) queue: QueueId,
    pub(crate) user_tag: u32,
    pub(crate) b: ClientId,
}

/// A queue with both sides alive.
pub struct ActiveQueue {
    pub(crate) queue: QueueId,
    pub(crate) user_tag: u32,
    pub(crate) a: ClientId,
    pub(crate) b: ClientId,
}

pub struct Socket {
    pub(crate) client: ClientId,
    pub(crate) queue: QueueId,
    pub(crate) port: Option<NonZeroU32>,
    pub(crate) pending_limit: u32,
    pub(crate) pending: Vec<QueueId>,
}

impl ControllerState {
    pub(crate) fn execute_control(
        &mut self,
        queue_owner: ClientId,
        mut queue: ShmQueue<'_>,
        mut events: Option<&mut Events>,
    ) {
        let queues = &mut queue.queues;
        if queues.half_to_write_to().prepare_write(8).is_none() {
            // No space for response. Don't.
            return;
        }

        let mut reader = queues.half_to_read_from();
        let available = match reader.available(8) {
            Some(available) => available,
            None => return,
        };

        let op = available.controller_u64();
        let msg = ControlMessage { op };
        let tag = msg.tag();
        let cmd = msg.cmd();

        let mut result = 0;
        let op;

        match cmd {
            Cmd::REQUEST_PING => {
                op = Cmd::REQUEST_PING;
            }
            Cmd::REQUEST_PIPE => {
                let queue = self.free_queues.pop();

                if let Some(queue) = queue {
                    self.open_server.push(OpenServer {
                        queue,
                        user_tag: msg.value0(),
                        a: queue_owner,
                    });
                    op = Cmd::REQUEST_PIPE;
                    result = queue.0;
                } else {
                    op = Cmd::OUT_OF_QUEUES;
                }
            }
            Cmd::PIPE_JOIN => {
                let matching = self
                    .open_server
                    .iter()
                    .position(|open| open.user_tag == msg.value0());

                if let Some(matching) = matching {
                    let open = self.open_server.remove(matching);
                    self.active.push(ActiveQueue {
                        queue: open.queue,
                        user_tag: open.user_tag,
                        a: open.a,
                        b: queue_owner,
                    });

                    op = Cmd::REQUEST_PIPE;
                    result = open.queue.0;
                } else {
                    op = Cmd::OUT_OF_QUEUES;
                }
            }
            Cmd::PIPE_LEAVE => {
                let matching = self
                    .active
                    .iter()
                    .position(|active| active.queue.0 == msg.value0());

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
                let queue = self.free_queues.pop();

                if let Some(queue) = queue {
                    self.sockets.push(Socket {
                        client: queue_owner,
                        queue,
                        port: None,
                        pending: vec![],
                        pending_limit: 0,
                    });
                    op = Cmd::SOCKET_CREATE;
                    result = queue.0;
                } else {
                    op = Cmd::OUT_OF_QUEUES;
                }
            }
            Cmd::SOCKET_CONNECT => {
                // FIXME: should really use try for these blocks..
                let matching = self.sockets.iter().position(|active| {
                    active.port.is_some() && active.port == NonZeroU32::new(msg.value0())
                });

                if let Some(socket) = matching {
                    let socket = &mut self.sockets[socket];
                    if (socket.pending_limit as usize) < socket.pending.len() {
                        if let Some(queue) = self.free_queues.pop() {
                            op = Cmd::SOCKET_CONNECT;
                            socket.pending.push(queue);
                        } else {
                            // FIXME: better error type?
                            op = Cmd::BAD;
                        }
                    } else {
                        // FIXME: better error type?
                        op = Cmd::BAD;
                    }
                } else {
                    op = Cmd::BAD;
                };
            }
            _ => {
                op = match cmd {
                    // FIXME: hint that this is a wrong queue?
                    Cmd::SOCKET_BIND => Cmd::BAD,
                    _ => Cmd::BAD,
                };
            }
        };

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

    pub(crate) fn execute_socket(
        &mut self,
        socket_id: SocketId,
        mut queue: ShmQueue<'_>,
        mut events: Option<&mut Events>,
    ) {
        // TODO: a whole lot of duplicate code. Can we encapsulate the atomic read/write
        // combination? Something like `queues.prepare::<READ_COUNT, WRITE_COUNT()>()`?
        let (mut read, mut write) = queue.queues.halves();

        let mut write = match write.prepare_write(8) {
            Some(write) => write,
            None => return,
        };

        let available = match read.available(8) {
            Some(available) => available,
            None => return,
        };

        let op = available.controller_u64();
        let msg = SocketMessage { op };
        let tag = msg.tag();
        let cmd = msg.cmd();

        let mut result = 0;
        let op;

        let _ = 'a: loop {
            match cmd {
                Cmd::SOCKET_BIND => {
                    let port = NonZeroU32::new(msg.value0());

                    let matching = self.sockets.iter().position(|active| {
                        active.port.is_some() && active.port == NonZeroU32::new(msg.value0())
                    });

                    if matching.is_some() {
                        op = Cmd::BAD;
                        break 'a;
                    }

                    if let Some(port) = port {
                        op = Cmd::SOCKET_BIND;
                        self.sockets[socket_id.0 as usize].port = Some(port);
                    } else {
                        op = Cmd::BAD;
                        break 'a;
                    }
                }
                _ => {
                    op = Cmd::UNIMPLEMENTED;
                }
            }
            break;
        };

        // Commit writing a result and consume.
        available.commit();

        let answer = ControlMessage::with_raw(op, tag).with_result(result);
        write.controller_u64(answer.op);
        write.commit();

        if let Some(ref mut events) = &mut events {
            todo!()
        }
    }
}
