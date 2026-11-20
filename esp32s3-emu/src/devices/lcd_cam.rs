//! LCD_CAM (0x6004_1000), LCD side in I8080 mode.
//!
//! Writing 1 to LCD_START starts a transfer: the command phase (one command
//! word, or two with LCD_CMD_2_CYCLE_EN) if LCD_CMD is set, the dummy cycles if
//! LCD_DUMMY is set, then the data phase if LCD_DOUT is set, with the data of
//! the GDMA OUT channel that serves LCD_CAM (all of it with LCD_ALWAYS_OUT_EN,
//! else LCD_DOUT_CYCLELEN + 1 cycles). Each command word, dummy cycle and data
//! cycle (one byte, two with LCD_2BYTE_EN) takes one LCD_PCLK cycle. When the
//! last one has been sent, LCD_START clears, LCD_TRANS_DONE rises and the
//! transfer goes to [`LcdCam::out`]. Clearing LCD_START or setting LCD_RESET
//! during a transfer stops it after the cycles already sent.
//!
//! The camera side keeps its registers and never receives.

use std::collections::VecDeque;

use super::TICK_HZ;

/// One transfer on the bus.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LcdWrite {
    /// Command words, in bus order (empty without a command phase). On an 8-bit
    /// bus only their low byte is sent.
    pub cmd: Vec<u16>,
    /// Data bytes in bus order; on a 16-bit bus each cycle carries two bytes,
    /// the one on `LCD_DATA[7:0]` first.
    pub data: Vec<u8>,
}

/// A transfer that ended.
pub struct TransferEnd {
    /// The GDMA OUT channel that supplied its data, if any.
    pub dma_channel: Option<usize>,
}

const LCD_CLOCK: u32 = 0x00;
const CAM_CTRL: u32 = 0x08;
const CAM_CTRL1: u32 = 0x0c;
const LCD_USER: u32 = 0x14;
const LCD_MISC: u32 = 0x18;
const LCD_CMD_VAL: u32 = 0x28;
const LC_DMA_INT_ENA: u32 = 0x64;
const LC_DMA_INT_RAW: u32 = 0x68;
const LC_DMA_INT_ST: u32 = 0x6c;
const LC_DMA_INT_CLR: u32 = 0x70;

/// LCD_CLOCK fields.
const LCD_CLKCNT_N: u32 = 0x3f;
const LCD_CLK_EQU_SYSCLK: u32 = 1 << 6;
const LCD_CLKM_DIV_NUM_SHIFT: u32 = 9;
const LCD_CLKM_DIV_B_SHIFT: u32 = 17;
const LCD_CLKM_DIV_A_SHIFT: u32 = 23;
const LCD_CLK_SEL_SHIFT: u32 = 29;

/// LCD_USER fields.
const LCD_DOUT_CYCLELEN: u32 = 0x1fff;
const LCD_ALWAYS_OUT_EN: u32 = 1 << 13;
const LCD_8BITS_ORDER: u32 = 1 << 19;
const LCD_UPDATE: u32 = 1 << 20;
const LCD_BIT_ORDER: u32 = 1 << 21;
const LCD_BYTE_ORDER: u32 = 1 << 22;
const LCD_2BYTE_EN: u32 = 1 << 23;
const LCD_DOUT: u32 = 1 << 24;
const LCD_DUMMY: u32 = 1 << 25;
const LCD_CMD: u32 = 1 << 26;
const LCD_START: u32 = 1 << 27;
const LCD_RESET: u32 = 1 << 28;
const LCD_DUMMY_CYCLELEN_SHIFT: u32 = 29;
const LCD_CMD_2_CYCLE_EN: u32 = 1 << 31;

/// LCD_MISC.LCD_AFIFO_RESET.
const LCD_AFIFO_RESET: u32 = 1 << 27;
/// CAM_CTRL.CAM_UPDATE.
const CAM_UPDATE: u32 = 1 << 4;
/// CAM_CTRL1.CAM_RESET and CAM_AFIFO_RESET.
const CAM_RESETS: u32 = (1 << 30) | (1 << 31);

/// LC_DMA_INT bits.
pub const LCD_TRANS_DONE: u32 = 1 << 1;
const INT_MASK: u32 = 0xf;

/// Source clocks: XTAL_CLK, and PLL_F160M_CLK (PLL_D2_CLK is half the PLL).
const XTAL_HZ: u64 = 40_000_000;
const PLL_F160M_HZ: u64 = 160_000_000;

struct Transfer {
    start: u64,
    /// End time, None while LCD_CLK has no source.
    end: Option<u64>,
    cmd: Vec<u16>,
    dummy_cycles: u64,
    data: Vec<u8>,
    bytes_per_cycle: usize,
    dma_channel: Option<usize>,
}

impl Transfer {
    fn cycles(&self) -> u64 {
        self.cmd.len() as u64 + self.dummy_cycles + self.data_cycles(self.data.len())
    }

    fn data_cycles(&self, bytes: usize) -> u64 {
        bytes.div_ceil(self.bytes_per_cycle) as u64
    }

