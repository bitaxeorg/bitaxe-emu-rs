//! ST7789 colour LCD controller on an 8-bit parallel bus: frame memory of
//! 240 × 320 RGB565 pixels written through the column/row window, with the
//! memory access order of MADCTL, and the 170-row panel behind it.

/// Frame memory: columns and rows.
pub const MEM_COLS: usize = 240;
pub const MEM_ROWS: usize = 320;
/// The panel shows memory columns `PANEL_COL0..PANEL_COL0 + PANEL_COLS`.
pub const PANEL_COL0: usize = 35;
pub const PANEL_COLS: usize = 170;
/// The panel seen in landscape.
pub const VIEW_WIDTH: usize = MEM_ROWS;
pub const VIEW_HEIGHT: usize = PANEL_COLS;

const SWRESET: u16 = 0x01;
const SLPIN: u16 = 0x10;
const SLPOUT: u16 = 0x11;
const INVOFF: u16 = 0x20;
const INVON: u16 = 0x21;
const DISPOFF: u16 = 0x28;
const DISPON: u16 = 0x29;
const CASET: u16 = 0x2a;
const RASET: u16 = 0x2b;
const RAMWR: u16 = 0x2c;
const MADCTL: u16 = 0x36;
const COLMOD: u16 = 0x3a;
const RAMWRC: u16 = 0x3c;
const RAMCTRL: u16 = 0xb0;

/// MADCTL bits: row address order, column address order, row/column exchange.
pub const MADCTL_MY: u8 = 0x80;
pub const MADCTL_MX: u8 = 0x40;
pub const MADCTL_MV: u8 = 0x20;

pub struct St7789 {
    /// Frame memory, row-major by memory row.
    mem: Vec<u16>,
    madctl: u8,
    /// Data bytes in little-endian order (RAMCTRL).
    little_endian: bool,
    inverted: bool,
    sleeping: bool,
    display_on: bool,
    /// Address window: columns and rows in the MADCTL order.
    cols: (u16, u16),
    rows: (u16, u16),
    /// Write position in the window.
    x: u16,
    y: u16,
    /// A first byte of a pixel waiting for its second.
    half: Option<u8>,
    /// Incremented when what the panel shows may have changed.
    pub generation: u64,
}

impl Default for St7789 {
    fn default() -> Self {
        let mut lcd = St7789 {
            mem: vec![0; MEM_COLS * MEM_ROWS],
            madctl: 0,
            little_endian: false,
            inverted: false,
            sleeping: true,
            display_on: false,
            cols: (0, MEM_COLS as u16 - 1),
            rows: (0, MEM_ROWS as u16 - 1),
            x: 0,
            y: 0,
            half: None,
            generation: 0,
        };
        lcd.reset();
        lcd
    }
}

impl St7789 {
    /// Hardware or software reset: the frame memory keeps its content.
    pub fn reset(&mut self) {
        self.madctl = 0;
        self.little_endian = false;
        self.inverted = false;
        self.sleeping = true;
        self.display_on = false;
        self.cols = (0, MEM_COLS as u16 - 1);
        self.rows = (0, MEM_ROWS as u16 - 1);
        self.half = None;
        self.generation += 1;
    }

    /// The panel shows the frame memory.
    pub fn is_on(&self) -> bool {
        self.display_on && !self.sleeping
    }

    /// A transfer on the bus: command words, then data bytes.
    pub fn transfer(&mut self, cmd: &[u16], data: &[u8]) {
        let Some(&command) = cmd.first() else {
            self.pixels(data);
            return;
        };
        let command = command & 0xff;
        match command {
            SWRESET => self.reset(),
            SLPIN => self.sleeping = true,
            SLPOUT => self.sleeping = false,
            INVOFF => self.inverted = false,
            INVON => self.inverted = true,
            DISPOFF => self.display_on = false,
            DISPON => self.display_on = true,
            CASET if data.len() >= 4 => {
                self.cols = (
                    u16::from_be_bytes([data[0], data[1]]),
                    u16::from_be_bytes([data[2], data[3]]),
                );
            }
            RASET if data.len() >= 4 => {
                self.rows = (
                    u16::from_be_bytes([data[0], data[1]]),
                    u16::from_be_bytes([data[2], data[3]]),
                );
            }
            MADCTL if !data.is_empty() => self.madctl = data[0],
            COLMOD => {}
            RAMCTRL if data.len() >= 2 => self.little_endian = data[1] & 0x08 != 0,
            RAMWR => {
                self.x = self.cols.0;
                self.y = self.rows.0;
                self.half = None;
                self.pixels(data);
            }
            RAMWRC => self.pixels(data),
            _ => {}
        }
        self.generation += 1;
    }

