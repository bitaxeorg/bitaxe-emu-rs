//! 802.11 frames: the fields an access point and a station need.

pub type Mac = [u8; 6];

pub const BROADCAST: Mac = [0xff; 6];

/// Frame types (frame control bits 3:2).
pub const TYPE_MGMT: u8 = 0;
pub const TYPE_DATA: u8 = 2;

/// Management subtypes.
pub const ASSOC_REQ: u8 = 0;
pub const ASSOC_RESP: u8 = 1;
pub const REASSOC_REQ: u8 = 2;
pub const REASSOC_RESP: u8 = 3;
pub const PROBE_REQ: u8 = 4;
pub const PROBE_RESP: u8 = 5;
pub const BEACON: u8 = 8;
pub const DISASSOC: u8 = 10;
pub const AUTH: u8 = 11;
pub const DEAUTH: u8 = 12;

/// Frame control flags (second byte).
pub const TO_DS: u8 = 0x01;
pub const FROM_DS: u8 = 0x02;
pub const PROTECTED: u8 = 0x40;

/// Information element IDs.
pub const IE_SSID: u8 = 0;
pub const IE_RATES: u8 = 1;
pub const IE_DS: u8 = 3;
pub const IE_TIM: u8 = 5;
pub const IE_RSN: u8 = 48;
pub const IE_EXT_RATES: u8 = 50;

/// 1, 2, 5.5, 11 (basic), 6, 9, 12, 18 Mb/s, then 24, 36, 48, 54.
pub const RATES: [u8; 8] = [0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24];
pub const EXT_RATES: [u8; 4] = [0x30, 0x48, 0x60, 0x6c];

/// LLC/SNAP header in front of the EtherType of data frames.
const SNAP: [u8; 6] = [0xaa, 0xaa, 0x03, 0x00, 0x00, 0x00];

/// The parsed header of a frame.
#[derive(Clone, Debug)]
pub struct Frame<'a> {
    pub kind: u8,
    pub subtype: u8,
    pub flags: u8,
    pub addr1: Mac,
    pub addr2: Mac,
    pub addr3: Mac,
    /// Present when both TO_DS and FROM_DS are set.
    pub addr4: Option<Mac>,
    pub seq: u16,
    pub body: &'a [u8],
}

fn mac(b: &[u8]) -> Mac {
    b[..6].try_into().unwrap()
}

impl<'a> Frame<'a> {
    pub fn parse(f: &'a [u8]) -> Option<Self> {
        if f.len() < 24 {
            return None;
        }
        let kind = (f[0] >> 2) & 3;
        let subtype = f[0] >> 4;
        let flags = f[1];
        let mut at = 24;
        let addr4 = if kind == TYPE_DATA && flags & (TO_DS | FROM_DS) == TO_DS | FROM_DS {
            at += 6;
            Some(mac(f.get(24..30)?))
        } else {
            None
        };
        // QoS data subtypes carry a QoS control field.
        if kind == TYPE_DATA && subtype & 8 != 0 {
            at += 2;
        }
        Some(Frame {
            kind,
            subtype,
            flags,
            addr1: mac(&f[4..]),
            addr2: mac(&f[10..]),
            addr3: mac(&f[16..]),
            addr4,
            seq: u16::from_le_bytes([f[22], f[23]]) >> 4,
            body: f.get(at..)?,
        })
    }

    pub fn is_mgmt(&self, subtype: u8) -> bool {
        self.kind == TYPE_MGMT && self.subtype == subtype
    }

    /// Data subtypes without payload (null, QoS null).
    pub fn is_null(&self) -> bool {
        self.kind == TYPE_DATA && self.subtype & 4 != 0
    }
}

/// The information elements of `ies`: (id, data).
pub fn elements(ies: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut rest = ies;
    std::iter::from_fn(move || {
        let (&id, after) = rest.split_first()?;
        let (&len, after) = after.split_first()?;
        let data = after.get(..len as usize)?;
        rest = &after[len as usize..];
        Some((id, data))
    })
}

