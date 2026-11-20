//! ESP32-S3 peripherals.
//!
//! [`Peripherals`] decodes the peripheral bus (0x6000_0000..0x600D_1000) to the
//! device models, combines their interrupt outputs through the interrupt matrix,
//! and reports when the next timed event (timer alarm, UART byte, I2C
//! completion, end of an LCD transfer) is due. Times are in ticks of
//! [`TICK_HZ`].

pub mod cache;
pub mod efuse;
pub mod gdma;
pub mod gpio;
pub mod i2c;
pub mod intmatrix;
pub mod lcd_cam;
pub mod regfile;
pub mod rtc_cntl;
pub mod sens;
pub mod sha;
pub mod spi_flash;
pub mod system;
pub mod systimer;
pub mod timg;
pub mod uart;
pub mod usb_serial_jtag;

use cache::Cache;
use efuse::Efuse;
use gdma::Gdma;
use gpio::Gpio;
use i2c::I2c;
use intmatrix::{IntMatrix, SOURCES};
use lcd_cam::LcdCam;
use regfile::RegFile;
use rtc_cntl::{ResetRequest, RtcCntl};
use sens::Sens;
use sha::Sha;
use spi_flash::{Flash, Spi1};
use system::System;
use systimer::Systimer;
use timg::Timg;
use uart::Uart;
use usb_serial_jtag::UsbSerialJtag;

/// Ticks per second.
pub const TICK_HZ: u64 = 2_400_000_000;

/// GPIO_STRAP value for booting from SPI flash.
pub const STRAP_SPI_BOOT: u32 = 0x4;

/// Interrupt matrix source numbers.
pub mod source {
    pub const GPIO: usize = 16;
    pub const LCD_CAM: usize = 24;
    pub const UART0: usize = 27;
    pub const EFUSE: usize = 36;
    pub const I2C_EXT0: usize = 42;
    pub const TG0_T0: usize = 50;
    pub const TG1_T0: usize = 53;
    pub const SYSTIMER_TARGET0: usize = 57;
    /// GDMA IN channels 0..4.
    pub const DMA_IN_CH0: usize = 66;
    /// GDMA OUT channels 0..4.
    pub const DMA_OUT_CH0: usize = 71;
    pub const FROM_CPU_INTR0: usize = 79;
    pub const USB_SERIAL_JTAG: usize = 96;
}

/// GDMA registers (4 KB).
pub const GDMA_BASE: u32 = 0x6003_f000;

/// SENS registers, as offsets in the RTC peripheral page at 0x6000_8000.
const SENS_START: u32 = 0x800;
const SENS_END: u32 = 0xa00;

pub struct PeripheralsConfig {
    /// Flash image (its size picks the flash capacity).
    pub flash: Vec<u8>,
    pub efuse: Efuse,
    pub strap: u32,
}

pub struct Peripherals {
    pub system: System,
    pub rtc_cntl: RtcCntl,
    pub intmatrix: IntMatrix,
    pub uart: [Uart; 3],
    pub usb_serial_jtag: UsbSerialJtag,
    pub timg: [Timg; 2],
    pub systimer: Systimer,
    pub efuse: Efuse,
    pub gpio: Gpio,
    pub i2c: [I2c; 2],
    pub spi1: Spi1,
    pub flash: Flash,
    pub cache: Cache,
    pub sha: Sha,
    pub sens: Sens,
    pub gdma: Gdma,
    pub lcd_cam: LcdCam,
    /// Peripherals whose registers only read back what was written.
    regfiles: Vec<(u32, RegFile)>,
    rng: u64,
    /// The interrupt lines must be recomputed.
    pub irq_dirty: bool,
}

