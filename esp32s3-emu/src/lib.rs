//! ESP32-S3 SoC emulator: the PRO_CPU is rax's Xtensa LX7 interpreter, around
//! it the memory map (internal ROM and SRAM, RTC memories, flash through the
//! cache MMU) and the peripheral models. [`machine::Esp32s3`] boots a flash
//! image from the internal ROM; the board around the chip talks to it through
//! the peripherals (I2C devices, GPIO levels, UART bytes, LCD bus transfers,
//! ADC inputs).

pub mod devices;
pub mod machine;
pub mod net;

/// Internal ROM image used when none is given: the path in `ESP32S3_ROM` at
/// build time, else the path below, relative to this crate.
pub const DEFAULT_ROM: &str = match option_env!("ESP32S3_ROM") {
    Some(path) => path,
    None => concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../upstream/qemu-espressif/pc-bios/esp32s3_rev0_rom.bin"
    ),
};
