//! Where the firmware comes from: a flash image file, or the latest CI build
//! of esp-miner-rs or ESP-Miner on a branch; and the choice of board and
//! firmware that the window remembers.

use std::path::{Path, PathBuf};

use md5::{Digest, Md5};

use crate::board::spec::{self, Spec};
use crate::github::{self, Github};

/// 2nd-stage bootloader for the ESP32-S3, as written by `espflash save-image
/// --merge --flash-size 16mb`.
static BOOTLOADER: &[u8] = include_bytes!("../assets/esp32s3-bootloader.bin");
/// Flash size of the CI builds.
const FLASH_SIZE: usize = 16 << 20;
const PARTITION_TABLE: usize = 0x8000;

const RS_REPO: &str = "bitaxeorg/esp-miner-rs";
const RS_WORKFLOW: &str = "rust_ci.yml";
pub const RS_BRANCH: &str = "main";
const IDF_REPO: &str = "bitaxeorg/ESP-Miner";
const IDF_WORKFLOW: &str = "build.yml";
const IDF_ARTIFACT: &str = "esp-miner-factory.bin";
pub const IDF_BRANCH: &str = "master";
pub const LATEST: &str = "latest";

#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// A flash image (bootloader, partition table and application) and,
    /// optionally, the ELF of the application.
    File {
        flash: PathBuf,
        elf: Option<PathBuf>,
    },
    /// The esp-miner-rs CI build for the board.
    EspMinerRs { branch: String },
    /// The ESP-Miner CI factory image.
    EspMiner { branch: String },
    /// The factory image for the board of an ESP-Miner release (with its
    /// board settings); `tag` "latest" for the newest one.
    EspMinerRelease { tag: String },
}

/// A firmware ready to run.
pub struct Image {
    pub flash: Vec<u8>,
    pub elf: Option<Vec<u8>>,
    /// What it is, for the window.
    pub label: String,
    /// The file the flash came from.
    pub path: Option<PathBuf>,
}

impl Source {
    /// Short description, for the window.
    pub fn label(&self) -> String {
        match self {
            Source::File { flash, .. } => flash.file_name().map_or_else(
                || flash.display().to_string(),
                |n| n.to_string_lossy().into(),
            ),
            Source::EspMinerRs { branch } => format!("esp-miner-rs CI ({branch})"),
            Source::EspMiner { branch } => format!("ESP-Miner CI ({branch})"),
            Source::EspMinerRelease { tag } => format!("ESP-Miner release ({tag})"),
        }
    }

    /// Loads the firmware for `spec`, downloading it when needed; `log` gets
    /// the progress.
    pub fn load(&self, spec: &Spec, log: &mut dyn FnMut(String)) -> Result<Image, String> {
        match self {
            Source::File { flash, elf } => {
                let image =
                    std::fs::read(flash).map_err(|e| format!("{}: {e}", flash.display()))?;
                let elf = match elf {
                    Some(p) => Some(std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?),
                    None => None,
                };
                Ok(Image {
                    flash: image,
                    elf,
                    label: self.label(),
                    path: Some(flash.clone()),
                })
            }
            Source::EspMinerRs { branch } => {
                let artifact = format!("esp-miner-rs-bitaxe-{}", spec.number);
                let (dir, run) = fetch(
                    RS_REPO,
                    RS_WORKFLOW,
                    branch,
                    &artifact,
                    "firmware/partitions.csv",
                    log,
                )?;
                let app = read(&dir.join(format!("{artifact}.bin")))?;
                let elf = find_file(&dir, "esp-miner-rs")
                    .map(|p| read(&p))
                    .transpose()?;
                let csv = read(&dir.join("partitions.csv"))?;
                let (table, app_offset) = partition_table(&String::from_utf8_lossy(&csv))?;
                Ok(Image {
                    flash: assemble(&table, &app, app_offset)?,
                    elf,
                    label: format!("esp-miner-rs {run}"),
                    path: None,
                })
            }
            Source::EspMiner { branch } => {
                let (dir, run) = fetch(IDF_REPO, IDF_WORKFLOW, branch, IDF_ARTIFACT, "", log)?;
                let flash = read(&dir.join("esp-miner-merged.bin"))?;
                log(
                    "WARN ESP-Miner reads its board from its NVS settings, which the CI image does not hold"
                        .into(),
                );
                Ok(Image {
                    flash,
                    elf: None,
                    label: format!("ESP-Miner {run}"),
                    path: None,
                })
            }
            Source::EspMinerRelease { tag } => {
                let (tag, file) = fetch_release(IDF_REPO, tag, spec, log)?;
                Ok(Image {
                    flash: read(&file)?,
                    elf: None,
                    label: format!("ESP-Miner {tag}"),
                    path: None,
                })
            }
        }
    }
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// The first file named `name` under `dir`.
fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|n| n == name) {
            return Some(path);
        }
    }
    None
}

