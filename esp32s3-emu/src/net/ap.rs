//! An open access point: beacons, probe responses, open authentication,
//! association, and data frames bridged to Ethernet frames.

use std::collections::{BTreeMap, VecDeque};

use super::ieee80211::{self as wlan, BROADCAST, Builder, Frame, Mac};
use crate::machine::TICK_HZ;

/// Beacon interval: 100 TU.
const BEACON_TU: u16 = 100;
const BEACON_PERIOD: u64 = TICK_HZ * 1024 * BEACON_TU as u64 / 1_000_000;
/// Capabilities: ESS, short slot time.
const CAPABILITIES: u16 = 0x0401;

struct Station {
    associated: bool,
    aid: u16,
}

pub struct AccessPoint {
    pub ssid: String,
    pub bssid: Mac,
    pub channel: u8,
    seq: u16,
    stations: BTreeMap<Mac, Station>,
    next_beacon: u64,
    /// 802.11 frames for the chip.
    pub out: VecDeque<Vec<u8>>,
    /// Ethernet frames from the stations.
    pub to_lan: VecDeque<Vec<u8>>,
    /// What happened, for the log.
    pub events: VecDeque<String>,
}

impl AccessPoint {
    pub fn new(ssid: &str, bssid: Mac, channel: u8) -> Self {
        AccessPoint {
            ssid: ssid.into(),
            bssid,
            channel,
            seq: 0,
            stations: BTreeMap::new(),
            next_beacon: 0,
            out: VecDeque::new(),
            to_lan: VecDeque::new(),
            events: VecDeque::new(),
        }
    }

    fn seq(&mut self) -> u16 {
        self.seq = (self.seq + 1) & 0xfff;
        self.seq
    }

    /// The fixed fields and elements of beacons and probe responses.
    fn bss_body(&self, b: Builder, now: u64, beacon: bool) -> Builder {
        let mut b = b
            .u64(now / (TICK_HZ / 1_000_000))
            .u16(BEACON_TU)
            .u16(CAPABILITIES)
            .element(wlan::IE_SSID, self.ssid.as_bytes())
            .element(wlan::IE_RATES, &wlan::RATES)
            .element(wlan::IE_DS, &[self.channel]);
        if beacon {
            // DTIM count 0, period 1, no traffic buffered
            b = b.element(wlan::IE_TIM, &[0, 1, 0, 0]);
        }
        b.element(wlan::IE_EXT_RATES, &wlan::EXT_RATES)
    }

    /// Time of the next beacon.
    pub fn next_event(&self) -> u64 {
        self.next_beacon
    }

    /// Sends the beacons due by `now`.
    pub fn poll(&mut self, now: u64) {
        if now >= self.next_beacon {
            let seq = self.seq();
            let b = Builder::mgmt(wlan::BEACON, BROADCAST, self.bssid, self.bssid, seq);
            let f = self.bss_body(b, now, true).build();
            self.out.push_back(f);
            self.next_beacon = now - now % BEACON_PERIOD + BEACON_PERIOD;
        }
    }

    /// The station `mac` is associated.
    pub fn associated(&self, mac: &Mac) -> bool {
        self.stations.get(mac).is_some_and(|s| s.associated)
    }

    /// A frame the chip sent.
    pub fn receive(&mut self, frame: &[u8], now: u64) {
        let Some(f) = Frame::parse(frame) else {
            return;
        };
        if f.is_mgmt(wlan::PROBE_REQ) {
            let ssid = wlan::element(f.body, wlan::IE_SSID).unwrap_or(&[]);
            if ssid.is_empty() || ssid == self.ssid.as_bytes() {
                let seq = self.seq();
                let b = Builder::mgmt(wlan::PROBE_RESP, f.addr2, self.bssid, self.bssid, seq);
                let resp = self.bss_body(b, now, false).build();
                self.out.push_back(resp);
            }
            return;
        }
        if f.addr1 != self.bssid {
            return;
        }
        let sta = f.addr2;
        if f.kind == wlan::TYPE_MGMT {
            match f.subtype {
                wlan::AUTH => {
                    let algorithm = u16::from_le_bytes([f.body[0], f.body[1]]);
                    // open system only
                    let status = if algorithm == 0 { 0 } else { 13 };
                    let seq = self.seq();
                    let resp = Builder::mgmt(wlan::AUTH, sta, self.bssid, self.bssid, seq)
                        .u16(algorithm)
                        .u16(2)
                        .u16(status)
                        .build();
                    self.out.push_back(resp);
                    if status == 0 {
                        let aid = self.stations.len() as u16 + 1;
                        self.stations.entry(sta).or_insert(Station {
                            associated: false,
                            aid,
                        });
                    }
                }
                wlan::ASSOC_REQ | wlan::REASSOC_REQ => {
                    let ies = if f.subtype == wlan::ASSOC_REQ {
                        f.body.get(4..)
                    } else {
                        f.body.get(10..)
                    };
                    let ssid = ies.and_then(|i| wlan::element(i, wlan::IE_SSID));
                    let Some(station) = self.stations.get_mut(&sta) else {
                        self.deauth(sta, 7);
                        return;
                    };
                    let ok = ssid == Some(self.ssid.as_bytes());
                    station.associated = ok;
                    let aid = station.aid;
                    let subtype = if f.subtype == wlan::ASSOC_REQ {
                        wlan::ASSOC_RESP
                    } else {
                        wlan::REASSOC_RESP
                    };
                    let seq = self.seq();
                    let resp = Builder::mgmt(subtype, sta, self.bssid, self.bssid, seq)
                        .u16(CAPABILITIES)
                        .u16(if ok { 0 } else { 1 })
                        .u16(0xc000 | aid)
                        .element(wlan::IE_RATES, &wlan::RATES)
                        .element(wlan::IE_EXT_RATES, &wlan::EXT_RATES)
                        .build();
                    self.out.push_back(resp);
                    if ok {
                        self.events.push_back(format!(
                            "station {} associated with {}",
                            wlan::fmt_mac(&sta),
                            self.ssid
                        ));
                    }
                }
                wlan::DEAUTH | wlan::DISASSOC => {
                    if self.stations.remove(&sta).is_some() {
                        self.events
                            .push_back(format!("station {} left", wlan::fmt_mac(&sta)));
                    }
                }
                _ => {}
            }
            return;
        }
        if f.kind != wlan::TYPE_DATA || f.flags & wlan::TO_DS == 0 {
            return;
        }
        if !self.associated(&sta) {
            // class 3 frame from a station that is not associated
            self.deauth(sta, 7);
            return;
        }
        if f.is_null() || f.flags & wlan::PROTECTED != 0 {
            return;
        }
        if let Some((ethertype, payload)) = wlan::llc(f.body) {
            self.to_lan
                .push_back(wlan::ethernet(f.addr3, sta, ethertype, payload));
        }
    }

