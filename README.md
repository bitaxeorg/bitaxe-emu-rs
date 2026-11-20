# bitaxe-emu-rs

A Bitaxe emulator. The ESP32-S3 of the board runs the miner firmware, and the
rest of the board is modelled around it. A window shows the front of the board
with its display, buttons and fans, next to the board telemetry and the firmware
logs.

The workspace has two crates:

- [`esp32s3-emu`](esp32s3-emu/README.md): the ESP32-S3. Its processor is
  [rax](https://github.com/Georges760/rax/tree/feat/xtensa-esp32s3)'s Xtensa LX7
  interpreter; the crate models the memory map, the boot from the internal ROM
  and the peripherals, and has a headless runner, `esp32s3-run`. Nothing in it is
  specific to the Bitaxe.
- `bitaxe-emu` (the root crate): the boards around the chip and the window.

## Boards

Every board number the [esp-miner-rs](https://github.com/bitaxeorg/esp-miner-rs)
firmware builds for:

| Board | Number | Chips | Core regulator | Fans and temperatures | Display |
|---|---|---|---|---|---|
| Max | 102 | 1 × BM1397 | TPS40305 + DS4432 DAC, INA260, VCore on ADC | EMC2101, chip diode | OLED |
| Ultra | 201, 202, 205 | 1 × BM1366 | TPS40305 + DS4432 DAC, INA260, VCore on ADC | EMC2101, internal sensor | OLED |
| Ultra | 207 | 1 × BM1366 | TPS546D24A | EMC2101, chip diode | OLED |
| Hex | 302, 303 | 6 × BM1366, 3 domains | TPS546D24A, 12 V | EMC2302 (2 fans), 2 × TMP1075 | OLED |
| Supra | 400, 401 | 1 × BM1368 | TPS40305 + DS4432 DAC, INA260, VCore on ADC | EMC2101, internal sensor | OLED |
| Supra | 402, 403 | 1 × BM1368 | TPS546D24A | EMC2101, chip diode | OLED |
| Gamma | 600 to 603 | 1 × BM1370 | TPS546D24A | EMC2101, chip diode | OLED |
| GammaDuo | 650 | 2 × BM1370 | TPS546D24A | EMC2101, chip diode | OLED |
| SupraHex | 701, 702 | 6 × BM1368, 3 domains | TPS546D24A, 12 V | EMC2302 (2 fans), 2 × TMP1075 | OLED |
| GammaTurbo | 801 | 2 × BM1370 | TPS546D24A, 12 V, 2 phases | EMC2103, 2 chip diodes | OLED |
| NajaDuo | 1201 | 2 × BM1373, 2 domains | TPS546D24A, 12 V, 2 phases, chip power enable | EMC2103, 2 chip diodes | ST7789 LCD |
| GammaHex | 1300 | 6 × BM1370, 2 domains | TPS546D24A, 12 V, 4 phases, chip power enable | EMC2103, 2 chip diodes | ST7789 LCD |

The models:

| Part | Model |
|---|---|
| ESP32-S3 | `esp32s3-emu`: boots from its internal ROM through the 2nd-stage bootloader to the application, from the flash image |
| SSD1306 0.91" OLED, 128×32 (I2C 0x3C) | display RAM, addressing modes, remap, scan direction, start line, offset, invert |
| ST7789 1.9" LCD, 320×170 (8-bit parallel bus on LCD_CAM, with DMA) | frame memory, address window, MADCTL access order, RGB565 byte order, inversion, sleep and display on/off; reset, panel power and backlight lines |
| TPS546D24A (PMBus 0x24) | commands read back; IC_DEVICE_ID; VIN, VOUT, IOUT and temperature telemetry; output on/off |
| TPS40305 + DS4432 (0x48) + INA260 (0x40) | the output voltage from the DAC current into the feedback divider, PWR_EN on GPIO10; input voltage, current and power; the output voltage on ADC1 (GPIO2) |
| EMC2101 (0x4C), EMC2103 (0x2E), EMC2302 (0x2F), TMP1075 (0x4A, 0x4B) | register files; internal, diode and board temperatures; fan tachometers from the fan drives |
| BM13xx chain on UART1, NRST on GPIO1 | CHIP_ID enumeration, address assignment along the chain, registers, PLL frequency, version-rolling response format, jobs; nonces searched on the host within a small hash budget |
| Power and heat | chip power from voltage (per domain), frequency and activity; heatsinks with a fan-dependent thermal resistance; regulator and board temperatures; input power |
| BOOT and RESET buttons | GPIO0 and chip enable, clickable on the board picture |

## Running

The development environment is the flake (the nightly Rust of
`rust-toolchain.toml`, which BLIT needs, the Wayland libraries, the fonts and
`espflash`): `direnv allow` (the local `.envrc` holds `use flake`), or prefix
commands with `nix develop -c`.

```bash
cargo build --release
# Flash image of the firmware: bootloader, partition table and application.
espflash save-image --chip esp32s3 --merge --flash-size 16mb \
  --partition-table partitions.csv esp-miner-rs esp-miner-rs-bitaxe-601.bin
target/release/bitaxe-emu --flash esp-miner-rs-bitaxe-601.bin --elf esp-miner-rs
```

- `--board` picks the board, by number (`601`) or family name (`gamma`, the
  latest revision, as the firmware's features). Without it, the board number in
  the flash or ELF file name is used (`...bitaxe-601...`, as CI names its images).
- `--elf` gives the firmware's defmt logs (read from RTT) and is optional.
- `--rom` points to the ESP32-S3 ROM image (`esp32s3_rev0_rom.bin`, shipped with
  Espressif's QEMU). It defaults to the path in `ESP32S3_ROM` at build time
  (the Nix package sets it), else to `../upstream/qemu-espressif/pc-bios/`.
- `--persist` writes the flash back to the image when the window closes, keeping
  the settings the firmware saved.
- `--headless <seconds>` runs without a window and prints the logs, then the
  OLED as text; `--display-png <file>` also saves the display's image.

Click RESET to restart the chip. BOOT is held as long as the mouse button is.

Debugging aids, all optional: `BITAXE_EMU_SCRIPT="2.0:boot_down,4.6:boot_up,6:reset"`
presses buttons at given times in headless runs, `BITAXE_EMU_MMIO_LOG=1` prints
the last peripheral accesses, `BITAXE_EMU_PROFILE=1` samples the call chains,
`ESP32S3_I2C_TRACE=1` traces the I2C command lists.

## Installing with Nix

The flake also packages the emulator: `nix build` gives `result/bin/bitaxe-emu`
and `result/bin/esp32s3-run`, and `nix run github:bitaxeorg/bitaxe-emu-rs --
--flash <image>` runs it. The package fetches the ESP32-S3 ROM image, so
`--rom` is not needed.

On NixOS, with the system configuration in a flake:

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    bitaxe-emu = {
      url = "github:bitaxeorg/bitaxe-emu-rs";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { nixpkgs, bitaxe-emu, ... }: {
    nixosConfigurations.my-machine = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ./configuration.nix
        bitaxe-emu.nixosModules.default
        { programs.bitaxe-emu.enable = true; }
      ];
    };
  };
}
```

Or, without the module: `nixpkgs.overlays = [ bitaxe-emu.overlays.default ];`
then `environment.systemPackages = [ pkgs.bitaxe-emu ];` (the same overlay
works in Home Manager's `home.packages`). Both build with the system's nixpkgs.

rax is fetched from GitHub at the revision pinned in `nix/rax.nix`; after a
change of rax, push it, put the new commit in `rev`, empty `hash`, and
`nix build` prints the hash to write there. A change of the BLIT revision in `Cargo.toml` needs the same for
its hash in `nix/package.nix`.

## Limits

- WiFi is not emulated: the radio's registers answer the calibration polls, so
  the firmware starts its access point, but no packet goes in or out. Without a
  network there is no pool and no job, so the chips stay idle.
- Hashing on the host is far slower than the chips: at the usual ticket
  difficulties a nonce is rare.
- The interpreter runs at about 60-80 MIPS: the firmware runs below real time
  while it is busy, and the window shows the speed.
- The power, thermal and fan figures come from a simple model, not from
  measurements of each board.

## Licenses

The code is MIT or Apache-2.0. The board pictures in `assets/` are rendered from
the boards' KiCad projects by `assets/render-boards.sh`:

| Picture | KiCad project (revision) | Licence |
|---|---|---|
| `gamma-front.png` (600 to 603, and 650, which has no published design) | bitaxeorg/bitaxeGamma (601) | CERN-OHL-S-2.0 |
| `max-front.png` | bitaxeorg/bitaxeMax (v2.2) | CERN-OHL-S-2.0 |
| `ultra-205-front.png` (201, 202, 205) | bitaxeorg/bitaxeUltra (205) | CERN-OHL-S-2.0 |
| `ultra-207-front.png` | bitaxeorg/bitaxeUltra (207) | GPL-3.0 |
| `supra-401-front.png` (400, 401), `supra-403-front.png` (402, 403) | bitaxeorg/bitaxeSupra (401, 403) | CERN-OHL-S-2.0 |
| `hex-front.png` (302, 303) | bitaxeorg/ultraHex (303) | MIT |
| `suprahex-front.png` (701, 702) | bitaxeorg/supraHex (701) | GPL-3.0 |
| `gammaturbo-front.png` | bitaxeorg/BitaxeGT (800x) | CERN-OHL-S-2.0 |
| `najaduo-front.png` | bitaxeorg/naja-duo (1201) | CERN-OHL-S-2.0 |
| `gammahex-front.png` | bitaxeorg/BitaxeGammaHex (1300) | CERN-OHL-S-2.0 |

The pictures are built into the binary. Where a design has no display module or
fan in its 3D models, the window places them where they mount.
