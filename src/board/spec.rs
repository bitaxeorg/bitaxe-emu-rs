//! The boards of the Bitaxe family: what each one carries, by board number.

use super::bm13xx::Asic;
use super::st7789::{MADCTL_MV, MADCTL_MX, MADCTL_MY};

/// The core regulator.
#[derive(Clone, Copy, Debug)]
pub enum Vcore {
    /// TPS40305 buck whose feedback node a DS4432 current DAC moves (output 0,
    /// full-scale resistor `rfs_ohm`), enabled by PWR_EN (GPIO10, active low),
    /// an INA260 on the input and the output on ADC1 (GPIO2).
    Dac { rfs_ohm: f32 },
    /// TPS546D24A over PMBus.
    Tps546,
}

/// The fan controller and the temperature sensors.
#[derive(Clone, Copy, Debug)]
pub enum Cooling {
    /// EMC2101, one fan. `internal_sensor`: the chip temperature is read from
    /// its internal sensor, next to the chip, instead of the chip's diode.
    Emc2101 { internal_sensor: bool },
    /// EMC2103, one fan, the diodes of two chips (indexes).
    Emc2103 { diodes: [usize; 2] },
    /// EMC2302, two fans, and two TMP1075 board sensors.
    Emc2302,
}

#[derive(Clone, Copy, Debug)]
pub enum Display {
    /// SSD1306 128 × 32 OLED on I2C.
    Oled,
    /// ST7789 320 × 170 LCD on the 8-bit parallel bus; `upright` is the MADCTL
    /// access order in which its image reads upright.
    St7789 { upright: u8 },
}

/// Where things are on a board's picture (its pixels).
#[derive(Clone, Copy, Debug)]
pub struct Art {
    pub png: &'static [u8],
    /// Center and size of the display's active area.
    pub display_center: (f32, f32),
    pub display_size: (f32, f32),
    /// Clockwise turn of the display image on the picture, degrees (0, 90, 180, 270).
    pub display_rotation: u16,
    /// The RESET and BOOT buttons, when the board has them.
    pub reset: Option<(f32, f32)>,
    pub boot: Option<(f32, f32)>,
    /// Fans drawn on the picture: center and size.
    pub fans: &'static [((f32, f32), f32)],
}

#[derive(Clone, Copy, Debug)]
pub struct Spec {
    /// The board number printed on the board.
    pub number: u16,
    pub family: &'static str,
    pub asic: Asic,
    pub chips: usize,
    /// Voltage domains in series, the chips split evenly among them.
    pub domains: usize,
    pub vcore: Vcore,
    pub cooling: Cooling,
    pub display: Display,
    /// Nominal input voltage, V.
    pub input_v: f32,
    /// Regulator phases.
    pub phases: u8,
    /// GPIO10 enables the chips' power (active high).
    pub asic_enable: bool,
    pub art: &'static Art,
}

impl Spec {
    pub fn name(&self) -> String {
        format!("Bitaxe {} {}", self.family, self.number)
    }

    pub fn fans(&self) -> usize {
        match self.cooling {
            Cooling::Emc2302 => 2,
            _ => 1,
        }
    }
}

/// OLED active area on the pictures rendered at the Gamma's scale.
const OLED: (f32, f32) = (256.0, 64.0);
/// The fan over the fan outline of the Gamma-sized boards.
const GAMMA_FAN: &[((f32, f32), f32)] = &[((344.0, 740.0), 400.0)];

const fn oled_art(
    png: &'static [u8],
    display_center: (f32, f32),
    reset: Option<(f32, f32)>,
    boot: Option<(f32, f32)>,
    fans: &'static [((f32, f32), f32)],
) -> Art {
    Art {
        png,
        display_center,
        display_size: OLED,
        display_rotation: 0,
        reset,
        boot,
        fans,
    }
}

