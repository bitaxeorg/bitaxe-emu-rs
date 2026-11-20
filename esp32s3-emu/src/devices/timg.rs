//! Timer groups (TIMG0 0x6001_F000, TIMG1 0x6002_0000): two 54-bit timers with
//! alarms, the main watchdog (interrupt stages only) and the RTC slow clock
//! calibration.

use super::TICK_HZ;

const T1_OFFSET: u32 = 0x24;
const WDTCONFIG0: u32 = 0x48;
const WDTFEED: u32 = 0x60;
const WDTWPROTECT: u32 = 0x64;
const RTCCALICFG: u32 = 0x68;
const RTCCALICFG1: u32 = 0x6c;
const INT_ENA: u32 = 0x70;
const INT_RAW: u32 = 0x74;
const INT_ST: u32 = 0x78;
const INT_CLR: u32 = 0x7c;
const RTCCALICFG2: u32 = 0x80;
const DATE: u32 = 0xf8;

const WDT_KEY: u32 = 0x50d8_3aa1;
const MASK54: u64 = (1 << 54) - 1;

#[derive(Default)]
struct Timer {
    config: u32,
    /// Count at `base_time`.
    base_value: u64,
    base_time: u64,
    latched: u64,
    alarm: u64,
    load: u64,
    load_lo: u32,
    load_hi: u32,
    alarm_lo: u32,
    alarm_hi: u32,
}

impl Timer {
    fn enabled(&self) -> bool {
        self.config & (1 << 31) != 0
    }

    fn increase(&self) -> bool {
        self.config & (1 << 30) != 0
    }

    fn alarm_en(&self) -> bool {
        self.config & (1 << 10) != 0
    }

    fn rate(&self) -> u64 {
        let src = if self.config & (1 << 9) != 0 {
            40_000_000
        } else {
            80_000_000
        };
        let div = match (self.config >> 13) & 0xffff {
            0 => 65536,
            1 => 2,
            d => u64::from(d),
        };
        src / div
    }

    fn value(&self, now: u64) -> u64 {
        if !self.enabled() {
            return self.base_value;
        }
        let ticks = (u128::from(now - self.base_time) * u128::from(self.rate())
            / u128::from(TICK_HZ)) as u64;
        if self.increase() {
            (self.base_value + ticks) & MASK54
        } else {
            self.base_value.wrapping_sub(ticks) & MASK54
        }
    }

    fn rebase(&mut self, now: u64) {
        self.base_value = self.value(now);
        self.base_time = now;
    }

    /// Time the alarm fires, if armed.
    fn alarm_time(&self, now: u64) -> Option<u64> {
        if !self.enabled() || !self.alarm_en() {
            return None;
        }
        let value = self.value(now);
        let ticks = if self.increase() {
            self.alarm.wrapping_sub(value) & MASK54
        } else {
            value.wrapping_sub(self.alarm) & MASK54
        };
        if ticks == 0 || ticks > (1 << 53) {
            return Some(now);
        }
        // The first time the count reaches the alarm.
        let elapsed_ticks = (u128::from(now - self.base_time) * u128::from(self.rate())
            / u128::from(TICK_HZ)) as u64;
        let target_ticks = elapsed_ticks + ticks;
        Some(
            self.base_time
                + (u128::from(target_ticks) * u128::from(TICK_HZ)).div_ceil(u128::from(self.rate()))
                    as u64,
        )
    }
}

pub struct Timg {
    timers: [Timer; 2],
    int_ena: u32,
    int_raw: u32,
    wdt_config: [u32; 6],
    wdt_protect: u32,
    wdt_stage: usize,
    wdt_stage_start: u64,
    rtccali: u32,
    rtccali1: u32,
    rtccali2: u32,
}

impl Default for Timg {
    fn default() -> Self {
        Self::new()
    }
}

impl Timg {
    pub fn new() -> Self {
        let mut t = Timg {
            timers: Default::default(),
            int_ena: 0,
            int_raw: 0,
            wdt_config: [0; 6],
            wdt_protect: WDT_KEY,
            wdt_stage: 0,
            wdt_stage_start: 0,
            // Cycling calibration at reset, with a first result ready.
            rtccali: 0x0001_b000,
            rtccali1: 0,
            rtccali2: 0,
        };
        t.reset();
        t
    }

    pub fn reset(&mut self) {
        for timer in &mut self.timers {
            timer.config = 0x6000_2000;
            timer.base_value = 0;
            timer.base_time = 0;
        }
        self.int_ena = 0;
        self.int_raw = 0;
        self.wdt_config = [0; 6];
        self.wdt_protect = WDT_KEY;
        self.wdt_stage = 0;
    }

    /// Interrupt source levels: T0, T1, WDT.
    pub fn irqs(&self) -> [bool; 3] {
        let st = self.int_raw & self.int_ena;
        [st & 1 != 0, st & 2 != 0, st & 4 != 0]
    }

    fn wdt_enabled(&self) -> bool {
        self.wdt_config[0] & (1 << 31) != 0
    }

    fn wdt_rate(&self) -> u64 {
        let src = if self.wdt_config[0] & (1 << 21) != 0 {
            40_000_000
        } else {
            80_000_000
        };
        let prescale = u64::from(self.wdt_config[1] >> 16).max(1);
        src / prescale
    }

    fn wdt_deadline(&self) -> Option<u64> {
        if !self.wdt_enabled() {
            return None;
        }
        let action = (self.wdt_config[0] >> (29 - 2 * self.wdt_stage as u32)) & 3;
        if action == 0 {
            return None;
        }
        let hold = u64::from(self.wdt_config[2 + self.wdt_stage]).max(1);
        Some(self.wdt_stage_start + hold * TICK_HZ / self.wdt_rate())
    }

