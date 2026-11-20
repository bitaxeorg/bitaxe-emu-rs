//! Interrupt matrix (0x600C_2000): routes the peripheral interrupt sources to
//! the external interrupts of each core.

pub const SOURCES: usize = 99;

const STATUS_0: u32 = 0x18c;
const CLOCK_GATE: u32 = 0x19c;
const DATE: u32 = 0x7fc;

/// Processor interrupts that peripheral sources can reach.
const ROUTABLE: u32 = 0b1101_1111_1111_1110_0111_0111_0011_1111;

pub struct IntMatrix {
    /// Map slots of the PRO_CPU block (0x000) and APP_CPU block (0x800).
    map: [[u8; 512]; 2],
    clock_gate: [u32; 2],
}

impl Default for IntMatrix {
    fn default() -> Self {
        Self::new()
    }
}

impl IntMatrix {
    pub fn new() -> Self {
        IntMatrix {
            map: [[6; 512]; 2],
            clock_gate: [1; 2],
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// The processor interrupt lines of `core` for the given source levels.
    pub fn lines(&self, core: usize, levels: &[bool; SOURCES]) -> u32 {
        let mut lines = 0;
        for (src, &level) in levels.iter().enumerate() {
            if level {
                let line = u32::from(self.map[core][src] & 0x1f);
                lines |= (1 << line) & ROUTABLE;
            }
        }
        lines
    }

    pub fn read(&self, off: u32, levels: &[bool; SOURCES]) -> u32 {
        let core = (off >> 11) as usize & 1;
        let o = off & 0x7ff;
        match o {
            o if (STATUS_0..STATUS_0 + 16).contains(&o) => {
                let word = ((o - STATUS_0) / 4) as usize;
                (0..32).fold(0, |acc, b| {
                    let src = word * 32 + b;
                    acc | (u32::from(src < SOURCES && levels[src]) << b)
                })
            }
            CLOCK_GATE => self.clock_gate[core],
            DATE => 0x0210_1220,
            _ => u32::from(self.map[core][(o / 4) as usize]),
        }
    }

    pub fn write(&mut self, off: u32, v: u32) {
        let core = (off >> 11) as usize & 1;
        let o = off & 0x7ff;
        match o {
            o if (STATUS_0..STATUS_0 + 16).contains(&o) => {}
            CLOCK_GATE => self.clock_gate[core] = v & 1,
            DATE => {}
            _ => self.map[core][(o / 4) as usize] = (v & 0x1f) as u8,
        }
    }
}