    fn pixels(&mut self, data: &[u8]) {
        for &b in data {
            let Some(first) = self.half.take() else {
                self.half = Some(b);
                continue;
            };
            let px = if self.little_endian {
                u16::from_le_bytes([first, b])
            } else {
                u16::from_be_bytes([first, b])
            };
            if let Some(i) = self.index(self.madctl, self.x, self.y) {
                self.mem[i] = px;
            }
            if self.x >= self.cols.1 {
                self.x = self.cols.0;
                self.y = if self.y >= self.rows.1 {
                    self.rows.0
                } else {
                    self.y + 1
                };
            } else {
                self.x += 1;
            }
        }
    }

    /// Frame memory index of window position (`x`, `y`) in the access order of
    /// `madctl`.
    fn index(&self, madctl: u8, x: u16, y: u16) -> Option<usize> {
        let (x, y) = (usize::from(x), usize::from(y));
        let (mut col, mut row) = if madctl & MADCTL_MV != 0 {
            (y, x)
        } else {
            (x, y)
        };
        if col >= MEM_COLS || row >= MEM_ROWS {
            return None;
        }
        if madctl & MADCTL_MX != 0 {
            col = MEM_COLS - 1 - col;
        }
        if madctl & MADCTL_MY != 0 {
            row = MEM_ROWS - 1 - row;
        }
        Some(row * MEM_COLS + col)
    }

    /// What the panel shows in landscape, RGBA, [`VIEW_WIDTH`] × [`VIEW_HEIGHT`].
    /// `upright` is the MADCTL access order in which the image reads upright on
    /// the board. The panel itself inverts colours, so INVON shows them true.
    pub fn view(&self, upright: u8) -> Vec<u8> {
        let mut rgba = Vec::with_capacity(VIEW_WIDTH * VIEW_HEIGHT * 4);
        for y in 0..VIEW_HEIGHT {
            for x in 0..VIEW_WIDTH {
                let px = self
                    .index(upright, x as u16, (PANEL_COL0 + y) as u16)
                    .map_or(0, |i| self.mem[i]);
                let px = if self.inverted { px } else { !px };
                let r = ((px >> 11) & 0x1f) as u8;
                let g = ((px >> 5) & 0x3f) as u8;
                let b = (px & 0x1f) as u8;
                rgba.extend([r << 3 | r >> 2, g << 2 | g >> 4, b << 3 | b >> 2, 0xff]);
            }
        }
        rgba
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landscape_window_lands_on_the_panel() {
        let mut lcd = St7789::default();
        lcd.transfer(&[SLPOUT], &[]);
        lcd.transfer(&[INVON], &[]);
        lcd.transfer(&[MADCTL], &[MADCTL_MV | MADCTL_MY]);
        lcd.transfer(&[CASET], &[0, 0, 0x01, 0x3f]);
        lcd.transfer(&[RASET], &[0, 35, 0, 35]);
        // First row of the panel: red then green.
        let mut row = vec![0xf8, 0x00, 0x07, 0xe0];
        row.resize(320 * 2, 0);
        lcd.transfer(&[RAMWR], &row);
        lcd.transfer(&[DISPON], &[]);
        assert!(lcd.is_on());
        let view = lcd.view(MADCTL_MV | MADCTL_MY);
        assert_eq!(&view[0..4], &[0xff, 0, 0, 0xff]);
        assert_eq!(&view[4..8], &[0, 0xff, 0, 0xff]);
        // Seen from the other side it is the bottom right corner.
        let flipped = lcd.view(MADCTL_MV | MADCTL_MX);
        let last = (VIEW_WIDTH * VIEW_HEIGHT - 1) * 4;
        assert_eq!(&flipped[last..last + 4], &[0xff, 0, 0, 0xff]);
    }
}
