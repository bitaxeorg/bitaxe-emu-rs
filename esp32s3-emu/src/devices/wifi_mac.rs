//! Wi-Fi MAC (0x6003_3000..0x6003_6000): what the Wi-Fi driver programs to send
//! and receive frames. The radio is not modelled: frames go to and come from
//! the host as whole 802.11 MPDUs (no FCS), see [`WifiMac::tx_frames`] and
//! [`WifiMac::rx_frames`].
//!
//! Transmit: each of the [`TX_SLOTS`] slots has a PLCP0 register (`0xd08 - 8 *
//! slot`) holding the low 20 bits of its first DMA descriptor; setting its bits
//! 31:30 sends the frame. The MAC reads the descriptor chain, reports the frame
//! as sent [`TX_AIRTIME`] later (completion bit of the slot, result 0 in its PMD
//! register, interrupt [`INT_TX_DONE`]) and clears the slot.
//!
//! Receive: `RX_BASE` gives the first descriptor. Each frame fills the next
//! descriptors the DMA owns with a 48-byte header (RSSI, rate, channel,
//! timestamp, which interfaces the frame is for, length) and the frame, sets
//! their length and SUC_EOF on the last one, which `RX_LAST` then points to, and
//! raises [`INT_RX`]. When the list runs out, the MAC waits for the driver to
//! append descriptors and set `RX_CTRL` bit 0, then follows the last descriptor's
//! link.
//!
//! The TSF counts microseconds from its reset. The other registers read back
//! what was written, except in the third page, which reads 0 (the radio status
//! registers there are polled until they clear).

use std::collections::VecDeque;

use super::TICK_HZ;
use super::gdma::DmaMemory;

/// Registers: three 4 KB pages.
pub const SIZE: u32 = 0x3000;

const RX_CTRL: u32 = 0x084;
const RX_CTRL_RELOAD: u32 = 1 << 0;
const RX_CTRL_ENABLE: u32 = 1 << 31;
const RX_BASE: u32 = 0x088;
const RX_NEXT: u32 = 0x08c;
const RX_LAST: u32 = 0x090;
/// Bits 31:20 complete the descriptor addresses of `RX_NEXT` and `RX_LAST`.
const DMA_ADDR_HIGH: u32 = 0xc64;
/// Receive address filters, per interface: address low and high (bits 15:0) at
/// 0x40 + 8 * i, mask low and high (bits 15:0, enable bit 16) at 0x60 + 8 * i.
const RA_FILTER: u32 = 0x040;
const RA_MASK: u32 = 0x060;
const RA_ENABLE: u32 = 1 << 16;
pub const INTERFACES: usize = 4;

const INT_STATUS: u32 = 0xc3c;
const INT_CLEAR: u32 = 0xc40;
/// A frame was received.
pub const INT_RX: u32 = 1 << 24;
/// A slot finished sending.
pub const INT_TX_DONE: u32 = 1 << 7;

/// Transmit errors: collision bits 10:0 and timeout bits 23:16, by slot.
const TXQ_ERROR_CLEAR: u32 = 0xca4;
const TXQ_ERROR: u32 = 0xca8;
const TXQ_COMPLETE_CLEAR: u32 = 0xcac;
const TXQ_COMPLETE: u32 = 0xcb0;

pub const TX_SLOTS: usize = 5;
/// Slot `s`: PLCP0 at `TX_PLCP0 - 8 * s`.
const TX_PLCP0: u32 = 0xd08;
const PLCP0_DMA_ADDR: u32 = 0x000f_ffff;
const PLCP0_GO: u32 = 0b11 << 30;
/// Slot `s`: PLCP1 (length with FCS in bits 11:0) and PMD (result, bits 15:12)
/// at these minus `TX_SLOT_STRIDE * s`.
const TX_PLCP1: u32 = 0x12f8;
const TX_PMD: u32 = 0x1320;
const TX_SLOT_STRIDE: u32 = 0x4c;

/// MAC control: setting bit 1 starts the MAC, bit 0 then reads 1 (ready).
const MAC_CTRL: u32 = 0xd14;
const MAC_CTRL_START: u32 = 1 << 1;
const MAC_CTRL_READY: u32 = 1 << 0;