/// Register files: base, size, name.
const REGFILES: &[(u32, u32, &str)] = &[
    (0x6000_3000, 0x1000, "spi0"),
    // The RTC peripherals around SENS (0x6000_8800..0x6000_8A00).
    (0x6000_8200, 0x0600, "rtc_periph"),
    (0x6000_8a00, 0x0600, "rtc_periph"),
    (0x6000_5000, 0x2000, "rf_frontend"),
    (0x6000_e000, 0x1000, "regi2c"),
    (0x6001_c000, 0x2000, "baseband"),
    (0x6003_3000, 0x2000, "wifi_mac"),
    (0x6001_6000, 0x1000, "rmt"),
    (0x6001_9000, 0x1000, "ledc"),
    (0x6002_6000, 0x1000, "apb_ctrl"),
    (0x6003_9000, 0x1000, "usb_wrap"),
    (0x6004_0000, 0x1000, "apb_saradc"),
    (0x600c_1000, 0x1000, "sensitive"),
    (0x600c_e000, 0x1000, "assist_debug"),
    (0x600d_0000, 0x1000, "world_cntl"),
];

impl Peripherals {
    pub fn new(cfg: PeripheralsConfig) -> Self {
        let regfiles = REGFILES
            .iter()
            .map(|&(base, size, name)| {
                let mut rf = RegFile::new(name, size);
                if name == "rtc_periph" || name == "apb_saradc" {
                    // The analog side (SAR ADCs, RTC IO, sensors) is not modelled.
                    rf = rf.write_only();
                }
                if name == "regi2c"
                    || name == "rf_frontend"
                    || name == "baseband"
                    || name == "wifi_mac"
                {
                    // The analog and radio hardware behind these is not modelled:
                    // their status bits toggle, so that calibration polls finish.
                    rf = rf.toggling();
                }
                (base, rf)
            })
            .collect();
        let mut p = Peripherals {
            system: System::new(),
            rtc_cntl: RtcCntl::new(),
            intmatrix: IntMatrix::new(),
            uart: [Uart::new(), Uart::new(), Uart::new()],
            usb_serial_jtag: UsbSerialJtag::default(),
            timg: [Timg::new(), Timg::new()],
            systimer: Systimer::new(),
            efuse: cfg.efuse,
            gpio: Gpio::new(cfg.strap),
            i2c: [I2c::new(), I2c::new()],
            spi1: Spi1::new(),
            flash: Flash::new(cfg.flash),
            cache: Cache::new(),
            sha: Sha::new(),
            sens: Sens::new(),
            gdma: Gdma::new(),
            lcd_cam: LcdCam::new(),
            regfiles,
            rng: 0x2545_f491_4f6c_dd1d,
            irq_dirty: true,
        };
        p.usb_serial_jtag.reset();
        p
    }

    /// System reset: every peripheral but RTC_CNTL, the eFuses and the flash array.
    pub fn reset(&mut self) {
        self.system.reset();
        self.intmatrix.reset();
        for u in &mut self.uart {
            u.reset();
        }
        self.usb_serial_jtag.reset();
        for t in &mut self.timg {
            t.reset();
        }
        self.systimer.reset();
        self.gpio.reset();
        for i in &mut self.i2c {
            i.reset();
        }
        self.spi1.reset();
        self.flash.reset();
        self.cache.reset();
        self.sha.reset();
        self.sens.reset();
        self.gdma.reset();
        self.lcd_cam.reset();
        for (_, rf) in &mut self.regfiles {
            rf.reset();
        }
        self.irq_dirty = true;
    }

    /// A reset the firmware requested, if any.
    pub fn take_reset(&mut self) -> Option<ResetRequest> {
        self.rtc_cntl.pending_reset.take()
    }

    /// Ticks per CPU cycle.
    pub fn cycle_ticks(&self) -> u64 {
        TICK_HZ / self.system.cpu_hz()
    }

