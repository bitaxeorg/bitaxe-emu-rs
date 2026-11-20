//! TMP1075 temperature sensor on the board.

use esp32s3_emu::devices::i2c::I2cDevice;

use super::pointer::WordAccess;

const TEMP: u8 = 0x00;
const CFGR: u8 = 0x01;
const LLIM: u8 = 0x02;
const HLIM: u8 = 0x03;
const DIEID: u8 = 0x0f;

pub struct Tmp1075 {
    access: WordAccess,
    regs: [u16; 4],
    /// Temperature, °C.
    pub temperature: f32,
}

impl Default for Tmp1075 {
    fn default() -> Self {
        Tmp1075 {
            access: WordAccess::default(),
            regs: [0, 0x00ff, 0x4b00, 0x5000],
            temperature: 25.0,
        }
    }
}

impl Tmp1075 {
    fn value(&self, reg: u8) -> u16 {
        match reg {
            // 12 bits left aligned, 1/16 °C.
            TEMP => ((self.temperature * 16.0).round().clamp(-2048.0, 2047.0) as i16 as u16) << 4,
            CFGR | LLIM | HLIM => self.regs[usize::from(reg)],
            DIEID => 0x7500,
            _ => 0,
        }
    }
}

impl I2cDevice for Tmp1075 {
    fn start(&mut self, read: bool) -> bool {
        let value = self.value(self.access.pointer.reg);
        self.access.start(read, value);
        true
    }

    fn write(&mut self, byte: u8) -> bool {
        self.access.write(byte);
        true
    }

    fn read(&mut self, _ack: bool) -> u8 {
        self.access.read()
    }

    fn stop(&mut self) {
        if let Some((reg @ (CFGR | LLIM | HLIM), v)) = self.access.stop() {
            self.regs[usize::from(reg)] = v;
        }
    }
}
