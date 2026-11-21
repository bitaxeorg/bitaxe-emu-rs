//! INA260 power monitor (0x40) on the board input: current, bus voltage and
//! power from the board state.

use esp32s3_rax::devices::i2c::I2cDevice;

use super::pointer::WordAccess;

const CONFIG: u8 = 0x00;
const CURRENT: u8 = 0x01;
const VOLTAGE: u8 = 0x02;
const POWER: u8 = 0x03;
const MASK_ENABLE: u8 = 0x06;
const ALERT_LIMIT: u8 = 0x07;
const MANUFACTURER_ID: u8 = 0xfe;
const DIE_ID: u8 = 0xff;

const CONFIG_RESET: u16 = 0x8000;
const CONFIG_DEFAULT: u16 = 0x6127;

pub struct Ina260 {
    access: WordAccess,
    config: u16,
    mask_enable: u16,
    alert_limit: u16,
    /// Input voltage, V.
    pub voltage: f32,
    /// Input current, A.
    pub current: f32,
}

impl Default for Ina260 {
    fn default() -> Self {
        Ina260 {
            access: WordAccess::default(),
            config: CONFIG_DEFAULT,
            mask_enable: 0,
            alert_limit: 0,
            voltage: 5.0,
            current: 0.0,
        }
    }
}

impl Ina260 {
    fn value(&self, reg: u8) -> u16 {
        // 1.25 mA, 1.25 mV and 10 mW per bit.
        match reg {
            CONFIG => self.config,
            CURRENT => (self.current / 0.00125).round().clamp(-32768.0, 32767.0) as i16 as u16,
            VOLTAGE => (self.voltage / 0.00125).round().clamp(0.0, 32767.0) as u16,
            POWER => (self.voltage * self.current.abs() / 0.010)
                .round()
                .clamp(0.0, 65535.0) as u16,
            MASK_ENABLE => self.mask_enable,
            ALERT_LIMIT => self.alert_limit,
            MANUFACTURER_ID => 0x5449,
            DIE_ID => 0x2270,
            _ => 0,
        }
    }
}

impl I2cDevice for Ina260 {
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
        match self.access.stop() {
            Some((CONFIG, v)) if v & CONFIG_RESET != 0 => {
                self.config = CONFIG_DEFAULT;
                self.mask_enable = 0;
                self.alert_limit = 0;
            }
            Some((CONFIG, v)) => self.config = v,
            Some((MASK_ENABLE, v)) => self.mask_enable = v,
            Some((ALERT_LIMIT, v)) => self.alert_limit = v,
            _ => {}
        }
    }
}
