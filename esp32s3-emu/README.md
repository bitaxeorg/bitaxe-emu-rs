# esp32s3-emu

An ESP32-S3 emulator: the PRO_CPU is [rax](https://github.com/Georges760/rax/tree/feat/xtensa-esp32s3)'s
Xtensa LX7 interpreter (`rax::isa::xtensa`, with the ESP32-S3 core configuration), and this crate
models the rest of the chip: the memory map, the boot from the internal ROM, and the peripherals.
The board around the chip (bitaxe-emu-rs) talks to it through the peripherals.

## Running an image

```bash
cargo run --release -p esp32s3-emu --bin esp32s3-run -- \
  --rom esp32s3_rev0_rom.bin --flash <merged image> [--elf <app ELF>]
```

It prints the console (UART0 and USB-Serial-JTAG) on stdout and writes the RTT up-buffer 0 to a
file, then reports the exceptions, the unmodelled registers touched and, with `--profile`, the
opcodes executed. `ESP32S3_I2C_TRACE=1` traces the I2C command lists. The ROM image ships with
Espressif's QEMU (`pc-bios/esp32s3_rev0_rom.bin`). `--rom` defaults to the path in `ESP32S3_ROM`
at build time (the Nix package fetches the image and sets it), else to
`../upstream/qemu-espressif/pc-bios/esp32s3_rev0_rom.bin` next to the bitaxe-emu-rs checkout.

## Embedding

A host reaches the board side through `Esp32s3::periph()`: UART bytes (`uart[n].tx_out`,
`receive`), I2C devices (`i2c[n].attach`, the `I2cDevice` trait), GPIO levels (`gpio.set_input`,
`gpio.output`), ADC1 inputs (`sens.set_adc1_mv(channel, mv)`) and the LCD transfers
(`lcd_cam.out`, a queue of `LcdWrite { cmd, data }` to drain).

## Status (2026-11-20)

Claim classes as in rax's documentation policy.

| Part | Where | Claim |
|---|---|---|
| ESP32-S3 machine: ROM, SRAM with its I-bus and D-bus views, RTC memories, flash windows through the cache MMU, instruction-count time, WAITI fast-forward, function stubs | `src/machine/` | **Implemented** |
| Devices: SYSTEM and clocks, RTC_CNTL (reset causes, software resets, RTC time), interrupt matrix, UART0-2 (baud-paced, thresholds, RX timeout), USB-Serial-JTAG (serial side), TIMG0/1 (timers, MWDT interrupt stages, RTC calibration), SYSTIMER, eFuse, SPI1 + SPI NOR flash, EXTMEM + MMU, SHA (register mode), GPIO + IO_MUX (levels, interrupts, strapping), I2C0/1 master with an `I2cDevice` trait for board devices, RNG; read-back register files for SPI0, LEDC, RMT, PMS and other configuration-only blocks; the analog (`regi2c`, APB_SARADC, the RTC peripherals other than SENS) and radio blocks answer polls without being modelled | `src/devices/` | **Implemented**; register behaviour from QEMU's models ([spec](docs/qemu-devices.md)) and the TRM, with documented differences: real CPU clock selection, flash coherent with the cache, I2C and GPIO modelled |
| SAR ADC1 one-shot (SENS, 0x6000_8800..0x6000_8A00): registers read back, read-only status fields read 0; a rising edge of MEAS1_START_SAR completes a conversion at once (MEAS1_DONE_SAR set, MEAS1_DATA_SAR = `round(mv × 4095 / full_scale)` clamped to 4095, full scale 950/1250/1750/3100 mV for SAR_ATTEN1 0-3); with no pad in SAR1_EN_PAD (the calibration channel 15 does not fit the 12-bit field) the input is a 1100 mV reference, so the line calibration maps codes back to millivolts; the host sets each channel's input (`Peripherals::sens`, `Sens::set_adc1_mv`). ADC2, touch, temperature sensor, ULP and the RTC_CORE/APB_ADC completion interrupts are **Unsupported** (the one-shot driver sees DONE before it would wait for them) | `src/devices/sens.rs` | **Implemented**; **Unit-tested** (`esp32s3_adc1_oneshot_codes`, `esp32s3_rtc_periph_around_sens_stays_write_only`); register layout from the `esp32s3` PAC 0.37.0; the linear transfer function is a model, not calibrated against silicon (A10) |
| GDMA OUT channels: OUTLINK_START/RESTART read the descriptor chain from internal SRAM (D-bus, OUTLINK_ADDR completed with 0x3FC0_0000), OUT_DSCR_ERR for a descriptor or buffer outside SRAM or a CPU-owned descriptor under OUT_CHECK_OWNER, owner write-back (OUT_AUTO_WRBACK), FIFO status non-empty while data waits, OUT_DONE/OUT_EOF/OUT_TOTAL_EOF when the peripheral has sent the data (OUT_EOF at read time with OUT_EOF_MODE 0), OUT_EOF_DES_ADDR, OUT_STATE/OUT_DSCR*, interrupt sources 71-75. IN channels are register storage (sources 66-70 never rise). Only LCD_CAM consumes OUT data | `src/devices/gdma.rs`, `src/machine/bus.rs` (`DmaSram`) | **Implemented**; **Unit-tested** (`esp32s3_gdma_chain_drives_lcd_transfer`, `esp32s3_gdma_descriptor_errors`); register layout from the PAC; chain read in one step at OUTLINK_START, owner bits written back then (A9) |
| LCD_CAM, LCD side in I8080 mode: LCD_START runs the command phase (LCD_CMD, one or two words), the dummy cycles and the data phase (LCD_DOUT; all the GDMA data with LCD_ALWAYS_OUT_EN, else LCD_DOUT_CYCLELEN + 1 cycles) from the OUT channel whose OUT_PERI_SEL is LCD_CAM (5); one LCD_PCLK cycle per command word, dummy cycle and data byte (16-bit word with LCD_2BYTE_EN), LCD_PCLK from LCD_CLOCK (XTAL 40 MHz, PLL/2 or 160 MHz, fractional LCD_CLKM divider, LCD_CLKCNT_N, LCD_CLK_EQU_SYSCLK); at the end LCD_START clears, LCD_TRANS_DONE rises (source 24) and the transfer is queued for the host (`Peripherals::lcd_cam`, `LcdCam::out`, `LcdWrite { cmd, data }`); clearing LCD_START or setting LCD_RESET stops it after the cycles sent; LCD_UPDATE, LCD_RESET and LCD_AFIFO_RESET self-clear; byte and bit order options. RGB (DPI) mode, setup/hold cycles and the camera side are **Unsupported** | `src/devices/lcd_cam.rs` | **Implemented**; **Unit-tested** (`esp32s3_gdma_chain_drives_lcd_transfer`, `esp32s3_lcd_16bit_data_without_command`, `esp32s3_lcd_start_cleared_stops_after_sent_cycles`); not yet exercised by a firmware run (**Unknown** until an ST7789 board run shows frames) |
| Boot | `esp32s3-run` | **Boot-demonstrated**: the esp-miner-rs Bitaxe Gamma image boots through the ROM, the 2nd-stage bootloader (image SHA-256 check by the SHA accelerator) and the application, which runs its tasks (settings from NVS, WiFi access point stack, I2C devices, ASIC chain on UART1), with no unexpected exception |

The register behaviour of Espressif QEMU's ESP32-S3 device models is written up in
[docs/qemu-devices.md](docs/qemu-devices.md).

## Assumptions

| ID | Assumption | Basis | Dependent result | Stress test | Falsification probe | Status |
|---|---|---|---|---|---|---|
| A3 | Internal SRAM is one physical memory seen at 0x3FC8xxxx (D-bus) and 0x4037xxxx (I-bus) | ESP32-S3 TRM memory map; the `esp-hal` linker leaves a DRAM gap for IRAM code (`.rwdata_dummy`) | SRAM model with aliases | Code written through D-bus then executed through I-bus | TRM table, then a test image that writes code via D-bus and calls it via I-bus, on a real board | retained (to confirm against TRM) |
| A4 | An instruction-count time base is enough for the firmware's timing loops | GSC/S5L precedent; `esp-hal` uses SYSTIMER/TIMG for time | §4.3 time model | Busy-wait calibration against CCOUNT, `ets_delay_us`, watchdog margins | M3 examples' delays within tolerance; WDT doesn't fire during normal runs | retained |
| A6 | The ROM image can be used locally but not committed | No license file next to `pc-bios/esp32s3_rev0_rom.bin` | §4.6 two ROM modes | — | Espressif's published terms for ROM images (to look up) | retained |
| A7 | An ELF boot that recreates the 2nd-stage bootloader's state is enough for `esp-hal` apps | QEMU `-kernel` mode and `esp-hal`'s own startup code | P4 boot | Apps reading bootloader-written registers (cache config, RTC state, WDT state) | M3 images print as on hardware; compare the MMIO report against a full flash boot (P8) | retained |
| A9 | Reading a GDMA OUT chain at OUTLINK_START (data and owner write-back in one step) is indistinguishable for drivers that do not touch the buffers before the transfer ends | `esp-hal` 1.2.2 i8080 and DMA buffer code: descriptors are prepared before OUTLINK_START and handed back after LCD_START clears | GDMA model | A driver that appends descriptors or edits buffers while the channel runs | A firmware that streams (circular chains, OUTLINK_RESTART) shows stale data | retained |
| A10 | The SAR ADC1 code is linear in the input up to the full scale of the attenuation, and the calibration channel samples a 1100 mV reference | Requested model; `esp-hal` 1.2.2 line calibration assumes 1100 mV when the eFuses hold no calibration | SENS model | Real chips are non-linear near the ends of the range and their reference varies (1000-1200 mV) | Compare codes against a Bitaxe reading a known voltage | retained |
| A11 | On a 16-bit LCD bus each cycle carries the two bytes of a little-endian word, the lower address on `LCD_DATA[7:0]`; LCD_BYTE_ORDER swaps them | LCD_USER field descriptions of the `esp32s3` PAC 0.37.0; no capture yet | `LcdWrite::data` order | RGB565 colour buffers on a 16-bit bus | Logic-analyzer capture of a 16-bit i8080 board | retained |
| A12 | An i8080 transfer takes one LCD_PCLK cycle per command word, dummy cycle and data cycle, with no setup or hold cycles | LCD_USER/LCD_MISC field descriptions of the PAC; the requested timing (4 MHz, 6400 bytes ≈ 1.6 ms) | LCD transfer end time | LCD_VFK/VBK_CYCLELEN set to large values | Capture of WR on a real board | retained |
