//! I2C controller (I2C0 at 0x6001_3000, I2C1 at 0x6002_7000), master mode.
//!
//! The firmware fills the TX FIFO and the command list (COMD0..7) and sets
//! CTR.TRANS_START; the controller runs the commands against the devices on its
//! bus and raises END_DETECT, TRANS_COMPLETE or NACK when the bus time of the
//! bytes has elapsed.

use std::collections::{BTreeMap, VecDeque};

use super::TICK_HZ;

/// A device on the I2C bus, seen from the controller.
pub trait I2cDevice: Send {
    /// A START (or repeated START) addressed this device. Returns the ACK.
    fn start(&mut self, read: bool) -> bool {
        let _ = read;
        true
    }
    /// A byte written by the master. Returns the ACK.
    fn write(&mut self, byte: u8) -> bool;
    /// A byte read by the master; `ack` is the master's acknowledge of it.
    fn read(&mut self, ack: bool) -> u8;
    /// STOP condition.
    fn stop(&mut self) {}
}

const CTR: u32 = 0x04;
const SR: u32 = 0x08;
const FIFO_ST: u32 = 0x14;
const FIFO_CONF: u32 = 0x18;
const DATA: u32 = 0x1c;
const INT_RAW: u32 = 0x20;
const INT_CLR: u32 = 0x24;
const INT_ENA: u32 = 0x28;
const INT_STATUS: u32 = 0x2c;
const SCL_LOW_PERIOD: u32 = 0x00;
const SCL_HIGH_PERIOD: u32 = 0x38;
const CLK_CONF: u32 = 0x54;
const COMD0: u32 = 0x58;
const DATE: u32 = 0xf8;
const TXFIFO_START: u32 = 0x100;
const RXFIFO_START: u32 = 0x180;

const CTR_TRANS_START: u32 = 1 << 5;
const CTR_FSM_RST: u32 = 1 << 10;
const CTR_CONF_UPGATE: u32 = 1 << 11;
const FIFO_CONF_RX_RST: u32 = 1 << 12;
const FIFO_CONF_TX_RST: u32 = 1 << 13;

const INT_RXFIFO_WM: u32 = 1 << 0;
const INT_TXFIFO_WM: u32 = 1 << 1;
const INT_END_DETECT: u32 = 1 << 3;
const INT_BYTE_TRANS_DONE: u32 = 1 << 4;
const INT_TRANS_COMPLETE: u32 = 1 << 7;
const INT_TRANS_START: u32 = 1 << 9;
const INT_NACK: u32 = 1 << 10;

const OP_WRITE: u32 = 1;
const OP_STOP: u32 = 2;
const OP_READ: u32 = 3;
const OP_END: u32 = 4;
const OP_RSTART: u32 = 6;

const CMD_DONE: u32 = 1 << 31;

pub struct I2c {
    regs: [u32; 0x40],
    tx: VecDeque<u8>,
    rx: VecDeque<u8>,
    pub devices: BTreeMap<u8, Box<dyn I2cDevice>>,
    /// The addressed device and direction, between START and STOP.
    selected: Option<(u8, bool)>,
    /// The next WRITE byte is an address byte.
    expect_address: bool,
    /// Interrupt bits raised when the bus time of the running transaction ends.
    pending: Option<(u64, u32)>,
    busy: bool,
    /// SR.RESP_REC: set after a transfer that ended without NACK.
    resp_rec: bool,
    /// Every byte the master put on the bus, with the address (for traces).
    pub log: Option<Vec<(u8, bool, u8)>>,
}

impl Default for I2c {
    fn default() -> Self {
        Self::new()
    }
}

impl I2c {
    pub fn new() -> Self {
        let mut i2c = I2c {
            regs: [0; 0x40],
            tx: VecDeque::new(),
            rx: VecDeque::new(),
            devices: BTreeMap::new(),
            selected: None,
            expect_address: false,
            pending: None,
            busy: false,
            resp_rec: false,
            log: None,
        };
        i2c.reset();
        i2c
    }