    /// What was on the bus after `cycles` cycles.
    fn sent(&self, cycles: u64) -> LcdWrite {
        let cmd_words = (cycles as usize).min(self.cmd.len());
        let data_cycles = cycles.saturating_sub(self.cmd.len() as u64 + self.dummy_cycles);
        let data_bytes = (data_cycles as usize)
            .saturating_mul(self.bytes_per_cycle)
            .min(self.data.len());
        LcdWrite {
            cmd: self.cmd[..cmd_words].to_vec(),
            data: self.data[..data_bytes].to_vec(),
        }
    }
}

pub struct LcdCam {
    regs: [u32; 0x40],
    int_raw: u32,
    transfer: Option<Transfer>,
    /// LCD_START was set with no transfer running: [`LcdCam::begin`] is due.
    start_request: bool,
    /// Transfers that ended, oldest first, for the board.
    pub out: VecDeque<LcdWrite>,
}

impl Default for LcdCam {
    fn default() -> Self {
        Self::new()
    }
}

impl LcdCam {
    pub fn new() -> Self {
        let mut l = LcdCam {
            regs: [0; 0x40],
            int_raw: 0,
            transfer: None,
            start_request: false,
            out: VecDeque::new(),
        };
        l.reset();
        l
    }

    pub fn reset(&mut self) {
        self.regs = [0; 0x40];
        self.set(LCD_MISC, 0x22);
        self.int_raw = 0;
        self.transfer = None;
        self.start_request = false;
    }

    fn get(&self, off: u32) -> u32 {
        self.regs.get((off / 4) as usize).copied().unwrap_or(0)
    }

    fn set(&mut self, off: u32, v: u32) {
        if let Some(r) = self.regs.get_mut((off / 4) as usize) {
            *r = v;
        }
    }

    pub fn irq(&self) -> bool {
        self.int_raw & self.get(LC_DMA_INT_ENA) != 0
    }

    /// Whether a transfer is running.
    pub fn busy(&self) -> bool {
        self.transfer.is_some()
    }

    pub fn next_event(&self) -> Option<u64> {
        self.transfer.as_ref().and_then(|t| t.end)
    }

    pub fn read(&self, off: u32) -> u32 {
        match off {
            LC_DMA_INT_RAW => self.int_raw,
            LC_DMA_INT_ST => self.int_raw & self.get(LC_DMA_INT_ENA),
            LC_DMA_INT_CLR => 0,
            _ => self.get(off),
        }
    }

    /// Returns whether the interrupt state may have changed.
    pub fn write(&mut self, off: u32, v: u32, now: u64) -> bool {
        match off {
            LCD_USER => {
                if v & LCD_RESET != 0 {
                    self.stop(now);
                }
                if v & LCD_START == 0 {
                    self.stop(now);
                } else if self.transfer.is_none() {
                    self.start_request = true;
                }
                self.set(LCD_USER, v & !(LCD_UPDATE | LCD_RESET));
                false
            }
            LCD_MISC => {
                self.set(LCD_MISC, v & !LCD_AFIFO_RESET);
                false
            }
            CAM_CTRL => {
                self.set(CAM_CTRL, v & !CAM_UPDATE);
                false
            }
            CAM_CTRL1 => {
                self.set(CAM_CTRL1, v & !CAM_RESETS);
                false
            }
            LC_DMA_INT_ENA => {
                self.set(LC_DMA_INT_ENA, v & INT_MASK);
                true
            }
            LC_DMA_INT_CLR => {
                self.int_raw &= !(v & INT_MASK);
                true
            }
            LC_DMA_INT_RAW | LC_DMA_INT_ST => false,
            _ => {
                self.set(off, v);
                false
            }
        }
    }

    /// Stops the running transfer, if any; the cycles already sent go to
    /// [`LcdCam::out`].
    fn stop(&mut self, now: u64) {
        self.start_request = false;
        let Some(t) = self.transfer.take() else {
            return;
        };
        let cycles = match t.end {
            Some(end) if end > t.start => {
                let elapsed = u128::from(now.saturating_sub(t.start));
                let total = u128::from(t.cycles());
                (elapsed * total / u128::from(end - t.start)) as u64
            }
            _ => 0,
        };
        let sent = t.sent(cycles);
        if !sent.cmd.is_empty() || !sent.data.is_empty() {
            self.out.push_back(sent);
        }
    }

    /// Whether LCD_START asked for a transfer that [`LcdCam::begin`] has not
    /// started yet. Taking it clears it.
    pub fn take_start_request(&mut self) -> bool {
        std::mem::take(&mut self.start_request)
    }

    /// The data the transfer takes from its GDMA channel: None without a data
    /// phase, else the most bytes it takes (None: all of them).
    pub fn data_request(&self) -> Option<Option<usize>> {
        let user = self.get(LCD_USER);
        if user & LCD_DOUT == 0 {
            return None;
        }
        if user & LCD_ALWAYS_OUT_EN != 0 {
            return Some(None);
        }
        let cycles = (user & LCD_DOUT_CYCLELEN) as usize + 1;
        Some(Some(cycles * bytes_per_cycle(user)))
    }

