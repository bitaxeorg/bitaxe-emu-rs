//! System timer (0x6002_3000): two 52-bit units at 16 MHz and three comparators.

use super::TICK_HZ;

const CONF: u32 = 0x00;
const UNIT0_OP: u32 = 0x04;
const INT_ENA: u32 = 0x64;
const INT_RAW: u32 = 0x68;
const INT_CLR: u32 = 0x6c;
const INT_ST: u32 = 0x70;
const DATE: u32 = 0xfc;

const RATE: u64 = 16_000_000;
const TICKS_PER_COUNT: u64 = TICK_HZ / RATE;
const MASK52: u64 = (1 << 52) - 1;

#[derive(Default, Clone, Copy)]
struct Unit {
    base_value: u64,
    base_time: u64,
    load: u64,
    latched: u64,
}

#[derive(Default, Clone, Copy)]
struct Comparator {
    target: u64,
    pending_target: u64,
    period: u32,
    pending_period: u32,
    conf: u32,
    /// Next time it fires.
    deadline: Option<u64>,
}

pub struct Systimer {
    conf: u32,
    units: [Unit; 2],
    comps: [Comparator; 3],
    int_ena: u32,
    int_raw: u32,
}

impl Default for Systimer {
    fn default() -> Self {
        Self::new()
    }
}

impl Systimer {
    pub fn new() -> Self {
        Systimer {
            conf: 0x4600_0000,
            units: [Unit::default(); 2],
            comps: [Comparator::default(); 3],
            int_ena: 0,
            int_raw: 0,
        }
    }

    pub fn reset(&mut self) {
        self.conf = 0x4600_0000;
        self.comps = [Comparator::default(); 3];
        self.int_ena = 0;
        self.int_raw = 0;
    }

    fn unit_enabled(&self, u: usize) -> bool {
        self.conf & (1 << (30 - u)) != 0
    }

    fn comp_enabled(&self, c: usize) -> bool {
        self.conf & (1 << (24 - c)) != 0
    }

    fn value(&self, u: usize, now: u64) -> u64 {
        let unit = &self.units[u];
        if !self.unit_enabled(u) {
            return unit.base_value;
        }
        (unit.base_value + (now - unit.base_time) / TICKS_PER_COUNT) & MASK52
    }

    /// Re-anchors unit `u` at `now`, keeping its value.
    fn rebase(&mut self, u: usize, now: u64) {
        let v = self.value(u, now);
        let unit = &mut self.units[u];
        unit.base_value = v;
        unit.base_time = now - (now - unit.base_time) % TICKS_PER_COUNT.max(1);
    }

    /// Time the unit of comparator `c` reaches its target.
    fn schedule(&mut self, c: usize, now: u64) {
        let comp = self.comps[c];
        let u = (comp.conf >> 31) as usize;
        if !self.comp_enabled(c) || !self.unit_enabled(u) {
            self.comps[c].deadline = None;
            return;
        }
        let period_mode = comp.conf & (1 << 30) != 0;
        let unit = self.units[u];
        let deadline = if period_mode {
            now + u64::from(comp.period.max(1)) * TICKS_PER_COUNT
        } else {
            let count = self.value(u, now);
            let d = comp.target.wrapping_sub(count) & MASK52;
            if d == 0 || d > (1 << 51) {
                now
            } else {
                // The count reaches the target at a whole count after the unit's anchor.
                let elapsed = (now - unit.base_time) / TICKS_PER_COUNT;
                unit.base_time + (elapsed + d) * TICKS_PER_COUNT
            }
        };
        self.comps[c].deadline = Some(deadline);
    }

    pub fn irqs(&self) -> [bool; 3] {
        let st = self.int_raw & self.int_ena;
        [st & 1 != 0, st & 2 != 0, st & 4 != 0]
    }

    pub fn next_event(&self) -> Option<u64> {
        self.comps.iter().filter_map(|c| c.deadline).min()
    }

    pub fn update(&mut self, now: u64) -> bool {
        let mut changed = false;
        for c in 0..3 {
            if let Some(t) = self.comps[c].deadline
                && t <= now
            {
                self.int_raw |= 1 << c;
                changed = true;
                if self.comps[c].conf & (1 << 30) != 0 {
                    let period = u64::from(self.comps[c].period.max(1)) * TICKS_PER_COUNT;
                    self.comps[c].deadline = Some(t + period);
                } else {
                    self.comps[c].deadline = None;
                }
            }
        }
        changed
    }

