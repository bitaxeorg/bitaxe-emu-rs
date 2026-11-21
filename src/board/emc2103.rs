//! EMC2103-2 fan controller and temperature sensor (0x2E): a register file with
//! the internal and diode temperatures and the tachometer computed from the
//! board state.

use esp32s3_rax::devices::i2c::I2cDevice;

use super::pointer::Pointer;

/// High bytes of the temperatures: internal, external 1, 2 and 3; the low byte
/// follows each.
const TEMP_HIGH: [u8; 4] = [0x00, 0x02, 0x04, 0x06];
const FAN_SETTING: u8 = 0x40;
const FAN_CONFIGURATION1: u8 = 0x42;
const TACH_READING_HIGH: u8 = 0x4e;
const TACH_READING_LOW: u8 = 0x4f;
const PRODUCT_ID: u8 = 0xfd;
const MANUFACTURER_ID: u8 = 0xfe;
const REVISION: u8 = 0xff;

/// TACH count for an RPM, the count multiplier given by the range.
pub fn tach_count(rpm: f32, multiplier: u32) -> u16 {
    if rpm < 100.0 {
        0x1fff
    } else {
        (3_932_160.0 * multiplier as f32 / rpm).clamp(1.0, 8191.0) as u16
    }
}

/// Temperature in the 11-bit, 1/8 °C format, left aligned on 16 bits.
fn temperature_word(t: f32) -> u16 {
    (((t * 8.0).round().clamp(-1024.0, 1023.0) as i16) << 5) as u16
}

pub struct Emc2103 {
    regs: [u8; 256],
    pointer: Pointer,
    /// Internal sensor, °C.
    pub internal_temp: f32,
    /// External diodes 1 and 2, °C (diode 3 is not connected).
    pub external_temp: [f32; 2],
    /// Fan speed, RPM.
    pub fan_rpm: f32,
}

impl Default for Emc2103 {
    fn default() -> Self {
        let mut regs = [0u8; 256];
        regs[0x11] = 0x12;
        regs[0x12] = 0x12;
        regs[0x14] = 0x08;
        regs[0x15] = 0x08;
        regs[0x20] = 0x00;
        regs[0x2a] = 0x00;
        regs[0x2b] = 0x00;
        regs[usize::from(FAN_SETTING)] = 0x00;
        regs[0x41] = 0x01;
        regs[usize::from(FAN_CONFIGURATION1)] = 0x2b;
        regs[0x43] = 0x28;
        regs[0x50] = 0x00;
        regs[usize::from(PRODUCT_ID)] = 0x26;
        regs[usize::from(MANUFACTURER_ID)] = 0x5d;
        regs[usize::from(REVISION)] = 0x01;
        Emc2103 {
            regs,
            pointer: Pointer::default(),
            internal_temp: 25.0,
            external_temp: [25.0; 2],
            fan_rpm: 0.0,
        }
    }
}

impl Emc2103 {
    /// Fan drive, 0..=1.
    pub fn duty(&self) -> f32 {
        f32::from(self.regs[usize::from(FAN_SETTING)]) / 255.0
    }

    fn value(&self, reg: u8) -> u8 {
        if let Some(i) = TEMP_HIGH.iter().position(|&r| r == reg & !1) {
            let word = match i {
                0 => temperature_word(self.internal_temp),
                1 | 2 => temperature_word(self.external_temp[i - 1]),
                // Diode fault.
                _ => 0x8000,
            };
            return if reg & 1 == 0 {
                (word >> 8) as u8
            } else {
                word as u8
            };
        }
        match reg {
            TACH_READING_HIGH | TACH_READING_LOW => {
                let range = (self.regs[usize::from(FAN_CONFIGURATION1)] >> 5) & 3;
                let count = tach_count(self.fan_rpm, 1 << range);
                if reg == TACH_READING_HIGH {
                    (count >> 5) as u8
                } else {
                    ((count & 0x1f) << 3) as u8
                }
            }
            _ => self.regs[usize::from(reg)],
        }
    }

    fn read_only(reg: u8) -> bool {
        reg < 0x08 || matches!(reg, TACH_READING_HIGH | TACH_READING_LOW) || reg >= PRODUCT_ID
    }
}

impl I2cDevice for Emc2103 {
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
