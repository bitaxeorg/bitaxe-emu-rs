//! Cache controller (EXTMEM, 0x600C_4000) and the MMU table (0x600C_5000) that
//! maps the external memory windows to flash pages.
//!
//! The windows read flash directly, so they are always coherent with flash
//! writes: the sync, preload, autoload, lock and freeze operations complete as
//! soon as they are started.

pub const MMU_ENTRIES: usize = 512;
const MMU_OFFSET: u32 = 0x1000;
const MMU_INVALID: u32 = 1 << 14;

/// Control registers whose ENA bits (mask) complete into a DONE bit.
const OPERATIONS: [(u32, u32, u32); 10] = [
    (0x01c, 0b011, 1 << 2),  // DCACHE_LOCK_CTRL
    (0x028, 0b111, 1 << 3),  // DCACHE_SYNC_CTRL
    (0x040, 1 << 0, 1 << 1), // DCACHE_PRELOAD_CTRL
    (0x04c, 1 << 2, 1 << 3), // DCACHE_AUTOLOAD_CTRL
    (0x07c, 0b011, 1 << 2),  // ICACHE_LOCK_CTRL
    (0x088, 1 << 0, 1 << 1), // ICACHE_SYNC_CTRL
    (0x094, 1 << 0, 1 << 1), // ICACHE_PRELOAD_CTRL
    (0x0a0, 1 << 2, 1 << 3), // ICACHE_AUTOLOAD_CTRL
    (0x150, 1 << 0, 1 << 2), // DCACHE_FREEZE
    (0x154, 1 << 0, 1 << 2), // ICACHE_FREEZE
];
const CACHE_STATE: u32 = 0x130;
const DATE: u32 = 0x3fc;

pub struct Cache {
    pub mmu: [u32; MMU_ENTRIES],
    regs: [u32; 0x400],
}

impl Default for Cache {
    fn default() -> Self {
        Self::new()
    }
}

impl Cache {
    pub fn new() -> Self {
        let mut c = Cache {
            mmu: [MMU_INVALID; MMU_ENTRIES],
            regs: [0; 0x400],
        };
        c.reset();
        c
    }

    pub fn reset(&mut self) {
        for e in &mut self.mmu {
            *e |= MMU_INVALID;
        }
        self.regs = [0; 0x400];
        self.regs[(0x040 / 4) as usize] = 2;
        self.regs[(0x094 / 4) as usize] = 2;
        self.regs[(0x04c / 4) as usize] = 8;
        self.regs[(0x0a0 / 4) as usize] = 8;
    }

    pub fn read(&self, off: u32) -> u32 {
        if (MMU_OFFSET..MMU_OFFSET + 4 * MMU_ENTRIES as u32).contains(&off) {
            return self.mmu[((off - MMU_OFFSET) / 4) as usize];
        }
        match off {
            CACHE_STATE => 0x0000_1001,
            DATE => 0x0210_1220,
            o if o < 0x1000 => self.regs[(o / 4) as usize],
            _ => 0,
        }
    }

    pub fn write(&mut self, off: u32, v: u32) {
        if (MMU_OFFSET..MMU_OFFSET + 4 * MMU_ENTRIES as u32).contains(&off) {
            self.mmu[((off - MMU_OFFSET) / 4) as usize] = v & 0xffff;
            return;
        }
        if off >= 0x1000 || off == DATE {
            return;
        }
        let mut v = v;
        if let Some(&(_, ena, done)) = OPERATIONS.iter().find(|(o, _, _)| *o == off) {
            let freeze = off == 0x150 || off == 0x154;
            if freeze {
                // DONE follows FREEZE_ENA.
                v = if v & ena != 0 { v | done } else { v & !done };
            } else if v & ena != 0 {
                v = (v & !ena) | done;
            }
        }
        self.regs[(off / 4) as usize] = v;
    }
}
