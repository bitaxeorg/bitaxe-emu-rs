//! SPI1 flash controller (0x6000_2000) and the SPI NOR flash on its CS0.
//!
//! Each transaction runs when it is started by a write to SPI_MEM_CMD:
//! predefined commands (read, write enable, page program, erases, status, ID) or
//! user transactions (command, address, dummy, MOSI and MISO phases). Program
//! and erase complete at once.

use super::regfile::RegFile;

const CMD: u32 = 0x000;
const ADDR: u32 = 0x004;
const USER: u32 = 0x018;
const USER1: u32 = 0x01c;
const USER2: u32 = 0x020;
const MOSI_DLEN: u32 = 0x024;
const MISO_DLEN: u32 = 0x028;
const RD_STATUS: u32 = 0x02c;
const W0: u32 = 0x058;
const DATE: u32 = 0x3fc;

const USER_ADDR: u32 = 1 << 30;
const USER_MISO: u32 = 1 << 28;
const USER_MOSI: u32 = 1 << 27;

/// SPI NOR flash: array, status register and write-enable latch.
pub struct Flash {
    pub data: Vec<u8>,
    pub jedec_id: [u8; 3],
    wel: bool,
    status: u8,
    status2: u8,
    four_byte: bool,
    /// Set when the array changes.
    pub dirty: bool,
}

impl Flash {
    pub fn new(mut data: Vec<u8>) -> Self {
        let size = data.len().next_power_of_two().max(1 << 20);
        data.resize(size, 0xff);
        // Manufacturer, memory type, capacity (log2 of the size).
        let jedec_id = [0x9d, 0x60, size.trailing_zeros() as u8];
        Flash {
            data,
            jedec_id,
            wel: false,
            status: 0,
            status2: 0,
            four_byte: false,
            dirty: false,
        }
    }

    pub fn reset(&mut self) {
        self.wel = false;
        self.four_byte = false;
    }

    fn erase(&mut self, addr: u32, size: usize) {
        if !self.wel {
            return;
        }
        let start = (addr as usize & !(size - 1)) % self.data.len();
        let end = (start + size).min(self.data.len());
        self.data[start..end].fill(0xff);
        self.wel = false;
        self.dirty = true;
    }

    /// Runs one command: opcode, address (if any), bytes written, bytes to read.
    pub fn exec(&mut self, opcode: u8, addr: Option<u32>, mosi: &[u8], miso_len: usize) -> Vec<u8> {
        let a = addr.unwrap_or(0);
        let len = self.data.len();
        match opcode {
            0x03 | 0x0b | 0x3b | 0x6b | 0xbb | 0xeb | 0x13 | 0x0c => (0..miso_len)
                .map(|i| self.data[(a as usize + i) % len])
                .collect(),
            0x05 => vec![self.status | (u8::from(self.wel) << 1); miso_len],
            0x35 => vec![self.status2; miso_len],
            0x15 => vec![0; miso_len],
            0x06 => {
                self.wel = true;
                vec![0; miso_len]
            }
            0x04 => {
                self.wel = false;
                vec![0; miso_len]
            }
            0x01 => {
                if self.wel {
                    if let Some(&b) = mosi.first() {
                        self.status = b & 0x9c;
                    }
                    if let Some(&b) = mosi.get(1) {
                        self.status2 = b;
                    }
                    self.wel = false;
                }
                vec![0; miso_len]
            }
            0x31 => {
                if self.wel
                    && let Some(&b) = mosi.first()
                {
                    self.status2 = b;
                }
                vec![0; miso_len]
            }
            0x02 | 0x12 | 0x32 | 0x38 => {
                if self.wel {
                    let page = a as usize & !0xff;
                    for (i, &b) in mosi.iter().enumerate() {
                        let off = (page | ((a as usize + i) & 0xff)) % len;
                        self.data[off] &= b;
                    }
                    self.wel = false;
                    self.dirty = true;
                }
                vec![0; miso_len]
            }
            0x20 | 0x21 => {
                self.erase(a, 4 << 10);
                vec![0; miso_len]
            }
            0x52 => {
                self.erase(a, 32 << 10);
                vec![0; miso_len]
            }
            0xd8 | 0xdc => {
                self.erase(a, 64 << 10);
                vec![0; miso_len]
            }
            0x60 | 0xc7 => {
                if self.wel {
                    self.data.fill(0xff);
                    self.wel = false;
                    self.dirty = true;
                }
                vec![0; miso_len]
            }
            0x9f => (0..miso_len)
                .map(|i| self.jedec_id.get(i).copied().unwrap_or(0))
                .collect(),
            0x90 => (0..miso_len)
                .map(|i| {
                    if i % 2 == 0 {
                        self.jedec_id[0]
                    } else {
                        self.jedec_id[2] - 1
                    }
                })
                .collect(),
            0xab => vec![self.jedec_id[2] - 1; miso_len],
            0xb7 => {
                self.four_byte = true;
                vec![0; miso_len]
            }
            0xe9 => {
                self.four_byte = false;
                vec![0; miso_len]
            }
            0x66 | 0x99 => {
                self.wel = false;
                vec![0; miso_len]
            }
            _ => vec![0; miso_len],
        }
    }
}