static GAMMA: Art = oled_art(
    include_bytes!("../../assets/gamma-front.png"),
    (365.0, 87.0),
    Some((81.0, 621.0)),
    Some((82.0, 714.0)),
    GAMMA_FAN,
);
// The Max has no buttons (EN and IO0 only reach test pads); its OLED header is
// not populated in the picture.
static MAX: Art = oled_art(
    include_bytes!("../../assets/max-front.png"),
    (390.0, 84.0),
    None,
    None,
    GAMMA_FAN,
);
static ULTRA_205: Art = oled_art(
    include_bytes!("../../assets/ultra-205-front.png"),
    (365.0, 86.0),
    Some((82.0, 621.0)),
    Some((83.0, 713.0)),
    GAMMA_FAN,
);
static ULTRA_207: Art = oled_art(
    include_bytes!("../../assets/ultra-207-front.png"),
    (365.0, 86.0),
    Some((82.0, 621.0)),
    Some((83.0, 713.0)),
    GAMMA_FAN,
);
static SUPRA_401: Art = oled_art(
    include_bytes!("../../assets/supra-401-front.png"),
    (364.0, 86.0),
    Some((81.0, 621.0)),
    Some((82.0, 713.0)),
    &[((343.0, 740.0), 400.0)],
);
static SUPRA_403: Art = oled_art(
    include_bytes!("../../assets/supra-403-front.png"),
    (365.0, 86.0),
    Some((82.0, 621.0)),
    Some((83.0, 713.0)),
    GAMMA_FAN,
);
// The Hex boards' fans are in the enclosure, not on the board.
static HEX: Art = oled_art(
    include_bytes!("../../assets/hex-front.png"),
    (681.0, 159.0),
    Some((446.0, 40.0)),
    Some((366.0, 40.0)),
    &[],
);
static SUPRA_HEX: Art = oled_art(
    include_bytes!("../../assets/suprahex-front.png"),
    (681.0, 159.0),
    Some((446.0, 40.0)),
    Some((366.0, 40.0)),
    &[],
);
static GAMMA_TURBO: Art = oled_art(
    include_bytes!("../../assets/gammaturbo-front.png"),
    (399.0, 85.0),
    Some((198.0, 1158.0)),
    Some((327.0, 1157.0)),
    &[((392.0, 801.0), 597.0)],
);
static NAJA_DUO: Art = Art {
    png: include_bytes!("../../assets/najaduo-front.png"),
    display_center: (300.0, 233.0),
    display_size: (374.0, 198.0),
    display_rotation: 0,
    reset: Some((354.0, 87.0)),
    boot: Some((253.0, 87.0)),
    fans: &[((911.0, 452.0), 620.0)],
};
// The buttons are on the back, under their labels at the top edge.
static GAMMA_HEX: Art = Art {
    png: include_bytes!("../../assets/gammahex-front.png"),
    display_center: (498.0, 184.0),
    display_size: (424.0, 225.0),
    display_rotation: 0,
    reset: Some((298.0, 41.0)),
    boot: Some((397.0, 41.0)),
    fans: &[((517.0, 832.0), 890.0)],
};

const DAC_80K: Vcore = Vcore::Dac { rfs_ohm: 80_000.0 };
const DAC_80K6: Vcore = Vcore::Dac { rfs_ohm: 80_600.0 };
const EMC2101_INTERNAL: Cooling = Cooling::Emc2101 {
    internal_sensor: true,
};
const EMC2101_DIODE: Cooling = Cooling::Emc2101 {
    internal_sensor: false,
};

const fn board(
    number: u16,
    family: &'static str,
    asic: Asic,
    chips: usize,
    vcore: Vcore,
    cooling: Cooling,
    art: &'static Art,
) -> Spec {
    Spec {
        number,
        family,
        asic,
        chips,
        domains: 1,
        vcore,
        cooling,
        display: Display::Oled,
        input_v: 5.0,
        phases: 1,
        asic_enable: false,
        art,
    }
}

const fn twelve_volts(mut spec: Spec, domains: usize, phases: u8) -> Spec {
    spec.input_v = 12.0;
    spec.domains = domains;
    spec.phases = phases;
    spec
}

const fn colour_lcd(mut spec: Spec, upright: u8) -> Spec {
    spec.display = Display::St7789 { upright };
    spec.asic_enable = true;
    spec
}

use Asic::*;

