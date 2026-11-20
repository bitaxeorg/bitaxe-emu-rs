//! GDMA (0x6003_F000): five channels, each with an IN (peripheral to memory)
//! and an OUT (memory to peripheral) side.
//!
//! An OUT channel follows a chain of descriptors of three words: DW0 holds the
//! buffer size (bits 11:0), the number of bytes to send (bits 23:12), SUC_EOF
//! (bit 30) and the owner (bit 31, 1: the DMA engine); DW1 the buffer address;
//! DW2 the next descriptor, 0 for the last one. OUTLINK_ADDR holds the low 20
//! bits of the first descriptor, whose high bits are those of internal SRAM.
//!
//! Setting OUTLINK_START makes the channel read the whole chain from memory
//! ([`Gdma::service`], which the bus runs with access to SRAM): its data then
//! waits in the channel for the peripheral OUT_PERI_SEL selects, which takes it
//! ([`Gdma::take_out_data`]) and reports when it has sent it out
//! ([`Gdma::finish_out`]); the channel then raises OUT_DONE, and OUT_EOF and
//! OUT_TOTAL_EOF for a chain with SUC_EOF set. Descriptors and buffers are in
//! internal SRAM or in PSRAM pages the MMU maps in the data window; one
//! elsewhere, or a descriptor the CPU owns while OUT_CHECK_OWNER is set, raises
//! OUT_DSCR_ERR. IN channels keep their registers and never transfer.

/// Channels.
pub const CHANNELS: usize = 5;

/// OUT_PERI_SEL / IN_PERI_SEL values.
pub mod peripheral {
    pub const LCD_CAM: u32 = 5;
}

/// High bits of the descriptor addresses that OUTLINK_ADDR and INLINK_ADDR
/// complete (internal SRAM, D-bus view).
pub const LINK_ADDR_BASE: u32 = 0x3fc0_0000;
const LINK_ADDR_MASK: u32 = 0x000f_ffff;

const SIZE: u32 = 0x410;
const CHANNEL_STRIDE: u32 = 0xc0;

// Per-channel registers.
const IN_CONF1: u32 = 0x04;
const IN_INT_RAW: u32 = 0x08;
const IN_INT_ST: u32 = 0x0c;
const IN_INT_ENA: u32 = 0x10;
const IN_INT_CLR: u32 = 0x14;
const INFIFO_STATUS: u32 = 0x18;
const IN_POP: u32 = 0x1c;
const IN_LINK: u32 = 0x20;
const IN_WIGHT: u32 = 0x3c;
const IN_PERI_SEL: u32 = 0x48;
const OUT_CONF0: u32 = 0x60;
const OUT_INT_RAW: u32 = 0x68;
const OUT_INT_ST: u32 = 0x6c;
const OUT_INT_ENA: u32 = 0x70;
const OUT_INT_CLR: u32 = 0x74;
const OUTFIFO_STATUS: u32 = 0x78;
const OUT_PUSH: u32 = 0x7c;
const OUT_LINK: u32 = 0x80;
const OUT_STATE: u32 = 0x84;
const OUT_EOF_DES_ADDR: u32 = 0x88;
const OUT_EOF_BFR_DES_ADDR: u32 = 0x8c;
const OUT_DSCR: u32 = 0x90;
const OUT_DSCR_BF0: u32 = 0x94;
const OUT_DSCR_BF1: u32 = 0x98;
const OUT_WIGHT: u32 = 0x9c;
const OUT_CONF1: u32 = 0x64;
const OUT_PERI_SEL: u32 = 0xa8;

// Common registers.
const PD_CONF: u32 = 0x3c4;
const IN_SRAM_SIZE_CH0: u32 = 0x3cc;
const OUT_SRAM_SIZE_CH0: u32 = 0x3d0;
const SRAM_SIZE_STRIDE: u32 = 0x08;