pub fn element(ies: &[u8], id: u8) -> Option<&[u8]> {
    elements(ies).find(|&(i, _)| i == id).map(|(_, d)| d)
}

/// Builds frames.
pub struct Builder(pub Vec<u8>);

impl Builder {
    /// A management frame header.
    pub fn mgmt(subtype: u8, addr1: Mac, addr2: Mac, addr3: Mac, seq: u16) -> Self {
        Self::header(TYPE_MGMT, subtype, 0, addr1, addr2, addr3, seq)
    }

    pub fn header(
        kind: u8,
        subtype: u8,
        flags: u8,
        addr1: Mac,
        addr2: Mac,
        addr3: Mac,
        seq: u16,
    ) -> Self {
        let mut f = Vec::with_capacity(256);
        f.push(subtype << 4 | kind << 2);
        f.push(flags);
        f.extend_from_slice(&[0, 0]);
        f.extend_from_slice(&addr1);
        f.extend_from_slice(&addr2);
        f.extend_from_slice(&addr3);
        f.extend_from_slice(&(seq << 4).to_le_bytes());
        Builder(f)
    }

    pub fn u16(mut self, v: u16) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn u64(mut self, v: u64) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn bytes(mut self, b: &[u8]) -> Self {
        self.0.extend_from_slice(b);
        self
    }

    pub fn element(mut self, id: u8, data: &[u8]) -> Self {
        self.0.push(id);
        self.0.push(data.len() as u8);
        self.0.extend_from_slice(data);
        self
    }

    /// The LLC/SNAP header and EtherType of a data frame, then its payload.
    pub fn llc(mut self, ethertype: u16, payload: &[u8]) -> Self {
        self.0.extend_from_slice(&SNAP);
        self.0.extend_from_slice(&ethertype.to_be_bytes());
        self.0.extend_from_slice(payload);
        self
    }

    pub fn build(self) -> Vec<u8> {
        self.0
    }
}

/// The EtherType and payload of a data frame body (LLC/SNAP).
pub fn llc(body: &[u8]) -> Option<(u16, &[u8])> {
    if body.len() < 8 || body[..6] != SNAP {
        return None;
    }
    Some((u16::from_be_bytes([body[6], body[7]]), &body[8..]))
}

/// An Ethernet II frame.
pub fn ethernet(dst: Mac, src: Mac, ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(14 + payload.len());
    f.extend_from_slice(&dst);
    f.extend_from_slice(&src);
    f.extend_from_slice(&ethertype.to_be_bytes());
    f.extend_from_slice(payload);
    f
}

/// Destination, source, EtherType and payload of an Ethernet II frame.
pub fn parse_ethernet(f: &[u8]) -> Option<(Mac, Mac, u16, &[u8])> {
    if f.len() < 14 {
        return None;
    }
    Some((
        mac(f),
        mac(&f[6..]),
        u16::from_be_bytes([f[12], f[13]]),
        &f[14..],
    ))
}

pub fn fmt_mac(m: &Mac) -> String {
    m.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let sta = [2, 0, 0, 0, 0, 1];
        let ap = [2, 0, 0, 0, 0, 2];
        let f = Builder::header(TYPE_DATA, 8, TO_DS, ap, sta, BROADCAST, 7)
            .bytes(&[0, 0])
            .llc(0x0800, &[1, 2, 3])
            .build();
        let p = Frame::parse(&f).unwrap();
        assert_eq!((p.kind, p.subtype, p.seq), (TYPE_DATA, 8, 7));
        assert_eq!((p.addr1, p.addr2, p.addr3), (ap, sta, BROADCAST));
        assert_eq!(llc(p.body), Some((0x0800, &[1u8, 2, 3][..])));
        let ies = [0, 3, b'a', b'b', b'c', 3, 1, 6];
        assert_eq!(element(&ies, IE_SSID), Some(&b"abc"[..]));
        assert_eq!(element(&ies, IE_DS), Some(&[6u8][..]));
        assert_eq!(element(&ies, IE_TIM), None);
    }
}
