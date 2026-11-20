//! Bitaxe emulator: a Bitaxe board whose ESP32-S3 (`esp32s3-emu`) runs the miner
//! firmware, with its board devices modelled, shown front side up with its
//! display.

mod board;
mod emu;
mod gui;
mod logs;

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser)]
#[command(about = "Bitaxe emulator")]
struct Args {
    /// The board: its number (601, 1201...) or family name (max, ultra, hex,
    /// supra, gamma, gamma-duo, supra-hex, gamma-turbo, naja-duo, gamma-hex).
    /// By default, the number in the flash or ELF file name (`...bitaxe-601...`).
    #[arg(long)]
    board: Option<String>,
    /// Flash image: bootloader, partition table and application
    /// (`espflash save-image --merge`).
    #[arg(long)]
    flash: PathBuf,
    /// ELF of the application, for its defmt logs over RTT.
    #[arg(long)]
    elf: Option<PathBuf>,
    /// ESP32-S3 internal ROM image.
    #[arg(long, default_value = esp32s3_emu::DEFAULT_ROM)]
    rom: PathBuf,
    /// MAC address of the chip.
    #[arg(long, default_value = "7c:df:a1:0b:17:ae")]
    mac: String,
    /// Write the flash back to the image file when the emulator stops (keeps the
    /// settings the firmware saved).
    #[arg(long)]
    persist: bool,
    /// Run without window for this many emulated seconds, printing the logs.
    #[arg(long)]
    headless: Option<f64>,
    /// After a headless run, save the display's image to this PNG file.
    #[arg(long, alias = "oled-png")]
    display_png: Option<PathBuf>,
    /// Decode the RTT up-buffer 0 of a dump of the data RAM (from 0x3fc88000) and exit.
    #[arg(long, hide = true)]
    decode_rtt: Option<PathBuf>,
}

fn parse_mac(s: &str) -> [u8; 6] {
    let bytes: Vec<u8> = s
        .split(':')
        .filter_map(|b| u8::from_str_radix(b, 16).ok())
        .collect();
    bytes
        .try_into()
        .unwrap_or([0x7c, 0xdf, 0xa1, 0x0b, 0x17, 0xae])
}

fn main() {
    let args = Args::parse();
    if let (Some(dump), Some(elf)) = (&args.decode_rtt, &args.elf) {
        decode_rtt_dump(&std::fs::read(dump).unwrap(), &std::fs::read(elf).unwrap());
        return;
    }
    let rom = std::fs::read(&args.rom)
        .unwrap_or_else(|e| panic!("ROM image {}: {e}", args.rom.display()));
    let flash = std::fs::read(&args.flash)
        .unwrap_or_else(|e| panic!("flash image {}: {e}", args.flash.display()));
    let elf = args
        .elf
        .as_ref()
        .map(|p| std::fs::read(p).unwrap_or_else(|e| panic!("ELF {}: {e}", p.display())));
    let spec = match &args.board {
        Some(name) => board::spec::find(name),
        None => board::spec::from_file_name(&args.flash)
            .or_else(|| args.elf.as_deref().and_then(board::spec::from_file_name)),
    };
    let Some(spec) = spec else {
        eprintln!("which board? --board {}", board::spec::list());
        std::process::exit(2);
    };
    let mut board = board::Board::new(spec, rom, flash, parse_mac(&args.mac));
    if args.persist {
        board.flash_path = Some(args.flash.clone());
    }
    let mut emulator = emu::Emulator::new(board, elf.as_deref());
    if std::env::var_os("BITAXE_EMU_MMIO_LOG").is_some() {
        emulator.board.machine.soc.mmio_log = Some(Default::default());
        emulator.board.machine.soc.mmio_log_cap = 1 << 24;
    }
    match args.headless {
        Some(seconds) => {
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
            }
            print!("{}", emulator.oled_ascii());
            if let Some(path) = &args.display_png {
                save_display_png(&mut emulator, path);
            }
            let cpu = &emulator.board.machine.cpu;
            eprintln!(
                "pc {:#010x} a0 {:#010x} a1 {:#010x} ps {:#x} intenable {:#x} interrupt {:#x}",
                cpu.pc,
                cpu.a(0),
                cpu.a(1),
                cpu.ps(),
                cpu.sr[228],
                cpu.sr[226]
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
            eprintln!("frames (a0): {}", chain.join(" "));
            if let Some(log) = emulator.board.machine.soc.mmio_log.as_ref() {
                let mut last = None;
                for &(a, v, w) in log.iter() {
                    if last != Some((a, v, w)) {
                        eprintln!("mmio {} {a:#010x} {v:#010x}", if w { "W" } else { "R" });
                    }
                    last = Some((a, v, w));
                }
            }
        }
        None => gui::run(emulator),
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

/// Saves the display's image, scaled up (4x for the OLED, 2x for the LCD).
fn save_display_png(emulator: &mut emu::Emulator, path: &std::path::Path) {
    let (rgba, w, h) = emulator.display_image();
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
    let file = std::fs::File::create(path).expect("PNG file");
    let mut enc = png::Encoder::new(
        std::io::BufWriter::new(file),
        (w * scale) as u32,
        (h * scale) as u32,
    );
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&rgb).unwrap();
}
