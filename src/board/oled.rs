//! The OLED on the boards' I2C bus: the boards' SSD1306 (128 × 32), or, chosen with
//! `BITAXE_EMU_OLED`, an SSD1309 (128 × 64, `ssd1309`) or an SH1107 (`sh1107-64x128`,
//! `sh1107-128x128`), as ESP-Miner's display types.

use esp32s3_rax::devices::i2c::I2cDevice;

use super::{OLED_ROWS, sh1107::Sh1107, ssd1306::Ssd1306};

pub enum Oled {
    Ssd1306(Box<Ssd1306>),
    Sh1107(Box<Sh1107>),
}

macro_rules! each {
    ($oled:expr, $d:ident => $body:expr) => {
        match $oled {
            Oled::Ssd1306($d) => $body,
            Oled::Sh1107($d) => $body,
        }
    };
}

impl Oled {
    /// The panel `BITAXE_EMU_OLED` names, the boards' SSD1306 by default.
    pub fn from_env() -> Self {
        match std::env::var("BITAXE_EMU_OLED").ok().as_deref() {
            Some("ssd1309") => Oled::Ssd1306(Box::new(Ssd1306::new(64))),
            Some("sh1107-64x128") => Oled::Sh1107(Box::new(Sh1107::new(64, 128))),
            Some("sh1107-128x128") => Oled::Sh1107(Box::new(Sh1107::new(128, 128))),
            _ => Oled::Ssd1306(Box::new(Ssd1306::new(OLED_ROWS))),
        }
    }

    pub fn is_on(&self) -> bool {
        each!(self, d => d.is_on())
    }

    /// The panel image, row-major, one byte per pixel (0 or 1).
    pub fn pixels(&self) -> Vec<u8> {
        each!(self, d => d.pixels())
    }

    pub fn width(&self) -> usize {
        each!(self, d => d.width())
    }

    pub fn rows(&self) -> usize {
        each!(self, d => d.rows())
    }

    /// Incremented on each change of the panel image.
    pub fn generation(&self) -> u64 {
        each!(self, d => d.generation)
    }
}

impl I2cDevice for Oled {
    fn start(&mut self, read: bool) -> bool {
        each!(self, d => d.start(read))
    }

    fn write(&mut self, byte: u8) -> bool {
        each!(self, d => d.write(byte))
    }

    fn read(&mut self, ack: bool) -> u8 {
        each!(self, d => d.read(ack))
    }

    fn stop(&mut self) {
        each!(self, d => d.stop())
    }
}