    fn deauth(&mut self, sta: Mac, reason: u16) {
        let seq = self.seq();
        let f = Builder::mgmt(wlan::DEAUTH, sta, self.bssid, self.bssid, seq)
            .u16(reason)
            .build();
        self.out.push_back(f);
    }

    /// An Ethernet frame for the stations.
    pub fn send(&mut self, eth: &[u8]) {
        let Some((dst, src, ethertype, payload)) = wlan::parse_ethernet(eth) else {
            return;
        };
        let group = dst[0] & 1 != 0;
        if !group && !self.associated(&dst) {
            return;
        }
        if group && !self.stations.values().any(|s| s.associated) {
            return;
        }
        let seq = self.seq();
        let f = Builder::header(wlan::TYPE_DATA, 0, wlan::FROM_DS, dst, self.bssid, src, seq)
            .llc(ethertype, payload)
            .build();
        self.out.push_back(f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BSSID: Mac = [0x02, 0xe5, 0, 0, 0, 1];
    const STA: Mac = [0x7c, 0xdf, 0xa1, 1, 2, 3];

    fn reply(ap: &mut AccessPoint) -> Vec<u8> {
        ap.out.pop_front().expect("a reply")
    }

    #[test]
    fn a_station_scans_authenticates_associates_and_sends_data() {
        let mut ap = AccessPoint::new("emu", BSSID, 1);
        ap.poll(0);
        let beacon = reply(&mut ap);
        let b = Frame::parse(&beacon).unwrap();
        assert!(b.is_mgmt(wlan::BEACON));
        assert_eq!(
            wlan::element(&b.body[12..], wlan::IE_SSID),
            Some(&b"emu"[..])
        );
        assert_eq!(ap.next_event(), BEACON_PERIOD);

        let probe = Builder::mgmt(wlan::PROBE_REQ, BROADCAST, STA, BROADCAST, 1)
            .element(wlan::IE_SSID, b"")
            .build();
        ap.receive(&probe, 0);
        let r = reply(&mut ap);
        let r = Frame::parse(&r).unwrap();
        assert!(r.is_mgmt(wlan::PROBE_RESP));
        assert_eq!(r.addr1, STA);

        // data before association: deauthenticated
        let data = Builder::header(wlan::TYPE_DATA, 0, wlan::TO_DS, BSSID, STA, BROADCAST, 2)
            .llc(0x0806, &[0; 28])
            .build();
        ap.receive(&data, 0);
        assert!(Frame::parse(&reply(&mut ap)).unwrap().is_mgmt(wlan::DEAUTH));

        let auth = Builder::mgmt(wlan::AUTH, BSSID, STA, BSSID, 3)
            .u16(0)
            .u16(1)
            .u16(0)
            .build();
        ap.receive(&auth, 0);
        let r = reply(&mut ap);
        assert_eq!(&Frame::parse(&r).unwrap().body[..6], &[0, 0, 2, 0, 0, 0]);

        let assoc = Builder::mgmt(wlan::ASSOC_REQ, BSSID, STA, BSSID, 4)
            .u16(0x0401)
            .u16(10)
            .element(wlan::IE_SSID, b"emu")
            .build();
        ap.receive(&assoc, 0);
        let r = reply(&mut ap);
        let r = Frame::parse(&r).unwrap();
        assert!(r.is_mgmt(wlan::ASSOC_RESP));
        assert_eq!(&r.body[2..6], &[0, 0, 1, 0xc0]);
        assert!(ap.associated(&STA));

        ap.receive(&data, 0);
        let eth = ap.to_lan.pop_front().unwrap();
        assert_eq!(
            wlan::parse_ethernet(&eth).unwrap(),
            (BROADCAST, STA, 0x0806, &[0u8; 28][..])
        );

        ap.send(&wlan::ethernet(STA, [2, 0, 0, 0, 0, 9], 0x0800, &[1, 2]));
        let r = reply(&mut ap);
        let r = Frame::parse(&r).unwrap();
        assert_eq!(
            (r.addr1, r.addr2, r.addr3),
            (STA, BSSID, [2, 0, 0, 0, 0, 9])
        );
        assert_eq!(r.flags, wlan::FROM_DS);
        assert_eq!(wlan::llc(r.body), Some((0x0800, &[1u8, 2][..])));
    }
}
