//! Runs a flash image on the ESP32-S3 machine.
//!
//! The console output (UART0 and USB-Serial-JTAG) goes to stdout, and the RTT
//! up-buffer 0 to a file when the ELF of the application is given (to find the
//! `_SEGGER_RTT` control block). At the end it reports the exceptions, the
//! unmodelled peripheral registers that were touched, and the opcodes executed.

use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use clap::Parser;
use esp32s3_emu::devices::efuse::Efuse;
use esp32s3_emu::devices::{PeripheralsConfig, STRAP_SPI_BOOT};
use esp32s3_emu::machine::{Esp32s3, Esp32s3Config, Event, TICK_HZ};
use rax::isa::xtensa::Profile;
use rax::isa::xtensa::generated::opcodes::{MNEMONICS, OPCODE_COUNT};

#[derive(Parser)]
#[command(about = "Run an ESP32-S3 flash image")]
struct Args {
    /// Internal ROM image (esp32s3_rev0_rom.bin).
    #[arg(long, default_value = esp32s3_emu::DEFAULT_ROM)]
    rom: PathBuf,
    /// Flash image (bootloader, partition table and application).
    #[arg(long)]
    flash: PathBuf,
    /// ELF of the application, to read its RTT output.
    #[arg(long)]
    elf: Option<PathBuf>,
    /// Where to write the RTT up-buffer 0 bytes.
    #[arg(long, default_value = "rtt.bin")]
    rtt_out: PathBuf,
    /// Emulated seconds to run.
    #[arg(long, default_value_t = 5.0)]
    seconds: f64,
    /// Count the executed opcodes and print the profile.
    #[arg(long)]
    profile: bool,
    /// Stop at the first exception.
    #[arg(long)]
    stop_on_exception: bool,
}

fn rtt_address(elf: &[u8]) -> Option<u32> {
    let obj = goblin::elf::Elf::parse(elf).ok()?;
    obj.syms
        .iter()
        .find(|s| obj.strtab.get_at(s.st_name) == Some("_SEGGER_RTT"))
        .map(|s| s.st_value as u32)
}

fn main() {
    let args = Args::parse();
    let rom = std::fs::read(&args.rom).expect("ROM image");
    let flash = std::fs::read(&args.flash).expect("flash image");
    let rtt = args
        .elf
        .as_ref()
        .and_then(|p| rtt_address(&std::fs::read(p).expect("ELF")));
    let mut m = Esp32s3::new(Esp32s3Config {
        rom,
        periph: PeripheralsConfig {
            flash,
            efuse: Efuse::with_mac([0x7c, 0xdf, 0xa1, 0x00, 0x00, 0x01]),
            strap: STRAP_SPI_BOOT,
        },
    });
    if args.profile {
        m.cpu.profile = Some(Box::default());
    }
    let mut rtt_file = std::fs::File::create(&args.rtt_out).expect("RTT output");
    let end = (args.seconds * TICK_HZ as f64) as u64;
    let step = TICK_HZ / 100;
    let started = Instant::now();
    let mut exceptions = 0u64;
    let mut stdout = std::io::stdout();
    'run: while m.now < end {
        let deadline = (m.now + step).min(end);
        while m.now < deadline {
            match m.run_until(deadline) {
                None => {}
                Some(Event::Exception { pc, cause, vaddr }) => {
                    exceptions += 1;
                    if exceptions <= 20 {
                        eprintln!(
                            "[{:.6}] exception {cause} at {pc:#010x} vaddr {:?} (a0 {:#010x} a1 {:#010x})",
                            m.now as f64 / TICK_HZ as f64,
                            vaddr.map(|v| format!("{v:#010x}")),
                            m.cpu.a(0),
                            m.cpu.a(1)
                        );
                    }
                    if args.stop_on_exception {
                        break 'run;
                    }
                }
                Some(Event::Break { pc, s, t }) => {
                    eprintln!(
                        "[{:.6}] break {s},{t} before {pc:#010x}",
                        m.now as f64 / TICK_HZ as f64
                    );
                }
                Some(Event::Reset) => eprintln!("[{:.6}] reset", m.now as f64 / TICK_HZ as f64),
            }
        }
        let console: Vec<u8> = m.periph().uart[0].tx_out.drain(..).collect();
        let usb: Vec<u8> = m.periph().usb_serial_jtag.tx_out.drain(..).collect();
        // Nothing is attached to the LCD bus.
        m.periph().lcd_cam.out.clear();
        stdout.write_all(&console).unwrap();
        stdout.write_all(&usb).unwrap();
        stdout.flush().unwrap();
        if let Some(cb) = rtt {
            let bytes = m.rtt_read(cb, 0);
            rtt_file.write_all(&bytes).unwrap();
        }
    }
    let wall = started.elapsed().as_secs_f64();
    eprintln!(
        "\n{:.3} s emulated in {wall:.3} s: {} instructions ({:.1} MIPS), pc {:#010x}, {exceptions} exceptions",
        m.now as f64 / TICK_HZ as f64,
        m.cpu.icount,
        m.cpu.icount as f64 / wall / 1e6,
        m.cpu.pc
    );
    if !m.soc.unmodelled.is_empty() {
        eprintln!("unmodelled peripheral registers (reads, writes):");
        for (addr, (r, w)) in &m.soc.unmodelled {
            eprintln!("  {addr:#010x}: {r} {w}");
        }
    }
    if let Some(p) = m.cpu.profile.as_deref() {
        print_profile(p);
    }
}

fn print_profile(p: &Profile) {
    let mut rows: Vec<(usize, u64)> = (0..OPCODE_COUNT)
        .map(|i| (i, p.executed[i]))
        .filter(|r| r.1 > 0)
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1));
    eprintln!(
        "{} opcodes executed, {} undecoded words:",
        rows.len(),
        p.undecoded
    );
    for (i, n) in rows {
        let unimpl = p.unimplemented[i];
        eprintln!(
            "  {:<24} {n:>12}{}",
            MNEMONICS[i],
            if unimpl > 0 {
                format!("  ({unimpl} unimplemented)")
            } else {
                String::new()
            }
        );
    }
}