/// OUT_CONF0 fields.
const OUT_RST: u32 = 1 << 0;
const OUT_AUTO_WRBACK: u32 = 1 << 2;
const OUT_EOF_MODE: u32 = 1 << 3;
/// OUT_CONF1.OUT_CHECK_OWNER.
const OUT_CHECK_OWNER: u32 = 1 << 12;
/// OUT_LINK fields.
const OUTLINK_STOP: u32 = 1 << 20;
const OUTLINK_START: u32 = 1 << 21;
const OUTLINK_RESTART: u32 = 1 << 22;
const OUTLINK_PARK: u32 = 1 << 23;
/// OUT_PUSH.OUTFIFO_PUSH.
const OUTFIFO_PUSH: u32 = 1 << 9;
/// OUT_STATE.OUTLINK_DSCR_ADDR.
const OUT_STATE_DSCR_ADDR: u32 = 0x3_ffff;
/// OUT_PERI_SEL.PERI_OUT_SEL.
const PERI_SEL_MASK: u32 = 0x3f;

/// OUT interrupt bits.
pub const OUT_DONE: u32 = 1 << 0;
pub const OUT_EOF: u32 = 1 << 1;
pub const OUT_DSCR_ERR: u32 = 1 << 2;
pub const OUT_TOTAL_EOF: u32 = 1 << 3;
const OUT_INT_MASK: u32 = 0xff;
const IN_INT_MASK: u32 = 0x1ff;

/// OUTFIFO_STATUS of an empty FIFO: L1, L2 and L3 empty.
const OUTFIFO_EMPTY: u32 = 0x0780_002a;
/// OUTFIFO_STATUS empty flags and byte counts (L1 bits 10:6, L2 bits 17:11,
/// L3 bits 22:18).
const OUTFIFO_EMPTY_FLAGS: u32 = (1 << 1) | (1 << 3) | (1 << 5);
const OUTFIFO_CNT_L1: (u32, u32) = (6, 0x1f);
const OUTFIFO_CNT_L2: (u32, u32) = (11, 0x7f);
const OUTFIFO_CNT_L3: (u32, u32) = (18, 0x1f);

/// Descriptor DW0 fields.
const DW0_LENGTH_SHIFT: u32 = 12;
const DW0_LENGTH_MASK: u32 = 0xfff;
const DW0_SUC_EOF: u32 = 1 << 30;
const DW0_OWNER_DMA: u32 = 1 << 31;

/// Bounds on one chain, so that a chain that loops is read once.
const MAX_DESCRIPTORS: usize = 0x1_0000;
const MAX_CHAIN_BYTES: usize = 0x8_0000;

/// Reset values of the per-channel registers that do not reset to 0 (OUT_LINK
/// and OUTFIFO_STATUS are computed).
const CHANNEL_RESET: [(u32, u32); 9] = [
    (IN_CONF1, 0x0000_000c),
    (INFIFO_STATUS, 0x0f00_003a),
    (IN_POP, 0x0000_0800),
    (IN_LINK, 0x0110_0000),
    (IN_WIGHT, 0x0000_0f00),
    (IN_PERI_SEL, 0x0000_003f),
    (OUT_CONF0, 0x0000_0008),
    (OUT_WIGHT, 0x0000_0f00),
    (OUT_PERI_SEL, 0x0000_003f),
];

/// Memory the DMA engine reads descriptors and buffers from and writes
/// descriptors back to.
pub trait DmaMemory {
    /// `len` bytes at `addr`, if the DMA engine reaches them.
    fn dma_read(&self, addr: u32, len: usize) -> Option<&[u8]>;
    /// Writes a word. Returns false if the DMA engine does not reach `addr`.
    fn dma_write_u32(&mut self, addr: u32, value: u32) -> bool;
}

