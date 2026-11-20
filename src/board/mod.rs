//! A Bitaxe board around its ESP32-S3: the core regulator, the fan controller
//! and temperature sensors, the display, the BM13xx chain on UART1, the reset
//! and power lines of the chips and the BOOT button, and a power and thermal
//! model tying them. [`spec::BOARDS`] says what each board carries.

pub mod bm13xx;
pub mod ds4432;
pub mod emc2101;
pub mod emc2103;
pub mod emc2302;
pub mod ina260;
pub mod pointer;
pub mod spec;
pub mod ssd1306;
pub mod st7789;
pub mod thermal;
pub mod tmp1075;
pub mod tps546;

use std::sync::{Arc, Mutex};

use esp32s3_emu::devices::efuse::Efuse;
use esp32s3_emu::devices::i2c::I2cDevice;
use esp32s3_emu::devices::{PeripheralsConfig, STRAP_SPI_BOOT};
use esp32s3_emu::machine::{Esp32s3, Esp32s3Config, Event, TICK_HZ};

use bm13xx::Chain;
use ds4432::Ds4432;
use emc2101::Emc2101;
use emc2103::Emc2103;
use emc2302::Emc2302;
use ina260::Ina260;
use spec::Spec;
use ssd1306::Ssd1306;
use st7789::St7789;
use thermal::Thermal;
use tmp1075::Tmp1075;
use tps546::Tps546;

/// I2C addresses on the boards.
pub const TPS546_ADDR: u8 = 0x24;
pub const EMC2101_ADDR: u8 = 0x4c;
pub const EMC2103_ADDR: u8 = 0x2e;
pub const EMC2302_ADDR: u8 = 0x2f;
pub const TMP1075_ADDRS: [u8; 2] = [0x4a, 0x4b];
pub const DS4432_ADDR: u8 = 0x48;
pub const INA260_ADDR: u8 = 0x40;
pub const OLED_ADDR: u8 = 0x3c;
/// OLED rows (0.91" 128×32 panel).
pub const OLED_ROWS: usize = 32;

/// GPIOs of the ESP32-S3 on the boards.
pub const BOOT_GPIO: usize = 0;
pub const ASIC_NRST_GPIO: usize = 1;
/// PWR_EN of the DAC regulator (active low), or the chips' power enable
/// (active high) on the boards with the colour LCD.
pub const POWER_GPIO: usize = 10;
pub const LCD_RESET_GPIO: usize = 5;
pub const LCD_POWER_GPIO: usize = 15;
pub const LCD_BACKLIGHT_GPIO: usize = 38;
/// ADC1 channel of GPIO2, the core voltage on the DAC boards.
const VCORE_ADC_CHANNEL: usize = 1;

/// The ASIC UART.
const ASIC_UART: usize = 1;

/// TPS40305 feedback: reference voltage and divider (RA, RB).
const TPS40305_VFB: f32 = 0.6;
const TPS40305_RA: f32 = 4990.0;
const TPS40305_RB: f32 = 3320.0;
/// Efficiency of the core regulator, and the power of the rest of the board, W.
const REGULATOR_EFFICIENCY: f32 = 0.9;
const BOARD_POWER: f32 = 0.8;
/// Input path resistance, ohms.
const INPUT_RESISTANCE: f32 = 0.01;
/// The TMP1075 board sensors read this much below the chips next to them, °C.
const TMP1075_BELOW_CHIP: f32 = 10.0;
/// The EMC2101 internal sensor next to the chip reads this much below it, °C.
const INTERNAL_SENSOR_BELOW_CHIP: f32 = 5.0;

/// Output of the TPS40305 with `current` A sourced into its feedback node.
fn tps40305_vout(current: f32) -> f32 {
    // Kirchhoff at the feedback node: (vout - VFB) / RA + I = VFB / RB.
    (TPS40305_VFB + TPS40305_RA * (TPS40305_VFB / TPS40305_RB - current)).max(0.0)
}

/// A device the machine owns on its I2C bus and the board reads.
pub struct Shared<T>(pub Arc<Mutex<T>>);

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Shared(self.0.clone())
    }
}

impl<T> Shared<T> {
    pub fn lock(&self) -> std::sync::MutexGuard<'_, T> {
        self.0.lock().unwrap()
    }
}

