//! SENS (0x6000_8800): the RTC controller of the SAR ADCs and of the other RTC
//! analog sensors.
//!
//! Registers read back what was written, apart from their read-only status
//! fields, which read 0. Both ADCs convert under software control: setting
//! MEASn_START_SAR samples the channel SARn_EN_PAD selects and completes at
//! once, with MEASn_DONE_SAR set and the code in MEASn_DATA_SAR; clearing
//! MEASn_START_SAR clears MEASn_DONE_SAR. The board sets the input voltage of
//! each ADC1 channel ([`Sens::set_adc1_mv`]); the ADC2 channels read 0 V. The
//! converters are linear through their calibration point, which the eFuses
//! hold, apart from a small deviation that the curve-fitting calibration
//! removes. The touch sensor, the temperature sensor and the ULP
//! coprocessor do not run.

/// ADC1 input channels (channel `n` is GPIO `n + 1`).
pub const ADC1_CHANNELS: usize = 10;
/// ADC2 input channels (channel `n` is GPIO `n + 11`).
pub const ADC2_CHANNELS: usize = 10;

/// Input of a conversion with no pad selected: the internal reference, in mV.
pub const ADC_REFERENCE_MV: u32 = 1100;

/// Input voltage of the full-scale code for attenuation 0 (0 dB), 1 (2.5 dB),
/// 2 (6 dB) and 3 (11 dB), in mV.
const FULL_SCALE_MV: [u32; 4] = [950, 1250, 1750, 3100];

/// Largest 12-bit code.
const CODE_MAX: u32 = 4095;

/// Channel whose SAR_ATTEN1 field sets the attenuation of a conversion with no
/// pad selected (the calibration channel).
const CAL_CHANNEL: u32 = 15;

const SIZE: u32 = 0x200;

const READER1_CTRL: u32 = 0x000;
const READER1_STATUS: u32 = 0x004;
const MEAS1_CTRL2: u32 = 0x00c;
const ATTEN1: u32 = 0x014;
const AMP_CTRL1: u32 = 0x018;
const AMP_CTRL2: u32 = 0x01c;
const AMP_CTRL3: u32 = 0x020;
const READER2_CTRL: u32 = 0x024;
const READER2_STATUS: u32 = 0x028;
const MEAS2_CTRL1: u32 = 0x02c;
const MEAS2_CTRL2: u32 = 0x030;
const ATTEN2: u32 = 0x038;
const SLAVE_ADDR1: u32 = 0x040;
const TSENS_CTRL: u32 = 0x050;
const TSENS_CTRL2: u32 = 0x054;
const TOUCH_CONF: u32 = 0x05c;
const TOUCH_DENOISE: u32 = 0x060;
const TOUCH_CHN_ST: u32 = 0x09c;
const TOUCH_APPR_STATUS: u32 = 0x0e0;
const COCPU_STATE: u32 = 0x0e4;
const COCPU_INT_RAW: u32 = 0x0e8;
const COCPU_INT_ENA: u32 = 0x0ec;
const COCPU_INT_ST: u32 = 0x0f0;
const COCPU_INT_CLR: u32 = 0x0f4;
const COCPU_DEBUG: u32 = 0x0f8;
const HALL_CTRL: u32 = 0x0fc;
const COCPU_INT_ENA_W1TS: u32 = 0x10c;
const COCPU_INT_ENA_W1TC: u32 = 0x110;
const SARDATE: u32 = 0x1fc;

/// SAR_MEAS1_CTRL2 / SAR_MEAS2_CTRL2 fields.
const MEAS_DATA: u32 = 0xffff;
const MEAS_DONE: u32 = 1 << 16;
const MEAS_START: u32 = 1 << 17;
const EN_PAD_SHIFT: u32 = 19;
const EN_PAD_MASK: u32 = 0xfff;

/// SAR_SLAVE_ADDR1.SAR_SARADC_MEAS_STATUS: nonzero while a conversion runs.
const SLAVE_ADDR1_MEAS_STATUS: u32 = 0x3fc0_0000;
/// SAR_TSENS_CTRL.TSENS_OUT and TSENS_READY.
const TSENS_CTRL_STATUS: u32 = 0x1ff;

/// Reset values of the registers that do not reset to 0.
const RESET: [(u32, u32); 13] = [
    (READER1_CTRL, 0x2004_0002),
    (ATTEN1, 0xffff_ffff),
    (AMP_CTRL1, 0x000a_000a),
    (AMP_CTRL2, 0x000a_0000),
    (AMP_CTRL3, 0x0073_38f3),
    (READER2_CTRL, 0x4005_0002),
    (MEAS2_CTRL1, 0x0702_0200),
    (ATTEN2, 0xffff_ffff),
    (TSENS_CTRL, 0x0001_9000),
    (TSENS_CTRL2, 0x0000_4002),
    (TOUCH_CONF, 0xfff0_7fff),
    (HALL_CTRL, 0xa000_0000),
    (SARDATE, 0x0210_1180),
];

