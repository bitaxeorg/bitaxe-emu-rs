//! Unit tests of the ESP32-S3 peripheral models, driven through the bus.

use super::bus::{SRAM_D_BASE, Soc};
use crate::devices::efuse::{self, Efuse};
use crate::devices::gdma::{OUT_DONE, OUT_DSCR_ERR, OUT_EOF, OUT_TOTAL_EOF};
use crate::devices::lcd_cam::LcdWrite;
use crate::devices::{Peripherals, PeripheralsConfig, STRAP_SPI_BOOT, TICK_HZ, source};
use rax::isa::xtensa::XtensaBus;

fn soc() -> Soc {
    let periph = Peripherals::new(PeripheralsConfig {
        flash: Vec::new(),
        psram: 0,
        efuse: Efuse::new([0; efuse::WORDS]),
        strap: STRAP_SPI_BOOT,
    });
    Soc::new(Vec::new(), periph)
}

fn write(soc: &mut Soc, addr: u32, value: u32) {
    soc.write32(addr, value).expect("mapped register");
}

fn read(soc: &mut Soc, addr: u32) -> u32 {
    soc.read32(addr).expect("mapped register")
}

/// Moves the bus clock to `ticks` and advances the peripherals to it.
fn advance(soc: &mut Soc, ticks: u64) {
    soc.begin_slice(ticks, 0);
    soc.periph.update(ticks);
}

// ---- SAR ADC1 (SENS) ----

const SENS: u32 = 0x6000_8800;
const SAR_READER1_CTRL: u32 = SENS;
const SAR_MEAS1_CTRL2: u32 = SENS + 0x0c;
const SAR_ATTEN1: u32 = SENS + 0x14;
const SAR_SLAVE_ADDR1: u32 = SENS + 0x40;
const MEAS1_DATA: u32 = 0xffff;
const MEAS1_DONE: u32 = 1 << 16;
const MEAS1_START: u32 = 1 << 17;
const SAR1_EN_PAD_SHIFT: u32 = 19;

/// The one-shot conversion of `pads` (SAR1_EN_PAD bitmap): MEAS1_CTRL2 after
/// the rising edge of MEAS1_START_SAR.
fn adc1_convert(soc: &mut Soc, pads: u32) -> u32 {
    let pad_field = pads << SAR1_EN_PAD_SHIFT;
    write(soc, SAR_MEAS1_CTRL2, pad_field);
    assert_eq!(
        read(soc, SAR_MEAS1_CTRL2) & MEAS1_DONE,
        0,
        "done before start"
    );
    write(soc, SAR_MEAS1_CTRL2, pad_field | MEAS1_START);
    read(soc, SAR_MEAS1_CTRL2)
}

#[test]
fn esp32s3_adc1_oneshot_codes() {
    let mut soc = soc();
    // GPIO2 is ADC1 channel 1.
    soc.periph.sens.set_adc1_mv(1, 1150);
    assert_eq!(read(&mut soc, SAR_READER1_CTRL), 0x2004_0002, "reset value");
    assert_eq!(read(&mut soc, SAR_ATTEN1), 0xffff_ffff, "reset value");
    // Attenuation 3 (11 dB, full scale 3100 mV) on channel 1 and on the
    // calibration channel 15.
    write(&mut soc, SAR_ATTEN1, (3 << 2) | (3 << 30));
    assert_eq!(read(&mut soc, SAR_ATTEN1), (3 << 2) | (3 << 30));
    assert_eq!(
        read(&mut soc, SAR_SLAVE_ADDR1) & 0x3fc0_0000,
        0,
        "SAR_SARADC_MEAS_STATUS idle"
    );

    // The line of ADC1 at attenuation 3 reads 1123 at 850 mV; 1150 mV sits at
    // 1163.4 mV on it (v - deviation(v) = 1150): round(1163.4 * 1123 / 850) = 1537.
    let ctrl2 = adc1_convert(&mut soc, 1 << 1);
    assert_ne!(ctrl2 & MEAS1_DONE, 0, "conversion done at once");
    assert_eq!(ctrl2 & MEAS1_DATA, 1537);
    assert_eq!(
        (ctrl2 >> SAR1_EN_PAD_SHIFT) & 0xfff,
        1 << 1,
        "pad field reads back"
    );
    // Software cannot write DATA or DONE.
    write(&mut soc, SAR_MEAS1_CTRL2, ctrl2 | MEAS1_DATA);
    assert_eq!(read(&mut soc, SAR_MEAS1_CTRL2) & MEAS1_DATA, 1537);
    // Clearing START clears DONE.
    write(&mut soc, SAR_MEAS1_CTRL2, (1 << 1) << SAR1_EN_PAD_SHIFT);
    assert_eq!(read(&mut soc, SAR_MEAS1_CTRL2) & MEAS1_DONE, 0);

    // The calibration channel (15) does not fit the 12-bit pad field: no pad is
    // selected and the input is the 1100 mV reference.
    // 1100 mV sits at 1111.5 mV on the line: round(1111.5 * 1123 / 850) = 1469.
    assert_eq!(adc1_convert(&mut soc, 0) & MEAS1_DATA, 1469);

    // Attenuation 0 (full scale 950 mV) clamps 1150 mV to 4095.
    write(&mut soc, SAR_ATTEN1, 3 << 30);
    assert_eq!(adc1_convert(&mut soc, 1 << 1) & MEAS1_DATA, 4095);
    // A channel the board does not drive reads 0.
    assert_eq!(adc1_convert(&mut soc, 1 << 5) & MEAS1_DATA, 0);
}

