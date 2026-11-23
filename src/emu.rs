//! The emulator loop: runs the board paced to the wall clock, takes commands
//! (buttons), and publishes snapshots of what the board shows.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use esp32s3_rax::machine::{Event, TICK_HZ};

use crate::board::{self, Board, Regulator, Screen};
use crate::logs::Logs;

/// The network around the chip, set once at start; `None` for no Wi-Fi.
pub static NETWORK: std::sync::OnceLock<Option<esp32s3_rax::net::NetworkConfig>> =
    std::sync::OnceLock::new();
/// Clear ESP-Miner's factory self-test flag in the NVS before boot: the
/// emulated chips cannot find the nonces the self-test counts.
pub static SKIP_SELF_TEST: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Lines of log kept for the GUI.
const LOG_LINES: usize = 200;
/// OLED pixel colours, lit and dark.
const OLED_LIT: [u8; 4] = [0xd8, 0xf0, 0xff, 0xff];
const OLED_DARK: [u8; 4] = [0x05, 0x07, 0x0a, 0xff];
const LCD_DARK: [u8; 4] = [0x02, 0x02, 0x03, 0xff];

pub enum Command {
    BootButton(bool),
    Reset,
    /// Write the flash back to its file when the emulator stops, or not.
    Persist(bool),
    /// Save the display's image to a PNG file.
    SaveDisplay(PathBuf),
    Quit,
}

/// The display's image, RGBA.
#[derive(Clone, Default)]
pub struct Frame {
    pub rgba: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
    /// Changes when the image changes.
    pub generation: u64,
}

#[derive(Clone, Default)]
pub struct Snapshot {
    /// The firmware running.
    pub firmware: String,
    pub display: Frame,
    pub seconds: f64,
    /// Emulation speed relative to real time.
    pub speed: f64,
    pub mips: f64,
    /// Output of the core regulator, V, when on.
    pub vcore: Option<f32>,
    pub input_v: f32,
    pub input_a: f32,
    pub asic_power: f32,
    pub chip_temps: Vec<f32>,
    /// Regulator temperature, when it measures one.
    pub vr_temp: Option<f32>,
    pub fan_rpm: Vec<f32>,
    pub fan_duty: Vec<f32>,
    pub freq_mhz: f32,
    pub asic_running: bool,
    pub jobs: u64,
    pub nonces: u64,
    pub logs: VecDeque<String>,
    /// Lines logged since the start, including those dropped from `logs`.
    pub log_count: u64,
}

pub struct Emulator {
    pub board: Board,
    logs: Logs,
    snapshot: Arc<Mutex<Snapshot>>,
    /// Console bytes of UART0 and of the USB-Serial-JTAG, until a line ends.
    console: [Vec<u8>; 2],
    /// Lines shown from one console that the other has not printed yet, by
    /// console: the firmware may print each line on both.
    unmatched: [std::collections::HashMap<String, u32>; 2],
    /// What the display showed at the last snapshot: its source generation and
    /// whether it was lit.
    display_state: Option<(u64, bool)>,
    display_generation: u64,
    /// PC at the end of each slice, counted, when profiling.
    pub pc_samples: Option<std::collections::BTreeMap<Vec<u32>, u64>>,
    /// The file the flash came from, for [`Command::Persist`].
    pub flash_file: Option<PathBuf>,
}

impl Emulator {
    pub fn new(board: Board, elf: Option<&[u8]>) -> Self {
        Self::with_snapshot(board, elf, Arc::new(Mutex::new(Snapshot::default())))
    }

    /// An emulator publishing into `snapshot`.
    pub fn with_snapshot(
        mut board: Board,
        elf: Option<&[u8]>,
        snapshot: Arc<Mutex<Snapshot>>,
    ) -> Self {
        if let Some(Some(cfg)) = NETWORK.get() {
            board.machine.soc.air = Some(Box::new(esp32s3_rax::net::Network::new(cfg.clone())));
        }
        let self_test_cleared = SKIP_SELF_TEST.load(std::sync::atomic::Ordering::Relaxed)
            && crate::nvs::set_unsigned(&mut board.machine.periph().flash.data, "selftest", 0) > 0;
        let mut emulator = Emulator {
            board,
            logs: Logs::new(elf),
            snapshot,
            console: Default::default(),
            unmatched: Default::default(),
            display_state: None,
            display_generation: 0,
            pc_samples: std::env::var_os("BITAXE_EMU_PROFILE").map(|_| Default::default()),
            flash_file: None,
        };
        if self_test_cleared {
            emulator.push_log("cleared the self-test flag in the NVS (--skip-self-test)".into());
        }
        emulator
    }

