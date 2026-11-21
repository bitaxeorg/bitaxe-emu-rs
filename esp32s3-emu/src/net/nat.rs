//! The router behind the access point: it gives the chip an address (DHCP),
//! answers its ARP requests for the gateway, and carries its connections out
//! through the host's sockets (NAT): DNS to the host's resolver, other UDP and
//! TCP to their destination, the gateway's own address meaning the host. Host
//! TCP ports can be forwarded to the chip.
//!
//! TCP goes through smoltcp, which accepts the chip's connections to any
//! address: each SYN gets a socket listening on its destination, connected to
//! the host socket opened for it. ARP, DHCP and UDP are handled here.

use std::collections::VecDeque;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream, UdpSocket};
use std::sync::mpsc;
use std::time::{Duration, Instant as WallClock};

use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::socket::tcp;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpCidr, IpListenEndpoint};

use super::ieee80211::{self as wlan, Mac};
use super::tcp::{Host, Pipe, TcpFlow, new_socket, open_host, pump, smol_now};

const ETHERTYPE_IPV4: u16 = 0x0800;
const ETHERTYPE_ARP: u16 = 0x0806;
const PROTO_TCP: u8 = 6;
const PROTO_UDP: u8 = 17;
const DHCP_SERVER: u16 = 67;
const DHCP_CLIENT: u16 = 68;
const DNS: u16 = 53;
/// UDP mappings unused this long are dropped.
const UDP_IDLE: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug)]
pub struct RouterConfig {
    pub mac: Mac,
    /// The gateway, DHCP and DNS server.
    pub ip: Ipv4Addr,
    pub prefix: u8,
    /// The address the chip gets.
    pub guest_ip: Ipv4Addr,
}

impl Default for RouterConfig {
    fn default() -> Self {
        RouterConfig {
            mac: [0x02, 0xe5, 0x00, 0x00, 0x00, 0x02],
            ip: Ipv4Addr::new(10, 0, 2, 2),
            prefix: 24,
            guest_ip: Ipv4Addr::new(10, 0, 2, 15),
        }
    }
}

struct UdpFlow {
    /// The chip's port and the address it sent to.
    guest_port: u16,
    remote: SocketAddrV4,
    socket: UdpSocket,
    used: WallClock,
}

pub struct Router {
    cfg: RouterConfig,
    iface: Interface,
    pipe: Pipe,
    sockets: SocketSet<'static>,
    tcp: Vec<TcpFlow>,
    udp: Vec<UdpFlow>,
    resolver: Option<SocketAddrV4>,
    guest_mac: Option<Mac>,
    /// The chip took its address.
    leased: bool,
    next_port: u16,
    ip_id: u16,
    /// Ethernet frames for the chip.
    pub out: VecDeque<Vec<u8>>,
    pub events: VecDeque<String>,
}

/// The first IPv4 name server of the host.
fn host_resolver() -> Option<SocketAddrV4> {
    let conf = std::fs::read_to_string("/etc/resolv.conf").ok()?;
    conf.lines().find_map(|l| {
        let mut w = l.split_whitespace();
        (w.next()? == "nameserver")
            .then(|| w.next()?.parse::<Ipv4Addr>().ok())
            .flatten()
            .map(|ip| SocketAddrV4::new(ip, DNS))
    })
}