/// TSF: control (setting bit 0 or 1 latches the time), time low and high (µs).
const TSF_CTRL: u32 = 0x200c;
const TSF_CTRL_RESET: u32 = 1 << 5;
const TSF_LOW: u32 = 0x2018;
const TSF_HIGH: u32 = 0x201c;
/// Start of the page that reads 0 apart from the TSF.
const RADIO_PAGE: u32 = 0x2000;

/// Time from the start of a transmission to its completion.
const TX_AIRTIME: u64 = TICK_HZ / 1_000_000 * 300;
/// Receive header in front of each frame.
pub const RX_HEADER: usize = 48;
/// Frames kept for the firmware while it has no receive descriptor.
const RX_QUEUE: usize = 64;
/// Descriptor word 0: buffer size 11:0, length 23:12, SUC_EOF, owner (DMA).
const DESC_SIZE: u32 = 0xfff;
const DESC_LENGTH_SHIFT: u32 = 12;
const DESC_EOF: u32 = 1 << 30;
const DESC_OWNER: u32 = 1 << 31;

/// A received frame and what the radio measured.
#[derive(Clone, Debug)]
pub struct RxFrame {
    /// The MPDU, without FCS.
    pub data: Vec<u8>,
    pub rssi: i8,
    pub channel: u8,
}

pub struct WifiMac {
    regs: Vec<u32>,
    int_status: u32,
    txq_error: u32,
    txq_complete: u32,
    /// Descriptor the next frame goes to, 0 when the list ran out.
    rx_next: u32,
    /// Last descriptor filled.
    rx_last: u32,
    /// Time of the TSF reset, ticks.
    tsf_base: u64,
    tsf_latch: Option<u64>,
    /// Slots sending, with their completion time.
    tx_busy: Vec<(usize, u64)>,
    /// A slot was started or a reload asked: [`WifiMac::service`] has work.
    pending: bool,
    /// Frames the firmware sent, oldest first.
    pub tx_frames: VecDeque<Vec<u8>>,
    /// Frames for the firmware, oldest first.
    pub rx_frames: VecDeque<RxFrame>,
    pub stats: Stats,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub tx: u64,
    pub rx: u64,
    /// Received frames dropped: receive off, or no descriptor for too long.
    pub rx_dropped: u64,
}

impl Default for WifiMac {
    fn default() -> Self {
        Self::new()
    }
}

impl WifiMac {
    pub fn new() -> Self {
        WifiMac {
            regs: vec![0; (SIZE / 4) as usize],
            int_status: 0,
            txq_error: 0,
            txq_complete: 0,
            rx_next: 0,
            rx_last: 0,
            tsf_base: 0,
            tsf_latch: None,
            tx_busy: Vec::new(),
            pending: false,
            tx_frames: VecDeque::new(),
            rx_frames: VecDeque::new(),
            stats: Stats::default(),
        }
    }

    pub fn reset(&mut self) {
        let frames = std::mem::take(&mut self.rx_frames);
        let stats = self.stats;
        *self = Self::new();
        self.rx_frames = frames;
        self.stats = stats;
    }

    /// The receive list: next descriptor to fill (0 when it ran out), last filled,
    /// frames waiting.
    pub fn rx_state(&self) -> (u32, u32, usize) {
        (self.rx_next, self.rx_last, self.rx_frames.len())
    }

    pub fn irq(&self) -> bool {
        self.int_status != 0
    }

    fn reg(&self, off: u32) -> u32 {
        self.regs.get((off / 4) as usize).copied().unwrap_or(0)
    }

    fn tsf_us(&self, now: u64) -> u64 {
        now.saturating_sub(self.tsf_base) / (TICK_HZ / 1_000_000)
    }

    /// The TSF, µs.
    pub fn tsf(&self, now: u64) -> u64 {
        self.tsf_us(now)
    }

    fn dma_high(&self) -> u32 {
        match self.reg(DMA_ADDR_HIGH) & 0xfff0_0000 {
            0 => 0x3fc0_0000,
            high => high,
        }
    }