impl<T: I2cDevice> I2cDevice for Shared<T> {
    fn start(&mut self, read: bool) -> bool {
        self.0.lock().unwrap().start(read)
    }
    fn write(&mut self, byte: u8) -> bool {
        self.0.lock().unwrap().write(byte)
    }
    fn read(&mut self, ack: bool) -> u8 {
        self.0.lock().unwrap().read(ack)
    }
    fn stop(&mut self) {
        self.0.lock().unwrap().stop()
    }
}

fn shared<T>(v: T) -> Shared<T> {
    Shared(Arc::new(Mutex::new(v)))
}

pub enum Regulator {
    Dac {
        dac: Shared<Ds4432>,
        monitor: Shared<Ina260>,
        rfs_ohm: f32,
    },
    Tps546(Shared<Tps546>),
}

pub enum Fans {
    Emc2101(Shared<Emc2101>),
    Emc2103(Shared<Emc2103>),
    Emc2302 {
        fans: Shared<Emc2302>,
        sensors: [Shared<Tmp1075>; 2],
    },
}

pub enum Screen {
    Oled(Shared<Ssd1306>),
    Lcd(St7789),
}

/// What the board measures and shows, for the window.
#[derive(Clone, Debug, Default)]
pub struct Readings {
    /// Output of the core regulator (all domains), V, when on.
    pub vcore: Option<f32>,
    pub input_v: f32,
    pub input_a: f32,
    /// Duty of each fan, 0..=1.
    pub fan_duty: Vec<f32>,
}

pub struct Board {
    pub spec: &'static Spec,
    pub machine: Esp32s3,
    pub regulator: Regulator,
    pub fans: Fans,
    pub screen: Screen,
    pub chain: Chain,
    pub thermal: Thermal,
    pub readings: Readings,
    /// Where to write the flash back (settings, OTA) when the emulator stops.
    pub flash_path: Option<std::path::PathBuf>,
    last_update: u64,
}

impl Board {
    pub fn new(spec: &'static Spec, rom: Vec<u8>, flash: Vec<u8>, mac: [u8; 6]) -> Self {
        let mut machine = Esp32s3::new(Esp32s3Config {
            rom,
            periph: PeripheralsConfig {
                flash,
                // The ESP32-S3 modules of the boards carry 8 MB of octal PSRAM.
                psram: 8 << 20,
                efuse: Efuse::with_mac(mac),
                strap: STRAP_SPI_BOOT,
            },
        });
        let i2c = &mut machine.periph().i2c[0];
        let regulator = match spec.vcore {
            spec::Vcore::Dac { rfs_ohm } => {
                let dac = shared(Ds4432::default());
                let monitor = shared(Ina260::default());
                monitor.lock().voltage = spec.input_v;
                i2c.attach(DS4432_ADDR, Box::new(dac.clone()));
                i2c.attach(INA260_ADDR, Box::new(monitor.clone()));
                Regulator::Dac {
                    dac,
                    monitor,
                    rfs_ohm,
                }
            }
            spec::Vcore::Tps546 => {
                let tps = shared(Tps546::new());
                tps.lock().telemetry.vin = spec.input_v;
                i2c.attach(TPS546_ADDR, Box::new(tps.clone()));
                Regulator::Tps546(tps)
            }
        };
        let fans = match spec.cooling {
            spec::Cooling::Emc2101 { .. } => {
                let emc = shared(Emc2101::new());
                i2c.attach(EMC2101_ADDR, Box::new(emc.clone()));
                Fans::Emc2101(emc)
            }
            spec::Cooling::Emc2103 { .. } => {
                let emc = shared(Emc2103::default());
                i2c.attach(EMC2103_ADDR, Box::new(emc.clone()));
                Fans::Emc2103(emc)
            }
            spec::Cooling::Emc2302 => {
                let fans = shared(Emc2302::default());
                let sensors = [shared(Tmp1075::default()), shared(Tmp1075::default())];
                i2c.attach(EMC2302_ADDR, Box::new(fans.clone()));
                for (addr, sensor) in TMP1075_ADDRS.into_iter().zip(&sensors) {
                    i2c.attach(addr, Box::new(sensor.clone()));
                }
                Fans::Emc2302 { fans, sensors }
            }
        };
        let screen = match spec.display {
            spec::Display::Oled => {
                let oled = shared(Ssd1306::new(OLED_ROWS));
                i2c.attach(OLED_ADDR, Box::new(oled.clone()));
                Screen::Oled(oled)
            }
            spec::Display::St7789 { .. } => Screen::Lcd(St7789::default()),
        };
        Board {
            spec,
            machine,
            regulator,
            fans,
            screen,
            chain: Chain::new(spec.asic, spec.chips),
            thermal: Thermal::new(spec.chips, spec.fans()),
            readings: Readings {
                input_v: spec.input_v,
                ..Readings::default()
            },
            flash_path: None,
            last_update: 0,
        }
    }