/// The directory holding `artifact` of the newest run of `workflow` on
/// `branch` (downloaded into the cache unless it is there already), with
/// `extra` (a file of the repository at the run's commit) next to it, and a
/// description of the run. Without network, the newest cached run.
fn fetch(
    repo: &str,
    workflow: &str,
    branch: &str,
    artifact: &str,
    extra: &str,
    log: &mut dyn FnMut(String),
) -> Result<(PathBuf, String), String> {
    let root = github::cache_dir()
        .join(repo.replace('/', "-"))
        .join(branch.replace('/', "-"));
    let gh = Github::new();
    log(format!(
        "Looking for the latest {artifact} of {repo} on {branch}"
    ));
    match gh.latest_artifact(repo, workflow, branch, artifact) {
        Ok((run, id)) => {
            let dir = root.join(run.id.to_string()).join(artifact);
            let short: String = run.sha.chars().take(7).collect();
            let description = format!("{short} ({})", run.date);
            if !dir.join(".complete").exists() {
                log(format!(
                    "Downloading {artifact} of run {}: {description}",
                    run.id
                ));
                let zip = gh.download_artifact(repo, id)?;
                github::unzip(&zip, &dir)?;
                if !extra.is_empty() {
                    let bytes = gh.file(repo, &run.sha, extra)?;
                    let name = Path::new(extra).file_name().unwrap_or_default();
                    std::fs::write(dir.join(name), bytes).map_err(|e| e.to_string())?;
                }
                std::fs::write(dir.join(".complete"), &description).map_err(|e| e.to_string())?;
            }
            Ok((dir, description))
        }
        Err(e) => {
            log(format!("WARN {e}"));
            // Run ids grow with time: the largest is the newest.
            let newest = std::fs::read_dir(&root)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| e.file_name().to_str()?.parse::<u64>().ok())
                .filter(|id| {
                    root.join(id.to_string())
                        .join(artifact)
                        .join(".complete")
                        .exists()
                })
                .max()
                .ok_or(e)?;
            let dir = root.join(newest.to_string()).join(artifact);
            let description = std::fs::read_to_string(dir.join(".complete")).unwrap_or_default();
            log(format!(
                "Using the cached {artifact} of run {newest}: {description}"
            ));
            Ok((dir, description))
        }
    }
}