#[test]
fn esp32s3_rtc_periph_around_sens_stays_write_only() {
    let mut soc = soc();
    for addr in [0x6000_8400, 0x6000_87fc, 0x6000_8a00, 0x6000_8ffc] {
        write(&mut soc, addr, 0x1234_5678);
        assert_eq!(read(&mut soc, addr), 0, "{addr:#x}");
    }
    assert!(soc.unmodelled.is_empty());
}

// ---- GDMA OUT channel driving an LCD_CAM i8080 transfer ----

const GDMA: u32 = 0x6003_f000;
const GDMA_CH_STRIDE: u32 = 0xc0;
const OUT_CONF0: u32 = 0x60;
const OUT_CONF1: u32 = 0x64;
const OUT_INT_RAW: u32 = 0x68;
const OUT_INT_ST: u32 = 0x6c;
const OUT_INT_ENA: u32 = 0x70;
const OUT_INT_CLR: u32 = 0x74;
const OUTFIFO_STATUS: u32 = 0x78;
const OUT_LINK: u32 = 0x80;
const OUT_EOF_DES_ADDR: u32 = 0x88;
const OUT_PERI_SEL: u32 = 0xa8;
const OUT_RST: u32 = 1 << 0;
const OUT_AUTO_WRBACK: u32 = 1 << 2;
const OUT_EOF_MODE: u32 = 1 << 3;
const OUT_CHECK_OWNER: u32 = 1 << 12;
const OUTLINK_START: u32 = 1 << 21;
const OUTLINK_PARK: u32 = 1 << 23;
const OUTFIFO_EMPTY_L3: u32 = 1 << 5;
const PERI_LCD_CAM: u32 = 5;

const LCD_CAM: u32 = 0x6004_1000;
const LCD_CLOCK: u32 = LCD_CAM;
const LCD_USER: u32 = LCD_CAM + 0x14;
const LCD_MISC: u32 = LCD_CAM + 0x18;
const LCD_CMD_VAL: u32 = LCD_CAM + 0x28;
const LC_DMA_INT_ENA: u32 = LCD_CAM + 0x64;
const LC_DMA_INT_RAW: u32 = LCD_CAM + 0x68;
const LC_DMA_INT_CLR: u32 = LCD_CAM + 0x70;
const LCD_ALWAYS_OUT_EN: u32 = 1 << 13;
const LCD_BYTE_ORDER: u32 = 1 << 22;
const LCD_UPDATE: u32 = 1 << 20;
const LCD_2BYTE_EN: u32 = 1 << 23;
const LCD_DOUT: u32 = 1 << 24;
const LCD_CMD: u32 = 1 << 26;
const LCD_START: u32 = 1 << 27;
const LCD_RESET: u32 = 1 << 28;
const LCD_AFIFO_RESET: u32 = 1 << 27;
const LCD_TRANS_DONE: u32 = 1 << 1;

