//! GPIO (0x6000_4000) and IO_MUX (0x6000_9000): pin levels, output enables,
//! pin interrupts, the strapping register, and the board side of the pins.

pub const PINS: usize = 49;

const OUT: u32 = 0x04;
const OUT_W1TS: u32 = 0x08;
const OUT_W1TC: u32 = 0x0c;
const OUT1: u32 = 0x10;
const OUT1_W1TS: u32 = 0x14;
const OUT1_W1TC: u32 = 0x18;
const ENABLE: u32 = 0x20;
const ENABLE_W1TS: u32 = 0x24;
const ENABLE_W1TC: u32 = 0x28;
const ENABLE1: u32 = 0x2c;
const ENABLE1_W1TS: u32 = 0x30;
const ENABLE1_W1TC: u32 = 0x34;
const STRAP: u32 = 0x38;
const IN: u32 = 0x3c;
const IN1: u32 = 0x40;
const STATUS: u32 = 0x44;
const STATUS_W1TS: u32 = 0x48;
const STATUS_W1TC: u32 = 0x4c;
const STATUS1: u32 = 0x50;
const STATUS1_W1TS: u32 = 0x54;
const STATUS1_W1TC: u32 = 0x58;
const PCPU_INT: u32 = 0x5c;
const PCPU_INT1: u32 = 0x68;
const PIN0: u32 = 0x74;
const DATE: u32 = 0x6fc;

/// IO_MUX pin register fields.
const IOMUX_FUN_WPD: u32 = 1 << 7;
const IOMUX_FUN_WPU: u32 = 1 << 8;

/// Interrupt enable bits of PRO_CPU in a PINn register (INT_ENA field, bits 17:13):
/// field bit 0, or bit 2 of the dual-core layout.
const PIN_INT_ENA_PRO: u32 = (1 << 13) | (1 << 15);

pub struct Gpio {
    regs: Vec<u32>,
    iomux: Vec<u32>,
    out: u64,
    enable: u64,
    status: u64,
    /// Levels driven by the board, per pin (None: floating, the pull decides).
    pub drive: [Option<bool>; PINS],
    /// Strapping pins latched at reset (GPIO_STRAP).
    pub strap: u32,
    last_in: u64,
}

impl Gpio {
    pub fn new(strap: u32) -> Self {
        let mut g = Gpio {
            regs: vec![0; 0x700 / 4],
            iomux: vec![0; 0x100 / 4],
            out: 0,
            enable: 0,
            status: 0,
            drive: [None; PINS],
            strap,
            last_in: 0,
        };
        g.reset();
        g
    }

    pub fn reset(&mut self) {
        self.regs.iter_mut().for_each(|r| *r = 0);
        self.regs[(DATE / 4) as usize] = 0x0190_7040;
        self.iomux.iter_mut().for_each(|r| *r = 0x0000_0b00);
        self.out = 0;
        self.enable = 0;
        self.status = 0;
        self.last_in = self.inputs();
    }

    /// The level of every pin: the output where enabled, else the board, else the pull.
    pub fn inputs(&self) -> u64 {
        let mut levels = 0u64;
        for pin in 0..PINS {
            let level = if self.enable >> pin & 1 != 0 {
                self.out >> pin & 1 != 0
            } else if let Some(l) = self.drive[pin] {
                l
            } else {
                let mux = self.iomux.get(pin + 1).copied().unwrap_or(0);
                mux & IOMUX_FUN_WPU != 0 && mux & IOMUX_FUN_WPD == 0
            };
            levels |= u64::from(level) << pin;
        }
        levels
    }

    /// The level the chip drives on `pin`, if its output is enabled.
    pub fn output(&self, pin: usize) -> Option<bool> {
        (self.enable >> pin & 1 != 0).then_some(self.out >> pin & 1 != 0)
    }