    /// Levels of the interrupt sources.
    pub fn source_levels(&self) -> [bool; SOURCES] {
        let mut l = [false; SOURCES];
        l[source::GPIO] = self.gpio.irq();
        l[source::LCD_CAM] = self.lcd_cam.irq();
        for (i, u) in self.uart.iter().enumerate() {
            l[source::UART0 + i] = u.irq();
        }
        l[source::EFUSE] = self.efuse.irq();
        l[source::I2C_EXT0] = self.i2c[0].irq();
        l[source::I2C_EXT0 + 1] = self.i2c[1].irq();
        for (g, base) in [source::TG0_T0, source::TG1_T0].into_iter().enumerate() {
            let irqs = self.timg[g].irqs();
            l[base..base + 3].copy_from_slice(&irqs);
        }
        let st = self.systimer.irqs();
        l[source::SYSTIMER_TARGET0..source::SYSTIMER_TARGET0 + 3].copy_from_slice(&st);
        let dma_in = self.gdma.in_irqs();
        l[source::DMA_IN_CH0..source::DMA_IN_CH0 + gdma::CHANNELS].copy_from_slice(&dma_in);
        let dma_out = self.gdma.out_irqs();
        l[source::DMA_OUT_CH0..source::DMA_OUT_CH0 + gdma::CHANNELS].copy_from_slice(&dma_out);
        let sw = self.system.from_cpu();
        l[source::FROM_CPU_INTR0..source::FROM_CPU_INTR0 + 4].copy_from_slice(&sw);
        l[source::USB_SERIAL_JTAG] = self.usb_serial_jtag.irq();
        l
    }

    /// Interrupt lines of PRO_CPU.
    pub fn irq_lines(&self, _now: u64) -> u32 {
        self.intmatrix.lines(0, &self.source_levels())
    }

    /// Time of the next timed event after `now`, if any.
    pub fn next_event(&self, now: u64) -> Option<u64> {
        let t = self.timg.iter().filter_map(|t| t.next_event(now));
        let u = self.uart.iter().filter_map(|u| u.next_event());
        let i = self.i2c.iter().filter_map(|i| i.next_event());
        t.chain(u)
            .chain(i)
            .chain(self.systimer.next_event())
            .chain(self.lcd_cam.next_event())
            .min()
    }

    /// Advances the timed state to `now`.
    pub fn update(&mut self, now: u64) {
        let apb = self.system.apb_hz();
        let mut changed = false;
        for t in &mut self.timg {
            changed |= t.update(now);
        }
        changed |= self.systimer.update(now);
        for u in &mut self.uart {
            u.apb_hz = apb;
            changed |= u.update(now);
        }
        for i in &mut self.i2c {
            changed |= i.update(now);
        }
        if let Some(end) = self.lcd_cam.update(now) {
            if let Some(ch) = end.dma_channel {
                self.gdma.finish_out(ch);
            }
            changed = true;
        }
        if changed {
            self.irq_dirty = true;
        }
    }

    /// Starts the LCD transfer LCD_START asked for, with the data of the GDMA OUT
    /// channel that serves LCD_CAM.
    fn start_lcd(&mut self, now: u64) {
        let (data, dma_channel) = match self.lcd_cam.data_request() {
            Some(max) => match self.gdma.out_channel_for(gdma::peripheral::LCD_CAM) {
                Some(ch) => (self.gdma.take_out_data(ch, max), Some(ch)),
                None => (Vec::new(), None),
            },
            None => (Vec::new(), None),
        };
        let pll_hz = self.system.pll_hz();
        self.lcd_cam.begin(now, data, dma_channel, pll_hz);
    }

    fn regfile(&mut self, addr: u32) -> Option<(u32, &mut RegFile)> {
        self.regfiles
            .iter_mut()
            .find(|(base, rf)| addr >= *base && rf.contains(addr - *base))
            .map(|(base, rf)| (addr - *base, rf))
    }