    pub fn read(&mut self, off: u32, now: u64) -> u32 {
        match off {
            INT_STATUS => self.int_status,
            TXQ_ERROR => self.txq_error,
            TXQ_COMPLETE => self.txq_complete,
            TXQ_ERROR_CLEAR | TXQ_COMPLETE_CLEAR | INT_CLEAR => 0,
            RX_NEXT => self.dma_high() | (self.rx_next & 0xf_ffff),
            RX_LAST => self.rx_last & 0xf_ffff,
            TSF_LOW | TSF_HIGH => {
                let t = self.tsf_latch.unwrap_or_else(|| self.tsf_us(now));
                if off == TSF_LOW {
                    t as u32
                } else {
                    (t >> 32) as u32
                }
            }
            MAC_CTRL => {
                let v = self.reg(MAC_CTRL);
                if v & MAC_CTRL_START != 0 {
                    v | MAC_CTRL_READY
                } else {
                    v
                }
            }
            o if o >= RADIO_PAGE && o != TSF_CTRL => 0,
            o => self.reg(o),
        }
    }

    /// Returns whether the interrupt output may have changed.
    pub fn write(&mut self, off: u32, v: u32, now: u64) -> bool {
        match off {
            INT_CLEAR => {
                self.int_status &= !v;
                return true;
            }
            TXQ_ERROR_CLEAR => {
                self.txq_error &= !v;
                return false;
            }
            TXQ_COMPLETE_CLEAR => {
                self.txq_complete &= !v;
                return false;
            }
            RX_BASE => {
                self.rx_next = v;
                self.rx_last = 0;
            }
            RX_CTRL if v & RX_CTRL_RELOAD != 0 => self.pending = true,
            TSF_CTRL => {
                if v & TSF_CTRL_RESET != 0 {
                    self.tsf_base = now;
                }
                self.tsf_latch = (v & 0b11 != 0).then(|| self.tsf_us(now));
            }
            o if (TX_PLCP0 - 8 * (TX_SLOTS as u32 - 1)..=TX_PLCP0).contains(&o)
                && (TX_PLCP0 - o) % 8 == 0
                && v & PLCP0_GO == PLCP0_GO =>
            {
                self.pending = true;
            }
            _ => {}
        }
        if let Some(r) = self.regs.get_mut((off / 4) as usize) {
            *r = v;
        }
        false
    }

    /// There are started slots or a descriptor reload to serve with
    /// [`WifiMac::service`].
    pub fn needs_service(&self) -> bool {
        self.pending
    }

    /// Serves the started slots (reads their frames) and the descriptor reload.
    pub fn service(&mut self, mem: &mut dyn DmaMemory, now: u64) {
        self.pending = false;
        let ctrl = self.reg(RX_CTRL);
        if ctrl & RX_CTRL_RELOAD != 0 {
            self.regs[(RX_CTRL / 4) as usize] = ctrl & !RX_CTRL_RELOAD;
            if self.rx_next == 0 && self.rx_last != 0 {
                self.rx_next = read_u32(mem, self.rx_last.wrapping_add(8)).unwrap_or(0);
            }
        }
        for slot in 0..TX_SLOTS {
            let plcp0_off = TX_PLCP0 - 8 * slot as u32;
            let plcp0 = self.reg(plcp0_off);
            if plcp0 & PLCP0_GO != PLCP0_GO || self.tx_busy.iter().any(|&(s, _)| s == slot) {
                continue;
            }
            let first = self.dma_high() | (plcp0 & PLCP0_DMA_ADDR);
            let mut data = read_chain(mem, first);
            // The descriptors count the FCS, which the MAC appends.
            let with_fcs = (self.reg(TX_PLCP1 - TX_SLOT_STRIDE * slot as u32) & 0xfff) as usize;
            let len = if (4..=data.len()).contains(&with_fcs) {
                with_fcs - 4
            } else {
                data.len().saturating_sub(4)
            };
            data.truncate(len);
            self.stats.tx += 1;
            self.tx_frames.push_back(data);
            self.tx_busy.push((slot, now + TX_AIRTIME));
        }
    }

    /// Time of the next transmission completion.
    pub fn next_event(&self) -> Option<u64> {
        self.tx_busy.iter().map(|&(_, t)| t).min()
    }