/// The factory image for `spec` of release `tag` ("latest" for the newest) of
/// `repo`, downloaded into the cache unless it is there already, with the
/// release tag. Without network, the cached release with the highest tag.
fn fetch_release(
    repo: &str,
    tag: &str,
    spec: &Spec,
    log: &mut dyn FnMut(String),
) -> Result<(String, PathBuf), String> {
    let root = github::cache_dir()
        .join(repo.replace('/', "-"))
        .join("releases");
    let gh = Github::new();
    let tag = if tag.trim().is_empty() {
        LATEST
    } else {
        tag.trim()
    };
    log(format!("Looking for the {repo} release {tag}"));
    match gh.release(repo, tag) {
        Ok((tag, assets)) => {
            let name = format!("esp-miner-factory-{}-{tag}.bin", spec.number);
            let file = root.join(&tag).join(&name);
            if !file.is_file() {
                let (_, url) = assets.iter().find(|(n, _)| *n == name).ok_or_else(|| {
                    format!(
                        "release {tag} has no factory image for board {}",
                        spec.number
                    )
                })?;
                log(format!("Downloading {name}"));
                let bytes = gh.download(url)?;
                std::fs::create_dir_all(root.join(&tag)).map_err(|e| e.to_string())?;
                // Written under another name first, so a cut download is not taken
                // for a whole one.
                let part = file.with_extension("part");
                std::fs::write(&part, bytes).map_err(|e| e.to_string())?;
                std::fs::rename(&part, &file).map_err(|e| e.to_string())?;
            }
            Ok((tag, file))
        }
        Err(e) => {
            log(format!("WARN {e}"));
            let suffix = format!("esp-miner-factory-{}-", spec.number);
            let mut cached: Vec<(String, PathBuf)> = std::fs::read_dir(&root)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|d| {
                    let tag = d.file_name().to_str()?.to_string();
                    let file = d.path().join(format!("{suffix}{tag}.bin"));
                    file.is_file().then_some((tag, file))
                })
                .collect();
            cached.sort();
            let (tag, file) = cached.pop().ok_or(e)?;
            log(format!("Using the cached release {tag}"));
            Ok((tag, file))
        }
    }
}

/// A full flash: the bootloader, the partition table and the application at
/// `app_offset`.
fn assemble(table: &[u8], app: &[u8], app_offset: usize) -> Result<Vec<u8>, String> {
    if app_offset + app.len() > FLASH_SIZE || PARTITION_TABLE + table.len() > app_offset {
        return Err("the application does not fit its partition".into());
    }
    let mut flash = vec![0xff; FLASH_SIZE];
    flash[..BOOTLOADER.len()].copy_from_slice(BOOTLOADER);
    flash[PARTITION_TABLE..PARTITION_TABLE + table.len()].copy_from_slice(table);
    flash[app_offset..app_offset + app.len()].copy_from_slice(app);
    Ok(flash)
}

/// A number of a partition CSV: decimal, `0x` hex, or with a `K`/`M` suffix.
fn parse_number(field: &str) -> Result<u32, String> {
    let f = field.trim();
    let (digits, unit) = match f.chars().last() {
        Some('k' | 'K') => (&f[..f.len() - 1], 1 << 10),
        Some('m' | 'M') => (&f[..f.len() - 1], 1 << 20),
        _ => (f, 1),
    };
    let value = match digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => digits.parse(),
    };
    value
        .map(|v| v * unit)
        .map_err(|_| format!("partition table: bad number {field:?}"))
}

fn subtype(kind: u8, name: &str) -> Result<u8, String> {
    let known = match (kind, name) {
        (0, "factory") => Some(0x00),
        (0, "test") => Some(0x20),
        (0, ota) if ota.starts_with("ota_") => ota[4..].parse::<u8>().ok().map(|n| 0x10 + n),
        (1, "ota") => Some(0x00),
        (1, "phy") => Some(0x01),
        (1, "nvs") => Some(0x02),
        (1, "coredump") => Some(0x03),
        (1, "nvs_keys") => Some(0x04),
        (1, "efuse") => Some(0x05),
        (1, "undefined") => Some(0x06),
        (1, "esphttpd") => Some(0x80),
        (1, "fat") => Some(0x81),
        (1, "spiffs") => Some(0x82),
        (1, "littlefs") => Some(0x83),
        _ => None,
    };
    match known {
        Some(s) => Ok(s),
        None => parse_number(name).map(|n| n as u8),
    }
}