    /// Reads a peripheral register. None for an unmodelled address.
    pub fn read(&mut self, addr: u32, now: u64) -> Option<u32> {
        let off = addr & 0xfff;
        Some(match addr >> 12 {
            0x6_0000 => self.uart[0].read(off),
            0x6_0010 => self.uart[1].read(off),
            0x6_002e => self.uart[2].read(off),
            0x6_0002 => self.spi1.read(off),
            0x6_0004 => self.gpio.read(off),
            0x6_0007 => self.efuse.read(off),
            0x6_0008 if off < 0x200 => self.rtc_cntl.read(off),
            0x6_0008 if (SENS_START..SENS_END).contains(&off) => self.sens.read(off - SENS_START),
            0x6_0009 | 0x6_000a => self.gpio.iomux_read(addr - 0x6000_9000),
            0x6_0013 => self.i2c[0].read(off),
            0x6_0027 => self.i2c[1].read(off),
            0x6_001f => self.timg[0].read(off, now),
            0x6_0020 => self.timg[1].read(off, now),
            0x6_0023 => self.systimer.read(off),
            0x6_0035 if off == 0x07c => {
                // xorshift64*
                self.rng ^= self.rng >> 12;
                self.rng ^= self.rng << 25;
                self.rng ^= self.rng >> 27;
                (self.rng.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 32) as u32
            }
            0x6_0038 => self.usb_serial_jtag.read(off),
            0x6_003b => self.sha.read(off),
            0x6_003f => self.gdma.read(off),
            0x6_0041 => self.lcd_cam.read(off),
            0x6_00c0 => self.system.read(off),
            0x6_00c2 => {
                let levels = self.source_levels();
                self.intmatrix.read(off, &levels)
            }
            0x6_00c4 | 0x6_00c5 => self.cache.read(addr - 0x600c_4000),
            _ => return self.regfile(addr).map(|(o, rf)| rf.read(o)),
        })
    }

    /// Writes a peripheral register. Returns false for an unmodelled address.
    pub fn write(&mut self, addr: u32, v: u32, now: u64) -> bool {
        let off = addr & 0xfff;
        let irq = match addr >> 12 {
            0x6_0000 => self.uart[0].write(off, v, now),
            0x6_0010 => self.uart[1].write(off, v, now),
            0x6_002e => self.uart[2].write(off, v, now),
            0x6_0002 => {
                self.spi1.write(off, v, &mut self.flash);
                false
            }
            0x6_0004 => self.gpio.write(off, v),
            0x6_0007 => self.efuse.write(off, v),
            0x6_0008 if off < 0x200 => {
                self.rtc_cntl.write(off, v, now);
                false
            }
            0x6_0008 if (SENS_START..SENS_END).contains(&off) => {
                self.sens.write(off - SENS_START, v);
                false
            }
            0x6_0009 | 0x6_000a => self.gpio.iomux_write(addr - 0x6000_9000, v),
            0x6_0013 => self.i2c[0].write(off, v, now),
            0x6_0027 => self.i2c[1].write(off, v, now),
            0x6_001f => self.timg[0].write(off, v, now),
            0x6_0020 => self.timg[1].write(off, v, now),
            0x6_0023 => self.systimer.write(off, v, now),
            0x6_0035 if off == 0x07c => false,
            0x6_0038 => self.usb_serial_jtag.write(off, v),
            0x6_003b => {
                self.sha.write(off, v);
                false
            }
            0x6_003f => self.gdma.write(off, v),
            0x6_0041 => {
                let irq = self.lcd_cam.write(off, v, now);
                if self.lcd_cam.take_start_request() {
                    self.start_lcd(now);
                }
                irq
            }
            0x6_00c0 => {
                self.system.write(off, v);
                true
            }
            0x6_00c2 => {
                self.intmatrix.write(off, v);
                true
            }
            0x6_00c4 | 0x6_00c5 => {
                self.cache.write(addr - 0x600c_4000, v);
                false
            }
            _ => match self.regfile(addr) {
                Some((o, rf)) => {
                    rf.write(o, v);
                    false
                }
                None => return false,
            },
        };
        if irq {
            self.irq_dirty = true;
        }
        true
    }
}