fn dma_read_u32(mem: &dyn DmaMemory, addr: u32) -> Option<u32> {
    let bytes = mem.dma_read(addr, 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// What OUT_LINK asked a channel to do.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LinkRequest {
    /// Start from the descriptor OUTLINK_ADDR points to.
    Start,
    /// Go on with the descriptor that follows the last one read.
    Restart,
}

#[derive(Default)]
struct OutChannel {
    /// A link request not yet run by [`Gdma::service`].
    request: Option<LinkRequest>,
    /// The channel has read a chain whose data has not all been sent out.
    busy: bool,
    /// Data read from the chain and not yet taken by the peripheral.
    data: Vec<u8>,
    /// The descriptor of the chain with SUC_EOF set, and the descriptor read
    /// before it.
    chain_eof: Option<(u32, u32)>,
    /// OUT_EOF_DES_ADDR and OUT_EOF_BFR_DES_ADDR.
    eof_des_addr: [u32; 2],
    /// The last descriptors read: the last one, the one before, and the one
    /// before that.
    dscr: [u32; 3],
    int_raw: u32,
}

impl OutChannel {
    /// Raises OUT_EOF for the SUC_EOF descriptor of the chain, if it has one.
    fn raise_eof(&mut self) {
        if let Some((desc, before)) = self.chain_eof {
            self.int_raw |= OUT_EOF;
            self.eof_des_addr = [desc, before];
        }
    }
}

pub struct Gdma {
    regs: [u32; (SIZE / 4) as usize],
    out: [OutChannel; CHANNELS],
    in_raw: [u32; CHANNELS],
}

impl Default for Gdma {
    fn default() -> Self {
        Self::new()
    }
}

impl Gdma {
    pub fn new() -> Self {
        let mut g = Gdma {
            regs: [0; (SIZE / 4) as usize],
            out: Default::default(),
            in_raw: [0; CHANNELS],
        };
        g.reset();
        g
    }

    pub fn reset(&mut self) {
        self.regs = [0; (SIZE / 4) as usize];
        for ch in 0..CHANNELS as u32 {
            for (off, v) in CHANNEL_RESET {
                self.set(ch * CHANNEL_STRIDE + off, v);
            }
            self.set(IN_SRAM_SIZE_CH0 + ch * SRAM_SIZE_STRIDE, 0x0e);
            self.set(OUT_SRAM_SIZE_CH0 + ch * SRAM_SIZE_STRIDE, 0x0e);
        }
        self.set(PD_CONF, 0x20);
        self.out = Default::default();
        self.in_raw = [0; CHANNELS];
    }

    fn get(&self, off: u32) -> u32 {
        self.regs.get((off / 4) as usize).copied().unwrap_or(0)
    }

    fn set(&mut self, off: u32, v: u32) {
        if let Some(r) = self.regs.get_mut((off / 4) as usize) {
            *r = v;
        }
    }

    fn out_reg(&self, ch: usize, off: u32) -> u32 {
        self.get(ch as u32 * CHANNEL_STRIDE + off)
    }

    /// Interrupt source levels of the IN channels.
    pub fn in_irqs(&self) -> [bool; CHANNELS] {
        core::array::from_fn(|ch| {
            self.in_raw[ch] & self.get(ch as u32 * CHANNEL_STRIDE + IN_INT_ENA) != 0
        })
    }

    /// Interrupt source levels of the OUT channels.
    pub fn out_irqs(&self) -> [bool; CHANNELS] {
        core::array::from_fn(|ch| self.out[ch].int_raw & self.out_reg(ch, OUT_INT_ENA) != 0)
    }

    pub fn read(&self, off: u32) -> u32 {
        if off >= CHANNEL_STRIDE * CHANNELS as u32 {
            return self.get(off);
        }
        let ch = (off / CHANNEL_STRIDE) as usize;
        let reg = off % CHANNEL_STRIDE;
        let out = &self.out[ch];
        match reg {
            IN_INT_RAW => self.in_raw[ch],
            IN_INT_ST => self.in_raw[ch] & self.get(off - IN_INT_ST + IN_INT_ENA),
            IN_INT_CLR => 0,
            OUT_INT_RAW => out.int_raw,
            OUT_INT_ST => out.int_raw & self.out_reg(ch, OUT_INT_ENA),
            OUT_INT_CLR => 0,
            OUTFIFO_STATUS => outfifo_status(out.data.len()),
            OUT_LINK => {
                let park = if out.busy { 0 } else { OUTLINK_PARK };
                (self.get(off) & LINK_ADDR_MASK) | park
            }
            OUT_STATE => out.dscr[0] & OUT_STATE_DSCR_ADDR,
            OUT_EOF_DES_ADDR => out.eof_des_addr[0],
            OUT_EOF_BFR_DES_ADDR => out.eof_des_addr[1],
            OUT_DSCR => out.dscr[0],
            OUT_DSCR_BF0 => out.dscr[1],
            OUT_DSCR_BF1 => out.dscr[2],
            _ => self.get(off),
        }
    }

    /// Returns whether the interrupt state may have changed.
    pub fn write(&mut self, off: u32, v: u32) -> bool {
        if off >= CHANNEL_STRIDE * CHANNELS as u32 {
            self.set(off, v);
            return false;
        }
        let ch = (off / CHANNEL_STRIDE) as usize;
        let reg = off % CHANNEL_STRIDE;
        match reg {
            IN_INT_RAW | IN_INT_CLR => {
                self.in_raw[ch] &= !(v & IN_INT_MASK);
                true
            }
            IN_INT_ENA => {
                self.set(off, v & IN_INT_MASK);
                true
            }
            IN_INT_ST => false,
            OUT_INT_RAW | OUT_INT_CLR => {
                self.out[ch].int_raw &= !(v & OUT_INT_MASK);
                true
            }
            OUT_INT_ENA => {
                self.set(off, v & OUT_INT_MASK);
                true
            }
            OUT_CONF0 => {
                if v & OUT_RST != 0 {
                    self.stop_out(ch);
                }
                self.set(off, v);
                false
            }
            OUT_LINK => {
                self.set(off, v & LINK_ADDR_MASK);
                if v & OUTLINK_STOP != 0 {
                    self.stop_out(ch);
                } else if v & OUTLINK_START != 0 {
                    self.out[ch].request = Some(LinkRequest::Start);
                } else if v & OUTLINK_RESTART != 0 {
                    self.out[ch].request = Some(LinkRequest::Restart);
                }
                false
            }
            OUT_PUSH => {
                self.set(off, v & !OUTFIFO_PUSH);
                false
            }
            OUT_PERI_SEL => {
                self.set(off, v & PERI_SEL_MASK);
                false
            }
            OUT_INT_ST | OUTFIFO_STATUS | OUT_STATE | OUT_EOF_DES_ADDR | OUT_EOF_BFR_DES_ADDR
            | OUT_DSCR | OUT_DSCR_BF0 | OUT_DSCR_BF1 => false,
            _ => {
                self.set(off, v);
                false
            }
        }
    }

    /// The channel stops and drops the data it holds.
    fn stop_out(&mut self, ch: usize) {
        let out = &mut self.out[ch];
        out.request = None;
        out.busy = false;
        out.data.clear();
    }

    /// Whether a channel waits for [`Gdma::service`].
    pub fn needs_service(&self) -> bool {
        self.out.iter().any(|out| out.request.is_some())
    }

    /// Runs the pending OUTLINK_START and OUTLINK_RESTART requests: reads the
    /// descriptor chains from `mem`. Returns whether the interrupt state may
    /// have changed.
    pub fn service(&mut self, mem: &mut dyn DmaMemory) -> bool {
        let mut changed = false;
        for ch in 0..CHANNELS {
            let Some(request) = self.out[ch].request.take() else {
                continue;
            };
            let first = match request {
                LinkRequest::Start => {
                    LINK_ADDR_BASE | (self.out_reg(ch, OUT_LINK) & LINK_ADDR_MASK)
                }
                LinkRequest::Restart => {
                    let last = self.out[ch].dscr[0];
                    match dma_read_u32(mem, last.wrapping_add(8)) {
                        Some(next) => next,
                        None => {
                            self.out[ch].int_raw |= OUT_DSCR_ERR;
                            changed = true;
                            continue;
                        }
                    }
                }
            };
            if request == LinkRequest::Restart && first == 0 {
                // No descriptor was added after the last one.
                continue;
            }
            self.load_chain(ch, first, mem);
            changed = true;
        }
        changed
    }

    /// Reads the chain that starts at `first` into channel `ch`.
    fn load_chain(&mut self, ch: usize, first: u32, mem: &mut dyn DmaMemory) {
        let conf0 = self.out_reg(ch, OUT_CONF0);
        let check_owner = self.out_reg(ch, OUT_CONF1) & OUT_CHECK_OWNER != 0;
        let write_back = conf0 & OUT_AUTO_WRBACK != 0;
        let out = &mut self.out[ch];
        out.data.clear();
        out.chain_eof = None;

        let mut addr = first;
        let mut error = false;
        for _ in 0..MAX_DESCRIPTORS {
            let (Some(dw0), Some(buffer), Some(next)) = (
                dma_read_u32(mem, addr),
                dma_read_u32(mem, addr.wrapping_add(4)),
                dma_read_u32(mem, addr.wrapping_add(8)),
            ) else {
                error = true;
                break;
            };
            if check_owner && dw0 & DW0_OWNER_DMA == 0 {
                error = true;
                break;
            }
            let length = ((dw0 >> DW0_LENGTH_SHIFT) & DW0_LENGTH_MASK) as usize;
            if length > 0 {
                let Some(bytes) = mem.dma_read(buffer, length) else {
                    error = true;
                    break;
                };
                out.data.extend_from_slice(bytes);
            }
            out.dscr = [addr, out.dscr[0], out.dscr[1]];
            if dw0 & DW0_SUC_EOF != 0 {
                out.chain_eof = Some((addr, out.dscr[1]));
            }
            if write_back {
                mem.dma_write_u32(addr, dw0 & !DW0_OWNER_DMA);
            }
            if next == 0 || next == first || out.data.len() >= MAX_CHAIN_BYTES {
                break;
            }
            addr = next;
        }

        if error {
            out.int_raw |= OUT_DSCR_ERR;
        }
        if conf0 & OUT_EOF_MODE == 0 {
            // EOF when the data of the SUC_EOF descriptor has been read from memory.
            out.raise_eof();
        }
        // The data read before a descriptor error still goes out.
        out.busy = !(error && out.data.is_empty());
        if out.busy && out.data.is_empty() {
            self.finish_out(ch);
        }
    }

    /// The OUT channel that serves `peripheral` (an OUT_PERI_SEL value) and has
    /// read a chain, if any.
    pub fn out_channel_for(&self, peripheral: u32) -> Option<usize> {
        (0..CHANNELS).find(|&ch| {
            self.out[ch].busy && self.out_reg(ch, OUT_PERI_SEL) & PERI_SEL_MASK == peripheral
        })
    }

    /// The peripheral takes up to `max` bytes (all of them for None) of the data
    /// channel `ch` holds.
    pub fn take_out_data(&mut self, ch: usize, max: Option<usize>) -> Vec<u8> {
        let data = &mut self.out[ch].data;
        let n = max.map_or(data.len(), |m| m.min(data.len()));
        data.drain(..n).collect()
    }

    /// The peripheral has sent out the data it took from channel `ch`. Once the
    /// channel holds no more data, the transfer ends: OUT_DONE, and OUT_EOF and
    /// OUT_TOTAL_EOF for a chain with SUC_EOF set. Returns whether the interrupt
    /// state may have changed.
    pub fn finish_out(&mut self, ch: usize) -> bool {
        let eof_mode = self.out_reg(ch, OUT_CONF0) & OUT_EOF_MODE != 0;
        let out = &mut self.out[ch];
        if !out.busy || !out.data.is_empty() {
            return false;
        }
        out.busy = false;
        out.int_raw |= OUT_DONE;
        if out.chain_eof.is_some() {
            out.int_raw |= OUT_TOTAL_EOF;
            if eof_mode {
                // EOF when the data of the SUC_EOF descriptor has left the FIFO.
                out.raise_eof();
            }
        }
        true
    }
}

/// OUTFIFO_STATUS of a channel that holds `len` bytes.
fn outfifo_status(len: usize) -> u32 {
    if len == 0 {
        return OUTFIFO_EMPTY;
    }
    let count = |(shift, max): (u32, u32)| (len.min(max as usize) as u32) << shift;
    (OUTFIFO_EMPTY & !OUTFIFO_EMPTY_FLAGS)
        | count(OUTFIFO_CNT_L1)
        | count(OUTFIFO_CNT_L2)
        | count(OUTFIFO_CNT_L3)
}