/// Every board, by number.
pub static BOARDS: &[Spec] = &[
    board(102, "Max", Bm1397, 1, DAC_80K, EMC2101_DIODE, &MAX),
    board(
        201,
        "Ultra",
        Bm1366,
        1,
        DAC_80K,
        EMC2101_INTERNAL,
        &ULTRA_205,
    ),
    board(
        202,
        "Ultra",
        Bm1366,
        1,
        DAC_80K6,
        EMC2101_INTERNAL,
        &ULTRA_205,
    ),
    board(
        205,
        "Ultra",
        Bm1366,
        1,
        DAC_80K6,
        EMC2101_INTERNAL,
        &ULTRA_205,
    ),
    board(
        207,
        "Ultra",
        Bm1366,
        1,
        Vcore::Tps546,
        EMC2101_DIODE,
        &ULTRA_207,
    ),
    twelve_volts(
        board(302, "Hex", Bm1366, 6, Vcore::Tps546, Cooling::Emc2302, &HEX),
        3,
        1,
    ),
    twelve_volts(
        board(303, "Hex", Bm1366, 6, Vcore::Tps546, Cooling::Emc2302, &HEX),
        3,
        1,
    ),
    board(
        400,
        "Supra",
        Bm1368,
        1,
        DAC_80K6,
        EMC2101_INTERNAL,
        &SUPRA_401,
    ),
    board(
        401,
        "Supra",
        Bm1368,
        1,
        DAC_80K6,
        EMC2101_INTERNAL,
        &SUPRA_401,
    ),
    board(
        402,
        "Supra",
        Bm1368,
        1,
        Vcore::Tps546,
        EMC2101_DIODE,
        &SUPRA_403,
    ),
    board(
        403,
        "Supra",
        Bm1368,
        1,
        Vcore::Tps546,
        EMC2101_DIODE,
        &SUPRA_403,
    ),
    board(
        600,
        "Gamma",
        Bm1370,
        1,
        Vcore::Tps546,
        EMC2101_DIODE,
        &GAMMA,
    ),
    board(
        601,
        "Gamma",
        Bm1370,
        1,
        Vcore::Tps546,
        EMC2101_DIODE,
        &GAMMA,
    ),
    board(
        602,
        "Gamma",
        Bm1370,
        1,
        Vcore::Tps546,
        EMC2101_DIODE,
        &GAMMA,
    ),
    board(
        603,
        "Gamma",
        Bm1370,
        1,
        Vcore::Tps546,
        EMC2101_DIODE,
        &GAMMA,
    ),
    // No published GammaDuo design: the Gamma picture stands in.
    board(
        650,
        "GammaDuo",
        Bm1370,
        2,
        Vcore::Tps546,
        EMC2101_DIODE,
        &GAMMA,
    ),
    twelve_volts(
        board(
            701,
            "SupraHex",
            Bm1368,
            6,
            Vcore::Tps546,
            Cooling::Emc2302,
            &SUPRA_HEX,
        ),
        3,
        1,
    ),
    twelve_volts(
        board(
            702,
            "SupraHex",
            Bm1368,
            6,
            Vcore::Tps546,
            Cooling::Emc2302,
            &SUPRA_HEX,
        ),
        3,
        1,
    ),
    twelve_volts(
        board(
            801,
            "GammaTurbo",
            Bm1370,
            2,
            Vcore::Tps546,
            Cooling::Emc2103 { diodes: [0, 1] },
            &GAMMA_TURBO,
        ),
        1,
        2,
    ),
    colour_lcd(
        twelve_volts(
            board(
                1201,
                "NajaDuo",
                Bm1373,
                2,
                Vcore::Tps546,
                Cooling::Emc2103 { diodes: [0, 1] },
                &NAJA_DUO,
            ),
            2,
            2,
        ),
        MADCTL_MV | MADCTL_MY,
    ),
    colour_lcd(
        twelve_volts(
            board(
                1300,
                "GammaHex",
                Bm1370,
                6,
                Vcore::Tps546,
                Cooling::Emc2103 { diodes: [0, 3] },
                &GAMMA_HEX,
            ),
            2,
            4,
        ),
        MADCTL_MV | MADCTL_MX,
    ),
];

/// Family names for the latest revision of each family.
const ALIASES: &[(&str, u16)] = &[
    ("max", 102),
    ("ultra", 207),
    ("hex", 303),
    ("supra", 403),
    ("gamma", 603),
    ("gamma-duo", 650),
    ("supra-hex", 702),
    ("gamma-turbo", 801),
    ("naja-duo", 1201),
    ("gamma-hex", 1300),
];

/// A board by number (`601`, `bitaxe-601`) or family name (`gamma`).
pub fn find(name: &str) -> Option<&'static Spec> {
    let name = name.trim().to_ascii_lowercase();
    let name = name.strip_prefix("bitaxe-").unwrap_or(&name);
    let number = match ALIASES.iter().find(|(alias, _)| *alias == name) {
        Some(&(_, n)) => n,
        None => name.parse().ok()?,
    };
    BOARDS.iter().find(|b| b.number == number)
}

/// The board a firmware file is named after (`esp-miner-rs-bitaxe-601.bin`).
pub fn from_file_name(path: &std::path::Path) -> Option<&'static Spec> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    let at = name.find("bitaxe-")? + "bitaxe-".len();
    let digits: String = name[at..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    find(&digits)
}

/// The board numbers and family names, for messages.
pub fn list() -> String {
    let numbers: Vec<String> = BOARDS.iter().map(|b| b.number.to_string()).collect();
    let names: Vec<String> = ALIASES
        .iter()
        .map(|(alias, n)| format!("{alias} ({n})"))
        .collect();
    format!("{}; or {}", numbers.join(", "), names.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_boards_by_number_name_and_file() {
        assert_eq!(find("601").unwrap().number, 601);
        assert_eq!(find("bitaxe-gamma-hex").unwrap().number, 1300);
        assert_eq!(find("Ultra").unwrap().number, 207);
        assert!(find("999").is_none());
        let path = std::path::Path::new("/tmp/esp-miner-rs-bitaxe-1201.bin");
        assert_eq!(from_file_name(path).unwrap().number, 1201);
        for b in BOARDS {
            assert_eq!(b.chips % b.domains, 0, "{}", b.number);
        }
    }
}
