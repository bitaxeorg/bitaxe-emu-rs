//! Power and temperatures of the board: chip power from the voltage, frequency
//! and activity, each chip on its heatsink in the fans' air, the regulator and
//! the board.

use super::bm13xx::Asic;

/// Ambient air, °C.
pub const AMBIENT: f32 = 25.0;
/// Fan speed at full drive, RPM.
pub const FAN_MAX_RPM: f32 = 6000.0;
/// Heat capacity of a chip and its share of the heatsink, J/°C.
const HEAT_CAPACITY: f32 = 20.0;
/// Air warms by this much past each chip, °C.
const AIR_RISE_PER_CHIP: f32 = 0.6;

#[derive(Clone, Debug)]
pub struct Thermal {
    /// Temperature of each chip, °C.
    pub chips: Vec<f32>,
    pub vr_temp: f32,
    pub board_temp: f32,
    /// Speed of each fan, RPM.
    pub fans: Vec<f32>,
    /// Power of all the chips, W.
    pub asic_power: f32,
}

impl Thermal {
    pub fn new(chips: usize, fans: usize) -> Self {
        Thermal {
            chips: vec![AMBIENT; chips],
            vr_temp: AMBIENT,
            board_temp: AMBIENT,
            fans: vec![0.0; fans],
            asic_power: 0.0,
        }
    }

    /// Power of one chip at core voltage `vchip` and hash frequency `freq_mhz`.
    pub fn chip_power(asic: Asic, vchip: f32, freq_mhz: f32, hashing: bool) -> f32 {
        let p = asic.power_point();
        if vchip < 0.25 * p.vcore {
            return 0.0;
        }
        let v2 = (vchip / p.vcore).powi(2);
        let dynamic = if hashing {
            p.hashing_w * (freq_mhz / p.freq_mhz)
        } else {
            0.0
        };
        (p.idle_w + dynamic) * v2
    }

    /// The hottest chip, °C.
    pub fn hottest(&self) -> f32 {
        self.chips.iter().copied().fold(f32::MIN, f32::max)
    }

    /// Advances by `dt` seconds: each chip dissipating `chip_power` W, the fans
    /// driven at `duties` (0..=1), the regulator delivering `iout` A over
    /// `phases` phases.
    pub fn step(&mut self, dt: f32, chip_power: f32, duties: &[f32], iout: f32, phases: u8) {
        self.asic_power = chip_power * self.chips.len() as f32;
        for (rpm, &duty) in self.fans.iter_mut().zip(duties) {
            let target = if duty > 0.02 {
                800.0 + (FAN_MAX_RPM - 800.0) * duty
            } else {
                0.0
            };
            *rpm += (target - *rpm) * (dt / 1.5).min(1.0);
        }
        let airflow = if self.fans.is_empty() {
            0.0
        } else {
            self.fans.iter().sum::<f32>() / self.fans.len() as f32 / FAN_MAX_RPM
        }
        .clamp(0.0, 1.0);
        // Thermal resistance from a chip to the air: still air without the fan.
        let r_th = 1.4 + 3.6 * (1.0 - airflow);
        for (i, t) in self.chips.iter_mut().enumerate() {
            let air = AMBIENT + AIR_RISE_PER_CHIP * i as f32;
            let flow = (*t - air) / r_th;
            *t += (chip_power - flow) / HEAT_CAPACITY * dt;
        }
        let hottest = self.hottest();
        let vr_target =
            AMBIENT + 6.0 + iout * 1.6 / f32::from(phases.max(1)) + (hottest - AMBIENT) * 0.25;
        self.vr_temp += (vr_target - self.vr_temp) * (dt / 8.0).min(1.0);
        let board_target = AMBIENT + 3.0 + self.asic_power * 0.35 / self.chips.len().max(1) as f32;
        self.board_temp += (board_target - self.board_temp) * (dt / 20.0).min(1.0);
    }
}