    pub fn reset(&mut self) {
        self.regs = [0; 0x40];
        self.regs[(FIFO_CONF / 4) as usize] = 0x0000_408b;
        self.regs[(SCL_LOW_PERIOD / 4) as usize] = 0;
        self.regs[(DATE / 4) as usize] = 0x2007_0201;
        self.tx.clear();
        self.rx.clear();
        self.selected = None;
        self.pending = None;
        self.busy = false;
    }

    pub fn attach(&mut self, address: u8, device: Box<dyn I2cDevice>) {
        self.devices.insert(address, device);
    }

    fn reg(&self, off: u32) -> u32 {
        self.regs[(off / 4) as usize]
    }

    fn set_reg(&mut self, off: u32, v: u32) {
        self.regs[(off / 4) as usize] = v;
    }

    pub fn irq(&self) -> bool {
        self.reg(INT_RAW) & self.reg(INT_ENA) != 0
    }

    pub fn next_event(&self) -> Option<u64> {
        self.pending.map(|(t, _)| t)
    }

    pub fn update(&mut self, now: u64) -> bool {
        if let Some((t, bits)) = self.pending
            && now >= t
        {
            self.pending = None;
            self.busy = false;
            let raw = self.reg(INT_RAW) | bits;
            self.set_reg(INT_RAW, raw);
            return true;
        }
        false
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            SR => {
                ((self.rx.len() as u32 & 0x3f) << 8)
                    | ((self.tx.len() as u32 & 0x3f) << 18)
                    | (u32::from(self.busy) << 4)
                    | u32::from(self.resp_rec)
            }
            FIFO_ST => 0,
            DATA => self.rx.pop_front().map_or(0, u32::from),
            INT_STATUS => self.reg(INT_RAW) & self.reg(INT_ENA),
            o if (RXFIFO_START..RXFIFO_START + 0x80).contains(&o) => {
                self.rx.pop_front().map_or(0, u32::from)
            }
            o if o < 0x100 => self.reg(o),
            _ => 0,
        }
    }

    /// Returns whether the interrupt state may have changed.
    pub fn write(&mut self, off: u32, v: u32, now: u64) -> bool {
        match off {
            CTR => {
                self.set_reg(CTR, v & !(CTR_TRANS_START | CTR_CONF_UPGATE | CTR_FSM_RST));
                if v & CTR_FSM_RST != 0 {
                    self.selected = None;
                    self.busy = false;
                    self.pending = None;
                }
                if v & CTR_TRANS_START != 0 {
                    self.run(now);
                    return true;
                }
                false
            }
            FIFO_CONF => {
                if v & FIFO_CONF_RX_RST != 0 {
                    self.rx.clear();
                }
                if v & FIFO_CONF_TX_RST != 0 {
                    self.tx.clear();
                }
                self.set_reg(FIFO_CONF, v);
                false
            }
            DATA => {
                if self.tx.len() < 32 {
                    self.tx.push_back(v as u8);
                }
                false
            }
            o if (TXFIFO_START..TXFIFO_START + 0x80).contains(&o) => {
                if self.tx.len() < 32 {
                    self.tx.push_back(v as u8);
                }
                false
            }
            INT_CLR => {
                let raw = self.reg(INT_RAW) & !v;
                self.set_reg(INT_RAW, raw);
                true
            }
            INT_ENA => {
                self.set_reg(INT_ENA, v);
                true
            }
            INT_RAW | INT_STATUS | SR | FIFO_ST => false,
            o if o < 0x100 => {
                self.set_reg(o, v);
                false
            }
            _ => false,
        }
    }

    /// Duration of one bit on the bus, in ticks.
    fn bit_ticks(&self) -> u64 {
        // SCL periods count source clock cycles (XTAL 40 MHz / (div + 1)).
        let div = u64::from(self.reg(CLK_CONF) & 0xff) + 1;
        let low = u64::from(self.reg(SCL_LOW_PERIOD) & 0x1ff) + 1;
        let high = u64::from(self.reg(SCL_HIGH_PERIOD) & 0x1ff) + 1;
        let cycles = (low + high).max(20);
        (cycles * div * TICK_HZ / 40_000_000).max(TICK_HZ / 1_000_000)
    }

    /// Runs the command list until END or STOP.
    fn run(&mut self, now: u64) {
        let trace = std::env::var_os("ESP32S3_I2C_TRACE").is_some();
        if trace {
            let cmds: Vec<String> = (0..8)
                .map(|i| format!("{:04x}", self.reg(COMD0 + 4 * i) & 0x3fff))
                .collect();
            eprintln!(
                "i2c: start, tx {:02x?}, commands {}",
                self.tx,
                cmds.join(" ")
            );
        }
        let mut bits: u64 = 0;
        let mut raise = INT_TRANS_START;
        for i in 0..8 {
            let off = COMD0 + 4 * i;
            let cmd = self.reg(off);
            let op = (cmd >> 11) & 7;
            let count = (cmd & 0xff).max(if op == OP_WRITE || op == OP_READ {
                1
            } else {
                0
            });
            let ack_check = cmd & (1 << 8) != 0;
            let ack_exp = (cmd >> 9) & 1;
            let ack_value = (cmd >> 10) & 1;
            self.set_reg(off, cmd | CMD_DONE);
            match op {
                OP_RSTART => {
                    self.expect_address = true;
                    bits += 2;
                }
                OP_WRITE => {
                    let mut nack = false;
                    for _ in 0..count {
                        let byte = self.tx.pop_front().unwrap_or(0xff);
                        bits += 9;
                        let acked = if self.expect_address {
                            self.expect_address = false;
                            let addr = byte >> 1;
                            let read = byte & 1 != 0;
                            match self.devices.get_mut(&addr) {
                                Some(dev) => {
                                    self.selected = Some((addr, read));
                                    dev.start(read)
                                }
                                None => {
                                    self.selected = None;
                                    false
                                }
                            }
                        } else {
                            match self.selected {
                                Some((addr, _)) => {
                                    if let Some(log) = self.log.as_mut() {
                                        log.push((addr, false, byte));
                                    }
                                    self.devices.get_mut(&addr).is_some_and(|d| d.write(byte))
                                }
                                None => false,
                            }
                        };
                        // The ACK bit is 0 when the device acknowledges.
                        if ack_check && u32::from(!acked) != ack_exp {
                            nack = true;
                            break;
                        }
                    }
                    raise |= INT_BYTE_TRANS_DONE;
                    if nack {
                        raise |= INT_NACK;
                        self.finish_stop();
                        break;
                    }
                }
                OP_READ => {
                    for k in 0..count {
                        bits += 9;
                        // ack_value applies to every byte of the command.
                        let _ = k;
                        let master_ack = ack_value == 0;
                        let byte = match self.selected {
                            Some((addr, _)) => {
                                let b = self
                                    .devices
                                    .get_mut(&addr)
                                    .map_or(0xff, |d| d.read(master_ack));
                                if let Some(log) = self.log.as_mut() {
                                    log.push((addr, true, b));
                                }
                                b
                            }
                            None => 0xff,
                        };
                        if self.rx.len() < 32 {
                            self.rx.push_back(byte);
                        }
                    }
                    raise |= INT_BYTE_TRANS_DONE | INT_RXFIFO_WM;
                }
                OP_STOP => {
                    bits += 2;
                    self.finish_stop();
                    raise |= INT_TRANS_COMPLETE;
                    break;
                }
                OP_END => {
                    raise |= INT_END_DETECT;
                    break;
                }
                _ => {}
            }
        }
        if self.tx.len() <= 8 {
            raise |= INT_TXFIFO_WM;
        }
        self.resp_rec = raise & INT_NACK == 0;
        self.busy = true;
        if trace {
            eprintln!("i2c: raise {raise:#x}, rx {:02x?}", self.rx);
        }
        self.pending = Some((now + bits.max(1) * self.bit_ticks(), raise));
    }

    fn finish_stop(&mut self) {
        if let Some((addr, _)) = self.selected.take()
            && let Some(dev) = self.devices.get_mut(&addr)
        {
            dev.stop();
        }
        self.expect_address = false;
    }
}