    /// Writes the flash back to [`Board::flash_path`] if it changed.
    pub fn save_flash(&mut self) {
        let Some(path) = self.flash_path.clone() else {
            return;
        };
        let flash = &mut self.machine.periph().flash;
        if flash.dirty {
            match std::fs::write(&path, &flash.data) {
                Ok(()) => flash.dirty = false,
                Err(e) => eprintln!("cannot save the flash to {}: {e}", path.display()),
            }
        }
    }

    /// Emulated time, seconds.
    pub fn seconds(&self) -> f64 {
        self.machine.now as f64 / TICK_HZ as f64
    }

    /// Presses (true) or releases the BOOT button.
    pub fn set_boot_button(&mut self, pressed: bool) {
        let level = if pressed { Some(false) } else { None };
        if self.machine.periph().gpio.set_input(BOOT_GPIO, level) {
            self.machine.periph().irq_dirty = true;
        }
    }

    /// The RESET button: the chip restarts from its ROM, the board stays powered.
    pub fn reset(&mut self) {
        self.machine.reset();
        self.chain.set_reset(true);
    }

    /// Runs the machine until `deadline` (ticks), exchanging the board signals
    /// every `step` ticks. Returns the machine events.
    pub fn run_until(&mut self, deadline: u64, step: u64) -> Vec<Event> {
        let mut events = Vec::new();
        while self.machine.now < deadline {
            let until = (self.machine.now + step).min(deadline);
            while self.machine.now < until {
                if let Some(ev) = self.machine.run_until(until) {
                    if ev == Event::Reset {
                        self.chain.set_reset(true);
                    }
                    events.push(ev);
                }
            }
            self.exchange();
        }
        events
    }

    /// The level the chip drives on `gpio`, if its output is enabled.
    fn output(&mut self, gpio: usize) -> Option<bool> {
        self.machine.periph().gpio.output(gpio)
    }

    /// Output voltage of the core regulator (all domains), V, when on.
    fn regulator_output(&mut self) -> Option<f32> {
        match &self.regulator {
            Regulator::Tps546(tps) => {
                let tps = tps.lock();
                tps.output_on().then(|| tps.vout_command())
            }
            Regulator::Dac { dac, rfs_ohm, .. } => {
                let vout = tps40305_vout(dac.lock().current(0, *rfs_ohm));
                let enabled = self.output(POWER_GPIO) == Some(false);
                enabled.then_some(vout)
            }
        }
    }

