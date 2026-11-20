//! UART controllers (UART0 0x6000_0000, UART1 0x6001_0000, UART2 0x6002_e000).
//!
//! Bytes written to the TX FIFO leave at the programmed baud rate into
//! [`Uart::tx_out`]; bytes the board sends ([`Uart::receive`]) enter the RX FIFO
//! at the same rate. RXFIFO_FULL, RXFIFO_TOUT, TXFIFO_EMPTY and TX_DONE follow
//! their thresholds.

use std::collections::VecDeque;

use super::TICK_HZ;

const FIFO: u32 = 0x00;
const INT_RAW: u32 = 0x04;
const INT_ST: u32 = 0x08;
const INT_ENA: u32 = 0x0c;
const INT_CLR: u32 = 0x10;
const CLKDIV: u32 = 0x14;
const STATUS: u32 = 0x1c;
const CONF0: u32 = 0x20;
const CONF1: u32 = 0x24;
const MEM_CONF: u32 = 0x60;
const FSM_STATUS: u32 = 0x6c;
const CLK_CONF: u32 = 0x78;
const DATE: u32 = 0x7c;
const ID: u32 = 0x80;

const INT_RXFIFO_FULL: u32 = 1 << 0;
const INT_TXFIFO_EMPTY: u32 = 1 << 1;
const INT_RXFIFO_OVF: u32 = 1 << 4;
const INT_RXFIFO_TOUT: u32 = 1 << 8;
const INT_TX_DONE: u32 = 1 << 14;

const CONF0_RXFIFO_RST: u32 = 1 << 17;
const CONF0_TXFIFO_RST: u32 = 1 << 18;
const CONF1_RX_TOUT_EN: u32 = 1 << 23;

const FIFO_SIZE: usize = 128;

pub struct Uart {
    regs: [u32; 0x24],
    tx: VecDeque<u8>,
    rx: VecDeque<u8>,
    /// Bytes sent by the board, not yet in the RX FIFO.
    rx_line: VecDeque<u8>,
    /// Bytes that left the TX line, for the board.
    pub tx_out: VecDeque<u8>,
    /// Time the TX shifter finishes its byte.
    tx_busy_until: Option<u64>,
    /// Time the next board byte enters the RX FIFO.
    rx_next: Option<u64>,
    /// Time of the RX timeout, after the last byte received.
    rx_tout_at: Option<u64>,
    /// APB clock in Hz (UART clock source 1).
    pub apb_hz: u64,
}

impl Default for Uart {
    fn default() -> Self {
        Self::new()
    }
}

impl Uart {
    pub fn new() -> Self {
        let mut u = Uart {
            regs: [0; 0x24],
            tx: VecDeque::new(),
            rx: VecDeque::new(),
            rx_line: VecDeque::new(),
            tx_out: VecDeque::new(),
            tx_busy_until: None,
            rx_next: None,
            rx_tout_at: None,
            apb_hz: 80_000_000,
        };
        u.reset();
        u
    }

    pub fn reset(&mut self) {
        self.regs = [0; 0x24];
        self.set(CLKDIV, 0x2b6);
        self.set(CONF0, 0x1000_001c);
        self.set(CONF1, 0x0000_a060);
        self.set(MEM_CONF, 0x0014_0012);
        self.set(CLK_CONF, 0x0320_0000);
        self.set(DATE, 0x2007_2300);
        self.set(ID, 0x4000_0500);
        self.set(INT_RAW, INT_TXFIFO_EMPTY);
        self.tx.clear();
        self.rx.clear();
        self.tx_busy_until = None;
        self.rx_next = None;
        self.rx_tout_at = None;
    }

    fn get(&self, off: u32) -> u32 {
        self.regs[(off / 4) as usize]
    }

    fn set(&mut self, off: u32, v: u32) {
        self.regs[(off / 4) as usize] = v;
    }

    fn raise(&mut self, bits: u32) {
        let raw = self.get(INT_RAW) | bits;
        self.set(INT_RAW, raw);
    }

    /// Baud rate from the clock configuration and divider.
    pub fn baud(&self) -> u64 {
        let clk_conf = self.get(CLK_CONF);
        let sclk = match (clk_conf >> 20) & 3 {
            1 => self.apb_hz,
            2 => 17_500_000,
            _ => 40_000_000,
        } / (u64::from(clk_conf & 0xff) + 1);
        let div = self.get(CLKDIV);
        let div16 = u64::from(div & 0xfff) * 16 + u64::from((div >> 20) & 0xf);
        (sclk * 16).checked_div(div16).unwrap_or(115_200)
    }

    /// Ticks per frame (start, 8 data, stop bits).
    fn frame_ticks(&self) -> u64 {
        let conf0 = self.get(CONF0);
        let data = 5 + u64::from((conf0 >> 2) & 3);
        let parity = u64::from((conf0 >> 1) & 1);
        let stop = match (conf0 >> 4) & 3 {
            3 => 2,
            _ => 1,
        };
        (1 + data + parity + stop) * TICK_HZ / self.baud().max(1)
    }