/// The binary partition table of a partition CSV, ended by its MD5 entry, and
/// the offset of the application that boots (the factory one, else the first
/// OTA one). Partitions without an offset follow the previous one, aligned to
/// 64 KiB for applications and 4 KiB for data.
pub fn partition_table(csv: &str) -> Result<(Vec<u8>, usize), String> {
    let mut table = Vec::new();
    let mut next = (PARTITION_TABLE + 0x1000) as u32;
    let (mut factory, mut ota) = (None, None);
    for line in csv.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        if fields.len() < 5 {
            return Err(format!("partition table: {line:?}"));
        }
        let kind = match fields[1] {
            "app" => 0,
            "data" => 1,
            other => parse_number(other)? as u8,
        };
        let sub = subtype(kind, fields[2])?;
        let align = if kind == 0 { 0x10000 } else { 0x1000 };
        let offset = if fields[3].is_empty() {
            next.next_multiple_of(align)
        } else {
            parse_number(fields[3])?
        };
        let size = parse_number(fields[4])?;
        let flags = fields.get(5).map_or(0u32, |f| {
            f.split(':')
                .map(|flag| match flag.trim() {
                    "encrypted" => 1,
                    "readonly" => 2,
                    _ => 0,
                })
                .fold(0, |a, b| a | b)
        });
        table.extend([0xaa, 0x50, kind, sub]);
        table.extend(offset.to_le_bytes());
        table.extend(size.to_le_bytes());
        let mut label = [0u8; 16];
        let name = fields[0].as_bytes();
        label[..name.len().min(16)].copy_from_slice(&name[..name.len().min(16)]);
        table.extend(label);
        table.extend(flags.to_le_bytes());
        next = offset + size;
        match (kind, sub) {
            (0, 0x00) => factory = factory.or(Some(offset)),
            (0, 0x10..0x20) => ota = ota.or(Some(offset)),
            _ => {}
        }
    }
    let digest = Md5::digest(&table);
    table.extend([0xeb, 0xeb]);
    table.extend([0xff; 14]);
    table.extend(digest);
    let app = factory
        .or(ota)
        .ok_or("partition table: no application partition")?;
    Ok((table, app as usize))
}

/// What the emulator runs: the board, the firmware, the chip's ROM image and
/// MAC address, and whether the flash goes back to its file. The window keeps
/// the last one in `$XDG_CONFIG_HOME/bitaxe-emu/choice` (else
/// `~/.config/bitaxe-emu/choice`).
#[derive(Clone, Debug)]
pub struct Choice {
    pub board: &'static Spec,
    pub source: Source,
    pub rom: PathBuf,
    pub mac: [u8; 6],
    /// Write the flash back to its file when the emulator stops.
    pub persist: bool,
}

/// MAC address of the chip when none is given.
pub const DEFAULT_MAC: [u8; 6] = [0x7c, 0xdf, 0xa1, 0x0b, 0x17, 0xae];

/// A MAC address written `7c:df:a1:0b:17:ae`.
pub fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let bytes: Vec<u8> = s
        .trim()
        .split(':')
        .map(|b| u8::from_str_radix(b, 16).ok())
        .collect::<Option<_>>()?;
    bytes.try_into().ok()
}

pub fn format_mac(mac: &[u8; 6]) -> String {
    mac.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn choice_file() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(dir.join("bitaxe-emu").join("choice"))
}

impl Choice {
    /// Gamma 601 with the esp-miner-rs CI build of its main branch.
    pub fn default_choice() -> Self {
        Choice {
            board: spec::find("601").expect("board 601"),
            source: Source::EspMinerRs {
                branch: RS_BRANCH.into(),
            },
            rom: esp32s3_rax::DEFAULT_ROM.into(),
            mac: DEFAULT_MAC,
            persist: false,
        }
    }