const DW0_SUC_EOF: u32 = 1 << 30;
const DW0_OWNER_DMA: u32 = 1 << 31;

/// LCD_CLOCK for LCD_PCLK = 4 MHz: XTAL_CLK (LCD_CLK_SEL 1, 40 MHz) divided by
/// LCD_CLKM_DIV_NUM 5 (DIV_A 1, DIV_B 0) gives LCD_CLK = 8 MHz, divided by
/// LCD_CLKCNT_N + 1 = 2.
const LCD_CLOCK_4MHZ: u32 = (1 << 31) | (1 << 29) | (1 << 23) | (5 << 9) | 1;
/// Ticks per LCD_PCLK cycle at 4 MHz.
const PCLK_4MHZ_TICKS: u64 = TICK_HZ / 4_000_000;

fn gdma_out(ch: u32, reg: u32) -> u32 {
    GDMA + ch * GDMA_CH_STRIDE + reg
}

/// Writes a descriptor at `addr`: `length` bytes at `buffer`, then `next`.
fn put_descriptor(soc: &mut Soc, addr: u32, flags: u32, length: u32, buffer: u32, next: u32) {
    let dw0 = flags | (length << 12) | length;
    assert!(soc.write_u32(addr, dw0));
    assert!(soc.write_u32(addr + 4, buffer));
    assert!(soc.write_u32(addr + 8, next));
}

/// Prepares GDMA OUT channel `ch` for LCD_CAM and starts it on the chain at
/// `first`, as a driver does: reset, owner check and write-back, peripheral
/// selection, link address, OUTLINK_START.
fn start_out_channel(soc: &mut Soc, ch: u32, first: u32) {
    let conf0 = OUT_AUTO_WRBACK | OUT_EOF_MODE;
    write(soc, gdma_out(ch, OUT_CONF1), OUT_CHECK_OWNER);
    write(soc, gdma_out(ch, OUT_INT_CLR), 0xff);
    write(soc, gdma_out(ch, OUT_CONF0), conf0 | OUT_RST);
    write(soc, gdma_out(ch, OUT_CONF0), conf0);
    write(soc, gdma_out(ch, OUT_LINK), first & 0xf_ffff);
    write(soc, gdma_out(ch, OUT_PERI_SEL), PERI_LCD_CAM);
    let link = read(soc, gdma_out(ch, OUT_LINK));
    write(soc, gdma_out(ch, OUT_LINK), link | OUTLINK_START);
}