impl Router {
    pub fn new(cfg: RouterConfig, now: u64) -> Self {
        let mut pipe = Pipe::default();
        let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(cfg.mac)));
        config.random_seed = 0x5eed;
        let mut iface = Interface::new(config, &mut pipe, smol_now(now));
        iface.update_ip_addrs(|a| {
            let _ = a.push(IpCidr::new(cfg.ip.into(), cfg.prefix));
        });
        iface.set_any_ip(true);
        let _ = iface.routes_mut().add_default_ipv4_route(cfg.ip);
        let mut events = VecDeque::new();
        let resolver = host_resolver();
        if resolver.is_none() {
            events.push_back("WARN no IPv4 name server in /etc/resolv.conf".into());
        }
        Router {
            cfg,
            iface,
            pipe,
            sockets: SocketSet::new(Vec::new()),
            tcp: Vec::new(),
            udp: Vec::new(),
            resolver,
            guest_mac: None,
            leased: false,
            next_port: 40000,
            ip_id: 0,
            out: VecDeque::new(),
            events,
        }
    }

    /// The chip's address, once it took it.
    pub fn guest_ip(&self) -> Option<Ipv4Addr> {
        self.leased.then_some(self.cfg.guest_ip)
    }

    /// An Ethernet frame from the chip.
    pub fn input(&mut self, eth: &[u8], now: u64) {
        let Some((dst, src, ethertype, payload)) = wlan::parse_ethernet(eth) else {
            return;
        };
        if dst != self.cfg.mac && dst[0] & 1 == 0 {
            return;
        }
        match ethertype {
            ETHERTYPE_ARP => {
                // Only requests for the gateway and replies: smoltcp would
                // answer for every address.
                let target = payload
                    .get(24..28)
                    .map(|b| Ipv4Addr::new(b[0], b[1], b[2], b[3]));
                let op = payload.get(6..8).map(|b| u16::from_be_bytes([b[0], b[1]]));
                if op == Some(2) || target == Some(self.cfg.ip) {
                    self.guest_mac = Some(src);
                    self.pipe.rx.push_back(eth.to_vec());
                    self.poll_smoltcp(now);
                }
            }
            ETHERTYPE_IPV4 => self.ipv4(eth, src, payload, now),
            _ => {}
        }
    }

    fn ipv4(&mut self, eth: &[u8], src_mac: Mac, ip: &[u8], now: u64) {
        let Some(p) = Ipv4::parse(ip) else {
            return;
        };
        match p.proto {
            PROTO_UDP => {
                let Some(u) = p.payload.get(..8) else {
                    return;
                };
                let sport = u16::from_be_bytes([u[0], u[1]]);
                let dport = u16::from_be_bytes([u[2], u[3]]);
                let data = &p.payload[8..];
                if dport == DHCP_SERVER {
                    self.dhcp(src_mac, data);
                } else if !p.dst.is_broadcast() && !p.dst.is_multicast() {
                    self.guest_mac = Some(src_mac);
                    self.udp_out(sport, SocketAddrV4::new(p.dst, dport), data);
                }
            }
            PROTO_TCP if p.dst != self.cfg.guest_ip => {
                self.guest_mac = Some(src_mac);
                let t = p.payload;
                if t.len() >= 20 {
                    let flags = t[13];
                    let (syn, ack) = (flags & 0x02 != 0, flags & 0x10 != 0);
                    let sport = u16::from_be_bytes([t[0], t[1]]);
                    let dport = u16::from_be_bytes([t[2], t[3]]);
                    if syn && !ack {
                        self.tcp_syn(p.src, sport, p.dst, dport);
                    }
                }
                self.pipe.rx.push_back(eth.to_vec());
                self.poll_smoltcp(now);
            }
            1 => {
                // ICMP: smoltcp answers echo requests.
                self.pipe.rx.push_back(eth.to_vec());
                self.poll_smoltcp(now);
            }
            _ => {}
        }
    }

    /// A SYN from the chip to `dst:dport`: a socket for it, unless one exists.
    fn tcp_syn(&mut self, src: Ipv4Addr, sport: u16, dst: Ipv4Addr, dport: u16) {
        let exists = self.tcp.iter().any(|f| {
            let s = self.sockets.get::<tcp::Socket>(f.handle);
            s.remote_endpoint()
                .is_some_and(|r| r.addr == src.into() && r.port == sport)
                && s.local_endpoint()
                    .is_some_and(|l| l.addr == dst.into() && l.port == dport)
        });
        if exists {
            return;
        }
        let mut socket = new_socket();
        if socket
            .listen(IpListenEndpoint {
                addr: Some(dst.into()),
                port: dport,
            })
            .is_err()
        {
            return;
        }
        let target = if dst == self.cfg.ip {
            SocketAddr::from((Ipv4Addr::LOCALHOST, dport))
        } else {
            SocketAddr::from((dst, dport))
        };
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(TcpStream::connect_timeout(&target, CONNECT_TIMEOUT));
        });
        let handle = self.sockets.add(socket);
        self.tcp.push(TcpFlow::new(
            handle,
            Host::Connecting(rx),
            format!("{src}:{sport} > {target}"),
        ));
    }

    fn udp_out(&mut self, guest_port: u16, remote: SocketAddrV4, data: &[u8]) {
        let target = if *remote.ip() == self.cfg.ip {
            if remote.port() == DNS {
                match self.resolver {
                    Some(r) => r,
                    None => return,
                }
            } else {
                SocketAddrV4::new(Ipv4Addr::LOCALHOST, remote.port())
            }
        } else {
            remote
        };
        let i = match self
            .udp
            .iter()
            .position(|f| f.guest_port == guest_port && f.remote == remote)
        {
            Some(i) => i,
            None => {
                let Ok(socket) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else {
                    return;
                };
                if socket.connect(target).is_err() || socket.set_nonblocking(true).is_err() {
                    return;
                }
                self.udp.push(UdpFlow {
                    guest_port,
                    remote,
                    socket,
                    used: WallClock::now(),
                });
                self.udp.len() - 1
            }
        };
        let f = &mut self.udp[i];
        f.used = WallClock::now();
        let _ = f.socket.send(data);
    }

    /// Answers DHCP discovers and requests: every client gets the guest address.
    fn dhcp(&mut self, src_mac: Mac, msg: &[u8]) {
        if msg.len() < 240 || msg[0] != 1 || msg[236..240] != [0x63, 0x82, 0x53, 0x63] {
            return;
        }
        let options = dhcp_options(&msg[240..]);
        let opt = |code: u8| options.iter().find(|(c, _)| *c == code).map(|(_, v)| *v);
        let kind = opt(53).and_then(|v| v.first().copied());
        let requested = opt(50)
            .filter(|v| v.len() == 4)
            .map(|v| Ipv4Addr::new(v[0], v[1], v[2], v[3]))
            .or_else(|| {
                let c = Ipv4Addr::new(msg[12], msg[13], msg[14], msg[15]);
                (!c.is_unspecified()).then_some(c)
            });
        let reply = match kind {
            Some(1) => 2,                                                     // discover: offer
            Some(3) if requested.is_none_or(|r| r == self.cfg.guest_ip) => 5, // request: ack
            Some(3) => 6,                                                     // nak
            _ => return,
        };
        let mut r = vec![0u8; 240];
        r[0] = 2;
        r[1] = 1;
        r[2] = 6;
        r[4..8].copy_from_slice(&msg[4..8]);
        r[10..12].copy_from_slice(&msg[10..12]);
        if reply != 6 {
            r[16..20].copy_from_slice(&self.cfg.guest_ip.octets());
            r[20..24].copy_from_slice(&self.cfg.ip.octets());
        }
        r[28..44].copy_from_slice(&msg[28..44]);
        r[236..240].copy_from_slice(&[0x63, 0x82, 0x53, 0x63]);
        let ip = self.cfg.ip.octets();
        let mask = (u32::MAX << (32 - self.cfg.prefix)).to_be_bytes();
        let mut option = |code: u8, v: &[u8]| {
            r.push(code);
            r.push(v.len() as u8);
            r.extend_from_slice(v);
        };
        option(53, &[reply]);
        option(54, &ip);
        if reply != 6 {
            option(51, &86400u32.to_be_bytes());
            option(1, &mask);
            option(3, &ip);
            option(6, &ip);
        }
        r.push(255);
        if reply == 5 {
            self.leased = true;
            self.guest_mac = Some(src_mac);
            self.events.push_back(format!(
                "DHCP: {} for {}",
                self.cfg.guest_ip,
                wlan::fmt_mac(&src_mac)
            ));
        }
        let packet = self.udp_packet(
            SocketAddrV4::new(self.cfg.ip, DHCP_SERVER),
            SocketAddrV4::new(Ipv4Addr::BROADCAST, DHCP_CLIENT),
            &r,
        );
        self.out.push_back(wlan::ethernet(
            wlan::BROADCAST,
            self.cfg.mac,
            ETHERTYPE_IPV4,
            &packet,
        ));
    }

    /// An IPv4 UDP packet.
    fn udp_packet(&mut self, src: SocketAddrV4, dst: SocketAddrV4, data: &[u8]) -> Vec<u8> {
        self.ip_id = self.ip_id.wrapping_add(1);
        let udp_len = 8 + data.len();
        let mut p = Vec::with_capacity(20 + udp_len);
        p.extend_from_slice(&[0x45, 0]);
        p.extend_from_slice(&((20 + udp_len) as u16).to_be_bytes());
        p.extend_from_slice(&self.ip_id.to_be_bytes());
        p.extend_from_slice(&[0x40, 0, 64, PROTO_UDP, 0, 0]);
        p.extend_from_slice(&src.ip().octets());
        p.extend_from_slice(&dst.ip().octets());
        let sum = checksum(&p[..20], 0);
        p[10..12].copy_from_slice(&sum.to_be_bytes());
        p.extend_from_slice(&src.port().to_be_bytes());
        p.extend_from_slice(&dst.port().to_be_bytes());
        p.extend_from_slice(&(udp_len as u16).to_be_bytes());
        p.extend_from_slice(&[0, 0]);
        p.extend_from_slice(data);
        let mut pseudo = Vec::with_capacity(12);
        pseudo.extend_from_slice(&src.ip().octets());
        pseudo.extend_from_slice(&dst.ip().octets());
        pseudo.extend_from_slice(&[0, PROTO_UDP]);
        pseudo.extend_from_slice(&(udp_len as u16).to_be_bytes());
        let sum = match checksum(&p[20..], sum_words(&pseudo)) {
            0 => 0xffff,
            s => s,
        };
        p[26..28].copy_from_slice(&sum.to_be_bytes());
        p
    }

    fn poll_smoltcp(&mut self, now: u64) {
        self.iface
            .poll(smol_now(now), &mut self.pipe, &mut self.sockets);
        self.out.append(&mut self.pipe.tx);
    }

    /// Moves data between the host sockets and the chip.
    pub fn poll(&mut self, now: u64) {
        pump(&mut self.tcp, &mut self.sockets, &mut self.events);
        self.poll_udp();
        self.poll_smoltcp(now);
    }

    /// Carries a host connection to `port` of the chip, from the gateway's
    /// address. False if the chip has no address yet.
    pub fn forward(&mut self, stream: TcpStream, label: String, port: u16) -> bool {
        if !self.leased {
            return false;
        }
        let mut socket = new_socket();
        self.next_port = if self.next_port >= 60000 {
            40000
        } else {
            self.next_port + 1
        };
        let local = (self.cfg.ip, self.next_port);
        if socket
            .connect(self.iface.context(), (self.cfg.guest_ip, port), local)
            .is_err()
        {
            return false;
        }
        let handle = self.sockets.add(socket);
        self.tcp.push(TcpFlow::new(
            handle,
            open_host(stream),
            format!("{label} > {}:{port}", self.cfg.guest_ip),
        ));
        true
    }

    fn poll_udp(&mut self) {
        let mut buf = vec![0u8; 2048];
        let mut packets = Vec::new();
        for f in &self.udp {
            while let Ok(n) = f.socket.recv(&mut buf) {
                packets.push((f.remote, f.guest_port, buf[..n].to_vec()));
            }
        }
        self.udp.retain(|f| f.used.elapsed() < UDP_IDLE);
        let Some(guest_mac) = self.guest_mac else {
            return;
        };
        for (remote, guest_port, data) in packets {
            let p = self.udp_packet(
                remote,
                SocketAddrV4::new(self.cfg.guest_ip, guest_port),
                &data,
            );
            self.out
                .push_back(wlan::ethernet(guest_mac, self.cfg.mac, ETHERTYPE_IPV4, &p));
        }
    }

    /// Open TCP connections.
    pub fn connections(&self) -> usize {
        self.tcp.len()
    }
}

