//! EMC2101 fan controller and temperature sensor: a register file with the
//! temperatures and the tachometer computed from the board state.

use esp32s3_rax::devices::i2c::I2cDevice;

const INTERNAL_TEMP: usize = 0x00;
const EXTERNAL_TEMP_MSB: usize = 0x01;
const EXTERNAL_TEMP_LSB: usize = 0x10;
const TACH_LSB: usize = 0x46;
const TACH_MSB: usize = 0x47;
const FAN_CONFIG: usize = 0x4a;
const FAN_SETTING: usize = 0x4c;
const PRODUCT_ID: usize = 0xfd;
const MANUFACTURER_ID: usize = 0xfe;
const REVISION: usize = 0xff;

pub struct Emc2101 {
    regs: [u8; 256],
    pointer: usize,
    first: bool,
    /// Temperature of the board around the chip, °C.
    pub internal_temp: f32,
    /// Temperature of the external diode (the ASIC), °C.
    pub external_temp: f32,
    /// Fan speed, RPM.
    pub fan_rpm: f32,
}

impl Default for Emc2101 {
    fn default() -> Self {
        Self::new()
    }
}

impl Emc2101 {
    pub fn new() -> Self {
        let mut regs = [0u8; 256];
        regs[0x03] = 0x00;
        regs[0x04] = 0x08;
        regs[0x17] = 0x12;
        regs[0x18] = 0x08;
        regs[FAN_CONFIG] = 0x20;
        regs[FAN_SETTING] = 0x00;
        regs[0x4d] = 0x17;
        regs[0x4e] = 0x01;
        regs[PRODUCT_ID] = 0x16;
        regs[MANUFACTURER_ID] = 0x5d;
        regs[REVISION] = 0x01;
        Emc2101 {
            regs,
            pointer: 0,
            first: true,
            internal_temp: 25.0,
            external_temp: 25.0,
            fan_rpm: 0.0,
        }
    }

    /// Fan drive, 0..=1, from FAN_SETTING (0..63).
    pub fn duty(&self) -> f32 {
        f32::from(self.regs[FAN_SETTING].min(63)) / 63.0
    }

    fn value(&self, reg: usize) -> u8 {
        match reg {
            INTERNAL_TEMP => self.internal_temp.round().clamp(-64.0, 127.0) as i8 as u8,
            EXTERNAL_TEMP_MSB => self.external_temp.floor().clamp(-64.0, 127.0) as i8 as u8,
            EXTERNAL_TEMP_LSB => {
                (((self.external_temp - self.external_temp.floor()) * 8.0) as u8 & 7) << 5
            }
            TACH_LSB | TACH_MSB => {
                let tach: u16 = if self.fan_rpm < 100.0 {
                    0xffff
                } else {
                    (5_400_000.0 / self.fan_rpm).min(65534.0) as u16
                };
                if reg == TACH_LSB {
                    tach as u8
                } else {
                    (tach >> 8) as u8
                }
            }
            _ => self.regs[reg],
        }
    }
}

impl I2cDevice for Emc2101 {
    fn start(&mut self, read: bool) -> bool {
        self.first = !read;
        true
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.first {
            self.pointer = usize::from(byte);
            self.first = false;
        } else {
            match self.pointer {
                PRODUCT_ID | MANUFACTURER_ID | REVISION | INTERNAL_TEMP | EXTERNAL_TEMP_MSB
                | EXTERNAL_TEMP_LSB | TACH_LSB | TACH_MSB => {}
                p => self.regs[p] = byte,
            }
            self.pointer = (self.pointer + 1) & 0xff;
        }
        true
    }

    fn read(&mut self, _ack: bool) -> u8 {
        self.value(self.pointer)
    }
}
