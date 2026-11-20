//! RTC_CNTL (0x6000_8000): reset causes, software resets, the RTC time counter
//! and the retention registers. The rest of its registers read back.

use super::TICK_HZ;
use super::regfile::RegFile;

const OPTIONS0: u32 = 0x000;
const TIME_UPDATE: u32 = 0x00c;
const TIME0: u32 = 0x010;
const TIME1: u32 = 0x014;
const RESET_STATE: u32 = 0x038;
const CLK_CONF: u32 = 0x074;
const DATE: u32 = 0x1fc;

const OPTIONS0_SW_SYS_RESET: u32 = 1 << 31;
const OPTIONS0_SW_PROCPU_RESET: u32 = 1 << 5;
const OPTIONS0_SW_APPCPU_RESET: u32 = 1 << 4;

pub const RESET_POWERON: u32 = 1;
pub const RESET_SW_SYS: u32 = 3;
pub const RESET_SW_CPU: u32 = 12;

/// A reset requested by the firmware.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetRequest {
    System,
    ProCpu,
}

pub struct RtcCntl {
    regs: RegFile,
    pub reset_cause: [u32; 2],
    vector_sel: u32,
    time: u64,
    pub pending_reset: Option<ResetRequest>,
}

impl Default for RtcCntl {
    fn default() -> Self {
        Self::new()
    }
}

impl RtcCntl {
    pub fn new() -> Self {
        RtcCntl {
            regs: RegFile::new("rtc_cntl", 0x200)
                .with_reset(CLK_CONF, 0x2000_0000)
                .with_reset(DATE, 0x0210_1220),
            reset_cause: [RESET_POWERON; 2],
            vector_sel: 0x3000,
            time: 0,
            pending_reset: None,
        }
    }

    /// Slow clock in Hz (ANA_CLK_RTC_SEL).
    fn slow_hz(&self) -> u64 {
        match self.regs.get(CLK_CONF) >> 30 {
            1 => 32_768,
            2 => 31_250,
            _ => 150_000,
        }
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            TIME_UPDATE => 0,
            TIME0 => self.time as u32,
            TIME1 => (self.time >> 32) as u32,
            RESET_STATE => self.reset_cause[0] | (self.reset_cause[1] << 6) | self.vector_sel,
            _ => self.regs.read(off),
        }
    }

    pub fn write(&mut self, off: u32, v: u32, now: u64) {
        match off {
            OPTIONS0 => {
                if v & OPTIONS0_SW_SYS_RESET != 0 {
                    self.reset_cause = [RESET_SW_SYS; 2];
                    self.pending_reset = Some(ResetRequest::System);
                } else if v & OPTIONS0_SW_PROCPU_RESET != 0 {
                    self.reset_cause[0] = RESET_SW_CPU;
                    self.pending_reset = Some(ResetRequest::ProCpu);
                }
                if v & OPTIONS0_SW_APPCPU_RESET != 0 {
                    self.reset_cause[1] = RESET_SW_CPU;
                }
                self.regs.write(
                    off,
                    v & !(OPTIONS0_SW_SYS_RESET
                        | OPTIONS0_SW_PROCPU_RESET
                        | OPTIONS0_SW_APPCPU_RESET),
                );
            }
            TIME_UPDATE => {
                if v & (1 << 31) != 0 {
                    self.time =
                        (u128::from(now) * u128::from(self.slow_hz()) / u128::from(TICK_HZ)) as u64;
                }
            }
            TIME0 | TIME1 | DATE => {}
            RESET_STATE => self.vector_sel = v & 0x3000,
            _ => self.regs.write(off, v),
        }
    }
}