#[test]
fn esp32s3_gdma_chain_drives_lcd_transfer() {
    let mut soc = soc();
    let ch = 2;
    // A 170-row band of RGB565 pixels on an 8-bit bus would be 6400 bytes: two
    // descriptors of 3200 bytes.
    let pixels: Vec<u8> = (0..6400u32).map(|i| (i * 7 % 251) as u8).collect();
    let desc0 = SRAM_D_BASE + 0x1_0000;
    let desc1 = desc0 + 0x10;
    let buf0 = SRAM_D_BASE + 0x2_0000;
    let buf1 = buf0 + 0x2000;
    assert!(soc.write_bytes(buf0, &pixels[..3200]));
    assert!(soc.write_bytes(buf1, &pixels[3200..]));
    put_descriptor(&mut soc, desc0, DW0_OWNER_DMA, 3200, buf0, desc1);
    put_descriptor(&mut soc, desc1, DW0_OWNER_DMA | DW0_SUC_EOF, 3200, buf1, 0);
    write(&mut soc, gdma_out(ch, OUT_INT_ENA), OUT_TOTAL_EOF);

    let t0 = 1_000_000;
    advance(&mut soc, t0);
    start_out_channel(&mut soc, ch, desc0);
    // A driver waits for data in the FIFO or for an interrupt bit.
    assert_eq!(
        read(&mut soc, gdma_out(ch, OUTFIFO_STATUS)) & OUTFIFO_EMPTY_L3,
        0
    );
    assert_eq!(
        read(&mut soc, gdma_out(ch, OUT_INT_RAW)),
        0,
        "no descriptor error"
    );
    assert_eq!(
        read(&mut soc, gdma_out(ch, OUT_LINK)) & OUTLINK_PARK,
        0,
        "link busy"
    );

    // The LCD route of the interrupt matrix: LCD_CAM to CPU interrupt 1.
    write(&mut soc, 0x600c_2000 + 4 * source::LCD_CAM as u32, 1);
    write(&mut soc, LCD_CLOCK, LCD_CLOCK_4MHZ);
    write(&mut soc, LCD_USER, LCD_RESET);
    write(&mut soc, LCD_MISC, LCD_AFIFO_RESET);
    assert_eq!(
        read(&mut soc, LCD_USER) & LCD_RESET,
        0,
        "LCD_RESET self-clears"
    );
    assert_eq!(
        read(&mut soc, LCD_MISC) & LCD_AFIFO_RESET,
        0,
        "AFIFO reset self-clears"
    );
    write(&mut soc, LCD_CMD_VAL, 0x2c);
    write(&mut soc, LC_DMA_INT_ENA, LCD_TRANS_DONE);
    write(&mut soc, LC_DMA_INT_CLR, LCD_TRANS_DONE);
    let user = LCD_CMD | LCD_DOUT | LCD_ALWAYS_OUT_EN;
    write(&mut soc, LCD_USER, user | LCD_UPDATE | LCD_START);
    assert_eq!(
        read(&mut soc, LCD_USER),
        user | LCD_START,
        "LCD_UPDATE self-clears"
    );

    // One command cycle and 6400 data cycles at 4 MHz: 6401 * 600 ticks,
    // 1.600_25 ms.
    let end = t0 + 6401 * PCLK_4MHZ_TICKS;
    assert_eq!(end - t0, 3_840_600);
    assert_eq!(soc.periph.next_event(t0), Some(end));

    advance(&mut soc, end - 1);
    assert_ne!(read(&mut soc, LCD_USER) & LCD_START, 0, "still sending");
    assert_eq!(read(&mut soc, LC_DMA_INT_RAW) & LCD_TRANS_DONE, 0);
    assert!(soc.periph.lcd_cam.out.is_empty());
    assert!(!soc.periph.source_levels()[source::LCD_CAM]);

    advance(&mut soc, end);
    assert_eq!(
        read(&mut soc, LCD_USER) & LCD_START,
        0,
        "LCD_START clears at the end"
    );
    assert_ne!(read(&mut soc, LC_DMA_INT_RAW) & LCD_TRANS_DONE, 0);
    assert!(soc.periph.source_levels()[source::LCD_CAM]);
    assert_ne!(
        soc.irq_lines() & (1 << 1),
        0,
        "LCD_CAM reaches CPU interrupt 1"
    );
    assert_eq!(
        soc.periph.lcd_cam.out.pop_front(),
        Some(LcdWrite {
            cmd: vec![0x2c],
            data: pixels,
        })
    );
    assert!(soc.periph.lcd_cam.out.is_empty());

    // The DMA channel ends without error, with EOF at the SUC_EOF descriptor.
    let raw = read(&mut soc, gdma_out(ch, OUT_INT_RAW));
    assert_eq!(raw, OUT_DONE | OUT_EOF | OUT_TOTAL_EOF);
    assert_eq!(raw & OUT_DSCR_ERR, 0, "has_error() is false");
    assert_eq!(read(&mut soc, gdma_out(ch, OUT_INT_ST)), OUT_TOTAL_EOF);
    assert_eq!(read(&mut soc, gdma_out(ch, OUT_EOF_DES_ADDR)), desc1);
    assert_ne!(
        read(&mut soc, gdma_out(ch, OUT_LINK)) & OUTLINK_PARK,
        0,
        "link idle"
    );
    assert!(soc.periph.source_levels()[source::DMA_OUT_CH0 + ch as usize]);
    assert!(!soc.periph.source_levels()[source::DMA_OUT_CH0]);
    // Owner write-back gave both descriptors to the CPU.
    for desc in [desc0, desc1] {
        assert_eq!(soc.read_u32(desc).unwrap() & DW0_OWNER_DMA, 0, "{desc:#x}");
    }

    write(&mut soc, LC_DMA_INT_CLR, LCD_TRANS_DONE);
    write(&mut soc, gdma_out(ch, OUT_INT_CLR), 0xff);
    assert!(!soc.periph.source_levels()[source::LCD_CAM]);
    assert!(!soc.periph.source_levels()[source::DMA_OUT_CH0 + ch as usize]);
    assert_eq!(soc.periph.next_event(end), None);
}

