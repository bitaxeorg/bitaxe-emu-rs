//! eFuse controller (0x6000_7000): the read registers of the eFuse blocks and
//! the read/program command protocol.

use super::sens;

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
/// Calibration fields: block, first bit, width. The ADC codes at the
/// calibration point, ADC1 then ADC2, attenuations 0 to 3.
const BLK_VERSION_MAJOR: (usize, usize, usize) = (2, 128, 2);
const ADC_CAL_CODE: [(usize, usize, usize); 8] = [
    (2, 201, 8),
    (2, 209, 8),
    (2, 217, 8),
    (2, 225, 8),
    (2, 233, 8),
    (2, 241, 7),
    (2, 248, 7),
    (1, 186, 6),
];
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

    /// eFuse contents of a chip with the given MAC address and its ADC
    /// calibration (version 1).
    pub fn with_mac(mac: [u8; 6]) -> Self {
        let mut words = [0u32; WORDS];
        words[BLOCK1] = u32::from_be_bytes([mac[2], mac[3], mac[4], mac[5]]);
        words[BLOCK1 + 1] = u32::from(u16::from_be_bytes([mac[0], mac[1]]));
        let mut efuse = Efuse::new(words);
        efuse.burn_adc_calibration();
        efuse
    }

    /// Burns `value` into the field `(block, first bit, width)`.
    fn burn(&mut self, (block, bit, width): (usize, usize, usize), value: u32) {
        let start = BLOCK_LAYOUT[block].0;
        for i in 0..width {
            if (value >> i) & 1 != 0 {
                let b = bit + i;
                self.words[start + b / 32] |= 1 << (b % 32);
            }
        }
    }

    /// Calibration version 1, with the code each ADC gives at the calibration
    /// point. ADC1 codes are stored as differences from the next attenuation's
    /// (from 900 for attenuation 3), ADC2 codes as differences from ADC1's.
    fn burn_adc_calibration(&mut self) {
        self.burn(BLK_VERSION_MAJOR, 1);
        let adc1: [u32; 4] = core::array::from_fn(|a| sens::calibration_code(0, a as u32));
        let adc2: [u32; 4] = core::array::from_fn(|a| sens::calibration_code(1, a as u32));
        let diffs = [
            adc1[0] - adc1[1] - 800,
            adc1[1] - adc1[2] - 700,
            adc1[2] - adc1[3] - 800,
            adc1[3] - 900,
            adc1[0] + 40 - adc2[0],
            adc1[1] + 10 - adc2[1],
            adc1[2] + 20 - adc2[2],
            adc1[3] + 15 - adc2[3],
        ];
        for (field, diff) in ADC_CAL_CODE.into_iter().zip(diffs) {
            self.burn(field, diff);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn field(e: &Efuse, (block, bit, width): (usize, usize, usize)) -> u32 {
        let start = BLOCK_LAYOUT[block].0;
        (0..width).fold(0, |acc, i| {
            let b = bit + i;
            acc | (((e.words[start + b / 32] >> (b % 32)) & 1) << i)
        })
    }

    #[test]
    fn adc_calibration_decodes_to_the_model_codes() {
        let e = Efuse::with_mac([0x7c, 0xdf, 0xa1, 0, 0, 1]);
        assert_eq!(field(&e, BLK_VERSION_MAJOR), 1);
        let d: Vec<u32> = ADC_CAL_CODE.iter().map(|&f| field(&e, f)).collect();
        // Calibration version 1: codes from the stored differences.
        let mut adc1 = [0; 4];
        adc1[3] = d[3] + 900;
        adc1[2] = d[2] + adc1[3] + 800;
        adc1[1] = d[1] + adc1[2] + 700;
        adc1[0] = d[0] + adc1[1] + 800;
        let adc2 = [
            adc1[0] - d[4] + 40,
            adc1[1] - d[5] + 10,
            adc1[2] - d[6] + 20,
            adc1[3] - d[7] + 15,
        ];
        for a in 0..4 {
            assert_eq!(adc1[a], sens::calibration_code(0, a as u32));
            assert_eq!(adc2[a], sens::calibration_code(1, a as u32));
        }
    }
}
