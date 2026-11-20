//! eFuse controller (0x6000_7000): the read registers of the eFuse blocks and
//! the read/program command protocol.

/// Block read registers: BLOCK0 (6 words) at 0x2C to BLOCK10 ending at 0x178.
const BLOCKS_START: u32 = 0x02c;
const BLOCKS_END: u32 = 0x17c;
pub const WORDS: usize = ((BLOCKS_END - BLOCKS_START) / 4) as usize;

const CONF: u32 = 0x1cc;
const STATUS: u32 = 0x1d0;
const CMD: u32 = 0x1d4;
const INT_RAW: u32 = 0x1d8;
const INT_ST: u32 = 0x1dc;
const INT_ENA: u32 = 0x1e0;
const INT_CLR: u32 = 0x1e4;
const DATE: u32 = 0x1fc;

/// Word index of BLOCK1 (MAC, SPI and system data) in [`Efuse::words`].
pub const BLOCK1: usize = 6;
/// Start word of each block (BLOCK0..BLOCK10) and its length in words.
const BLOCK_LAYOUT: [(usize, usize); 11] = [
    (0, 6),
    (6, 6),
    (12, 8),
    (20, 8),
    (28, 8),
    (36, 8),
    (44, 8),
    (52, 8),
    (60, 8),
    (68, 8),
    (76, 8),
];

pub struct Efuse {
    /// The burnt eFuse bits, as the block read registers show them.
    pub words: [u32; WORDS],
    pgm_data: [u32; 8],
    regs: [u32; 0x80],
    int_raw: u32,
    int_ena: u32,
}

impl Efuse {
    pub fn new(words: [u32; WORDS]) -> Self {
        Efuse {
            words,
            pgm_data: [0; 8],
            regs: [0; 0x80],
            int_raw: 0,
            int_ena: 0,
        }
    }

    /// eFuse contents with the given MAC address and nothing else burnt.
    pub fn with_mac(mac: [u8; 6]) -> Self {
        let mut words = [0u32; WORDS];
        words[BLOCK1] = u32::from_be_bytes([mac[2], mac[3], mac[4], mac[5]]);
        words[BLOCK1 + 1] = u32::from(u16::from_be_bytes([mac[0], mac[1]]));
        Efuse::new(words)
    }

    pub fn irq(&self) -> bool {
        self.int_raw & self.int_ena != 0
    }

    pub fn read(&self, off: u32) -> u32 {
        match off {
            0x000..=0x01c => self.pgm_data[(off / 4) as usize],
            BLOCKS_START..BLOCKS_END => self.words[((off - BLOCKS_START) / 4) as usize],
            STATUS => 1,
            CMD => 0,
            INT_RAW => self.int_raw,
            INT_ST => self.int_raw & self.int_ena,
            INT_ENA => self.int_ena,
            DATE => 0x0210_1150,
            o if o < 0x200 => self.regs[(o / 4) as usize],
            _ => 0,
        }
    }

    /// Returns whether the interrupt state may have changed.
    pub fn write(&mut self, off: u32, v: u32) -> bool {
        match off {
            0x000..=0x01c => self.pgm_data[(off / 4) as usize] = v,
            CMD => {
                let op = self.regs[(CONF / 4) as usize] & 0xffff;
                if v & 1 != 0 && op == 0x5aa5 {
                    self.int_raw |= 1;
                } else if v & 2 != 0 && op == 0x5a5a {
                    let block = ((v >> 2) & 0xf) as usize;
                    if let Some(&(start, len)) = BLOCK_LAYOUT.get(block) {
                        for i in 0..len {
                            self.words[start + i] |= self.pgm_data[i];
                        }
                    }
                    self.int_raw |= 2;
                }
            }
            INT_ENA => self.int_ena = v & 3,
            INT_CLR => self.int_raw &= !(v & 3),
            BLOCKS_START..BLOCKS_END | STATUS | INT_RAW | INT_ST | DATE => return false,
            o if o < 0x200 => self.regs[(o / 4) as usize] = v,
            _ => return false,
        }
        true
    }
}
