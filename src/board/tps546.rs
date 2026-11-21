//! TPS546D24A buck converter on PMBus: every command reads back what was written,
//! IC_DEVICE_ID and the telemetry commands are computed from the board state,
//! MFR_ID, MFR_MODEL and MFR_REVISION start as three zero bytes.

use std::collections::BTreeMap;

use esp32s3_rax::devices::i2c::I2cDevice;

const OPERATION: u8 = 0x01;
const CLEAR_FAULTS: u8 = 0x03;
const VOUT_MODE: u8 = 0x20;
const VOUT_COMMAND: u8 = 0x21;
const STATUS_BYTE: u8 = 0x78;
const STATUS_WORD: u8 = 0x79;
const READ_VIN: u8 = 0x88;
const READ_VOUT: u8 = 0x8b;
const READ_IOUT: u8 = 0x8c;
const READ_TEMPERATURE_1: u8 = 0x8d;
const MFR_ID: u8 = 0x99;
const MFR_MODEL: u8 = 0x9a;
const MFR_REVISION: u8 = 0x9b;
const IC_DEVICE_ID: u8 = 0xad;
const IC_DEVICE_REV: u8 = 0xae;

/// Encodes `value` in PMBus LINEAR11 (5-bit exponent, 11-bit mantissa).
pub fn linear11(value: f32) -> u16 {
    let mut exp: i32 = -16;
    while exp < 15 && (value / 2f32.powi(exp)).abs() > 1023.0 {
        exp += 1;
    }
    let mantissa = (value / 2f32.powi(exp)).round() as i32;
    (((exp & 0x1f) as u16) << 11) | ((mantissa & 0x7ff) as u16)
}

/// Decodes a LINEAR16 value with the exponent of VOUT_MODE.
fn linear16(raw: u16, vout_mode: u8) -> f32 {
    let exp = ((i32::from(vout_mode & 0x1f)) << 27) >> 27;
    f32::from(raw) * 2f32.powi(exp)
}

/// What the regulator measures, from the rest of the board.
#[derive(Clone, Copy, Debug, Default)]
pub struct Telemetry {
    pub vin: f32,
    pub iout: f32,
    pub temperature: f32,
}

pub struct Tps546 {
    store: BTreeMap<u8, Vec<u8>>,
    cmd: Option<u8>,
    data: Vec<u8>,
    reading: Option<(Vec<u8>, usize)>,
    pub telemetry: Telemetry,
}

impl Default for Tps546 {
    fn default() -> Self {
        Self::new()
    }
}

impl Tps546 {
    pub fn new() -> Self {
        let mut store = BTreeMap::new();
        store.insert(VOUT_MODE, vec![0x17]);
        store.insert(OPERATION, vec![0x00]);
        store.insert(IC_DEVICE_ID, vec![6, 0x54, 0x49, 0x54, 0x6d, 0x24, 0x41]);
        store.insert(IC_DEVICE_REV, vec![1, 0x02]);
        // Blocks of 3 user bytes, zero on a new chip.
        for cmd in [MFR_ID, MFR_MODEL, MFR_REVISION] {
            store.insert(cmd, vec![3, 0, 0, 0]);
        }
        Tps546 {
            store,
            cmd: None,
            data: Vec::new(),
            reading: None,
            telemetry: Telemetry {
                vin: 5.0,
                iout: 0.0,
                temperature: 30.0,
            },
        }
    }

    fn byte(&self, cmd: u8) -> u8 {
        self.store
            .get(&cmd)
            .and_then(|v| v.first().copied())
            .unwrap_or(0)
    }

    fn word(&self, cmd: u8) -> u16 {
        match self.store.get(&cmd) {
            Some(v) if v.len() >= 2 => u16::from_le_bytes([v[0], v[1]]),
            _ => 0,
        }
    }

    /// The output is on (OPERATION bit 7).
    pub fn output_on(&self) -> bool {
        self.byte(OPERATION) & 0x80 != 0
    }

    /// The commanded output voltage, in volts.
    pub fn vout_command(&self) -> f32 {
        linear16(self.word(VOUT_COMMAND), self.byte(VOUT_MODE))
    }

    /// The output voltage, in volts (0 when off).
    pub fn vout(&self) -> f32 {
        if self.output_on() {
            self.vout_command()
        } else {
            0.0
        }
    }

    fn response(&self, cmd: u8) -> Vec<u8> {
        let word = |v: u16| v.to_le_bytes().to_vec();
        match cmd {
            READ_VIN => word(linear11(self.telemetry.vin)),
            READ_IOUT => word(linear11(if self.output_on() {
                self.telemetry.iout
            } else {
                0.0
            })),
            READ_TEMPERATURE_1 => word(linear11(self.telemetry.temperature)),
            READ_VOUT => {
                let exp = ((i32::from(self.byte(VOUT_MODE) & 0x1f)) << 27) >> 27;
                word((self.vout() / 2f32.powi(exp)).round() as u16)
            }
            STATUS_BYTE => vec![if self.output_on() { 0 } else { 0x40 }],
            STATUS_WORD => word(if self.output_on() { 0 } else { 0x0040 }),
            _ => self.store.get(&cmd).cloned().unwrap_or_else(|| vec![0, 0]),
        }
    }
}

impl I2cDevice for Tps546 {
    fn start(&mut self, read: bool) -> bool {
        if read {
            let resp = self.cmd.map(|c| self.response(c)).unwrap_or_default();
            self.reading = Some((resp, 0));
        } else {
            self.cmd = None;
            self.data.clear();
            self.reading = None;
        }
        true
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.cmd.is_none() {
            self.cmd = Some(byte);
        } else {
            self.data.push(byte);
        }
        true
    }

    fn read(&mut self, _ack: bool) -> u8 {
        match self.reading.as_mut() {
            Some((resp, pos)) => {
                let b = resp.get(*pos).copied().unwrap_or(0xff);
                *pos += 1;
                b
            }
            None => 0xff,
        }
    }

    fn stop(&mut self) {
        if self.reading.take().is_none()
            && let Some(cmd) = self.cmd
        {
            match cmd {
                CLEAR_FAULTS => {}
                VOUT_MODE | IC_DEVICE_ID | IC_DEVICE_REV => {}
                _ if !self.data.is_empty() => {
                    self.store.insert(cmd, std::mem::take(&mut self.data));
                }
                _ => {}
            }
        }
    }
}