#[test]
fn esp32s3_gdma_descriptor_errors() {
    let mut soc = soc();
    // OUTLINK_ADDR 0x01000 is 0x3fc0_1000, below internal SRAM.
    start_out_channel(&mut soc, 0, 0x3fc0_1000);
    assert_eq!(read(&mut soc, gdma_out(0, OUT_INT_RAW)), OUT_DSCR_ERR);

    // A descriptor the CPU owns while the owner check is on.
    let desc = SRAM_D_BASE + 0x1_0000;
    let buf = SRAM_D_BASE + 0x2_0000;
    put_descriptor(&mut soc, desc, DW0_SUC_EOF, 4, buf, 0);
    start_out_channel(&mut soc, 1, desc);
    assert_eq!(read(&mut soc, gdma_out(1, OUT_INT_RAW)), OUT_DSCR_ERR);

    // A buffer outside internal SRAM (flash, through the cache).
    put_descriptor(
        &mut soc,
        desc,
        DW0_OWNER_DMA | DW0_SUC_EOF,
        4,
        0x3c00_0000,
        0,
    );
    start_out_channel(&mut soc, 3, desc);
    assert_eq!(read(&mut soc, gdma_out(3, OUT_INT_RAW)), OUT_DSCR_ERR);
}

#[test]
fn esp32s3_lcd_16bit_data_without_command() {
    let mut soc = soc();
    let desc = SRAM_D_BASE + 0x1_0000;
    let buf = SRAM_D_BASE + 0x2_0000;
    assert!(soc.write_bytes(buf, &[0x11, 0x22, 0x33, 0x44]));
    put_descriptor(&mut soc, desc, DW0_OWNER_DMA | DW0_SUC_EOF, 4, buf, 0);
    start_out_channel(&mut soc, 0, desc);
    write(&mut soc, LCD_CLOCK, LCD_CLOCK_4MHZ);
    let user = LCD_DOUT | LCD_ALWAYS_OUT_EN | LCD_2BYTE_EN | LCD_BYTE_ORDER;
    write(&mut soc, LCD_USER, user | LCD_START);
    // Two 16-bit cycles; LCD_BYTE_ORDER swaps the bytes of each cycle.
    assert_eq!(soc.periph.next_event(0), Some(2 * PCLK_4MHZ_TICKS));
    advance(&mut soc, 2 * PCLK_4MHZ_TICKS);
    assert_eq!(
        soc.periph.lcd_cam.out.pop_front(),
        Some(LcdWrite {
            cmd: Vec::new(),
            data: vec![0x22, 0x11, 0x44, 0x33],
        })
    );
}

#[test]
fn esp32s3_lcd_start_cleared_stops_after_sent_cycles() {
    let mut soc = soc();
    let desc = SRAM_D_BASE + 0x1_0000;
    let buf = SRAM_D_BASE + 0x2_0000;
    assert!(soc.write_bytes(buf, &[1, 2, 3, 4, 5, 6, 7, 8]));
    put_descriptor(&mut soc, desc, DW0_OWNER_DMA | DW0_SUC_EOF, 8, buf, 0);
    start_out_channel(&mut soc, 0, desc);
    write(&mut soc, LCD_CLOCK, LCD_CLOCK_4MHZ);
    write(&mut soc, LCD_CMD_VAL, 0x2c);
    let user = LCD_CMD | LCD_DOUT | LCD_ALWAYS_OUT_EN;
    write(&mut soc, LCD_USER, user | LCD_START);
    // After 4 cycles: the command and 3 data bytes.
    advance(&mut soc, 4 * PCLK_4MHZ_TICKS);
    write(&mut soc, LCD_USER, user);
    assert_eq!(
        soc.periph.lcd_cam.out.pop_front(),
        Some(LcdWrite {
            cmd: vec![0x2c],
            data: vec![1, 2, 3],
        })
    );
    assert_eq!(read(&mut soc, LC_DMA_INT_RAW) & LCD_TRANS_DONE, 0);
    assert_eq!(soc.periph.next_event(4 * PCLK_4MHZ_TICKS), None);
    assert_eq!(read(&mut soc, gdma_out(0, OUT_INT_RAW)), 0);
}