    /// Moves the signals between the chip and the board.
    fn exchange(&mut self) {
        let now = self.machine.now;
        let spec = self.spec;
        // Core power and the chips' reset line: they run while NRST is high.
        let vcore = self.regulator_output();
        let enabled = !spec.asic_enable || self.output(POWER_GPIO) == Some(true);
        let vcore = vcore.filter(|_| enabled);
        let nrst = self.output(ASIC_NRST_GPIO).unwrap_or(false);
        self.chain.set_reset(!nrst || vcore.is_none());
        // ASIC UART.
        let uart = &mut self.machine.periph().uart[ASIC_UART];
        let to_chain: Vec<u8> = uart.tx_out.drain(..).collect();
        self.chain.receive(&to_chain);
        self.chain.update();
        if !self.chain.tx.is_empty() {
            let bytes: Vec<u8> = self.chain.tx.drain(..).collect();
            self.machine.periph().uart[ASIC_UART].receive(&bytes, now);
            self.machine.periph().irq_dirty = true;
        }
        self.exchange_screen();

        // Power and temperatures.
        let dt = (now - self.last_update) as f32 / TICK_HZ as f32;
        self.last_update = now;
        let vout = vcore.unwrap_or(0.0);
        let vchip = vout / spec.domains as f32;
        let chip_power = Thermal::chip_power(
            spec.asic,
            vchip,
            self.chain.frequency_mhz(),
            self.chain.hashing(),
        );
        let core_power = chip_power * spec.chips as f32;
        let iout = if vout > 0.1 { core_power / vout } else { 0.0 };
        let duties = match &self.fans {
            Fans::Emc2101(emc) => vec![emc.lock().duty()],
            Fans::Emc2103(emc) => vec![emc.lock().duty()],
            Fans::Emc2302 { fans, .. } => {
                let fans = fans.lock();
                vec![fans.duty(0), fans.duty(1)]
            }
        };
        self.thermal
            .step(dt, chip_power, &duties, iout, spec.phases);
        let input_power = core_power / REGULATOR_EFFICIENCY + BOARD_POWER;
        let input_a = input_power / spec.input_v;
        let input_v = spec.input_v - INPUT_RESISTANCE * input_a;
        self.readings = Readings {
            vcore,
            input_v,
            input_a,
            fan_duty: duties,
        };

        let th = &self.thermal;
        match &self.regulator {
            Regulator::Tps546(tps) => {
                let mut tps = tps.lock();
                tps.telemetry.vin = input_v;
                tps.telemetry.iout = iout;
                tps.telemetry.temperature = th.vr_temp;
            }
            Regulator::Dac { monitor, .. } => {
                let mut ina = monitor.lock();
                ina.voltage = input_v;
                ina.current = input_a;
            }
        }
        match (&self.fans, spec.cooling) {
            (Fans::Emc2101(emc), spec::Cooling::Emc2101 { internal_sensor }) => {
                let mut emc = emc.lock();
                emc.internal_temp = if internal_sensor {
                    th.chips[0] - INTERNAL_SENSOR_BELOW_CHIP
                } else {
                    th.board_temp
                };
                emc.external_temp = th.chips[0];
                emc.fan_rpm = th.fans[0];
            }
            (Fans::Emc2103(emc), spec::Cooling::Emc2103 { diodes }) => {
                let mut emc = emc.lock();
                emc.internal_temp = th.board_temp;
                emc.external_temp = diodes.map(|i| th.chips[i]);
                emc.fan_rpm = th.fans[0];
            }
            (Fans::Emc2302 { fans, sensors }, _) => {
                fans.lock().fan_rpm = [th.fans[0], th.fans[1]];
                // Each sensor sits by one half of the chips.
                let half = th.chips.len().div_ceil(2);
                for (sensor, chips) in sensors.iter().zip(th.chips.chunks(half)) {
                    let warmest = chips.iter().copied().fold(f32::MIN, f32::max);
                    sensor.lock().temperature = warmest - TMP1075_BELOW_CHIP;
                }
            }
            _ => {}
        }
        if matches!(self.regulator, Regulator::Dac { .. }) {
            let mv = (vout * 1000.0).round() as u32;
            self.machine
                .periph()
                .sens
                .set_adc1_mv(VCORE_ADC_CHANNEL, mv);
        }
    }

    /// Hands the LCD bus transfers to the panel and follows its control lines.
    fn exchange_screen(&mut self) {
        if !matches!(self.screen, Screen::Lcd(_)) {
            return;
        }
        let in_reset = self.output(LCD_RESET_GPIO) == Some(false);
        let writes: Vec<_> = self.machine.periph().lcd_cam.out.drain(..).collect();
        let Screen::Lcd(lcd) = &mut self.screen else {
            return;
        };
        if in_reset {
            lcd.reset();
            return;
        }
        for w in writes {
            lcd.transfer(&w.cmd, &w.data);
        }
    }

    /// The colour LCD is lit: panel power and backlight on.
    pub fn lcd_lit(&mut self) -> bool {
        self.output(LCD_POWER_GPIO) != Some(false) && self.output(LCD_BACKLIGHT_GPIO) == Some(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dac_moves_the_regulator_output() {
        let mut dac = Ds4432::default();
        assert!((tps40305_vout(dac.current(0, 80_000.0)) - 1.502).abs() < 0.001);
        // 1.2 V: 60.5 uA sourced, code 77 with RFS = 80 kOhm.
        dac.out[0] = 0x80 | 77;
        assert!((tps40305_vout(dac.current(0, 80_000.0)) - 1.2).abs() < 0.005);
        // Sinking raises the output.
        dac.out[0] = 20;
        assert!((tps40305_vout(dac.current(0, 80_000.0)) - 1.58).abs() < 0.005);
    }
}