    /// Completes the transmissions due by `now`. Returns whether the interrupt
    /// output may have changed.
    pub fn update(&mut self, now: u64) -> bool {
        let mut changed = false;
        let regs = &mut self.regs;
        let (int_status, txq_complete) = (&mut self.int_status, &mut self.txq_complete);
        self.tx_busy.retain(|&(slot, end)| {
            if end > now {
                return true;
            }
            let plcp0 = &mut regs[((TX_PLCP0 - 8 * slot as u32) / 4) as usize];
            *plcp0 &= !PLCP0_GO;
            regs[((TX_PMD - TX_SLOT_STRIDE * slot as u32) / 4) as usize] = 0;
            *txq_complete |= 1 << slot;
            *int_status |= INT_TX_DONE;
            changed = true;
            false
        });
        changed
    }

    /// The interfaces whose receive address filter `addr` passes, as bits.
    fn interfaces_for(&self, addr: &[u8]) -> u32 {
        let group = addr[0] & 1 != 0;
        let mut bits = 0;
        for i in 0..INTERFACES as u32 {
            let mask_high = self.reg(RA_MASK + 8 * i + 4);
            if mask_high & RA_ENABLE == 0 {
                continue;
            }
            let filter = u64::from(self.reg(RA_FILTER + 8 * i))
                | u64::from(self.reg(RA_FILTER + 8 * i + 4) & 0xffff) << 32;
            let mask = u64::from(self.reg(RA_MASK + 8 * i)) | u64::from(mask_high & 0xffff) << 32;
            let a = addr[..6]
                .iter()
                .rev()
                .fold(0u64, |acc, &b| acc << 8 | u64::from(b));
            if group || (a ^ filter) & mask == 0 {
                bits |= 1 << i;
            }
        }
        bits
    }

    /// Writes the waiting received frames into the receive descriptors. Returns
    /// whether the interrupt output may have changed.
    pub fn deliver(&mut self, mem: &mut dyn DmaMemory, now: u64) -> bool {
        let mut delivered = false;
        while let Some(frame) = self.rx_frames.front() {
            if self.reg(RX_CTRL) & RX_CTRL_ENABLE == 0 || frame.data.len() < 10 {
                self.rx_frames.pop_front();
                self.stats.rx_dropped += 1;
                continue;
            }
            let mut buf = vec![0u8; RX_HEADER];
            let mut word =
                |i: usize, v: u32| buf[4 * i..4 * i + 4].copy_from_slice(&v.to_le_bytes());
            // Non-HT, 6 Mb/s.
            word(
                0,
                u32::from(frame.rssi as u8)
                    | 0x0b << 8
                    | self.interfaces_for(&frame.data[4..]) << 28,
            );
            word(2, u32::from(frame.channel & 0xf) << 16);
            word(3, self.tsf_us(now) as u32);
            word(5, u32::from(-97i8 as u8));
            word(11, (frame.data.len() as u32 + 4) & 0xfff);
            buf.extend_from_slice(&frame.data);
            if !self.fill(mem, &buf) {
                if self.rx_frames.len() > RX_QUEUE {
                    self.rx_frames.pop_front();
                    self.stats.rx_dropped += 1;
                    continue;
                }
                break;
            }
            self.rx_frames.pop_front();
            self.stats.rx += 1;
            delivered = true;
        }
        if delivered {
            self.int_status |= INT_RX;
        }
        delivered
    }

    /// Writes `data` into the descriptors from `rx_next`. False if they are too
    /// few.
    fn fill(&mut self, mem: &mut dyn DmaMemory, data: &[u8]) -> bool {
        // Check the room first, so that a frame is never cut.
        let mut room = 0;
        let mut d = self.rx_next;
        let mut chain = Vec::new();
        while room < data.len() {
            let (Some(w0), Some(buf), Some(next)) = (
                read_u32(mem, d),
                read_u32(mem, d.wrapping_add(4)),
                read_u32(mem, d.wrapping_add(8)),
            ) else {
                return false;
            };
            if d == 0 || w0 & DESC_OWNER == 0 || w0 & DESC_SIZE == 0 {
                return false;
            }
            room += (w0 & DESC_SIZE) as usize;
            chain.push((d, w0, buf, next));
            d = next;
        }
        let mut at = 0;
        let count = chain.len();
        for (i, &(d, w0, buf, next)) in chain.iter().enumerate() {
            let n = ((w0 & DESC_SIZE) as usize).min(data.len() - at);
            for (k, word) in data[at..at + n].chunks(4).enumerate() {
                let mut b = [0u8; 4];
                b[..word.len()].copy_from_slice(word);
                if word.len() < 4 {
                    // Keep the bytes after the end of the frame.
                    let old = read_u32(mem, buf + 4 * k as u32).unwrap_or(0).to_le_bytes();
                    b[word.len()..].copy_from_slice(&old[word.len()..]);
                }
                mem.dma_write_u32(buf + 4 * k as u32, u32::from_le_bytes(b));
            }
            at += n;
            let mut w = (w0 & !(DESC_SIZE << DESC_LENGTH_SHIFT) & !DESC_EOF)
                | (n as u32) << DESC_LENGTH_SHIFT;
            if i + 1 == count {
                w |= DESC_EOF;
            }
            mem.dma_write_u32(d, w);
            self.rx_last = d;
            self.rx_next = next;
        }
        true
    }
}

