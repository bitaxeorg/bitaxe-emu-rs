//! What the router and the client share: the smoltcp device that exchanges
//! Ethernet frames with the rest of the emulator, and TCP connections carried
//! between a smoltcp socket (the chip's side) and a host socket.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::mpsc::{Receiver, TryRecvError};

use smoltcp::iface::{SocketHandle, SocketSet};
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::socket::tcp;
use smoltcp::time::Instant;

use crate::machine::TICK_HZ;

const TCP_BUFFER: usize = 64 * 1024;

pub fn smol_now(now: u64) -> Instant {
    Instant::from_micros((now / (TICK_HZ / 1_000_000)) as i64)
}

/// Frames between a smoltcp interface and the emulator.
#[derive(Default)]
pub struct Pipe {
    /// For smoltcp.
    pub rx: VecDeque<Vec<u8>>,
    /// From smoltcp.
    pub tx: VecDeque<Vec<u8>>,
}

pub struct RxToken(Vec<u8>);
pub struct TxToken<'a>(&'a mut VecDeque<Vec<u8>>);

impl phy::RxToken for RxToken {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl phy::TxToken for TxToken<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut b = vec![0; len];
        let r = f(&mut b);
        self.0.push_back(b);
        r
    }
}

impl Device for Pipe {
    type RxToken<'a> = RxToken;
    type TxToken<'a> = TxToken<'a>;

    fn receive(&mut self, _: Instant) -> Option<(RxToken, TxToken<'_>)> {
        let f = self.rx.pop_front()?;
        Some((RxToken(f), TxToken(&mut self.tx)))
    }

    fn transmit(&mut self, _: Instant) -> Option<TxToken<'_>> {
        Some(TxToken(&mut self.tx))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = 1514;
        caps
    }
}

/// The host side of a TCP connection.
pub enum Host {
    Connecting(Receiver<std::io::Result<TcpStream>>),
    Open(TcpStream),
    Closed,
}

pub struct TcpFlow {
    pub handle: SocketHandle,
    pub host: Host,
    /// Data from the chip not yet written to the host.
    to_host: Vec<u8>,
    host_eof: bool,
    host_shut: bool,
    /// The chip's side reached ESTABLISHED: from then on, a socket that may not
    /// receive means the chip closed its side.
    established: bool,
    /// For the log.
    pub label: String,
}

impl TcpFlow {
    pub fn new(handle: SocketHandle, host: Host, label: String) -> Self {
        TcpFlow {
            handle,
            host,
            to_host: Vec::new(),
            host_eof: false,
            host_shut: false,
            established: false,
            label,
        }
    }
}

/// A smoltcp TCP socket with room for fast transfers.
pub fn new_socket() -> tcp::Socket<'static> {
    let mut s = tcp::Socket::new(
        tcp::SocketBuffer::new(vec![0; TCP_BUFFER]),
        tcp::SocketBuffer::new(vec![0; TCP_BUFFER]),
    );
    s.set_nagle_enabled(false);
    s.set_ack_delay(None);
    s
}

/// The host side of a forwarded connection, ready to use.
pub fn open_host(stream: TcpStream) -> Host {
    let _ = stream.set_nonblocking(true);
    let _ = stream.set_nodelay(true);
    Host::Open(stream)
}

/// Moves data between the host sockets and the smoltcp sockets of `flows`, and
/// drops the flows that ended.
pub fn pump(
    flows: &mut Vec<TcpFlow>,
    sockets: &mut SocketSet<'static>,
    events: &mut VecDeque<String>,
) {
    let mut buf = vec![0u8; 16 * 1024];
    for flow in flows.iter_mut() {
        let socket = sockets.get_mut::<tcp::Socket>(flow.handle);
        if let Host::Connecting(rx) = &flow.host {
            match rx.try_recv() {
                Ok(Ok(stream)) => flow.host = open_host(stream),
                Ok(Err(e)) => {
                    events.push_back(format!("TCP {}: {e}", flow.label));
                    socket.abort();
                    flow.host = Host::Closed;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    socket.abort();
                    flow.host = Host::Closed;
                }
            }
        }
        let Host::Open(stream) = &mut flow.host else {
            continue;
        };
        flow.established |= socket.state() == tcp::State::Established;
        // chip to host
        while flow.to_host.len() < TCP_BUFFER && socket.can_recv() {
            let _ = socket.recv(|b| {
                flow.to_host.extend_from_slice(b);
                (b.len(), ())
            });
        }
        if !flow.to_host.is_empty() {
            match stream.write(&flow.to_host) {
                Ok(n) => {
                    flow.to_host.drain(..n);
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(_) => {
                    socket.abort();
                    flow.host = Host::Closed;
                    continue;
                }
            }
        }
        if flow.established && !socket.may_recv() && flow.to_host.is_empty() && !flow.host_shut {
            let _ = stream.shutdown(Shutdown::Write);
            flow.host_shut = true;
        }
        // host to chip
        if !flow.host_eof && socket.may_send() {
            let room = socket.send_capacity() - socket.send_queue();
            if room > 0 {
                let n = room.min(buf.len());
                match stream.read(&mut buf[..n]) {
                    Ok(0) => {
                        flow.host_eof = true;
                        socket.close();
                    }
                    Ok(n) => {
                        let _ = socket.send_slice(&buf[..n]);
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                    Err(_) => {
                        socket.abort();
                        flow.host = Host::Closed;
                    }
                }
            }
        }
    }
    flows.retain(|flow| {
        let state = sockets.get::<tcp::Socket>(flow.handle).state();
        let done = matches!(state, tcp::State::Closed | tcp::State::TimeWait)
            && !matches!(flow.host, Host::Connecting(_));
        if done {
            sockets.remove(flow.handle);
        }
        !done
    });
}