pub struct Spi1 {
    regs: RegFile,
}

impl Default for Spi1 {
    fn default() -> Self {
        Self::new()
    }
}

impl Spi1 {
    pub fn new() -> Self {
        Spi1 {
            regs: RegFile::new("spi1", 0x400)
                .with_reset(0x00c, 0x0000_0ffc)
                .with_reset(0x014, 0x0003_0103)
                .with_reset(USER, 0x8000_0000)
                .with_reset(USER1, 0x5c00_0007)
                .with_reset(USER2, 0x7000_0000)
                .with_reset(0x034, 0x0000_0002)
                .with_reset(DATE, 0x0210_1040),
        }
    }

    pub fn reset(&mut self) {
        self.regs.reset();
    }

    fn buffer(&self) -> [u8; 64] {
        let mut b = [0u8; 64];
        for i in 0..16 {
            b[i * 4..i * 4 + 4].copy_from_slice(&self.regs.get(W0 + 4 * i as u32).to_le_bytes());
        }
        b
    }

    fn set_buffer(&mut self, data: &[u8]) {
        let mut b = self.buffer();
        let n = data.len().min(64);
        b[..n].copy_from_slice(&data[..n]);
        for i in 0..16 {
            self.regs.set(
                W0 + 4 * i as u32,
                u32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap()),
            );
        }
    }

    fn addr_bytes(&self) -> u32 {
        ((self.regs.get(USER1) >> 26) + 1) / 8
    }

    fn address(&self) -> u32 {
        let n = self.addr_bytes().clamp(1, 4);
        let a = self.regs.get(ADDR);
        if n >= 4 { a } else { a & ((1 << (8 * n)) - 1) }
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            CMD => 0,
            _ => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, v: u32, flash: &mut Flash) {
        if off != CMD {
            if off != DATE {
                self.regs.write(off, v);
            }
            return;
        }
        let miso_len = ((self.regs.get(MISO_DLEN) & 0x3ff) as usize + 1) / 8;
        let mosi_len = ((self.regs.get(MOSI_DLEN) & 0x3ff) as usize + 1) / 8;
        if v & (1 << 18) != 0 {
            let user = self.regs.get(USER);
            let opcode = (self.regs.get(USER2) & 0xff) as u8;
            let addr = (user & USER_ADDR != 0).then(|| self.address());
            let mosi: Vec<u8> = if user & USER_MOSI != 0 {
                self.buffer()[..mosi_len.min(64)].to_vec()
            } else {
                Vec::new()
            };
            let miso = if user & USER_MISO != 0 {
                miso_len.min(64)
            } else {
                0
            };
            let data = flash.exec(opcode, addr, &mosi, miso);
            if miso > 0 {
                self.set_buffer(&data);
            }
            return;
        }
        match v & 0xfff8_0000 {
            c if c == 1 << 31 => {
                let data = flash.exec(0x03, Some(self.address()), &[], miso_len.min(64));
                self.set_buffer(&data);
            }
            c if c == 1 << 30 => {
                flash.exec(0x06, None, &[], 0);
            }
            c if c == 1 << 29 => {
                flash.exec(0x04, None, &[], 0);
            }
            c if c == 1 << 28 => {
                let id = flash.exec(0x9f, None, &[], 3);
                self.set_buffer(&id);
            }
            c if c == 1 << 27 => {
                let s = flash.exec(0x05, None, &[], 1);
                let old = self.regs.get(RD_STATUS);
                self.regs.set(RD_STATUS, (old & !0xff) | u32::from(s[0]));
            }
            c if c == 1 << 26 => {
                let s = self.regs.get(RD_STATUS);
                flash.exec(0x01, None, &[s as u8, (s >> 8) as u8], 0);
            }
            c if c == 1 << 25 => {
                let a = self.regs.get(ADDR);
                let len = ((a >> 24) as usize).min(64);
                let data = self.buffer()[..len].to_vec();
                flash.exec(0x02, Some(a & 0xff_ffff), &data, 0);
            }
            c if c == 1 << 24 => {
                flash.exec(0x20, Some(self.address()), &[], 0);
            }
            c if c == 1 << 23 => {
                flash.exec(0xd8, Some(self.address()), &[], 0);
            }
            c if c == 1 << 22 => {
                flash.exec(0x60, None, &[], 0);
            }
            c if c == 1 << 20 || c == 1 << 19 => {
                let id = flash.exec(0xab, None, &[], 3);
                self.set_buffer(&id);
            }
            _ => {}
        }
    }
}
