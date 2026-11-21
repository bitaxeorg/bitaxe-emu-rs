//! Bitaxe emulator: a Bitaxe board whose ESP32-S3 (`esp32s3-rax`) runs the miner
//! firmware, with its board devices modelled, shown front side up with its
//! display.

mod board;
mod emu;
mod firmware;
mod github;
mod gui;
mod logs;
mod nvs;

use std::path::PathBuf;

use clap::builder::BoolishValueParser;
use clap::{Parser, ValueEnum};

use crate::firmware::{Choice, Source};

#[derive(Clone, Copy, ValueEnum)]
enum Ci {
    /// The esp-miner-rs build for the board.
    #[value(name = "esp-miner-rs")]
    Rs,
    /// The ESP-Miner CI factory image.
    #[value(name = "esp-miner")]
    Idf,
    /// The board's factory image of an ESP-Miner release.
    #[value(name = "esp-miner-release")]
    Release,
}

/// Every option can also come from its environment variable. Without a flash
/// image or a CI build, the emulator starts with the choices made last in the
/// window, whose menus change them while it runs.
#[derive(Parser)]
#[command(about = "Bitaxe emulator")]
struct Args {
    /// The board: its number (601, 1201...) or family name (max, ultra, hex,
    /// supra, gamma, gamma-duo, supra-hex, gamma-turbo, naja-duo, gamma-hex).
    /// By default, the number in the flash or ELF file name (`...bitaxe-601...`).
    #[arg(long, env = "BITAXE_EMU_BOARD")]
    board: Option<String>,
    /// Flash image: bootloader, partition table and application
    /// (`espflash save-image --merge`).
    #[arg(long, env = "BITAXE_EMU_FLASH")]
    flash: Option<PathBuf>,
    /// ELF of the application, for its defmt logs over RTT. By default, the
    /// flash image's path with the `.elf` extension, when it exists.
    #[arg(long, env = "BITAXE_EMU_ELF")]
    elf: Option<PathBuf>,
    /// Run the newest CI build of a firmware (needs a GitHub token: GH_TOKEN,
    /// GITHUB_TOKEN or `gh auth login`), or an ESP-Miner release.
    #[arg(long, env = "BITAXE_EMU_FIRMWARE", conflicts_with = "flash")]
    firmware: Option<Ci>,
    /// Branch of the CI build (main for esp-miner-rs, master for ESP-Miner), or
    /// tag of the ESP-Miner release (latest by default).
    #[arg(long, env = "BITAXE_EMU_BRANCH")]
    branch: Option<String>,
    /// ESP32-S3 internal ROM image. By default, the one of the build.
    #[arg(long, env = "BITAXE_EMU_ROM")]
    rom: Option<PathBuf>,
    /// MAC address of the chip (default 7c:df:a1:0b:17:ae).
    #[arg(long, env = "BITAXE_EMU_MAC")]
    mac: Option<String>,
    /// Write the flash back to the image file when the emulator stops (keeps the
    /// settings the firmware saved); `--persist false` turns it off.
    #[arg(long, env = "BITAXE_EMU_PERSIST", num_args = 0..=1, default_missing_value = "true",
          value_parser = BoolishValueParser::new())]
    persist: Option<bool>,
    /// Run without window for this many emulated seconds, printing the logs.
    #[arg(long, env = "BITAXE_EMU_HEADLESS")]
    headless: Option<f64>,
    /// Where to save the display's image as PNG: after a headless run, or with
    /// the window's Save button.
    #[arg(long, env = "BITAXE_EMU_DISPLAY_PNG", alias = "oled-png")]
    display_png: Option<PathBuf>,
    /// SSID of the Wi-Fi access point the emulator offers to the chip.
    #[arg(long, env = "BITAXE_EMU_WIFI_SSID", default_value = "bitaxe-emu")]
    wifi_ssid: String,
    /// WPA2-Personal passphrase of that access point (8 to 63 characters); empty
    /// for an open network.
    #[arg(long, env = "BITAXE_EMU_WIFI_PASS", default_value = "bitaxe-emu", value_parser = parse_passphrase)]
    wifi_pass: String,
    /// Host port (on 127.0.0.1) forwarded to the chip's web server, through the
    /// emulated access point or the chip's own; 0 for none.
    #[arg(long, env = "BITAXE_EMU_HTTP_PORT", default_value_t = 8080)]
    http_port: u16,
    /// Clear ESP-Miner's factory self-test flag in the flash's NVS before boot (the
    /// emulated chips cannot pass the self-test, which counts nonces).
    #[arg(long, env = "BITAXE_EMU_SKIP_SELF_TEST")]
    skip_self_test: bool,
    /// No Wi-Fi network around the chip.
    #[arg(long, env = "BITAXE_EMU_NO_WIFI")]
    no_wifi: bool,
    /// Log every Wi-Fi frame.
    #[arg(long, env = "BITAXE_EMU_WIFI_TRACE")]
    wifi_trace: bool,
    /// Decode the RTT up-buffer 0 of a dump of the data RAM (from 0x3fc88000) and exit.
    #[arg(long, env = "BITAXE_EMU_DECODE_RTT", hide = true)]
    decode_rtt: Option<PathBuf>,
}