/// Bits of `off` that software cannot write.
fn read_only(off: u32) -> u32 {
    match off {
        MEAS1_CTRL2 | MEAS2_CTRL2 => MEAS_DATA | MEAS_DONE,
        SLAVE_ADDR1 => SLAVE_ADDR1_MEAS_STATUS,
        TSENS_CTRL => TSENS_CTRL_STATUS,
        READER1_STATUS | READER2_STATUS | TOUCH_DENOISE | COCPU_STATE | COCPU_INT_RAW
        | COCPU_INT_ST | COCPU_INT_CLR | COCPU_DEBUG => u32::MAX,
        // Touch channel and scan status.
        TOUCH_CHN_ST..=TOUCH_APPR_STATUS => u32::MAX,
        _ => 0,
    }
}

pub struct Sens {
    regs: [u32; (SIZE / 4) as usize],
    /// Input voltage of each ADC1 channel, in mV.
    pub adc1_mv: [u32; ADC1_CHANNELS],
}

impl Default for Sens {
    fn default() -> Self {
        Self::new()
    }
}

impl Sens {
    pub fn new() -> Self {
        let mut s = Sens {
            regs: [0; (SIZE / 4) as usize],
            adc1_mv: [0; ADC1_CHANNELS],
        };
        s.reset();
        s
    }

    /// Resets the registers. The input voltages belong to the board and stay.
    pub fn reset(&mut self) {
        self.regs = [0; (SIZE / 4) as usize];
        for (off, v) in RESET {
            self.set(off, v);
        }
    }

    /// The board drives `mv` millivolts on ADC1 channel `channel`.
    pub fn set_adc1_mv(&mut self, channel: usize, mv: u32) {
        if let Some(input) = self.adc1_mv.get_mut(channel) {
            *input = mv;
        }
    }

    fn get(&self, off: u32) -> u32 {
        self.regs.get((off / 4) as usize).copied().unwrap_or(0)
    }

    fn set(&mut self, off: u32, v: u32) {
        if let Some(r) = self.regs.get_mut((off / 4) as usize) {
            *r = v;
        }
    }

    pub fn read(&self, off: u32) -> u32 {
        self.get(off)
    }

    pub fn write(&mut self, off: u32, v: u32) {
        match off {
            COCPU_INT_ENA_W1TS => {
                let ena = self.get(COCPU_INT_ENA) | v;
                self.set(COCPU_INT_ENA, ena);
            }
            COCPU_INT_ENA_W1TC => {
                let ena = self.get(COCPU_INT_ENA) & !v;
                self.set(COCPU_INT_ENA, ena);
            }
            _ => {
                let ro = read_only(off);
                let old = self.get(off);
                let mut new = (old & ro) | (v & !ro);
                if off == MEAS1_CTRL2 {
                    new = self.meas(old, new, |s, pads| s.adc_code(0, pads, ATTEN1, &s.adc1_mv));
                } else if off == MEAS2_CTRL2 {
                    new = self.meas(old, new, |s, pads| {
                        s.adc_code(1, pads, ATTEN2, &[0; ADC2_CHANNELS])
                    });
                }
                self.set(off, new);
            }
        }
    }

    /// SAR_MEASn_CTRL2 after a write that changed it from `old` to `new`;
    /// `convert` gives the code of the pads selected.
    fn meas(&self, old: u32, new: u32, convert: impl Fn(&Self, u32) -> u32) -> u32 {
        if new & MEAS_START == 0 {
            return new & !MEAS_DONE;
        }
        if old & MEAS_START != 0 {
            return new;
        }
        // A rising edge of MEAS1_START_SAR starts a conversion.
        let pads = (new >> EN_PAD_SHIFT) & EN_PAD_MASK;
        let code = convert(self, pads);
        (new & !MEAS_DATA) | MEAS_DONE | code
    }

    /// The code of a conversion of the pads in the SARn_EN_PAD bitmap `pads`
    /// (the lowest one selected wins) by ADC `unit` (0 or 1), with the
    /// attenuations of register `atten_reg` and the channel inputs `inputs` (mV).
    fn adc_code(&self, unit: usize, pads: u32, atten_reg: u32, inputs: &[u32]) -> u32 {
        let (channel, mv) = if pads == 0 {
            (CAL_CHANNEL, ADC_REFERENCE_MV)
        } else {
            let channel = pads.trailing_zeros();
            let mv = inputs.get(channel as usize).copied().unwrap_or(0);
            (channel, mv)
        };
        let atten = (self.get(atten_reg) >> (2 * channel)) & 3;
        code(unit, mv, atten)
    }
}