/// The fields of an IPv4 packet the router needs.
struct Ipv4<'a> {
    proto: u8,
    src: Ipv4Addr,
    dst: Ipv4Addr,
    payload: &'a [u8],
}

impl<'a> Ipv4<'a> {
    fn parse(p: &'a [u8]) -> Option<Self> {
        if p.len() < 20 || p[0] >> 4 != 4 {
            return None;
        }
        let ihl = (p[0] & 0xf) as usize * 4;
        let total = u16::from_be_bytes([p[2], p[3]]) as usize;
        // fragments are not handled
        if u16::from_be_bytes([p[6], p[7]]) & 0x3fff != 0 {
            return None;
        }
        Some(Ipv4 {
            proto: p[9],
            src: Ipv4Addr::new(p[12], p[13], p[14], p[15]),
            dst: Ipv4Addr::new(p[16], p[17], p[18], p[19]),
            payload: p.get(ihl..total.min(p.len()))?,
        })
    }
}

fn dhcp_options(mut o: &[u8]) -> Vec<(u8, &[u8])> {
    let mut v = Vec::new();
    while let Some((&code, rest)) = o.split_first() {
        match code {
            0 => o = rest,
            255 => break,
            _ => {
                let Some((&len, rest)) = rest.split_first() else {
                    break;
                };
                let Some(data) = rest.get(..len as usize) else {
                    break;
                };
                v.push((code, data));
                o = &rest[len as usize..];
            }
        }
    }
    v
}

