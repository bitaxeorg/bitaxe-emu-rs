//! SSD1306 OLED controller on I2C: command and data streams, the 128×64 display
//! RAM with its three addressing modes, and the panel image (128×`rows`).

use esp32s3_rax::devices::i2c::I2cDevice;

pub const WIDTH: usize = 128;
const PAGES: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Addressing {
    Horizontal,
    Vertical,
    Page,
}

pub struct Ssd1306 {
    ram: [[u8; WIDTH]; PAGES],
    rows: usize,
    on: bool,
    invert: bool,
    entire_on: bool,
    contrast: u8,
    seg_remap: bool,
    com_reverse: bool,
    start_line: usize,
    offset: usize,
    mux: usize,
    mode: Addressing,
    col: usize,
    page: usize,
    col_range: (usize, usize),
    page_range: (usize, usize),
    // I2C stream state.
    first: bool,
    data: bool,
    continuation: bool,
    cmd: Vec<u8>,
    /// Incremented on each change of the panel image.
    pub generation: u64,
}

impl Ssd1306 {
    pub fn new(rows: usize) -> Self {
        Ssd1306 {
            ram: [[0; WIDTH]; PAGES],
            rows,
            on: false,
            invert: false,
            entire_on: false,
            contrast: 0x7f,
            seg_remap: false,
            com_reverse: false,
            start_line: 0,
            offset: 0,
            mux: 63,
            mode: Addressing::Page,
            col: 0,
            page: 0,
            col_range: (0, WIDTH - 1),
            page_range: (0, PAGES - 1),
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

    /// The panel's rows.
    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn width(&self) -> usize {
        WIDTH
    }

    /// The panel image, row-major, one byte per pixel (0 or 1).
    pub fn pixels(&self) -> Vec<u8> {
        let mut out = vec![0u8; WIDTH * self.rows];
        if !self.on {
            return out;
        }
        let lines = self.mux + 1;
        for (row, line) in out.chunks_mut(WIDTH).enumerate() {
            // Row `row` of the panel is driven by COM `row` (or reversed).
            let com = if self.com_reverse {
                lines.saturating_sub(1 + row)
            } else {
                row
            };
            if com >= lines {
                continue;
            }
            let ram_row = (com + self.start_line + self.offset) % 64;
            for (x, px) in line.iter_mut().enumerate() {
                let seg = if self.seg_remap { WIDTH - 1 - x } else { x };
                let lit = self.entire_on || (self.ram[ram_row / 8][seg] >> (ram_row % 8)) & 1 != 0;
                *px = u8::from(lit != self.invert);
            }
        }
        out
    }

    fn write_data(&mut self, byte: u8) {
        self.ram[self.page][self.col] = byte;
        self.generation += 1;
        match self.mode {
            Addressing::Horizontal => {
                if self.col >= self.col_range.1 {
                    self.col = self.col_range.0;
                    self.page = if self.page >= self.page_range.1 {
                        self.page_range.0
                    } else {
                        self.page + 1
                    };
                } else {
                    self.col += 1;
                }
            }
            Addressing::Vertical => {
                if self.page >= self.page_range.1 {
                    self.page = self.page_range.0;
                    self.col = if self.col >= self.col_range.1 {
                        self.col_range.0
                    } else {
                        self.col + 1
                    };
                } else {
                    self.page += 1;
                }
            }
            Addressing::Page => {
                if self.col < WIDTH - 1 {
                    self.col += 1;
                }
            }
        }
    }

    /// Bytes of the command starting with `first`, including it.
    fn command_len(first: u8) -> usize {
        match first {
            0x81 | 0x8d | 0xa8 | 0xd3 | 0xd5 | 0xd9 | 0xda | 0xdb | 0x20 | 0xa3 => 2,
            0x21 | 0x22 => 3,
            0x26 | 0x27 => 7,
            0x29 | 0x2a => 6,
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
            0x81 => self.contrast = c[1],
            0xa4 => self.entire_on = false,
            0xa5 => self.entire_on = true,
            0xa6 => self.invert = false,
            0xa7 => self.invert = true,
            0xae => self.on = false,
            0xaf => self.on = true,
            0x20 => {
                self.mode = match c[1] & 3 {
                    0 => Addressing::Horizontal,
                    1 => Addressing::Vertical,
                    _ => Addressing::Page,
                }
            }
            0x21 => {
                self.col_range = (usize::from(c[1] & 0x7f), usize::from(c[2] & 0x7f));
                self.col = self.col_range.0;
            }
            0x22 => {
                self.page_range = (usize::from(c[1] & 7), usize::from(c[2] & 7));
                self.page = self.page_range.0;
            }
            0x00..=0x0f => self.col = (self.col & 0xf0) | usize::from(c[0] & 0x0f),
            0x10..=0x1f => self.col = ((usize::from(c[0] & 0x0f)) << 4 | (self.col & 0x0f)) & 0x7f,
            0xb0..=0xb7 => self.page = usize::from(c[0] & 7),
            0x40..=0x7f => self.start_line = usize::from(c[0] & 0x3f),
            0xa0 => self.seg_remap = false,
            0xa1 => self.seg_remap = true,
            0xc0 => self.com_reverse = false,
            0xc8 => self.com_reverse = true,
            0xa8 => self.mux = usize::from(c[1] & 0x3f).max(15),
            0xd3 => self.offset = usize::from(c[1] & 0x3f),
            _ => {}
        }
    }
}

impl I2cDevice for Ssd1306 {
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
            // With Co set, a control byte follows each byte.
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
