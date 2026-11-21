//! EMC2302 two-fan controller (0x2F): a register file with each fan's
//! tachometer computed from the board state.

use esp32s3_rax::devices::i2c::I2cDevice;

use super::emc2103::tach_count;
use super::pointer::Pointer;

/// Register blocks of fans 1 and 2.
const FAN_BASE: [u8; 2] = [0x30, 0x40];
const SETTING: u8 = 0x00;
const CONFIGURATION1: u8 = 0x02;
const TACH_READING_HIGH: u8 = 0x0e;
const TACH_READING_LOW: u8 = 0x0f;
const PRODUCT_ID: u8 = 0xfd;
const MANUFACTURER_ID: u8 = 0xfe;
const REVISION: u8 = 0xff;

pub struct Emc2302 {
    regs: [u8; 256],
    pointer: Pointer,
    /// Speed of each fan, RPM.
    pub fan_rpm: [f32; 2],
}

impl Default for Emc2302 {
    fn default() -> Self {
        let mut regs = [0u8; 256];
        regs[0x20] = 0x40;
        regs[0x2b] = 0x00;
        for base in FAN_BASE {
            let b = usize::from(base);
            regs[b + usize::from(SETTING)] = 0x00;
            regs[b + 0x01] = 0x01;
            regs[b + usize::from(CONFIGURATION1)] = 0x2b;
            regs[b + 0x03] = 0x28;
            regs[b + 0x05] = 0x2b;
            regs[b + 0x06] = 0x19;
            regs[b + 0x07] = 0x10;
            regs[b + 0x08] = 0x66;
            regs[b + 0x09] = 0xf5;
            regs[b + 0x0c] = 0xf8;
            regs[b + 0x0d] = 0xff;
        }
        regs[usize::from(PRODUCT_ID)] = 0x36;
        regs[usize::from(MANUFACTURER_ID)] = 0x5d;
        regs[usize::from(REVISION)] = 0x80;
        Emc2302 {
            regs,
            pointer: Pointer::default(),
            fan_rpm: [0.0; 2],
        }
    }
}

impl Emc2302 {
    /// Drive of `fan` (0 or 1), 0..=1.
    pub fn duty(&self, fan: usize) -> f32 {
        f32::from(self.regs[usize::from(FAN_BASE[fan] + SETTING)]) / 255.0
    }

    fn value(&self, reg: u8) -> u8 {
        for (fan, base) in FAN_BASE.into_iter().enumerate() {
            if reg == base + TACH_READING_HIGH || reg == base + TACH_READING_LOW {
                let range = (self.regs[usize::from(base + CONFIGURATION1)] >> 5) & 3;
                let count = tach_count(self.fan_rpm[fan], 1 << range);
                return if reg == base + TACH_READING_HIGH {
                    (count >> 5) as u8
                } else {
                    ((count & 0x1f) << 3) as u8
                };
            }
        }
        self.regs[usize::from(reg)]
    }

    fn read_only(reg: u8) -> bool {
        reg >= PRODUCT_ID
            || FAN_BASE
                .iter()
                .any(|&b| reg == b + TACH_READING_HIGH || reg == b + TACH_READING_LOW)
    }
}

impl I2cDevice for Emc2302 {
    fn start(&mut self, read: bool) -> bool {
        self.pointer.start(read);
        true
    }

    fn write(&mut self, byte: u8) -> bool {
        if let Some(reg) = self.pointer.write(byte)
            && !Self::read_only(reg)
        {
            self.regs[usize::from(reg)] = byte;
        }
        true
    }

    fn read(&mut self, _ack: bool) -> u8 {
        self.value(self.pointer.reg)
    }
}