/// Input voltage of the factory calibration point, mV.
pub const CAL_MV: u32 = 850;

/// Deviation of each converter from its line through the origin and the
/// calibration point, in mV, as a polynomial of the line's voltage `v` (mV):
/// the coefficients of `v^0` to `v^4`, by ADC and attenuation.
const DEVIATION: [[[f64; 5]; 4]; 2] = [
    [
        [
            -2.785_653_141_953_83,
            -5.087_154_056_952_8e-3,
            9.798_249_589e-6,
            0.0,
            0.0,
        ],
        [
            -2.983_102_291_502_87,
            -4.939_318_586_880_6e-3,
            1.013_794_305_48e-5,
            0.0,
            0.0,
        ],
        [
            -2.328_554_574_629_64,
            -1.476_401_810_474_14e-2,
            2.083_855_253_14e-5,
            0.0,
            0.0,
        ],
        [
            -0.644_403_418_269_478,
            -6.443_348_886_475_36e-2,
            1.297_891_447_611e-4,
            -7.076_971_8e-8,
            1.3515e-11,
        ],
    ],
    [
        [
            -2.566_865_165_432_89,
            1.353_548_869_615e-4,
            3.661_526_518_9e-6,
            0.0,
            0.0,
        ],
        [
            -2.369_018_469_029_84,
            -6.631_989_422_618_5e-3,
            1.189_649_959_59e-5,
            0.0,
            0.0,
        ],
        [
            -0.945_249_939_702_062,
            -2.009_967_739_543_87e-2,
            2.590_114_679_56e-5,
            0.0,
            0.0,
        ],
        [
            1.224_771_976_433_69,
            -7.557_179_049_434_62e-2,
            1.478_791_187_119e-4,
            -7.967_252_8e-8,
            1.5038e-11,
        ],
    ],
];

fn deviation(unit: usize, atten: u32, v: f64) -> f64 {
    DEVIATION[unit & 1][(atten & 3) as usize]
        .iter()
        .rev()
        .fold(0.0, |acc, c| acc * v + c)
}

/// The 12-bit code ADC `unit` gives for `mv` millivolts at attenuation `atten`:
/// the voltage `v` on its line, where `v - deviation(v) = mv`, times the slope
/// of the line (the calibration code over [`CAL_MV`]), rounded, clamped to 4095.
fn code(unit: usize, mv: u32, atten: u32) -> u32 {
    let target = f64::from(mv);
    let mut v = target;
    for _ in 0..8 {
        v = target + deviation(unit, atten, v);
    }
    let slope = f64::from(calibration_code(unit, atten)) / f64::from(CAL_MV);
    (v.max(0.0) * slope).round().min(f64::from(CODE_MAX)) as u32
}

/// The code that defines the line of ADC `unit` at attenuation `atten`, at the
/// calibration point ([`CAL_MV`]); the eFuses hold it. The full scale of the
/// attenuation reads 4095 on the line.
pub fn calibration_code(_unit: usize, atten: u32) -> u32 {
    let full_scale = FULL_SCALE_MV[(atten & 3) as usize];
    (CAL_MV * CODE_MAX + full_scale / 2) / full_scale
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Millivolts from a code, as the curve-fitting calibration computes them:
    /// the line through the origin and the calibration point, less the
    /// deviation.
    fn calibrated_mv(unit: usize, atten: u32, raw: u32) -> f64 {
        let v = f64::from(raw) * f64::from(CAL_MV) / f64::from(calibration_code(unit, atten));
        v - deviation(unit, atten, v)
    }

    #[test]
    fn calibration_recovers_the_input() {
        for unit in 0..2 {
            for atten in 0..4 {
                let full_scale = FULL_SCALE_MV[atten as usize];
                for mv in (100..full_scale - 50).step_by(50) {
                    let raw = code(unit, mv, atten);
                    if raw == CODE_MAX {
                        // The top of the range saturates.
                        continue;
                    }
                    let back = calibrated_mv(unit, atten, raw);
                    // One code is under 1 mV.
                    assert!(
                        (back - f64::from(mv)).abs() < 1.0,
                        "ADC{} atten {atten}: {mv} mV -> {raw} -> {back:.1} mV",
                        unit + 1
                    );
                }
            }
        }
    }
}