    /// Starts the transfer LCD_START asked for, with `data` from GDMA OUT
    /// channel `dma_channel`. `pll_hz` is the PLL frequency (PLL_D2_CLK is half
    /// of it).
    pub fn begin(&mut self, now: u64, mut data: Vec<u8>, dma_channel: Option<usize>, pll_hz: u64) {
        let user = self.get(LCD_USER);
        let two_byte = user & LCD_2BYTE_EN != 0;
        let cmd = if user & LCD_CMD != 0 {
            let value = self.get(LCD_CMD_VAL);
            let width_mask = if two_byte { 0xffff } else { 0xff };
            let mut words = vec![(value & width_mask) as u16];
            if user & LCD_CMD_2_CYCLE_EN != 0 {
                words.push(((value >> 16) & width_mask) as u16);
            }
            words
        } else {
            Vec::new()
        };
        let dummy_cycles = if user & LCD_DUMMY != 0 {
            u64::from((user >> LCD_DUMMY_CYCLELEN_SHIFT) & 3) + 1
        } else {
            0
        };
        bus_order(&mut data, user);
        let mut t = Transfer {
            start: now,
            end: None,
            cmd,
            dummy_cycles,
            data,
            bytes_per_cycle: bytes_per_cycle(user),
            dma_channel,
        };
        t.end = self.bus_ticks(t.cycles(), pll_hz).map(|ticks| now + ticks);
        self.transfer = Some(t);
    }

    /// Duration of `cycles` LCD_PCLK cycles in ticks, None when LCD_CLK has no
    /// source. f_LCD_CLK = f_source / (DIV_NUM + DIV_B / DIV_A) (DIV_NUM 0 means
    /// 256), and f_LCD_PCLK = f_LCD_CLK / (LCD_CLKCNT_N + 1) unless
    /// LCD_CLK_EQU_SYSCLK is set.
    fn bus_ticks(&self, cycles: u64, pll_hz: u64) -> Option<u64> {
        let clock = self.get(LCD_CLOCK);
        let source_hz = match (clock >> LCD_CLK_SEL_SHIFT) & 3 {
            1 => XTAL_HZ,
            2 => pll_hz / 2,
            3 => PLL_F160M_HZ,
            _ => return None,
        };
        let div_num = match (clock >> LCD_CLKM_DIV_NUM_SHIFT) & 0xff {
            0 => 256,
            n => u64::from(n),
        };
        let div_b = u64::from((clock >> LCD_CLKM_DIV_B_SHIFT) & 0x3f);
        let div_a = u64::from((clock >> LCD_CLKM_DIV_A_SHIFT) & 0x3f);
        // The divider as the fraction numerator / denominator.
        let (numerator, denominator) = if div_a == 0 || div_b == 0 {
            (div_num, 1)
        } else {
            (div_num * div_a + div_b, div_a)
        };
        let pclk_div = if clock & LCD_CLK_EQU_SYSCLK != 0 {
            1
        } else {
            u64::from(clock & LCD_CLKCNT_N) + 1
        };
        let ticks =
            u128::from(cycles) * u128::from(TICK_HZ) * u128::from(numerator) * u128::from(pclk_div);
        let per_second = u128::from(source_hz) * u128::from(denominator);
        Some(ticks.div_ceil(per_second) as u64)
    }

    /// Advances to `now`: ends the transfer whose last cycle has been sent.
    pub fn update(&mut self, now: u64) -> Option<TransferEnd> {
        let end = self.transfer.as_ref()?.end?;
        if now < end {
            return None;
        }
        let t = self.transfer.take()?;
        let user = self.get(LCD_USER) & !LCD_START;
        self.set(LCD_USER, user);
        self.int_raw |= LCD_TRANS_DONE;
        self.out.push_back(LcdWrite {
            cmd: t.cmd,
            data: t.data,
        });
        Some(TransferEnd {
            dma_channel: t.dma_channel,
        })
    }
}

/// Data bytes per LCD_PCLK cycle.
fn bytes_per_cycle(user: u32) -> usize {
    if user & LCD_2BYTE_EN != 0 { 2 } else { 1 }
}

/// Puts `data`, in memory order, in bus order. LCD_8BITS_ORDER swaps every two
/// bytes on an 8-bit bus, LCD_BYTE_ORDER swaps the two bytes of each cycle on a
/// 16-bit bus, and LCD_BIT_ORDER reverses the bits of each cycle (8 or 16).
fn bus_order(data: &mut [u8], user: u32) {
    let two_byte = user & LCD_2BYTE_EN != 0;
    let swap_pairs = if two_byte {
        user & LCD_BYTE_ORDER != 0
    } else {
        user & LCD_8BITS_ORDER != 0
    };
    if swap_pairs {
        data.as_chunks_mut::<2>()
            .0
            .iter_mut()
            .for_each(|pair| pair.swap(0, 1));
    }
    if user & LCD_BIT_ORDER != 0 {
        data.iter_mut().for_each(|b| *b = b.reverse_bits());
        if two_byte {
            // Reversing 16 bits also exchanges the two bytes.
            data.as_chunks_mut::<2>()
                .0
                .iter_mut()
                .for_each(|pair| pair.swap(0, 1));
        }
    }
}
