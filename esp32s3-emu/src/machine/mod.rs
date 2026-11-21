//! Espressif ESP32-S3 machine.
//!
//! PRO_CPU runs the [`rax::isa::xtensa`] interpreter against [`Soc`], the
//! address map with the peripheral models of [`crate::devices`]. It
//! boots like the chip: from the reset vector in the internal ROM, which loads
//! the second-stage bootloader from flash, which loads the application. The ROM
//! image is an input of the machine. APP_CPU leaves reset when the firmware
//! starts it; the two cores run each time slice one after the other.
//!
//! Time is deterministic: each instruction is one CPU cycle at the frequency the
//! clock registers select, and `WAITI` skips to the next timer event. The unit of
//! time is the tick, 1/2.4 GHz, an integer number of cycles of every clock used.
//!
//! The host drives the machine with [`Esp32s3::run_until`], and reaches the
//! board side of the peripherals (UART bytes, I2C devices, GPIO levels) through
//! [`Esp32s3::periph`].

pub mod bus;

#[cfg(test)]
mod tests;

pub use bus::Soc;

use crate::devices::{Peripherals, PeripheralsConfig};
use rax::isa::xtensa::regs::cause;
use rax::isa::xtensa::{Exit, XtensaConfig, XtensaCpu};

/// Ticks per second.
pub const TICK_HZ: u64 = 2_400_000_000;

/// Processor IDs of PRO_CPU and APP_CPU.
pub const PRO_CPU_PRID: u32 = 0xcdcd;
pub const APP_CPU_PRID: u32 = 0xabab;

/// Inputs of the machine.
pub struct Esp32s3Config {
    /// Internal ROM image (384 KB).
    pub rom: Vec<u8>,
    pub periph: PeripheralsConfig,
}

/// Something the host should know about, reported by [`Esp32s3::run_until`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// `BREAK s, t` at `pc` (the PC after it). Execution goes on past it.
    Break { pc: u32, s: u32, t: u32 },
    /// An exception the firmware will handle, reported for diagnosis.
    Exception {
        pc: u32,
        cause: u32,
        vaddr: Option<u32>,
    },
    /// The chip was reset (software or watchdog reset).
    Reset,
}

pub struct Esp32s3 {
    /// PRO_CPU.
    pub cpu: XtensaCpu,
    /// APP_CPU: in reset until the firmware starts it.
    pub app: XtensaCpu,
    pub soc: Soc,
    /// Time, in ticks.
    pub now: u64,
    /// Exceptions that are reported as [`Event::Exception`].
    pub report_causes: u64,
    /// Longest slice, in instructions.
    pub max_slice: u64,
    /// Functions replaced by an immediate return: entry address → result.
    stubs: std::collections::BTreeMap<u32, u32>,
}

impl Esp32s3 {
    pub fn new(cfg: Esp32s3Config) -> Self {
        let periph = Peripherals::new(cfg.periph);
        let report_causes = [
            cause::ILLEGAL_INSTRUCTION,
            cause::INSTRUCTION_FETCH_ERROR,
            cause::LOAD_STORE_ERROR,
            cause::INTEGER_DIVIDE_BY_ZERO,
            cause::LOAD_STORE_ALIGNMENT,
        ]
        .iter()
        .fold(0u64, |m, &c| m | (1 << c));
        let mut cpu = XtensaCpu::new(XtensaConfig::esp32s3(), PRO_CPU_PRID);
        cpu.stop_on_cause = report_causes;
        let mut app = XtensaCpu::new(XtensaConfig::esp32s3(), APP_CPU_PRID);
        app.stop_on_cause = report_causes;
        Esp32s3 {
            cpu,
            app,
            soc: Soc::new(cfg.rom, periph),
            now: 0,
            report_causes,
            max_slice: 2000,
            stubs: std::collections::BTreeMap::new(),
        }
    }

    /// Makes the function at `addr` return `value` at once when called (for
    /// firmware parts whose hardware is not emulated, such as the radio).
    pub fn stub_function(&mut self, addr: u32, value: u32) {
        self.stubs.insert(addr, value);
        self.cpu.set_hook(addr);
        self.app.set_hook(addr);
    }

    pub fn periph(&mut self) -> &mut Peripherals {
        &mut self.soc.periph
    }

    /// Instructions executed by both cores.
    pub fn icount(&self) -> u64 {
        self.cpu.icount + self.app.icount
    }

    /// Chip reset: the processors and the peripherals, not the memories.
    pub fn reset(&mut self) {
        self.cpu.reset();
        self.app.reset();
        self.soc.periph.reset();
        self.soc.invalidate_fetch();
    }

    /// Runs until `deadline` (ticks) or until an event to report.
    pub fn run_until(&mut self, deadline: u64) -> Option<Event> {
        while self.now < deadline {
            if let Some(ev) = self.slice(deadline) {
                return Some(ev);
            }
        }
        None
    }

    /// Whether APP_CPU runs, after restarting it if it went through reset.
    fn app_runs(&mut self) -> bool {
        if self.soc.periph.system.take_app_reset() {
            self.app.reset();
        }
        self.soc.periph.system.app_cpu_runs()
    }

    /// Resets the chip if the firmware asked for it.
    fn chip_reset(&mut self) -> Option<Event> {
        self.soc.periph.take_reset()?;
        self.reset();
        Some(Event::Reset)
    }