    /// The board drives `pin` (None releases it). Returns whether an interrupt was latched.
    pub fn set_input(&mut self, pin: usize, level: Option<bool>) -> bool {
        self.drive[pin] = level;
        self.latch()
    }

    /// Latches pin interrupts from level changes. Returns whether any was latched.
    fn latch(&mut self) -> bool {
        let now = self.inputs();
        let changed = now ^ self.last_in;
        let mut latched = false;
        for pin in 0..PINS {
            let cfg = self.regs[((PIN0 / 4) as usize) + pin];
            let ty = (cfg >> 7) & 7;
            let level = now >> pin & 1 != 0;
            let edge = changed >> pin & 1 != 0;
            let hit = match ty {
                1 => edge && level,
                2 => edge && !level,
                3 => edge,
                4 => !level,
                5 => level,
                _ => false,
            };
            if hit && self.status >> pin & 1 == 0 {
                self.status |= 1 << pin;
                latched = true;
            }
        }
        self.last_in = now;
        latched
    }

    fn pro_int(&self) -> u64 {
        let mut m = 0;
        for pin in 0..PINS {
            if self.regs[((PIN0 / 4) as usize) + pin] & PIN_INT_ENA_PRO != 0 {
                m |= 1 << pin;
            }
        }
        self.status & m
    }

    pub fn irq(&self) -> bool {
        self.pro_int() != 0
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            OUT => self.out as u32,
            OUT1 => (self.out >> 32) as u32,
            ENABLE => self.enable as u32,
            ENABLE1 => (self.enable >> 32) as u32,
            STRAP => self.strap,
            IN => self.inputs() as u32,
            IN1 => (self.inputs() >> 32) as u32,
            STATUS => self.status as u32,
            STATUS1 => (self.status >> 32) as u32,
            PCPU_INT => self.pro_int() as u32,
            PCPU_INT1 => (self.pro_int() >> 32) as u32,
            o if (o as usize) < self.regs.len() * 4 => self.regs[(o / 4) as usize],
            _ => 0,
        }
    }

    /// Returns whether the interrupt state may have changed.
    pub fn write(&mut self, off: u32, v: u32) -> bool {
        let lo = u64::from(v);
        let hi = u64::from(v) << 32;
        match off {
            OUT => self.out = (self.out & !0xffff_ffff) | lo,
            OUT_W1TS => self.out |= lo,
            OUT_W1TC => self.out &= !lo,
            OUT1 => self.out = (self.out & 0xffff_ffff) | hi,
            OUT1_W1TS => self.out |= hi,
            OUT1_W1TC => self.out &= !hi,
            ENABLE => self.enable = (self.enable & !0xffff_ffff) | lo,
            ENABLE_W1TS => self.enable |= lo,
            ENABLE_W1TC => self.enable &= !lo,
            ENABLE1 => self.enable = (self.enable & 0xffff_ffff) | hi,
            ENABLE1_W1TS => self.enable |= hi,
            ENABLE1_W1TC => self.enable &= !hi,
            STATUS => self.status = (self.status & !0xffff_ffff) | lo,
            STATUS_W1TS => self.status |= lo,
            STATUS_W1TC => self.status &= !lo,
            STATUS1 => self.status = (self.status & 0xffff_ffff) | hi,
            STATUS1_W1TS => self.status |= hi,
            STATUS1_W1TC => self.status &= !hi,
            STRAP | IN | IN1 | PCPU_INT | PCPU_INT1 => return false,
            o if (o as usize) < self.regs.len() * 4 => self.regs[(o / 4) as usize] = v,
            _ => return false,
        }
        self.latch();
        true
    }

    pub fn iomux_read(&self, off: u32) -> u32 {
        self.iomux.get((off / 4) as usize).copied().unwrap_or(0)
    }

    pub fn iomux_write(&mut self, off: u32, v: u32) -> bool {
        if let Some(r) = self.iomux.get_mut((off / 4) as usize) {
            *r = v;
        }
        self.latch()
    }
}