    fn bit_ticks(&self) -> u64 {
        TICK_HZ / self.baud().max(1)
    }

    /// The board sends bytes to the chip.
    pub fn receive(&mut self, bytes: &[u8], now: u64) {
        self.rx_line.extend(bytes);
        if self.rx_next.is_none() {
            self.rx_next = Some(now + self.frame_ticks());
        }
    }

    pub fn irq(&self) -> bool {
        self.get(INT_RAW) & self.get(INT_ENA) != 0
    }

    fn thresholds(&mut self) {
        let conf1 = self.get(CONF1);
        let full = (conf1 & 0x3ff) as usize;
        let empty = ((conf1 >> 10) & 0x3ff) as usize;
        if self.rx.len() >= full.max(1) {
            self.raise(INT_RXFIFO_FULL);
        }
        if self.tx.len() <= empty {
            self.raise(INT_TXFIFO_EMPTY);
        }
    }

    pub fn next_event(&self) -> Option<u64> {
        [self.tx_busy_until, self.rx_next, self.rx_tout_at]
            .into_iter()
            .flatten()
            .min()
    }

    /// Advances to `now`. Returns whether the interrupt state may have changed.
    pub fn update(&mut self, now: u64) -> bool {
        let mut changed = false;
        while let Some(t) = self.tx_busy_until
            && now >= t
        {
            if let Some(b) = self.tx.pop_front() {
                self.tx_out.push_back(b);
            }
            changed = true;
            if self.tx.is_empty() {
                self.tx_busy_until = None;
                self.raise(INT_TX_DONE);
            } else {
                self.tx_busy_until = Some(t + self.frame_ticks());
            }
            self.thresholds();
        }
        while let Some(t) = self.rx_next
            && now >= t
        {
            if let Some(b) = self.rx_line.pop_front() {
                if self.rx.len() < FIFO_SIZE {
                    self.rx.push_back(b);
                } else {
                    self.raise(INT_RXFIFO_OVF);
                }
            }
            changed = true;
            self.rx_next = if self.rx_line.is_empty() {
                None
            } else {
                Some(t + self.frame_ticks())
            };
            let thrhd = u64::from((self.get(MEM_CONF) >> 17) & 0x3ff);
            self.rx_tout_at = if self.get(CONF1) & CONF1_RX_TOUT_EN != 0 {
                Some(t + thrhd.max(1) * self.bit_ticks())
            } else {
                None
            };
            self.thresholds();
        }
        if let Some(t) = self.rx_tout_at
            && now >= t
        {
            self.rx_tout_at = None;
            if !self.rx.is_empty() {
                self.raise(INT_RXFIFO_TOUT);
            }
            changed = true;
        }
        changed
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            FIFO => {
                let b = self.rx.pop_front().map_or(0, u32::from);
                self.thresholds();
                b
            }
            STATUS => {
                let tx_cnt = self.tx.len() as u32
                    + u32::from(self.tx_busy_until.is_some() && self.tx.is_empty());
                (self.rx.len() as u32 & 0x3ff)
                    | ((tx_cnt.min(0x3ff)) << 16)
                    | (0x3 << 14)
                    | (0x3 << 29)
            }
            INT_ST => self.get(INT_RAW) & self.get(INT_ENA),
            FSM_STATUS => {
                if self.tx_busy_until.is_some() {
                    2 << 4
                } else {
                    0
                }
            }
            o if (o as usize) < self.regs.len() * 4 => self.get(o),
            _ => 0,
        }
    }

    /// Returns whether the interrupt state may have changed.
    pub fn write(&mut self, off: u32, v: u32, now: u64) -> bool {
        match off {
            FIFO => {
                if self.tx.len() < FIFO_SIZE {
                    self.tx.push_back(v as u8);
                }
                if self.tx_busy_until.is_none() {
                    self.tx_busy_until = Some(now + self.frame_ticks());
                }
                let raw = self.get(INT_RAW) & !(INT_TXFIFO_EMPTY | INT_TX_DONE);
                self.set(INT_RAW, raw);
                self.thresholds();
                true
            }
            INT_CLR => {
                let raw = self.get(INT_RAW) & !v;
                self.set(INT_RAW, raw);
                // Level conditions come back at once.
                self.thresholds();
                true
            }
            INT_ENA => {
                self.set(INT_ENA, v);
                true
            }
            CONF0 => {
                if v & CONF0_RXFIFO_RST != 0 {
                    self.rx.clear();
                }
                if v & CONF0_TXFIFO_RST != 0 {
                    self.tx.clear();
                    self.tx_busy_until = None;
                }
                self.set(CONF0, v);
                self.thresholds();
                true
            }
            CONF1 => {
                self.set(CONF1, v);
                self.thresholds();
                true
            }
            INT_RAW | INT_ST | STATUS | FSM_STATUS | DATE | ID => false,
            o if (o as usize) < self.regs.len() * 4 => {
                self.set(o, v);
                false
            }
            _ => false,
        }
    }
}
