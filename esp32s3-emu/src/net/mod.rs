//! What surrounds the chip's Wi-Fi: [`Air`] takes the frames the MAC sends and
//! gives it the frames to receive. [`Network`] is an open access point with a
//! router behind it for the chip's station, a client for the chip's own access
//! point, and host ports forwarded to the chip through whichever is connected.

use std::collections::VecDeque;
use std::net::{Ipv4Addr, TcpListener};
use std::time::{Duration, Instant};

use crate::devices::wifi_mac::RxFrame;

/// The radio environment of the chip. Times are ticks of [`crate::machine::TICK_HZ`].
pub trait Air: Send {
    /// A frame the chip sent (an 802.11 MPDU without FCS).
    fn transmit(&mut self, frame: &[u8], now: u64);

    /// Adds the frames the chip receives by `now` to `rx`.
    fn poll(&mut self, now: u64, rx: &mut VecDeque<RxFrame>);

    /// Time of the next frame for the chip, if known.
    fn next_event(&self) -> Option<u64> {
        None
    }

    /// Lines for the log, since the last call.
    fn take_events(&mut self) -> Vec<String> {
        Vec::new()
    }
}

pub mod ap;
pub mod client;
pub mod ieee80211;
pub mod nat;
pub mod tcp;

use ieee80211::Mac;

/// Settings of the emulated network.
#[derive(Clone, Debug)]
pub struct NetworkConfig {
    /// SSID of the open access point the chip can join.
    pub ssid: String,
    pub bssid: Mac,
    pub channel: u8,
    /// Log every frame.
    pub trace: bool,
    pub router: nat::RouterConfig,
    /// MAC address of the station that joins the chip's access point.
    pub client_mac: Mac,
    /// Host TCP ports (on 127.0.0.1) forwarded to ports of the chip.
    pub forwards: Vec<(u16, u16)>,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        NetworkConfig {
            ssid: "bitaxe-emu".into(),
            bssid: [0x02, 0xe5, 0x00, 0x00, 0x00, 0x01],
            channel: 1,
            trace: false,
            router: nat::RouterConfig::default(),
            client_mac: [0x02, 0xe5, 0x00, 0x00, 0x00, 0x10],
            forwards: vec![(8080, 80)],
        }
    }
}

/// A host port forwarded to the chip, listening once bound.
struct Forward {
    host_port: u16,
    port: u16,
    listener: Option<TcpListener>,
    /// Last attempt to bind, and whether it failed (logged once).
    tried: Option<Instant>,
    failed: bool,
}

/// How often a port that could not be bound is tried again (it may be released
/// by an emulator that is stopping).
const BIND_RETRY: Duration = Duration::from_secs(1);

/// The emulated network around the chip.
pub struct Network {
    cfg: NetworkConfig,
    pub ap: ap::AccessPoint,
    pub router: nat::Router,
    pub client: client::Client,
    forwards: Vec<Forward>,
    /// What happened, for the log.
    pub events: VecDeque<String>,
}

impl Network {
    pub fn new(cfg: NetworkConfig) -> Self {
        Network {
            ap: ap::AccessPoint::new(&cfg.ssid, cfg.bssid, cfg.channel),
            router: nat::Router::new(cfg.router.clone(), 0),
            client: client::Client::new(cfg.client_mac, 0),
            forwards: cfg
                .forwards
                .iter()
                .map(|&(host_port, port)| Forward {
                    host_port,
                    port,
                    listener: None,
                    tried: None,
                    failed: false,
                })
                .collect(),
            cfg,
            events: VecDeque::new(),
        }
    }

    /// Binds the forwarded ports, and hands the connections to the chip through
    /// the router when its station has an address, else through the client on
    /// its access point; closes them when neither is up.
    fn accept(&mut self) {
        for i in 0..self.forwards.len() {
            let f = &mut self.forwards[i];
            if f.listener.is_none() && f.tried.is_none_or(|t| t.elapsed() >= BIND_RETRY) {
                f.tried = Some(Instant::now());
                match TcpListener::bind((Ipv4Addr::LOCALHOST, f.host_port)) {
                    Ok(l) => {
                        let _ = l.set_nonblocking(true);
                        f.listener = Some(l);
                        self.events.push_back(format!(
                            "http://localhost:{}/ reaches port {} of the chip",
                            f.host_port, f.port
                        ));
                    }
                    Err(e) if !f.failed => {
                        f.failed = true;
                        self.events
                            .push_back(format!("WARN port {}: {e}, trying again", f.host_port));
                    }
                    Err(_) => {}
                }
            }
            let f = &self.forwards[i];
            let Some(listener) = &f.listener else {
                continue;
            };
            let port = f.port;
            while let Ok((stream, peer)) = listener.accept() {
                let label = peer.to_string();
                let carried = if self.router.guest_ip().is_some() {
                    self.router.forward(stream, label, port)
                } else {
                    self.client.forward(stream, label, port)
                };
                if !carried && self.cfg.trace {
                    self.events
                        .push_back("a forwarded connection found the chip off the network".into());
                }
            }
        }
    }