    pub fn next_event(&self, now: u64) -> Option<u64> {
        let alarms = self.timers.iter().filter_map(|t| t.alarm_time(now));
        alarms.chain(self.wdt_deadline()).min()
    }

    /// Advances to `now`. Returns whether the interrupt state may have changed.
    pub fn update(&mut self, now: u64) -> bool {
        let mut changed = false;
        for (i, timer) in self.timers.iter_mut().enumerate() {
            if let Some(t) = timer.alarm_time(now)
                && t <= now
            {
                timer.config &= !(1 << 10);
                if timer.config & (1 << 29) != 0 {
                    timer.base_value = timer.load;
                    timer.base_time = now;
                }
                self.int_raw |= 1 << i;
                changed = true;
            }
        }
        if let Some(t) = self.wdt_deadline()
            && t <= now
        {
            let action = (self.wdt_config[0] >> (29 - 2 * self.wdt_stage as u32)) & 3;
            if action == 1 {
                self.int_raw |= 4;
                changed = true;
            }
            // Reset stages are not modelled: the watchdog moves to the next stage.
            self.wdt_stage = (self.wdt_stage + 1) % 4;
            self.wdt_stage_start = now;
        }
        changed
    }

    pub fn read(&mut self, off: u32, now: u64) -> u32 {
        let (t, o) = if (T1_OFFSET..T1_OFFSET + 0x24).contains(&off) {
            (1, off - T1_OFFSET)
        } else {
            (0, off)
        };
        if off < T1_OFFSET * 2 {
            let timer = &self.timers[t];
            return match o {
                0x00 => timer.config,
                0x04 => timer.latched as u32,
                0x08 => (timer.latched >> 32) as u32,
                0x10 => timer.alarm_lo,
                0x14 => timer.alarm_hi,
                0x18 => timer.load_lo,
                0x1c => timer.load_hi,
                _ => {
                    let _ = now;
                    0
                }
            };
        }
        match off {
            WDTCONFIG0..=0x5c => self.wdt_config[((off - WDTCONFIG0) / 4) as usize],
            WDTWPROTECT => self.wdt_protect,
            RTCCALICFG => self.rtccali,
            RTCCALICFG1 => self.rtccali1,
            RTCCALICFG2 => self.rtccali2,
            INT_ENA => self.int_ena,
            INT_RAW => self.int_raw,
            INT_ST => self.int_raw & self.int_ena,
            DATE => 0x0200_3071,
            _ => 0,
        }
    }

    /// Returns whether the interrupt state may have changed.
    pub fn write(&mut self, off: u32, v: u32, now: u64) -> bool {
        if off < T1_OFFSET * 2 {
            let (t, o) = if off >= T1_OFFSET {
                (1, off - T1_OFFSET)
            } else {
                (0, off)
            };
            let timer = &mut self.timers[t];
            match o {
                0x00 => {
                    timer.rebase(now);
                    timer.config = v & !(1 << 12);
                    if v & (1 << 31) != 0 {
                        timer.base_time = now;
                    }
                }
                0x0c => timer.latched = timer.value(now),
                0x10 => {
                    timer.alarm_lo = v;
                    timer.alarm = (timer.alarm & !0xffff_ffff) | u64::from(v);
                }
                0x14 => {
                    timer.alarm_hi = v & 0x3f_ffff;
                    timer.alarm = (timer.alarm & 0xffff_ffff) | (u64::from(v & 0x3f_ffff) << 32);
                }
                0x18 => {
                    timer.load_lo = v;
                    timer.load = (timer.load & !0xffff_ffff) | u64::from(v);
                }
                0x1c => {
                    timer.load_hi = v & 0x3f_ffff;
                    timer.load = (timer.load & 0xffff_ffff) | (u64::from(v & 0x3f_ffff) << 32);
                }
                0x20 => {
                    timer.base_value = timer.load;
                    timer.base_time = now;
                }
                _ => {}
            }
            return true;
        }
        match off {
            WDTCONFIG0 => {
                if self.wdt_protect == WDT_KEY {
                    let was = self.wdt_enabled();
                    self.wdt_config[0] = v & !(1 << 22);
                    if !was && self.wdt_enabled() {
                        self.wdt_stage = 0;
                        self.wdt_stage_start = now;
                    }
                }
            }
            0x4c..=0x5c => {
                if self.wdt_protect == WDT_KEY {
                    self.wdt_config[((off - WDTCONFIG0) / 4) as usize] = v;
                }
            }
            WDTFEED => {
                self.wdt_stage = 0;
                self.wdt_stage_start = now;
            }
            WDTWPROTECT => self.wdt_protect = v,
            RTCCALICFG => {
                let mut v = v & !(1 << 15);
                if v & ((1 << 31) | (1 << 12)) != 0 {
                    let freq: u64 = match (v >> 13) & 3 {
                        0 => 136_000,
                        1 => 68_359,
                        _ => 32_768,
                    };
                    let max = u64::from((v >> 16) & 0x7fff);
                    self.rtccali1 = ((40_000_000 * max / freq) << 7) as u32;
                    v |= 1 << 15;
                    self.rtccali2 &= !1;
                }
                self.rtccali = v;
            }
            RTCCALICFG2 => {
                // TIMEOUT when the threshold (XTAL cycles) is below what the
                // calibration of MAX slow cycles takes.
                let thres = u64::from(v >> 7);
                let max = u64::from((self.rtccali >> 16) & 0x7fff);
                let timeout = thres < max * 40_000_000 / 136_000;
                self.rtccali2 = (v & !1) | u32::from(timeout);
            }
            INT_ENA => self.int_ena = v & 7,
            INT_CLR => self.int_raw &= !(v & 7),
            _ => return false,
        }
        true
    }
}