    pub fn read(&self, off: u32) -> u32 {
        match off {
            CONF => self.conf,
            UNIT0_OP | 0x08 => 1 << 29,
            0x0c => (self.units[0].load >> 32) as u32,
            0x10 => self.units[0].load as u32,
            0x14 => (self.units[1].load >> 32) as u32,
            0x18 => self.units[1].load as u32,
            0x1c | 0x24 | 0x2c => {
                (self.comps[((off - 0x1c) / 8) as usize].pending_target >> 32) as u32
            }
            0x20 | 0x28 | 0x30 => self.comps[((off - 0x20) / 8) as usize].pending_target as u32,
            0x34 | 0x38 | 0x3c => {
                let c = &self.comps[((off - 0x34) / 4) as usize];
                (c.conf & 0xc000_0000) | c.period
            }
            0x40 => (self.units[0].latched >> 32) as u32,
            0x44 => self.units[0].latched as u32,
            0x48 => (self.units[1].latched >> 32) as u32,
            0x4c => self.units[1].latched as u32,
            INT_ENA => self.int_ena,
            INT_RAW => self.int_raw,
            INT_ST => self.int_raw & self.int_ena,
            0x74 | 0x7c | 0x84 => self.comps[((off - 0x74) / 8) as usize].target as u32,
            0x78 | 0x80 | 0x88 => (self.comps[((off - 0x78) / 8) as usize].target >> 32) as u32,
            DATE => 0x0201_2251,
            _ => 0,
        }
    }

    /// Returns whether the interrupt state may have changed.
    pub fn write(&mut self, off: u32, v: u32, now: u64) -> bool {
        match off {
            CONF => {
                for u in 0..2 {
                    self.rebase(u, now);
                }
                let old = self.conf;
                self.conf = v;
                for (u, unit) in self.units.iter_mut().enumerate() {
                    if (old ^ v) & (1 << (30 - u)) != 0 {
                        unit.base_time = now;
                    }
                }
                for c in 0..3 {
                    self.schedule(c, now);
                }
            }
            UNIT0_OP | 0x08 => {
                let u = ((off - UNIT0_OP) / 4) as usize;
                if v & (1 << 30) != 0 {
                    self.units[u].latched = self.value(u, now);
                }
                return false;
            }
            0x0c => {
                self.units[0].load =
                    (self.units[0].load & 0xffff_ffff) | (u64::from(v & 0xf_ffff) << 32)
            }
            0x10 => self.units[0].load = (self.units[0].load & !0xffff_ffff) | u64::from(v),
            0x14 => {
                self.units[1].load =
                    (self.units[1].load & 0xffff_ffff) | (u64::from(v & 0xf_ffff) << 32)
            }
            0x18 => self.units[1].load = (self.units[1].load & !0xffff_ffff) | u64::from(v),
            0x1c | 0x24 | 0x2c => {
                let c = &mut self.comps[((off - 0x1c) / 8) as usize];
                c.pending_target =
                    (c.pending_target & 0xffff_ffff) | (u64::from(v & 0xf_ffff) << 32);
            }
            0x20 | 0x28 | 0x30 => {
                let c = &mut self.comps[((off - 0x20) / 8) as usize];
                c.pending_target = (c.pending_target & !0xffff_ffff) | u64::from(v);
            }
            0x34 | 0x38 | 0x3c => {
                let i = ((off - 0x34) / 4) as usize;
                self.comps[i].conf = v & 0xc000_0000;
                self.comps[i].pending_period = v & 0x3ff_ffff;
            }
            0x50 | 0x54 | 0x58 => {
                let i = ((off - 0x50) / 4) as usize;
                if v & 1 != 0 {
                    self.comps[i].target = self.comps[i].pending_target;
                    self.comps[i].period = self.comps[i].pending_period;
                    self.schedule(i, now);
                }
            }
            0x5c | 0x60 => {
                let u = ((off - 0x5c) / 4) as usize;
                self.units[u].base_value = self.units[u].load;
                self.units[u].base_time = now;
                for c in 0..3 {
                    self.schedule(c, now);
                }
            }
            INT_ENA => self.int_ena = v & 7,
            INT_CLR => self.int_raw &= !(v & 7),
            _ => return false,
        }
        true
    }
}