    fn trace(&mut self, dir: &str, frame: &[u8]) {
        if !self.cfg.trace {
            return;
        }
        let line = match ieee80211::Frame::parse(frame) {
            Some(f) => format!(
                "wifi {dir} type {} sub {:2} flags {:02x} {} > {} bssid {} seq {} len {}",
                f.kind,
                f.subtype,
                f.flags,
                ieee80211::fmt_mac(&f.addr2),
                ieee80211::fmt_mac(&f.addr1),
                ieee80211::fmt_mac(&f.addr3),
                f.seq,
                frame.len()
            ),
            None => format!("wifi {dir} {} bytes", frame.len()),
        };
        let payload = ieee80211::Frame::parse(frame)
            .filter(|f| f.kind == ieee80211::TYPE_DATA)
            .and_then(|f| ieee80211::llc(f.body).map(|(t, p)| describe(t, p)));
        match payload {
            Some(p) => self.events.push_back(format!("{line}: {p}")),
            None => self.events.push_back(line),
        }
    }
}

/// A short description of an Ethernet payload: ARP, or IPv4 with its ports and
/// TCP flags.
fn describe(ethertype: u16, p: &[u8]) -> String {
    match ethertype {
        0x0806 if p.len() >= 28 => format!(
            "ARP op {} {}.{}.{}.{} > {}.{}.{}.{}",
            p[7], p[14], p[15], p[16], p[17], p[24], p[25], p[26], p[27]
        ),
        0x0800 if p.len() >= 20 => {
            let ihl = (p[0] & 0xf) as usize * 4;
            let src = format!("{}.{}.{}.{}", p[12], p[13], p[14], p[15]);
            let dst = format!("{}.{}.{}.{}", p[16], p[17], p[18], p[19]);
            let l4 = &p[ihl.min(p.len())..];
            let port = |i: usize| {
                l4.get(i..i + 2)
                    .map_or(0, |b| u16::from_be_bytes([b[0], b[1]]))
            };
            match p[9] {
                6 => {
                    let flags = l4.get(13).copied().unwrap_or(0);
                    let names: String = [
                        (0x02, 'S'),
                        (0x10, '.'),
                        (0x01, 'F'),
                        (0x04, 'R'),
                        (0x08, 'P'),
                    ]
                    .iter()
                    .filter(|(b, _)| flags & b != 0)
                    .map(|(_, c)| *c)
                    .collect();
                    format!(
                        "TCP {src}:{} > {dst}:{} [{names}] {} bytes",
                        port(0),
                        port(2),
                        p.len()
                    )
                }
                17 => format!(
                    "UDP {src}:{} > {dst}:{} {} bytes",
                    port(0),
                    port(2),
                    p.len()
                ),
                proto => format!("IPv4 proto {proto} {src} > {dst}"),
            }
        }
        t => format!("ethertype {t:#06x}"),
    }
}

impl Air for Network {
    fn transmit(&mut self, frame: &[u8], now: u64) {
        self.trace("tx", frame);
        self.ap.receive(frame, now);
        self.client.receive(frame, now);
    }

    fn poll(&mut self, now: u64, rx: &mut VecDeque<RxFrame>) {
        self.accept();
        self.ap.poll(now);
        self.client.poll(now);
        while let Some(eth) = self.ap.to_lan.pop_front() {
            self.router.input(&eth, now);
        }
        self.router.poll(now);
        while let Some(eth) = self.router.out.pop_front() {
            self.ap.send(&eth);
        }
        let client_frames: Vec<_> = self.client.out.drain(..).collect();
        for frame in client_frames {
            self.trace("rx", &frame);
            rx.push_back(RxFrame {
                data: frame,
                rssi: -40,
                channel: self.cfg.channel,
            });
        }
        while let Some(frame) = self.ap.out.pop_front() {
            if !ieee80211::Frame::parse(&frame).is_some_and(|f| f.is_mgmt(ieee80211::BEACON)) {
                self.trace("rx", &frame);
            }
            rx.push_back(RxFrame {
                data: frame,
                rssi: -40,
                channel: self.cfg.channel,
            });
        }
        self.events.append(&mut self.ap.events);
        self.events.append(&mut self.router.events);
        self.events.append(&mut self.client.events);
    }

    fn next_event(&self) -> Option<u64> {
        Some(self.ap.next_event().min(self.client.next_event()))
    }

    fn take_events(&mut self) -> Vec<String> {
        self.events.drain(..).collect()
    }
}
