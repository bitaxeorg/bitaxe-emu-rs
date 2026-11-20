//! The register pointer of I2C devices: the first byte written after the
//! address selects a register, the following bytes of the transfer go to it.

#[derive(Default)]
pub struct Pointer {
    /// The selected register.
    pub reg: u8,
    /// The next written byte selects the register.
    expect_reg: bool,
}

impl Pointer {
    /// A transfer starts.
    pub fn start(&mut self, read: bool) {
        self.expect_reg = !read;
    }

    /// A written byte: None when it selected the register, else the register
    /// it is data for.
    pub fn write(&mut self, byte: u8) -> Option<u8> {
        if self.expect_reg {
            self.reg = byte;
            self.expect_reg = false;
            None
        } else {
            Some(self.reg)
        }
    }
}

/// 16-bit registers, most significant byte first, behind a [`Pointer`]: the
/// bytes of a write are collected until the stop, those of a read come from
/// the value latched at the start.
#[derive(Default)]
pub struct WordAccess {
    pub pointer: Pointer,
    written: Vec<u8>,
    reading: [u8; 2],
    read_pos: usize,
}

impl WordAccess {
    /// A transfer starts; `value` is the selected register's value, for a read.
    pub fn start(&mut self, read: bool, value: u16) {
        self.pointer.start(read);
        self.written.clear();
        if read {
            self.reading = value.to_be_bytes();
            self.read_pos = 0;
        }
    }

    pub fn write(&mut self, byte: u8) {
        if self.pointer.write(byte).is_some() {
            self.written.push(byte);
        }
    }

    pub fn read(&mut self) -> u8 {
        let b = self.reading.get(self.read_pos).copied().unwrap_or(0xff);
        self.read_pos += 1;
        b
    }

    /// At the stop: the register and the word written to it, if any.
    pub fn stop(&mut self) -> Option<(u8, u16)> {
        let word = (self.written.len() >= 2).then(|| {
            (
                self.pointer.reg,
                u16::from_be_bytes([self.written[0], self.written[1]]),
            )
        });
        self.written.clear();
        word
    }
}
