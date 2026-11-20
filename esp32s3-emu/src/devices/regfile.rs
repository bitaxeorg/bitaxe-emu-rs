//! Register file for peripherals whose registers only need to read back what
//! was written, with fixed values for chosen registers.

use std::collections::BTreeMap;

pub struct RegFile {
    pub name: &'static str,
    regs: Vec<u32>,
    reset: BTreeMap<u32, u32>,
    fixed: BTreeMap<u32, u32>,
    /// Bits that always read as 0 (self-clearing or busy bits), per register.
    clear_on_read: BTreeMap<u32, u32>,
    /// Bits that always read as 1 (status bits of hardware that is always ready).
    set_on_read: BTreeMap<u32, u32>,
    /// Writes are dropped: registers read 0 apart from the set and fixed bits.
    write_only: bool,
    /// Each read of a register alternates between 0 and all ones (per register).
    toggle: Option<Vec<bool>>,
}

impl RegFile {
    pub fn new(name: &'static str, size: u32) -> Self {
        RegFile {
            name,
            regs: vec![0; (size / 4) as usize],
            reset: BTreeMap::new(),
            fixed: BTreeMap::new(),
            clear_on_read: BTreeMap::new(),
            set_on_read: BTreeMap::new(),
            write_only: false,
            toggle: None,
        }
    }

    /// Reads alternate between 0 and all ones, so that any poll of a status bit
    /// of hardware that is not modelled finishes. Writes are dropped.
    pub fn toggling(mut self) -> Self {
        self.toggle = Some(vec![false; self.regs.len()]);
        self.write_only = true;
        self
    }

    /// Writes are dropped and registers read 0, apart from fixed and set bits.
    pub fn write_only(mut self) -> Self {
        self.write_only = true;
        self
    }

    /// Value of `off` at reset.
    pub fn with_reset(mut self, off: u32, v: u32) -> Self {
        self.reset.insert(off, v);
        self.set(off, v);
        self
    }

    /// `off` always reads `v`.
    pub fn with_fixed(mut self, off: u32, v: u32) -> Self {
        self.fixed.insert(off, v);
        self
    }

    /// `mask` bits of `off` read as 0 whatever was written.
    pub fn with_cleared_bits(mut self, off: u32, mask: u32) -> Self {
        self.clear_on_read.insert(off, mask);
        self
    }

    /// `mask` bits of `off` read as 1 whatever was written.
    pub fn with_set_bits(mut self, off: u32, mask: u32) -> Self {
        self.set_on_read.insert(off, mask);
        self
    }

    pub fn reset(&mut self) {
        self.regs.iter_mut().for_each(|r| *r = 0);
        for (&off, &v) in &self.reset.clone() {
            self.set(off, v);
        }
    }

    pub fn get(&self, off: u32) -> u32 {
        self.regs.get((off / 4) as usize).copied().unwrap_or(0)
    }

    pub fn set(&mut self, off: u32, v: u32) {
        if let Some(r) = self.regs.get_mut((off / 4) as usize) {
            *r = v;
        }
    }

    pub fn contains(&self, off: u32) -> bool {
        (off as usize) < self.regs.len() * 4
    }

    pub fn read(&mut self, off: u32) -> u32 {
        if let Some(&v) = self.fixed.get(&off) {
            return v;
        }
        if let Some(t) = self.toggle.as_mut()
            && let Some(state) = t.get_mut((off / 4) as usize)
        {
            *state = !*state;
            return if *state { u32::MAX } else { 0 };
        }
        let v = self.get(off) | self.set_on_read.get(&off).copied().unwrap_or(0);
        match self.clear_on_read.get(&off) {
            Some(&m) => v & !m,
            None => v,
        }
    }

    pub fn write(&mut self, off: u32, v: u32) {
        if self.write_only {
            return;
        }
        self.set(off, v);
    }
}
