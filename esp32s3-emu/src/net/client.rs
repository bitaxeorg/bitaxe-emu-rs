//! A station that joins the chip's own access point (the open setup network of
//! the firmware): it probes for it, authenticates, associates and takes an
//! address by DHCP, then carries host connections to the access point's
//! gateway ([`Client::forward`]).

use std::collections::VecDeque;
use std::net::{Ipv4Addr, TcpStream};

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::socket::dhcpv4;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpCidr};

use super::ieee80211::{self as wlan, BROADCAST, Builder, Frame, Mac};
use super::tcp::{Pipe, TcpFlow, new_socket, open_host, pump, smol_now};
use crate::machine::TICK_HZ;

/// Probes until an access point answers, this often.
const PROBE_PERIOD: u64 = TICK_HZ / 2;
/// While associated, the access point is probed this often; it is considered
/// gone after [`LOST_PROBES`] probes without answer.
const KEEP_PERIOD: u64 = TICK_HZ * 5;
const LOST_PROBES: u32 = 3;
/// A step of the association not answered by then starts over.
const STEP_TIMEOUT: u64 = TICK_HZ;
/// Capabilities: ESS, short slot time.
const CAPABILITIES: u16 = 0x0401;

#[derive(Clone, Debug, PartialEq)]
enum State {
    Searching,
    Authenticating {
        bssid: Mac,
        ssid: Vec<u8>,
        since: u64,
    },
    Associating {
        bssid: Mac,
        ssid: Vec<u8>,
        since: u64,
    },
    Associated {
        bssid: Mac,
        ssid: Vec<u8>,
    },
}

pub struct Client {
    mac: Mac,
    state: State,
    seq: u16,
    next_probe: u64,
    /// Probes sent while associated without an answer.
    unanswered: u32,
    iface: Interface,
    pipe: Pipe,
    sockets: SocketSet<'static>,
    dhcp: SocketHandle,
    /// The address taken, and the access point's gateway.
    address: Option<(Ipv4Addr, Ipv4Addr)>,
    flows: Vec<TcpFlow>,
    next_port: u16,
    /// 802.11 frames for the chip.
    pub out: VecDeque<Vec<u8>>,
    pub events: VecDeque<String>,
}

impl Client {
    pub fn new(mac: Mac, now: u64) -> Self {
        let mut pipe = Pipe::default();
        let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
        config.random_seed = 0xc11e;
        let iface = Interface::new(config, &mut pipe, smol_now(now));
        let mut sockets = SocketSet::new(Vec::new());
        let dhcp = sockets.add(dhcpv4::Socket::new());
        Client {
            mac,
            state: State::Searching,
            seq: 0,
            next_probe: 0,
            unanswered: 0,
            iface,
            pipe,
            sockets,
            dhcp,
            address: None,
            flows: Vec::new(),
            next_port: 50000,
            out: VecDeque::new(),
            events: VecDeque::new(),
        }
    }

    fn seq(&mut self) -> u16 {
        self.seq = (self.seq + 1) & 0xfff;
        self.seq
    }

    /// Associated, with an address.
    pub fn ready(&self) -> bool {
        matches!(self.state, State::Associated { .. }) && self.address.is_some()
    }

    fn probe(&mut self) {
        let seq = self.seq();
        let f = Builder::mgmt(wlan::PROBE_REQ, BROADCAST, self.mac, BROADCAST, seq)
            .element(wlan::IE_SSID, b"")
            .element(wlan::IE_RATES, &wlan::RATES)
            .element(wlan::IE_EXT_RATES, &wlan::EXT_RATES)
            .build();
        self.out.push_back(f);
    }

    /// Back to searching for an access point.
    fn reset(&mut self, why: &str) {
        if let State::Associated { ssid, .. } = &self.state {
            self.events.push_back(format!(
                "left the access point {}: {why}",
                String::from_utf8_lossy(ssid)
            ));
        }
        self.state = State::Searching;
        self.address = None;
        self.unanswered = 0;
        self.iface.update_ip_addrs(|a| a.clear());
        self.iface.routes_mut().remove_default_ipv4_route();
        self.sockets.get_mut::<dhcpv4::Socket>(self.dhcp).reset();
        for flow in self.flows.drain(..) {
            self.sockets.remove(flow.handle);
        }
    }

