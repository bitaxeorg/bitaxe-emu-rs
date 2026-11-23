//! SH1107 OLED controller on I2C: command and data streams, the 128 × 128 display RAM (16 pages of
//! 8 rows by 128 columns) in page or vertical addressing, and the panel image as the RAM is laid
//! out, column `x` and page bit `y` (a byte is 8 rows of a column). How a panel is wired to the
//! commons (its display offset, start line and the commons it uses) is not modelled; segment
//! remap and common scan direction mirror the image across the panel.

use esp32s3_rax::devices::i2c::I2cDevice;

const COLUMNS: usize = 128;
const PAGES: usize = 16;

pub struct Sh1107 {
    ram: [[u8; COLUMNS]; PAGES],
    width: usize,
    height: usize,
    on: bool,
    invert: bool,
    entire_on: bool,
    contrast: u8,
    seg_remap: bool,
    com_reverse: bool,
    vertical: bool,
    col: usize,
    page: usize,
    // I2C stream state.
    first: bool,
    data: bool,
    continuation: bool,
    cmd: Vec<u8>,
    /// Incremented on each change of the panel image.
    pub generation: u64,
}

impl Sh1107 {
    /// A panel of `width` × `height` pixels (64 × 128 or 128 × 128).
    pub fn new(width: usize, height: usize) -> Self {
        Sh1107 {
            ram: [[0; COLUMNS]; PAGES],
            width: width.min(COLUMNS),
            height: height.min(PAGES * 8),
            on: false,
            invert: false,
            entire_on: false,
            contrast: 0x80,
            seg_remap: false,
            com_reverse: false,
            vertical: false,
            col: 0,
            page: 0,
            first: true,
            data: false,
            continuation: false,
            cmd: Vec::new(),
            generation: 0,
        }
    }

    pub fn is_on(&self) -> bool {
        self.on
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn rows(&self) -> usize {
        self.height
    }

    /// The panel image, row-major, one byte per pixel (0 or 1).
    pub fn pixels(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.width * self.height];
        if !self.on {
            return out;
        }
        for (y, line) in out.chunks_mut(self.width).enumerate() {
            let row = if self.com_reverse {
                self.height - 1 - y
            } else {
                y
            };
            for (x, px) in line.iter_mut().enumerate() {
                let col = if self.seg_remap {
                    self.width - 1 - x
                } else {
                    x
                };
                let lit = self.entire_on || (self.ram[row / 8][col] >> (row % 8)) & 1 != 0;
                *px = u8::from(lit != self.invert);
            }
        }
        out
    }

    fn write_data(&mut self, byte: u8) {
        self.ram[self.page][self.col] = byte;
        self.generation += 1;
        if self.vertical {
            self.page = (self.page + 1) % PAGES;
        } else {
            self.col = (self.col + 1) % COLUMNS;
        }
    }

    /// Bytes of the command starting with `first`, including it.
    fn command_len(first: u8) -> usize {
        match first {
            0x81 | 0xa8 | 0xad | 0xd3 | 0xd5 | 0xd9 | 0xdb | 0xdc => 2,
            _ => 1,
        }
    }

    fn command(&mut self, byte: u8) {
        self.cmd.push(byte);
        if self.cmd.len() < Self::command_len(self.cmd[0]) {
            return;
        }
        let c = std::mem::take(&mut self.cmd);
        self.generation += 1;
        match c[0] {
            0x00..=0x0f => self.col = (self.col & 0x70) | usize::from(c[0] & 0x0f),
            0x10..=0x17 => self.col = usize::from(c[0] & 0x07) << 4 | (self.col & 0x0f),
            0x20 => self.vertical = false,
            0x21 => self.vertical = true,
            0x81 => self.contrast = c[1],
            0xa0 => self.seg_remap = false,
            0xa1 => self.seg_remap = true,
            0xa4 => self.entire_on = false,
            0xa5 => self.entire_on = true,
            0xa6 => self.invert = false,
            0xa7 => self.invert = true,
            0xae => self.on = false,
            0xaf => self.on = true,
            0xb0..=0xbf => self.page = usize::from(c[0] & 0x0f),
            0xc0..=0xcf => self.com_reverse = c[0] & 0x08 != 0,
            _ => {}
        }
    }
}

impl I2cDevice for Sh1107 {
    fn start(&mut self, _read: bool) -> bool {
        self.first = true;
        true
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.first {
            // Control byte: Co (bit 7), D/C# (bit 6).
            self.first = false;
            self.continuation = byte & 0x80 != 0;
            self.data = byte & 0x40 != 0;
            return true;
        }
        if self.data {
            self.write_data(byte);
        } else {
            self.command(byte);
        }
        if self.continuation {
            self.first = true;
        }
        true
    }

    fn read(&mut self, _ack: bool) -> u8 {
        // Status: display off flag in bit 6.
        if self.on { 0x00 } else { 0x40 }
    }

    fn stop(&mut self) {
        self.cmd.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn send(d: &mut Sh1107, control: u8, bytes: &[u8]) {
        d.start(false);
        d.write(control);
        for &b in bytes {
            d.write(b);
        }
        d.stop();
    }

    #[test]
    fn a_page_of_data_lights_its_rows() {
        let mut d = Sh1107::new(64, 128);
        send(&mut d, 0x00, &[0xaf, 0x10, 0x02, 0xb3]);
        send(&mut d, 0x40, &[0x01, 0x80]);
        let px = d.pixels();
        // page 3, columns 2 and 3: row 24 of column 2, row 31 of column 3
        assert_eq!(px[24 * 64 + 2], 1);
        assert_eq!(px[31 * 64 + 3], 1);
        assert_eq!(px.iter().filter(|&&p| p != 0).count(), 2);
    }
}