fn sum_words(b: &[u8]) -> u32 {
    b.chunks(2)
        .map(|c| u32::from(u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)])))
        .sum()
}

/// The Internet checksum of `b`, from the partial sum `start`.
fn checksum(b: &[u8], start: u32) -> u16 {
    let mut s = start + sum_words(b);
    while s >> 16 != 0 {
        s = (s & 0xffff) + (s >> 16);
    }
    !(s as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUEST: Mac = [0x7c, 0xdf, 0xa1, 0x0b, 0x17, 0xae];

    fn router() -> Router {
        Router::new(RouterConfig::default(), 0)
    }

    fn dhcp_message(kind: u8, requested: Option<[u8; 4]>) -> Vec<u8> {
        let mut m = vec![0u8; 240];
        m[0] = 1;
        m[1] = 1;
        m[2] = 6;
        m[4..8].copy_from_slice(&[1, 2, 3, 4]);
        m[28..34].copy_from_slice(&GUEST);
        m[236..240].copy_from_slice(&[0x63, 0x82, 0x53, 0x63]);
        m.extend_from_slice(&[53, 1, kind]);
        if let Some(r) = requested {
            m.extend_from_slice(&[50, 4]);
            m.extend_from_slice(&r);
        }
        m.push(255);
        m
    }

    fn udp_frame(r: &mut Router, src: SocketAddrV4, dst: SocketAddrV4, data: &[u8]) -> Vec<u8> {
        let p = r.udp_packet(src, dst, data);
        wlan::ethernet(wlan::BROADCAST, GUEST, ETHERTYPE_IPV4, &p)
    }

    #[test]
    fn dhcp_offers_then_acks_the_guest_address() {
        let mut r = router();
        let client = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, DHCP_CLIENT);
        let server = SocketAddrV4::new(Ipv4Addr::BROADCAST, DHCP_SERVER);
        let f = udp_frame(&mut r, client, server, &dhcp_message(1, None));
        r.input(&f, 0);
        let reply = r.out.pop_front().unwrap();
        let (_, _, _, ip) = wlan::parse_ethernet(&reply).unwrap();
        let p = Ipv4::parse(ip).unwrap();
        assert_eq!(checksum(&ip[..20], 0), 0, "IPv4 header checksum");
        let m = &p.payload[8..];
        assert_eq!(m[0], 2);
        assert_eq!(&m[16..20], &[10, 0, 2, 15]);
        let opts = dhcp_options(&m[240..]);
        assert!(opts.contains(&(53, &[2][..])));
        assert!(opts.contains(&(3, &[10, 0, 2, 2][..])));
        assert!(r.guest_ip().is_none());

        let f = udp_frame(
            &mut r,
            client,
            server,
            &dhcp_message(3, Some([10, 0, 2, 15])),
        );
        r.input(&f, 0);
        let reply = r.out.pop_front().unwrap();
        let (_, _, _, ip) = wlan::parse_ethernet(&reply).unwrap();
        let m = &Ipv4::parse(ip).unwrap().payload[8..];
        assert!(dhcp_options(&m[240..]).contains(&(53, &[5][..])));
        assert_eq!(r.guest_ip(), Some(Ipv4Addr::new(10, 0, 2, 15)));

        // another address: refused
        let f = udp_frame(
            &mut r,
            client,
            server,
            &dhcp_message(3, Some([192, 168, 1, 9])),
        );
        r.input(&f, 0);
        let reply = r.out.pop_front().unwrap();
        let (_, _, _, ip) = wlan::parse_ethernet(&reply).unwrap();
        let m = &Ipv4::parse(ip).unwrap().payload[8..];
        assert!(dhcp_options(&m[240..]).contains(&(53, &[6][..])));
    }

    #[test]
    fn udp_goes_out_through_a_host_socket_and_back() {
        let echo = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = echo.local_addr().unwrap().port();
        let mut r = router();
        r.guest_mac = Some(GUEST);
        // the gateway's address means the host
        let guest = SocketAddrV4::new(Ipv4Addr::new(10, 0, 2, 15), 5555);
        let gw = SocketAddrV4::new(Ipv4Addr::new(10, 0, 2, 2), port);
        let mut f = udp_frame(&mut r, guest, gw, b"ping");
        f[..6].copy_from_slice(&r.cfg.mac);
        r.input(&f, 0);
        let mut buf = [0u8; 16];
        let (n, from) = echo.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"ping");
        echo.send_to(b"pong", from).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        r.poll(0);
        let reply = r
            .out
            .iter()
            .find_map(|e| {
                let (_, _, t, ip) = wlan::parse_ethernet(e)?;
                let p = Ipv4::parse(ip)?;
                (t == ETHERTYPE_IPV4 && p.proto == PROTO_UDP).then(|| p.payload.to_vec())
            })
            .unwrap();
        assert_eq!(&reply[..2], &port.to_be_bytes());
        assert_eq!(&reply[2..4], &5555u16.to_be_bytes());
        assert_eq!(&reply[8..], b"pong");
    }
}