    /// A frame the chip sent.
    pub fn receive(&mut self, frame: &[u8], now: u64) {
        let Some(f) = Frame::parse(frame) else {
            return;
        };
        let group = f.addr1[0] & 1 != 0;
        if f.addr1 != self.mac && !group {
            return;
        }
        match (&self.state, f.kind, f.subtype) {
            (_, wlan::TYPE_MGMT, wlan::PROBE_RESP) if f.addr1 == self.mac => {
                let ssid = f
                    .body
                    .get(12..)
                    .and_then(|ies| wlan::element(ies, wlan::IE_SSID))
                    .unwrap_or_default()
                    .to_vec();
                match &self.state {
                    State::Searching => {
                        let bssid = f.addr2;
                        let seq = self.seq();
                        let auth = Builder::mgmt(wlan::AUTH, bssid, self.mac, bssid, seq)
                            .u16(0)
                            .u16(1)
                            .u16(0)
                            .build();
                        self.out.push_back(auth);
                        self.state = State::Authenticating {
                            bssid,
                            ssid,
                            since: now,
                        };
                    }
                    State::Associated { bssid, .. } if *bssid == f.addr2 => self.unanswered = 0,
                    _ => {}
                }
            }
            (State::Authenticating { bssid, ssid, .. }, wlan::TYPE_MGMT, wlan::AUTH)
                if f.addr2 == *bssid =>
            {
                let status = f.body.get(4..6).map(|b| u16::from_le_bytes([b[0], b[1]]));
                if status != Some(0) {
                    self.reset("authentication refused");
                    return;
                }
                let (bssid, ssid) = (*bssid, ssid.clone());
                let seq = self.seq();
                let assoc = Builder::mgmt(wlan::ASSOC_REQ, bssid, self.mac, bssid, seq)
                    .u16(CAPABILITIES)
                    .u16(10)
                    .element(wlan::IE_SSID, &ssid)
                    .element(wlan::IE_RATES, &wlan::RATES)
                    .element(wlan::IE_EXT_RATES, &wlan::EXT_RATES)
                    .build();
                self.out.push_back(assoc);
                self.state = State::Associating {
                    bssid,
                    ssid,
                    since: now,
                };
            }
            (State::Associating { bssid, ssid, .. }, wlan::TYPE_MGMT, wlan::ASSOC_RESP)
                if f.addr2 == *bssid =>
            {
                let status = f.body.get(2..4).map(|b| u16::from_le_bytes([b[0], b[1]]));
                if status != Some(0) {
                    self.state = State::Searching;
                    return;
                }
                self.events.push_back(format!(
                    "joined the chip's access point {}",
                    String::from_utf8_lossy(ssid)
                ));
                self.state = State::Associated {
                    bssid: *bssid,
                    ssid: ssid.clone(),
                };
                self.next_probe = now + KEEP_PERIOD;
                // the address now, not at the next retry of a discover sent before
                self.sockets.get_mut::<dhcpv4::Socket>(self.dhcp).reset();
            }
            (State::Associated { bssid, .. }, wlan::TYPE_MGMT, wlan::DEAUTH | wlan::DISASSOC)
                if f.addr2 == *bssid =>
            {
                self.reset("the access point sent us away");
            }
            (State::Associated { bssid, .. }, wlan::TYPE_DATA, _)
                if f.addr2 == *bssid && f.flags & wlan::FROM_DS != 0 && !f.is_null() =>
            {
                if let Some((ethertype, payload)) = wlan::llc(f.body) {
                    let eth = wlan::ethernet(f.addr1, f.addr3, ethertype, payload);
                    self.pipe.rx.push_back(eth);
                    self.poll_smoltcp(now);
                }
            }
            _ => {}
        }
    }

    fn poll_smoltcp(&mut self, now: u64) {
        self.iface
            .poll(smol_now(now), &mut self.pipe, &mut self.sockets);
        let State::Associated { bssid, .. } = self.state else {
            self.pipe.tx.clear();
            return;
        };
        while let Some(eth) = self.pipe.tx.pop_front() {
            let Some((dst, _, ethertype, payload)) = wlan::parse_ethernet(&eth) else {
                continue;
            };
            let seq = self.seq();
            let f = Builder::header(wlan::TYPE_DATA, 0, wlan::TO_DS, bssid, self.mac, dst, seq)
                .llc(ethertype, payload)
                .build();
            self.out.push_back(f);
        }
    }

    /// Probes, times out the association steps, takes the address, and moves
    /// the forwarded connections on.
    pub fn poll(&mut self, now: u64) {
        match &self.state {
            State::Searching if now >= self.next_probe => {
                self.probe();
                self.next_probe = now + PROBE_PERIOD;
            }
            State::Authenticating { since, .. } | State::Associating { since, .. }
                if now >= since + STEP_TIMEOUT =>
            {
                self.state = State::Searching;
            }
            State::Associated { .. } if now >= self.next_probe => {
                if self.unanswered >= LOST_PROBES {
                    self.reset("it stopped answering");
                } else {
                    self.unanswered += 1;
                    self.probe();
                    self.next_probe = now + KEEP_PERIOD;
                }
            }
            _ => {}
        }
        if matches!(self.state, State::Associated { .. }) {
            let event = self.sockets.get_mut::<dhcpv4::Socket>(self.dhcp).poll();
            match event {
                Some(dhcpv4::Event::Configured(config)) => {
                    let address = config.address;
                    let gateway = config.router.unwrap_or(config.server.address);
                    self.iface.update_ip_addrs(|a| {
                        a.clear();
                        let _ = a.push(IpCidr::Ipv4(address));
                    });
                    let _ = self.iface.routes_mut().add_default_ipv4_route(gateway);
                    self.events
                        .push_back(format!("took {address} on the chip's access point"));
                    self.address = Some((address.address(), gateway));
                }
                Some(dhcpv4::Event::Deconfigured) => {
                    self.iface.update_ip_addrs(|a| a.clear());
                    self.address = None;
                }
                None => {}
            }
        }
        pump(&mut self.flows, &mut self.sockets, &mut self.events);
        self.poll_smoltcp(now);
    }

    /// Time of the next probe.
    pub fn next_event(&self) -> u64 {
        self.next_probe
    }

    /// Carries a host connection to `port` of the access point's gateway. False
    /// if the client is not ready.
    pub fn forward(&mut self, stream: TcpStream, label: String, port: u16) -> bool {
        let Some((local, gateway)) = self.address else {
            return false;
        };
        if !self.ready() {
            return false;
        }
        let mut socket = new_socket();
        self.next_port = if self.next_port >= 60000 {
            50000
        } else {
            self.next_port + 1
        };
        if socket
            .connect(
                self.iface.context(),
                (gateway, port),
                (local, self.next_port),
            )
            .is_err()
        {
            return false;
        }
        let handle = self.sockets.add(socket);
        self.flows.push(TcpFlow::new(
            handle,
            open_host(stream),
            format!("{label} > {gateway}:{port}"),
        ));
        true
    }
}