    pub fn load() -> Option<Self> {
        let text = std::fs::read_to_string(choice_file()?).ok()?;
        let get = |key: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
                .map(str::to_string)
        };
        let board = spec::find(&get("board")?)?;
        let branch = get("branch");
        let source = match get("source")?.as_str() {
            "esp-miner-rs" => Source::EspMinerRs {
                branch: branch.unwrap_or_else(|| RS_BRANCH.into()),
            },
            "esp-miner" => Source::EspMiner {
                branch: branch.unwrap_or_else(|| IDF_BRANCH.into()),
            },
            "esp-miner-release" => Source::EspMinerRelease {
                tag: get("tag").unwrap_or_else(|| LATEST.into()),
            },
            "file" => Source::File {
                flash: get("flash")?.into(),
                elf: get("elf").filter(|e| !e.is_empty()).map(PathBuf::from),
            },
            _ => return None,
        };
        Some(Choice {
            board,
            source,
            rom: get("rom").map_or_else(|| esp32s3_rax::DEFAULT_ROM.into(), PathBuf::from),
            mac: get("mac")
                .and_then(|m| parse_mac(&m))
                .unwrap_or(DEFAULT_MAC),
            persist: get("persist").is_some_and(|p| p == "true"),
        })
    }

    /// Saves the choice. The ROM and MAC are kept only when they differ from
    /// the defaults, which follow the build.
    pub fn save(&self) {
        let Some(path) = choice_file() else { return };
        let mut text = format!("board={}\n", self.board.number);
        match &self.source {
            Source::EspMinerRs { branch } => {
                text += &format!("source=esp-miner-rs\nbranch={branch}\n");
            }
            Source::EspMiner { branch } => text += &format!("source=esp-miner\nbranch={branch}\n"),
            Source::EspMinerRelease { tag } => {
                text += &format!("source=esp-miner-release\ntag={tag}\n");
            }
            Source::File { flash, elf } => {
                text += &format!("source=file\nflash={}\n", flash.display());
                if let Some(elf) = elf {
                    text += &format!("elf={}\n", elf.display());
                }
            }
        }
        if self.rom != Path::new(esp32s3_rax::DEFAULT_ROM) {
            text += &format!("rom={}\n", self.rom.display());
        }
        if self.mac != DEFAULT_MAC {
            text += &format!("mac={}\n", format_mac(&self.mac));
        }
        text += &format!("persist={}\n", self.persist);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, text);
    }
}

/// The ELF next to a flash image: the same path with the `.elf` extension.
pub fn elf_beside(flash: &Path) -> Option<PathBuf> {
    let elf = flash.with_extension("elf");
    elf.is_file().then_some(elf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partition_table_matches_espflash() {
        let csv = "\
# Name,      Type, SubType,   Offset,    Size, Flags
nvs,         data, nvs,       0x9000,    0x6000
phy_init,    data, phy,       0xf000,    0x1000
factory,     app,  factory,   0x10000,   4M
www,         data, spiffs,    0x410000,  3M
ota_0,       app,  ota_0,     0x710000,  4M
ota_1,       app,  ota_1,     0xb10000,  4M
otadata,     data, ota,       0xf10000,  8k
coredump,    data, coredump,          ,  64K
";
        let (table, app) = partition_table(csv).unwrap();
        assert_eq!(app, 0x10000);
        assert_eq!(table.len(), 9 * 32);
        // The coredump partition follows otadata.
        assert_eq!(
            &table[7 * 32 + 4..7 * 32 + 12],
            &[0, 0x20, 0xf1, 0, 0, 0, 1, 0]
        );
        // The MD5 entry espflash writes for the same table.
        assert_eq!(
            &table[8 * 32..],
            &[
                0xeb, 0xeb, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xff, 0x30, 0xed, 0x11, 0x9c, 0xa4, 0x2c, 0xc0, 0x08, 0x61, 0x18, 0x0e, 0x5d,
                0xd6, 0xcd, 0x2c, 0xd3
            ]
        );
    }

    #[test]
    fn mac_addresses() {
        assert_eq!(parse_mac("7c:df:a1:0b:17:ae"), Some(DEFAULT_MAC));
        assert_eq!(format_mac(&DEFAULT_MAC), "7c:df:a1:0b:17:ae");
        assert_eq!(parse_mac("7c:df:a1:0b:17"), None);
        assert_eq!(parse_mac("7c:df:a1:0b:17:zz"), None);
    }

    #[test]
    fn bootloader_is_an_esp_image() {
        assert_eq!(BOOTLOADER[0], 0xe9);
        // DIO, 16 MB.
        assert_eq!(&BOOTLOADER[2..4], &[0x02, 0x40]);
    }
}