    /// Runs for `seconds` of emulated time as fast as possible, printing the logs.
    pub fn run_headless(&mut self, seconds: f64) {
        let end = (seconds * TICK_HZ as f64) as u64;
        let started = Instant::now();
        // Timed button actions, "seconds:action,...", action boot_down, boot_up or reset.
        let mut script: Vec<(u64, String)> = std::env::var("BITAXE_EMU_SCRIPT")
            .unwrap_or_default()
            .split(',')
            .filter_map(|item| {
                let (t, a) = item.split_once(':')?;
                Some((
                    (t.parse::<f64>().ok()? * TICK_HZ as f64) as u64,
                    a.to_string(),
                ))
            })
            .collect();
        script.sort();
        // Paced to the wall clock, for the network: the host's sockets run in
        // real time.
        let realtime = std::env::var_os("BITAXE_EMU_REALTIME").is_some();
        let emu_start = self.board.machine.now;
        while self.board.machine.now < end {
            if realtime {
                let emu = Duration::from_secs_f64(
                    (self.board.machine.now - emu_start) as f64 / TICK_HZ as f64,
                );
                if let Some(ahead) = emu.checked_sub(started.elapsed()) {
                    std::thread::sleep(ahead);
                }
            }
            while let Some((t, action)) = script.first().cloned()
                && t <= self.board.machine.now
            {
                script.remove(0);
                match action.as_str() {
                    "boot_down" => self.board.set_boot_button(true),
                    "boot_up" => self.board.set_boot_button(false),
                    "reset" => self.board.reset(),
                    _ => {}
                }
                self.push_log(format!("(script) {action}"));
            }
            let deadline = (self.board.machine.now + TICK_HZ / 100).min(end);
            self.slice(deadline);
            for line in self.drain_lines() {
                println!("[{:9.3}] {line}", self.board.seconds());
            }
        }
        let s = self.snapshot.lock().unwrap();
        eprintln!(
            "{:.2} s emulated in {:.2} s ({:.0} MIPS)",
            s.seconds,
            started.elapsed().as_secs_f64(),
            self.board.machine.icount() as f64 / started.elapsed().as_secs_f64() / 1e6
        );
        eprintln!(
            "ASIC chain: {} jobs received, {} nonces found",
            self.board.chain.jobs_received, self.board.chain.nonces_found
        );
    }

