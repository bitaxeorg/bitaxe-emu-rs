//! SYSTEM registers (0x600C_0000): clock selection, the FROM_CPU software
//! interrupts, APP_CPU control, peripheral clock and reset enables.

use super::regfile::RegFile;

const CPU_PER_CONF: u32 = 0x010;
const CPU_INTR_FROM_CPU_0: u32 = 0x030;
const SYSCLK_CONF: u32 = 0x060;
const DATE: u32 = 0xffc;

pub struct System {
    regs: RegFile,
}

impl Default for System {
    fn default() -> Self {
        Self::new()
    }
}

impl System {
    pub fn new() -> Self {
        let regs = RegFile::new("system", 0x1000)
            .with_reset(CPU_PER_CONF, 0x0000_000c)
            .with_reset(0x008, 0x0000_0001)
            .with_reset(0x018, 0xf9c1_e06f)
            .with_reset(0x01c, 0x0000_0000)
            .with_reset(0x020, 0x0000_0000)
            .with_reset(0x024, 0x0000_0000)
            .with_reset(0x028, 0x0000_00ff)
            .with_reset(0x02c, 0x0200_1001)
            // PRE_DIV_CNT 0, SOC_CLK_SEL XTAL, CLK_XTAL_FREQ 40.
            .with_reset(SYSCLK_CONF, 0x0002_8000)
            .with_reset(DATE, 0x0210_1220);
        System { regs }
    }

    pub fn reset(&mut self) {
        self.regs.reset();
    }

    /// CPU clock in Hz.
    pub fn cpu_hz(&self) -> u64 {
        let sysclk = self.regs.get(SYSCLK_CONF);
        let per = self.regs.get(CPU_PER_CONF);
        let div = u64::from(sysclk & 0x3ff) + 1;
        match (sysclk >> 10) & 3 {
            0 => 40_000_000 / div,
            1 => {
                let pll480 = per & 4 != 0;
                match (per & 3, pll480) {
                    (0, _) => 80_000_000,
                    (1, _) => 160_000_000,
                    (_, true) => 240_000_000,
                    (_, false) => 160_000_000,
                }
            }
            _ => 17_500_000 / div,
        }
    }

    /// PLL frequency in Hz: 480 MHz when CPU_PER_CONF.PLL_FREQ_SEL is set, else
    /// 320 MHz.
    pub fn pll_hz(&self) -> u64 {
        if self.regs.get(CPU_PER_CONF) & 4 != 0 {
            480_000_000
        } else {
            320_000_000
        }
    }

    /// APB clock in Hz: 80 MHz from the PLL, else the CPU clock.
    pub fn apb_hz(&self) -> u64 {
        if (self.regs.get(SYSCLK_CONF) >> 10) & 3 == 1 {
            80_000_000
        } else {
            self.cpu_hz()
        }
    }

    /// Levels of the FROM_CPU interrupt sources 0..3.
    pub fn from_cpu(&self) -> [bool; 4] {
        core::array::from_fn(|i| self.regs.get(CPU_INTR_FROM_CPU_0 + 4 * i as u32) & 1 != 0)
    }

    pub fn read(&mut self, off: u32) -> u32 {
        self.regs.read(off)
    }

    pub fn write(&mut self, off: u32, v: u32) {
        let v = match off {
            o if (CPU_INTR_FROM_CPU_0..CPU_INTR_FROM_CPU_0 + 16).contains(&o) => v & 1,
            DATE => return,
            _ => v,
        };
        self.regs.write(off, v);
    }
}