    /// Both cores run the same time slice, one after the other.
    fn slice(&mut self, deadline: u64) -> Option<Event> {
        let cycle = self.soc.periph.cycle_ticks();
        let next = self
            .soc
            .periph
            .next_event(self.now)
            .unwrap_or(u64::MAX)
            .min(deadline);
        let budget = (next.saturating_sub(self.now) / cycle).clamp(1, self.max_slice);
        let start = self.now;
        let app = self.app_runs();
        let (pro_waits, mut event) = self.run_core(0, start, budget, cycle);
        let mut app_waits = true;
        if app && !self.soc.periph.reset_pending() {
            let (waits, e) = self.run_core(1, start, budget, cycle);
            app_waits = waits;
            event = event.or(e);
        }
        self.now = (start + budget * cycle).max(self.soc.time_floor());
        self.soc.periph.update(self.now);
        if let Some(reset) = self.chip_reset() {
            return Some(reset);
        }
        if pro_waits && app_waits && event.is_none() {
            self.idle(deadline, app);
        }
        event
    }

    /// Runs `core` for `budget` cycles from `start`. Returns whether it ended
    /// waiting for an interrupt, and the first event to report.
    fn run_core(
        &mut self,
        core: usize,
        start: u64,
        budget: u64,
        cycle: u64,
    ) -> (bool, Option<Event>) {
        let mut event = None;
        let mut done = 0;
        self.soc.core = core;
        while done < budget {
            let cpu = if core == 0 {
                &mut self.cpu
            } else {
                &mut self.app
            };
            self.soc.begin_slice(start + done * cycle, cpu.icount);
            let lines = self.soc.irq_lines();
            cpu.set_irq_lines(lines);
            let before = cpu.icount;
            let exit = if self.soc.mmio_log.is_some() {
                // one instruction at a time, its PC known to the peripheral access log
                let mut exit = Exit::Budget;
                while cpu.icount - before < budget - done {
                    self.soc.trace_pc = cpu.pc;
                    exit = cpu.run(&mut self.soc, 1);
                    if !matches!(exit, Exit::Budget) {
                        break;
                    }
                }
                exit
            } else {
                cpu.run(&mut self.soc, budget - done)
            };
            self.soc.end_slice(cpu.icount);
            done += cpu.icount - before;
            match exit {
                Exit::Budget | Exit::Simcall => {}
                Exit::Waiti => {
                    cpu.advance_ccount(budget.saturating_sub(done));
                    return (true, event);
                }
                Exit::Hook => {
                    match self.stubs.get(&cpu.pc) {
                        Some(&value) => cpu.return_from_function(value),
                        None => {
                            let _ = cpu.step(&mut self.soc);
                        }
                    }
                    done += 1;
                }
                Exit::Break { s, t } => {
                    event.get_or_insert(Event::Break { pc: cpu.pc, s, t });
                }
                Exit::Exception { cause, vaddr } => {
                    let pc = cpu.pc;
                    cpu.raise(cause, vaddr);
                    event.get_or_insert(Event::Exception { pc, cause, vaddr });
                }
            }
            if self.soc.periph.reset_pending() {
                break;
            }
        }
        (false, event)
    }

    /// Every running core waits for an interrupt: skips to the next event.
    fn idle(&mut self, deadline: u64, app: bool) {
        let cycle = self.soc.periph.cycle_ticks();
        let mut next = self
            .soc
            .periph
            .next_event(self.now)
            .unwrap_or(u64::MAX)
            .min(self.now + self.cpu.cycles_to_ccompare() * cycle)
            .min(deadline);
        if app {
            next = next.min(self.now + self.app.cycles_to_ccompare() * cycle);
        }
        if next > self.now {
            let cycles = (next - self.now).div_ceil(cycle);
            self.cpu.advance_ccount(cycles);
            if app {
                self.app.advance_ccount(cycles);
            }
            self.now += cycles * cycle;
            self.soc.periph.update(self.now);
        }
    }

    /// Reads a little-endian word of guest memory.
    pub fn read_u32(&self, addr: u32) -> Option<u32> {
        self.soc.read_u32(addr)
    }

    /// Takes the bytes the firmware wrote to RTT up-buffer `channel`, given the
    /// address of the `_SEGGER_RTT` control block.
    pub fn rtt_read(&mut self, control_block: u32, channel: u32) -> Vec<u8> {
        // Control block: id[16], max_up, max_down, then up buffers of 24 bytes:
        // name, buffer, size, write offset, read offset, flags.
        let Some(max_up) = self.soc.read_u32(control_block + 16) else {
            return Vec::new();
        };
        if channel >= max_up {
            return Vec::new();
        }
        let desc = control_block + 24 + 24 * channel;
        let field = |soc: &Soc, i: u32| soc.read_u32(desc + 4 * i).unwrap_or(0);
        let (buf, size, wr, rd) = (
            field(&self.soc, 1),
            field(&self.soc, 2),
            field(&self.soc, 3),
            field(&self.soc, 4),
        );
        if size == 0 || wr >= size || rd >= size || wr == rd {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut pos = rd;
        while pos != wr {
            if let Some(b) = self.soc.read_bytes(buf + pos, 1) {
                out.push(b[0]);
            }
            pos = (pos + 1) % size;
        }
        self.soc.write_u32(desc + 16, wr);
        out
    }
}