    /// The OLED as text, for headless runs (empty on the colour LCD boards).
    pub fn oled_ascii(&self) -> String {
        let Screen::Oled(oled) = &self.board.screen else {
            return String::new();
        };
        let (px, width) = {
            let oled = oled.lock();
            (oled.pixels(), oled.width())
        };
        let mut out = String::new();
        for row in px.chunks(width).collect::<Vec<_>>().chunks(2) {
            for x in 0..width {
                let top = row[0][x] != 0;
                let bottom = row.get(1).is_some_and(|r| r[x] != 0);
                out.push(match (top, bottom) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    _ => ' ',
                });
            }
            out.push('\n');
        }
        out
    }

    /// The display's current image, RGBA, with its size.
    pub fn display_image(&mut self) -> (Vec<u8>, u32, u32) {
        let lit = self.board.lcd_lit();
        match &self.board.screen {
            Screen::Oled(oled) => {
                let oled = oled.lock();
                let on = oled.is_on();
                let rgba = oled
                    .pixels()
                    .iter()
                    .flat_map(|&p| if on && p != 0 { OLED_LIT } else { OLED_DARK })
                    .collect();
                (rgba, oled.width() as u32, oled.rows() as u32)
            }
            Screen::Lcd(lcd) => {
                let (w, h) = (board::st7789::VIEW_WIDTH, board::st7789::VIEW_HEIGHT);
                let rgba = match self.board.spec.display {
                    board::spec::Display::St7789 { upright } if lit && lcd.is_on() => {
                        lcd.view(upright)
                    }
                    _ => LCD_DARK.repeat(w * h),
                };
                (rgba, w as u32, h as u32)
            }
        }
    }

    /// Saves the display's image, scaled up (4x for the OLED, 2x for the LCD).
    pub fn save_display_png(&mut self, path: &Path) -> Result<(), String> {
        let (rgba, w, h) = self.display_image();
        let (w, h) = (w as usize, h as usize);
        let scale = if w > 200 { 2 } else { 4 };
        let mut rgb = vec![0u8; w * scale * h * scale * 3];
        for y in 0..h * scale {
            for x in 0..w * scale {
                let i = ((y / scale) * w + x / scale) * 4;
                let o = (y * w * scale + x) * 3;
                rgb[o..o + 3].copy_from_slice(&rgba[i..i + 3]);
            }
        }
        let error = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
        let file = std::fs::File::create(path).map_err(|e| error(&e))?;
        let mut encoder = png::Encoder::new(
            std::io::BufWriter::new(file),
            (w * scale) as u32,
            (h * scale) as u32,
        );
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .and_then(|mut writer| writer.write_image_data(&rgb))
            .map_err(|e| error(&e))
    }

    /// The source generation of the display and whether it is lit.
    fn display_state(&mut self) -> (u64, bool) {
        let lit = self.board.lcd_lit();
        match &self.board.screen {
            Screen::Oled(oled) => {
                let o = oled.lock();
                (o.generation(), o.is_on())
            }
            Screen::Lcd(lcd) => (lcd.generation, lit && lcd.is_on()),
        }
    }

    fn slice(&mut self, deadline: u64) {
        let events = self.board.run_until(deadline, TICK_HZ / 1000);
        if let Some(air) = self.board.machine.soc.air.as_mut() {
            for line in air.take_events() {
                self.push_log(line);
            }
        }
        if std::env::var_os("BITAXE_EMU_WIFI_STATS").is_some()
            && self.board.machine.now / TICK_HZ != deadline.saturating_sub(TICK_HZ / 100) / TICK_HZ
        {
            let wifi = &self.board.machine.soc.periph.wifi;
            let (next, last, queued) = wifi.rx_state();
            let st = wifi.stats;
            let line = format!(
                "wifi mac: tx {} rx {} dropped {} no key {}/{} queued {queued} next {next:#010x} last {last:#010x}",
                st.tx, st.rx, st.rx_dropped, st.tx_no_key, st.rx_undecrypted
            );
            self.push_log(line);
        }
        if let Some(samples) = self.pc_samples.as_mut() {
            // The live call chain from the register windows, as one key.
            let cpu = &self.board.machine.cpu;
            let (wb, ws) = (cpu.sr[72], cpu.sr[73]);
            let mut chain = vec![cpu.pc];
            for k in 0..16u32 {
                let frame = (wb + 16 - k) % 16;
                if ws & (1 << frame) != 0 {
                    chain.push((cpu.ar[(frame * 4) as usize] & 0x3fff_ffff) | 0x4000_0000);
                }
            }
            *samples.entry(chain).or_insert(0u64) += 1;
        }
        for ev in events {
            let line = match ev {
                Event::Exception { pc, cause, vaddr } => {
                    format!("CPU exception {cause} at {pc:#010x} (vaddr {vaddr:x?})")
                }
                Event::Break { pc, s, t } => format!("CPU break {s},{t} at {pc:#010x}"),
                Event::Reset => "chip reset".into(),
            };
            self.push_log(line);
        }
        let periph = self.board.machine.periph();
        let uart: Vec<u8> = periph.uart[0].tx_out.drain(..).collect();
        let usb: Vec<u8> = periph.usb_serial_jtag.tx_out.drain(..).collect();
        if let Some(prefix) = std::env::var_os("BITAXE_EMU_CONSOLE_DUMP") {
            use std::io::Write;
            for (ext, bytes) in [("uart", &uart), ("usb", &usb)] {
                let mut path = prefix.clone();
                path.push(format!(".{ext}"));
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                {
                    let _ = f.write_all(bytes);
                }
            }
        }
        self.console[0].extend(uart);
        self.console[1].extend(usb);
        for source in 0..2 {
            while let Some(pos) = self.console[source].iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.console[source].drain(..=pos).collect();
                let text = strip_ansi(String::from_utf8_lossy(&line).trim_end());
                if !text.is_empty() && self.first_copy(source, &text) {
                    self.push_log(text);
                }
            }
        }
        if let Some(cb) = self.logs.rtt() {
            let bytes = self.board.machine.rtt_read(cb, 0);
            if !bytes.is_empty() {
                for line in self.logs.decode(&bytes) {
                    self.push_log(line);
                }
            }
        }
        self.publish();
    }

    /// Whether `line` from console `source` is new: false when the other
    /// console already printed it.
    fn first_copy(&mut self, source: usize, line: &str) -> bool {
        let other = &mut self.unmatched[1 - source];
        if let Some(n) = other.get_mut(line) {
            *n -= 1;
            if *n == 0 {
                other.remove(line);
            }
            return false;
        }
        let own = &mut self.unmatched[source];
        // A console the other does not mirror would grow this without bound.
        if own.len() > 4096 {
            own.clear();
        }
        *own.entry(line.to_string()).or_insert(0) += 1;
        true
    }

    fn push_log(&mut self, line: String) {
        let mut s = self.snapshot.lock().unwrap();
        s.logs.push_back(line);
        s.log_count += 1;
        while s.logs.len() > LOG_LINES * 4 {
            s.logs.pop_front();
        }
    }

    fn drain_lines(&mut self) -> Vec<String> {
        let mut s = self.snapshot.lock().unwrap();
        s.logs.drain(..).collect()
    }

    fn publish(&mut self) {
        let state = self.display_state();
        let frame = if self.display_state != Some(state) {
            self.display_state = Some(state);
            self.display_generation += 1;
            let (rgba, width, height) = self.display_image();
            Some(Frame {
                rgba: Arc::new(rgba),
                width,
                height,
                generation: self.display_generation,
            })
        } else {
            None
        };
        let vr_temp = match &self.board.regulator {
            Regulator::Tps546(_) => Some(self.board.thermal.vr_temp),
            Regulator::Dac { .. } => None,
        };
        let b = &self.board;
        let mut s = self.snapshot.lock().unwrap();
        if let Some(frame) = frame {
            s.display = frame;
        }
        s.seconds = b.seconds();
        s.vcore = b.readings.vcore;
        s.input_v = b.readings.input_v;
        s.input_a = b.readings.input_a;
        s.asic_power = b.thermal.asic_power;
        s.chip_temps = b.thermal.chips.clone();
        s.vr_temp = vr_temp;
        s.fan_rpm = b.thermal.fans.clone();
        s.fan_duty = b.readings.fan_duty.clone();
        s.freq_mhz = b.chain.frequency_mhz();
        s.asic_running = !b.chain.in_reset;
        s.jobs = b.chain.jobs_received;
        s.nonces = b.chain.nonces_found;
    }

    /// Runs paced to the wall clock until [`Command::Quit`], calling `notify`
    /// after each published snapshot.
    pub fn run_realtime(&mut self, commands: Receiver<Command>, mut notify: impl FnMut()) {
        let slice = TICK_HZ / 100;
        let mut wall_start = Instant::now();
        let mut emu_start = self.board.machine.now;
        let mut last_rate = (
            Instant::now(),
            self.board.machine.icount(),
            self.board.machine.now,
        );
        loop {
            loop {
                match commands.try_recv() {
                    Ok(Command::BootButton(p)) => self.board.set_boot_button(p),
                    Ok(Command::Reset) => {
                        self.board.reset();
                        self.push_log("RESET button".into());
                    }
                    Ok(Command::Persist(on)) => {
                        self.board.flash_path = if on { self.flash_file.clone() } else { None };
                        if on && self.flash_file.is_none() {
                            self.push_log(
                                "WARN the flash comes from a CI build, it has no file to write back to"
                                    .into(),
                            );
                        }
                    }
                    Ok(Command::SaveDisplay(path)) => {
                        let line = match self.save_display_png(&path) {
                            Ok(()) => format!("Saved the display to {}", path.display()),
                            Err(e) => format!("ERROR {e}"),
                        };
                        self.push_log(line);
                    }
                    Ok(Command::Quit) | Err(TryRecvError::Disconnected) => return,
                    Err(TryRecvError::Empty) => break,
                }
            }
            let deadline = self.board.machine.now + slice;
            self.slice(deadline);
            // Speed and MIPS over about one second of wall time.
            let elapsed = last_rate.0.elapsed().as_secs_f64();
            if elapsed >= 1.0 {
                let mut s = self.snapshot.lock().unwrap();
                s.mips = (self.board.machine.icount() - last_rate.1) as f64 / elapsed / 1e6;
                s.speed = (self.board.machine.now - last_rate.2) as f64 / TICK_HZ as f64 / elapsed;
                drop(s);
                last_rate = (
                    Instant::now(),
                    self.board.machine.icount(),
                    self.board.machine.now,
                );
            }
            notify();
            // Do not run ahead of the wall clock; when behind by more than 0.5 s,
            // stop trying to catch up.
            let emu = Duration::from_secs_f64(
                (self.board.machine.now - emu_start) as f64 / TICK_HZ as f64,
            );
            let wall = wall_start.elapsed();
            if emu > wall {
                std::thread::sleep(emu - wall);
            } else if wall - emu > Duration::from_millis(500) {
                wall_start = Instant::now();
                emu_start = self.board.machine.now;
            }
        }
    }
}

/// `line` without its ANSI escape sequences (terminal colours).
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        // CSI: ESC [ parameters, then a final byte in @..~.
        if chars.clone().next() == Some('[') {
            chars.next();
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn strips_terminal_colours() {
        assert_eq!(
            super::strip_ansi("\x1b[0;31mE (316) octal_psram: no PSRAM\x1b[0m"),
            "E (316) octal_psram: no PSRAM"
        );
    }
}
