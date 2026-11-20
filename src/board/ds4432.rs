//! DS4432 dual current DAC (0x48): output 0 sources or sinks a current into the
//! feedback node of the core regulator, moving its output voltage.

use esp32s3_emu::devices::i2c::I2cDevice;

use super::pointer::Pointer;

const OUT0: u8 = 0xf8;
const OUT1: u8 = 0xf9;

#[derive(Default)]
pub struct Ds4432 {
    /// The output registers: bit 7 sources (1) or sinks (0), bits 6:0 the code.
    pub out: [u8; 2],
    pointer: Pointer,
}

impl Ds4432 {
    /// Current of `output` with its full-scale resistor `rfs_ohm`, in amperes:
    /// positive when it sources into the node.
    pub fn current(&self, output: usize, rfs_ohm: f32) -> f32 {
        let v = self.out[output];
        // Full scale (code 127) is 0.997 V / RFS * 127 / 16.
        let magnitude = f32::from(v & 0x7f) * 0.997 / 16.0 / rfs_ohm;
        if v & 0x80 != 0 { magnitude } else { -magnitude }
    }
}

impl I2cDevice for Ds4432 {
    fn start(&mut self, read: bool) -> bool {
        self.pointer.start(read);
        true
    }

    fn write(&mut self, byte: u8) -> bool {
        match self.pointer.write(byte) {
            None => matches!(byte, OUT0 | OUT1),
            Some(reg) => {
                self.out[usize::from(reg - OUT0)] = byte;
                true
            }
        }
    }

    fn read(&mut self, _ack: bool) -> u8 {
        match self.pointer.reg {
            OUT0 | OUT1 => self.out[usize::from(self.pointer.reg - OUT0)],
            _ => 0xff,
        }
    }
}