fn read_u32(mem: &dyn DmaMemory, addr: u32) -> Option<u32> {
    let b = mem.dma_read(addr, 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// The data of the descriptor chain from `first`, up to the SUC_EOF one.
fn read_chain(mem: &dyn DmaMemory, first: u32) -> Vec<u8> {
    let mut data = Vec::new();
    let mut d = first;
    for _ in 0..64 {
        let (Some(w0), Some(buf), Some(next)) = (
            read_u32(mem, d),
            read_u32(mem, d.wrapping_add(4)),
            read_u32(mem, d.wrapping_add(8)),
        ) else {
            break;
        };
        let len = ((w0 >> DESC_LENGTH_SHIFT) & DESC_SIZE) as usize;
        if let Some(bytes) = mem.dma_read(buf, len) {
            data.extend_from_slice(bytes);
        }
        if w0 & DESC_EOF != 0 || next == 0 {
            break;
        }
        d = next;
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flat memory from 0x3fc8_0000.
    struct Mem(Vec<u8>);
    const BASE: u32 = 0x3fc8_0000;

    impl DmaMemory for Mem {
        fn dma_read(&self, addr: u32, len: usize) -> Option<&[u8]> {
            let off = addr.checked_sub(BASE)? as usize;
            self.0.get(off..off + len)
        }
        fn dma_write_u32(&mut self, addr: u32, value: u32) -> bool {
            let Some(off) = addr.checked_sub(BASE) else {
                return false;
            };
            let off = off as usize;
            match self.0.get_mut(off..off + 4) {
                Some(b) => {
                    b.copy_from_slice(&value.to_le_bytes());
                    true
                }
                None => false,
            }
        }
    }

    impl Mem {
        fn put(&mut self, addr: u32, v: u32) {
            self.dma_write_u32(addr, v);
        }
        fn get(&self, addr: u32) -> u32 {
            read_u32(self, addr).unwrap()
        }
    }

    #[test]
    fn a_started_slot_sends_its_frame_and_completes() {
        let mut mem = Mem(vec![0; 0x1000]);
        let frame: Vec<u8> = (0..30u8).collect();
        // descriptor at BASE, buffer at BASE + 0x100, length with the FCS
        mem.put(BASE, DESC_OWNER | DESC_EOF | 34 << 12 | 34);
        mem.put(BASE + 4, BASE + 0x100);
        for (i, w) in frame.chunks(4).enumerate() {
            let mut b = [0; 4];
            b[..w.len()].copy_from_slice(w);
            mem.put(BASE + 0x100 + 4 * i as u32, u32::from_le_bytes(b));
        }
        let mut mac = WifiMac::new();
        mac.write(TX_PLCP1 - TX_SLOT_STRIDE * 2, 34, 0);
        mac.write(TX_PLCP0 - 16, 0x0060_0000 | (BASE & 0xf_ffff), 0);
        assert!(!mac.needs_service());
        let v = mac.read(TX_PLCP0 - 16, 0);
        mac.write(TX_PLCP0 - 16, v | PLCP0_GO, 0);
        assert!(mac.needs_service());
        mac.service(&mut mem, 0);
        assert_eq!(mac.tx_frames.pop_front().unwrap(), frame);
        assert!(!mac.irq());
        assert!(mac.update(TX_AIRTIME));
        assert!(mac.irq());
        assert_eq!(mac.read(TXQ_COMPLETE, 0), 1 << 2);
        assert_eq!(mac.read(INT_STATUS, 0), INT_TX_DONE);
        assert_eq!(mac.read(TX_PLCP0 - 16, 0) & PLCP0_GO, 0);
        mac.write(TXQ_COMPLETE_CLEAR, 1 << 2, 0);
        mac.write(INT_CLEAR, INT_TX_DONE, 0);
        assert!(!mac.irq());
        assert_eq!(mac.read(TXQ_COMPLETE, 0), 0);
    }

    #[test]
    fn frames_fill_the_receive_list_then_wait_for_a_reload() {
        let mut mem = Mem(vec![0; 0x2000]);
        // two descriptors of 256 bytes, then the end of the list
        for (i, d) in [BASE, BASE + 16].into_iter().enumerate() {
            mem.put(d, DESC_OWNER | 256 << 12 | 256);
            mem.put(d + 4, BASE + 0x100 + 0x100 * i as u32);
            mem.put(d + 8, if i == 0 { BASE + 16 } else { 0 });
        }
        let mut mac = WifiMac::new();
        mac.write(DMA_ADDR_HIGH, 0x3fcf_ffff, 0);
        mac.write(RX_BASE, BASE, 0);
        mac.write(RX_CTRL, RX_CTRL_ENABLE, 0);
        // interface 0 at 02:00:00:00:00:01
        mac.write(RA_FILTER, 0x0000_0002, 0);
        mac.write(RA_FILTER + 4, 0x0100, 0);
        mac.write(RA_MASK, u32::MAX, 0);
        mac.write(RA_MASK + 4, 0xffff | RA_ENABLE, 0);
        let mut frame = vec![0x08, 0x02, 0, 0, 0x02, 0, 0, 0, 0, 0x01];
        frame.extend_from_slice(&[0xaa; 20]);
        for _ in 0..3 {
            mac.rx_frames.push_back(RxFrame {
                data: frame.clone(),
                rssi: -40,
                channel: 1,
            });
        }
        assert!(mac.deliver(&mut mem, 0));
        assert!(mac.irq());
        // two delivered, the third waits
        assert_eq!(mac.rx_frames.len(), 1);
        assert_eq!(mac.read(RX_LAST, 0), (BASE + 16) & 0xf_ffff);
        assert_eq!(mac.read(RX_NEXT, 0), 0x3fc0_0000);
        let w0 = mem.get(BASE);
        assert_ne!(w0 & DESC_EOF, 0);
        assert_eq!((w0 >> 12) & 0xfff, (RX_HEADER + frame.len()) as u32);
        let header = mem.get(BASE + 0x100);
        assert_eq!(header & 0xff, -40i8 as u8 as u32);
        assert_eq!(header >> 28, 1, "for interface 0");
        assert_eq!(mem.get(BASE + 0x100 + 44) & 0xfff, frame.len() as u32 + 4);
        assert_eq!(mem.get(BASE + 0x100 + 48), 0x0000_0208);
        // the driver appends a descriptor and asks for a reload
        mem.put(BASE + 32, DESC_OWNER | 256 << 12 | 256);
        mem.put(BASE + 36, BASE + 0x300);
        mem.put(BASE + 16 + 8, BASE + 32);
        mac.write(RX_CTRL, RX_CTRL_ENABLE | RX_CTRL_RELOAD, 0);
        assert!(mac.needs_service());
        mac.service(&mut mem, 0);
        assert_eq!(mac.read(RX_CTRL, 0) & RX_CTRL_RELOAD, 0);
        assert!(mac.deliver(&mut mem, 0));
        assert!(mac.rx_frames.is_empty());
        assert_eq!(mac.read(RX_LAST, 0), (BASE + 32) & 0xf_ffff);
    }

    #[test]
    fn the_tsf_counts_microseconds_from_its_reset() {
        let mut mac = WifiMac::new();
        let us = TICK_HZ / 1_000_000;
        mac.write(TSF_CTRL, TSF_CTRL_RESET, 1000 * us);
        mac.write(TSF_CTRL, 1, 3500 * us);
        assert_eq!(mac.read(TSF_LOW, 9000 * us), 2500);
        assert_eq!(mac.read(TSF_HIGH, 9000 * us), 0);
        mac.write(TSF_CTRL, 0, 9000 * us);
        assert_eq!(mac.read(TSF_LOW, 9000 * us), 8000);
    }
}