/// A WPA2 passphrase: 8 to 63 printable ASCII characters, or empty.
fn parse_passphrase(s: &str) -> Result<String, String> {
    let printable = s.bytes().all(|b| (0x20..0x7f).contains(&b));
    if s.is_empty() || (8..=63).contains(&s.len()) && printable {
        Ok(s.into())
    } else {
        Err("a WPA2 passphrase has 8 to 63 printable ASCII characters (empty: open)".into())
    }
}

fn main() {
    let args = Args::parse();
    if let (Some(dump), Some(elf)) = (&args.decode_rtt, &args.elf) {
        decode_rtt_dump(&std::fs::read(dump).unwrap(), &std::fs::read(elf).unwrap());
        return;
    }
    emu::SKIP_SELF_TEST.store(args.skip_self_test, std::sync::atomic::Ordering::Relaxed);
    let _ = emu::NETWORK.set((!args.no_wifi).then(|| esp32s3_rax::net::NetworkConfig {
        ssid: args.wifi_ssid.clone(),
        passphrase: (!args.wifi_pass.is_empty()).then(|| args.wifi_pass.clone()),
        trace: args.wifi_trace,
        forwards: if args.http_port == 0 {
            Vec::new()
        } else {
            vec![(args.http_port, 80)]
        },
        ..Default::default()
    }));
    let saved = Choice::load().unwrap_or_else(Choice::default_choice);
    let source = match (&args.flash, args.firmware) {
        (Some(flash), _) => Source::File {
            flash: flash.clone(),
            elf: args.elf.clone().or_else(|| firmware::elf_beside(flash)),
        },
        (None, Some(Ci::Rs)) => Source::EspMinerRs {
            branch: args
                .branch
                .clone()
                .unwrap_or_else(|| firmware::RS_BRANCH.into()),
        },
        (None, Some(Ci::Idf)) => Source::EspMiner {
            branch: args
                .branch
                .clone()
                .unwrap_or_else(|| firmware::IDF_BRANCH.into()),
        },
        (None, Some(Ci::Release)) => Source::EspMinerRelease {
            tag: args
                .branch
                .clone()
                .unwrap_or_else(|| firmware::LATEST.into()),
        },
        (None, None) => saved.source.clone(),
    };
    let named = match &args.board {
        Some(name) => {
            let Some(spec) = board::spec::find(name) else {
                eprintln!("unknown board {name}: --board {}", board::spec::list());
                std::process::exit(2);
            };
            Some(spec)
        }
        None => args
            .flash
            .as_deref()
            .and_then(board::spec::from_file_name)
            .or_else(|| args.elf.as_deref().and_then(board::spec::from_file_name)),
    };
    let mac = match &args.mac {
        Some(text) => firmware::parse_mac(text).unwrap_or_else(|| {
            eprintln!("bad MAC address {text}: write it 7c:df:a1:0b:17:ae");
            std::process::exit(2);
        }),
        None => saved.mac,
    };
    let choice = Choice {
        board: named.unwrap_or(saved.board),
        source,
        rom: args.rom.clone().unwrap_or(saved.rom),
        mac,
        persist: args.persist.unwrap_or(saved.persist),
    };
    let Some(seconds) = args.headless else {
        choice.save();
        gui::run(gui::Launch {
            choice,
            display_png: args.display_png.clone(),
        });
        return;
    };
    if named.is_none() && args.flash.is_some() {
        eprintln!("which board? --board {}", board::spec::list());
        std::process::exit(2);
    }
    let rom = std::fs::read(&choice.rom).unwrap_or_else(|e| {
        eprintln!("ROM image {}: {e}", choice.rom.display());
        std::process::exit(1);
    });
    let image = choice
        .source
        .load(choice.board, &mut |line| eprintln!("{line}"))
        .unwrap_or_else(|e| {
            eprintln!("ERROR {e}");
            std::process::exit(1);
        });
    let mut board = board::Board::new(choice.board, rom, image.flash, choice.mac);
    if choice.persist {
        board.flash_path = image.path;
    }
    let mut emulator = emu::Emulator::new(board, image.elf.as_deref());
    if let Some(range) = std::env::var("BITAXE_EMU_WATCH").ok().and_then(|v| {
        let (a, n) = v.split_once(':').unwrap_or((&v, "4"));
        let a = u32::from_str_radix(a.trim_start_matches("0x"), 16).ok()?;
        Some(a..a + n.parse::<u32>().ok()?)
    }) {
        emulator.board.machine.soc.watch = Some(range);
    }
    if let Some(from) = std::env::var("BITAXE_EMU_BREAK_FROM")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
    {
        emulator.board.machine.break_from = (from * esp32s3_rax::machine::TICK_HZ as f64) as u64;
    }
    for pc in std::env::var("BITAXE_EMU_BREAK")
        .unwrap_or_default()
        .split(',')
    {
        if let Ok(pc) = u32::from_str_radix(pc.trim_start_matches("0x"), 16) {
            emulator.board.machine.add_breakpoint(pc);
        }
    }
    if let Some(range) = std::env::var("BITAXE_EMU_STACK_WATCH").ok().and_then(|v| {
        let (lo, hi) = v.split_once(':')?;
        let parse = |x: &str| u32::from_str_radix(x.trim_start_matches("0x"), 16).ok();
        Some(parse(lo)?..parse(hi)?)
    }) {
        emulator.board.machine.soc.stack_watch = Some(range);
    }
    if std::env::var_os("BITAXE_EMU_MMIO_LOG").is_some() {
        emulator.board.machine.soc.mmio_log = Some(Default::default());
        emulator.board.machine.soc.mmio_log_cap = 1 << 24;
    }
    emulator.run_headless(seconds);
    emulator.board.save_flash();
    if let Some(samples) = emulator.pc_samples.as_ref() {
        let mut v: Vec<_> = samples.iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        // Callers aggregated: count by each function address of the chains.
        let mut by_addr = std::collections::BTreeMap::new();
        for (chain, n) in samples {
            let mut seen = std::collections::BTreeSet::new();
            for a in chain {
                if seen.insert(*a) {
                    *by_addr.entry(*a).or_insert(0u64) += n;
                }
            }
        }
        let mut v: Vec<_> = by_addr.into_iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        for (a, n) in v.iter().take(40) {
            eprintln!("pc-sample {a:#010x} {n}");
        }
        // The PCs themselves, without their callers.
        let mut leaves = std::collections::BTreeMap::new();
        for (chain, n) in samples {
            *leaves.entry(chain[0]).or_insert(0u64) += n;
        }
        let mut v: Vec<_> = leaves.into_iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        for (a, n) in v.iter().take(40) {
            eprintln!("pc-leaf {a:#010x} {n}");
        }
    }
    print!("{}", emulator.oled_ascii());
    if let Some(path) = &args.display_png
        && let Err(e) = emulator.save_display_png(path)
    {
        eprintln!("{e}");
    }
    {
        // the interrupt sources routed to PRO_CPU, and their levels
        let periph = &mut emulator.board.machine.soc.periph;
        let levels = periph.source_levels();
        let mut routed = Vec::new();
        for src in 0..esp32s3_rax::devices::intmatrix::SOURCES {
            let line = periph.intmatrix.read((src * 4) as u32, &levels) & 0x1f;
            if line != 6 {
                routed.push(format!(
                    "{src}>{line}{}",
                    if levels[src] { "!" } else { "" }
                ));
            }
        }
        eprintln!("PRO_CPU sources: {}", routed.join(" "));
    }
    for (name, cpu) in [
        ("PRO_CPU", &emulator.board.machine.cpu),
        ("APP_CPU", &emulator.board.machine.app),
    ] {
        eprintln!(
            "{name} pc {:#010x} a0 {:#010x} a1 {:#010x} ps {:#x} intenable {:#x} interrupt {:#x} waiting {}",
            cpu.pc,
            cpu.a(0),
            cpu.a(1),
            cpu.ps(),
            cpu.sr[228],
            cpu.sr[226],
            cpu.waiting
        );
        // Live call chain: a0 of each frame of the register windows.
        let wb = cpu.sr[72];
        let ws = cpu.sr[73];
        let mut chain = Vec::new();
        for k in 0..16u32 {
            let frame = (wb + 16 - k) % 16;
            if ws & (1 << frame) != 0 {
                let a0 = cpu.ar[(frame * 4) as usize];
                chain.push(format!("{:#010x}", (a0 & 0x3fff_ffff) | 0x4000_0000));
            }
        }
        eprintln!("{name} frames (a0): {}", chain.join(" "));
        let regs: Vec<String> = (2..8).map(|i| format!("a{i} {:#010x}", cpu.a(i))).collect();
        eprintln!("{name} {}", regs.join(" "));
    }
    // Memory dumps: "address:length:file,...".
    for item in std::env::var("BITAXE_EMU_DUMP")
        .unwrap_or_default()
        .split(',')
    {
        let mut parts = item.splitn(3, ':');
        let (Some(addr), Some(len), Some(file)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let number = |s: &str| {
            s.strip_prefix("0x")
                .map_or_else(|| s.parse().ok(), |h| u32::from_str_radix(h, 16).ok())
        };
        let (Some(addr), Some(len)) = (number(addr), number(len)) else {
            continue;
        };
        let bytes: Vec<u8> = (0..len)
            .map(|i| {
                emulator
                    .board
                    .machine
                    .soc
                    .read_bytes(addr.wrapping_add(i), 1)
                    .map_or(0, |b| b[0])
            })
            .collect();
        match std::fs::write(file, bytes) {
            Ok(()) => eprintln!("dumped {len:#x} bytes at {addr:#010x} to {file}"),
            Err(e) => eprintln!("{file}: {e}"),
        }
    }
    if let Some((sp, chain)) = emulator.board.machine.soc.stack_low.as_ref() {
        let chain: Vec<String> = chain
            .iter()
            .map(|(a, s)| format!("{a:#010x}@{s:#010x}"))
            .collect();
        eprintln!("stack low {sp:#010x}: {}", chain.join(" "));
    }
    for hit in &emulator.board.machine.break_log {
        let regs: Vec<String> = hit.regs.iter().map(|r| format!("{r:#010x}")).collect();
        let chain: Vec<String> = hit
            .chain
            .iter()
            .map(|(a, s)| format!("{a:#010x}@{s:#010x}"))
            .collect();
        eprintln!(
            "break {:.6} s core {} a2.. {} chain {}",
            hit.time as f64 / esp32s3_rax::machine::TICK_HZ as f64,
            hit.core,
            regs.join(" "),
            chain.join(" ")
        );
    }
    let soc = &emulator.board.machine.soc;
    for (i, &(a, v, pc)) in soc.watch_log.iter().enumerate() {
        let (sp, chain) = soc.watch_chains.get(i).cloned().unwrap_or_default();
        let chain: Vec<String> = chain
            .iter()
            .map(|(a, s)| format!("{a:#010x}@{s:#010x}"))
            .collect();
        eprintln!(
            "watch W {a:#010x} {v:#010x} pc {pc:#010x} sp {sp:#010x} chain {}",
            chain.join(" ")
        );
    }
    if let Some(log) = emulator.board.machine.soc.mmio_log.as_ref() {
        let mut last = None;
        for &(a, v, w, pc) in log.iter() {
            if last != Some((a, v, w, pc)) {
                eprintln!(
                    "mmio {} {a:#010x} {v:#010x} pc {pc:#010x}",
                    if w { "W" } else { "R" }
                );
            }
            last = Some((a, v, w, pc));
        }
    }
}

fn decode_rtt_dump(dram: &[u8], elf: &[u8]) {
    const BASE: u32 = 0x3fc8_8000;
    let mut logs = logs::Logs::new(Some(elf));
    let Some(cb) = logs.rtt() else { return };
    let word = |addr: u32| {
        let o = (addr - BASE) as usize;
        u32::from_le_bytes(dram[o..o + 4].try_into().unwrap())
    };
    let desc = cb + 24;
    let (buf, size, wr) = (word(desc + 4), word(desc + 8), word(desc + 12));
    eprintln!("RTT buffer {buf:#x} size {size} write offset {wr}");
    let o = (buf - BASE) as usize;
    for line in logs.decode(&dram[o..o + wr as usize]) {
        println!("{line}");
    }
}
