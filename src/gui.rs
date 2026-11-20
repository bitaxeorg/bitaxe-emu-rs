//! The window: the front of the board with its display, its RESET and BOOT
//! buttons and its fans, next to the board telemetry and the firmware logs
//! (BLIT). Menus pick the board and the firmware; each choice runs in its own
//! emulator thread, a session.

use std::path::PathBuf;
use std::sync::mpsc::{Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use blit::{
    Animation, AnimationOptions, Atom, AtomCx, Constraints, LogicalRect, NodeTarget, Sides, Size,
};
use blit_desktop::{Application, Config, DesktopContext, EventLoopProxy, Scope, cpu};
use blit_gui::clip::BoundsClip;
use blit_gui::color::Color;
use blit_gui::image::{Image, ImageData, ImageFormat, ImageHandle, ImagePixels, ImageSampling};
use blit_gui::mesh::{Mesh, MeshVertex};
use blit_gui::rectangle::Rectangle;
use blit_gui::style::{Border, BorderRadius};
use blit_gui::text::system::{FontStack, TextConfig, TextSystem};
use blit_gui::text::{FontId, Text, TextStyle};
use blit_gui_widgets::interact;
use blit_gui_widgets::layout::{Align, Sizing, absolute, flex};
use blit_gui_widgets::popover::{Popover, PopoverState};
use blit_gui_widgets::scroll::ScrollState;
use blit_gui_widgets::scroll::uniform_list::UniformList;
use blit_gui_widgets::text_input::{TextInput, TextInputState};
use blit_text::{Family, FontData};

use crate::board::Board;
use crate::board::spec::{self, Spec};
use crate::emu::{Command, Emulator, Frame, Snapshot};
use crate::firmware::{self, Choice, Source};

type Ctx = DesktopContext<cpu::Backend>;
type Ui<'a> = blit::Ui<'a, Ctx>;

static FONT: &[u8] = include_bytes!(env!("BITAXE_EMU_FONT"));
static FONT_MONO: &[u8] = include_bytes!(env!("BITAXE_EMU_FONT_MONO"));
const MONO: FontId = FontId(1);

/// Size of the board picture in the window, at most.
const BOARD_HEIGHT: f32 = 895.0;
const BOARD_WIDTH: f32 = 1000.0;
/// Width of the side panel (telemetry and logs).
const SIDE_WIDTH: f32 = 640.0;
const GAP: f32 = 18.0;
/// Hit size of the buttons on the board picture, in its pixels.
const BUTTON_SIZE: f32 = 52.0;
/// Height of a line of the firmware logs.
const LOG_LINE: f32 = 16.0;

const BACKGROUND: Color = Color::rgb(0x12, 0x14, 0x18);
const PANEL: Color = Color::rgb(0x1c, 0x1f, 0x26);
const MENU: Color = Color::rgb(0x24, 0x28, 0x31);
const TEXT: Color = Color::rgb(0xe6, 0xe8, 0xee);
const MUTED: Color = Color::rgb(0x8c, 0x93, 0xa3);
const ACCENT: Color = Color::rgb(0xf2, 0xc8, 0x1e);
const HOVER: Color = Color::rgba(0xff, 0xff, 0xff, 0x28);
const PRESSED: Color = Color::rgba(0xf2, 0xc8, 0x1e, 0x60);

/// Wakes the window after each emulator snapshot.
pub struct Wake;

/// A 40 mm fan seen from above, its rotor at `angle` radians.
#[derive(Clone, Copy, PartialEq)]
struct Fan {
    angle: f32,
}

impl Atom<Ctx> for Fan {
    fn measure(&self, _: &mut AtomCx<'_, Ctx>, constraints: Constraints<f32>) -> Size<f32> {
        constraints.constrain(Size::ZERO)
    }

    fn paint(&self, cx: &mut AtomCx<'_, Ctx>, area: LogicalRect) {
        let context = cx.context_mut();
        let frame = Color::rgb(0x1a, 0x1b, 0x1e);
        Rectangle::new()
            .background(frame)
            .border(Border::solid(2.0, Color::rgb(0x33, 0x35, 0x3a)))
            .radius(BorderRadius::uniform(area.width * 0.08))
            .paint(area, context);
        let (cx0, cy0) = (area.x + area.width / 2.0, area.y + area.height / 2.0);
        let r = area.width * 0.46;
        // Duct, then the blades, then the hub.
        Rectangle::new()
            .background(Color::rgb(0x0b, 0x0b, 0x0c))
            .radius(BorderRadius::uniform(r))
            .paint(
                LogicalRect::new(cx0 - r, cy0 - r, 2.0 * r, 2.0 * r),
                context,
            );
        for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
            let d = area.width * 0.42;
            let s = area.width * 0.035;
            Rectangle::new()
                .background(Color::rgb(0x55, 0x58, 0x5e))
                .radius(BorderRadius::uniform(s))
                .paint(
                    LogicalRect::new(cx0 + sx * d - s, cy0 + sy * d - s, 2.0 * s, 2.0 * s),
                    context,
                );
        }
        let blade = Color::rgb(0x2b, 0x2d, 0x31);
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let (r0, r1) = (r * 0.3, r * 0.95);
        for b in 0..7 {
            let a = self.angle + b as f32 * std::f32::consts::TAU / 7.0;
            let base = vertices.len() as u32;
            for (rad, off) in [(r0, -0.32), (r1, -0.05), (r1, 0.38), (r0, 0.12)] {
                let t = a + off;
                vertices.push(MeshVertex::new(
                    cx0 + rad * t.cos(),
                    cy0 + rad * t.sin(),
                    blade,
                ));
            }
            indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        Mesh {
            vertices: &vertices,
            indices: &indices,
        }
        .paint(area, context);
        let hub = r * 0.32;
        Rectangle::new()
            .background(Color::rgb(0x22, 0x23, 0x26))
            .border(Border::solid(1.5, Color::rgb(0x3a, 0x3c, 0x42)))
            .radius(BorderRadius::uniform(hub))
            .paint(
                LogicalRect::new(cx0 - hub, cy0 - hub, 2.0 * hub, 2.0 * hub),
                context,
            );
    }
}

/// What the window starts with.
pub struct Launch {
    pub choice: Choice,
    /// Where the Save button writes the display's image.
    pub display_png: Option<PathBuf>,
}

/// An emulator thread: loads the firmware, then runs it until told to quit.
struct Session {
    snapshot: Arc<Mutex<Snapshot>>,
    commands: Sender<Command>,
    worker: JoinHandle<()>,
}

/// The running session, also stopped by [`run`] when the window closes.
static SESSION: Mutex<Option<Session>> = Mutex::new(None);

fn log(snapshot: &Mutex<Snapshot>, line: String) {
    let mut s = snapshot.lock().unwrap();
    s.logs.push_back(line);
    s.log_count += 1;
}

impl Session {
    /// Starts `choice` once `previous` has stopped (and saved its flash).
    fn start(choice: &Choice, proxy: EventLoopProxy<Wake>, previous: Option<Session>) -> Session {
        let snapshot = Arc::new(Mutex::new(Snapshot {
            firmware: choice.source.label(),
            ..Default::default()
        }));
        let (commands, rx) = channel();
        let choice = choice.clone();
        let snap = snapshot.clone();
        let worker = std::thread::spawn(move || {
            if let Some(previous) = previous {
                previous.stop();
            }
            let wake = || {
                let _ = proxy.send_event(Wake);
            };
            let fail = |e: String| {
                log(&snap, format!("ERROR {e}"));
                wake();
                // Keep the message on screen until the next choice.
                while let Ok(command) = rx.recv() {
                    if matches!(command, Command::Quit) {
                        break;
                    }
                }
            };
            let rom = match std::fs::read(&choice.rom) {
                Ok(rom) => rom,
                Err(e) => return fail(format!("ROM image {}: {e}", choice.rom.display())),
            };
            let image = match choice.source.load(choice.board, &mut |line| {
                log(&snap, line);
                wake();
            }) {
                Ok(image) => image,
                Err(e) => return fail(e),
            };
            if matches!(
                rx.try_recv(),
                Ok(Command::Quit) | Err(TryRecvError::Disconnected)
            ) {
                return;
            }
            snap.lock().unwrap().firmware = image.label.clone();
            let mut board = Board::new(choice.board, rom, image.flash, choice.mac);
            if choice.persist {
                board.flash_path = image.path.clone();
            }
            let mut emulator = Emulator::with_snapshot(board, image.elf.as_deref(), snap);
            emulator.flash_file = image.path;
            let mut last = Instant::now();
            emulator.run_realtime(rx, || {
                // About 60 frames per second.
                if last.elapsed().as_millis() >= 16 {
                    last = Instant::now();
                    wake();
                }
            });
            emulator.board.save_flash();
        });
        Session {
            snapshot,
            commands,
            worker,
        }
    }

    fn stop(self) {
        let _ = self.commands.send(Command::Quit);
        let _ = self.worker.join();
    }
}

/// The picture of a board, loaded.
struct Picture {
    spec: &'static Spec,
    image: ImageHandle,
    size: (f32, f32),
    /// Pixels of the window per pixel of the picture.
    scale: f32,
}

/// A text field and its editing state.
#[derive(Default)]
struct Field {
    value: String,
    state: TextInputState,
}

impl Field {
    fn new(value: impl Into<String>) -> Self {
        Field {
            value: value.into(),
            state: TextInputState::default(),
        }
    }
}

/// What the settings menu asks for.
enum Setting {
    /// Restart with this ROM image and MAC address.
    Restart(PathBuf, [u8; 6]),
    Persist(bool),
    SaveDisplay(PathBuf),
}

struct App {
    proxy: EventLoopProxy<Wake>,
    choice: Choice,
    snapshot: Arc<Mutex<Snapshot>>,
    commands: Sender<Command>,
    picture: Option<Picture>,
    display: Option<(u64, ImageHandle)>,
    boot_pressed: bool,
    fan_angles: Vec<f32>,
    last_frame: Instant,
    board_menu: PopoverState,
    firmware_menu: PopoverState,
    settings_menu: PopoverState,
    rs_branch: Field,
    idf_branch: Field,
    file: Field,
    elf: Field,
    rom: Field,
    mac: Field,
    png: Field,
    logs_scroll: ScrollState,
    /// [`Snapshot::log_count`] at the last frame.
    seen_logs: u64,
}

/// Decodes a PNG to RGBA.
fn decode_png(png_bytes: &[u8]) -> (Vec<u8>, u32, u32) {
    let decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    let mut reader = decoder.read_info().expect("board image");
    let mut buf = vec![0; reader.output_buffer_size().expect("board image size")];
    let info = reader.next_frame(&mut buf).expect("board image");
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf[..info.buffer_size()].to_vec(),
        png::ColorType::Rgb => buf[..info.buffer_size()]
            .chunks(3)
            .flat_map(|p| [p[0], p[1], p[2], 0xff])
            .collect(),
        other => panic!("board image: unsupported colour type {other:?}"),
    };
    (rgba, info.width, info.height)
}

/// Size of a PNG, from its header.
fn png_size(png_bytes: &[u8]) -> (u32, u32) {
    let reader = png::Decoder::new(std::io::Cursor::new(png_bytes))
        .read_info()
        .expect("board image");
    let info = reader.info();
    (info.width, info.height)
}

/// `rgba` (`w` × `h`) turned clockwise by `degrees` (0, 90, 180 or 270).
fn rotate(rgba: &[u8], w: u32, h: u32, degrees: u16) -> (Vec<u8>, u32, u32) {
    let (w, h) = (w as usize, h as usize);
    let (ow, oh) = if degrees % 180 == 0 { (w, h) } else { (h, w) };
    let mut out = vec![0u8; ow * oh * 4];
    for y in 0..h {
        for x in 0..w {
            let (ox, oy) = match degrees {
                90 => (h - 1 - y, x),
                180 => (w - 1 - x, h - 1 - y),
                270 => (y, w - 1 - x),
                _ => (x, y),
            };
            let (i, o) = ((y * w + x) * 4, (oy * ow + ox) * 4);
            out[o..o + 4].copy_from_slice(&rgba[i..i + 4]);
        }
    }
    (out, ow as u32, oh as u32)
}

fn display_image(ctx: &mut Ctx, frame: &Frame, degrees: u16) -> ImageHandle {
    let (rgba, w, h) = rotate(&frame.rgba, frame.width, frame.height, degrees);
    ctx.create_image(ImageData::new(
        ImagePixels::Owned(rgba.into_boxed_slice()),
        ImageFormat::Rgba8,
        w,
        h,
    ))
    .expect("display image")
}

/// A text button; returns whether it was clicked. `selected` marks the
/// current choice.
fn button(ui: Ui<'_>, label: &str, selected: bool) -> bool {
    let mut b = ui
        .layout(flex::column())
        .pad(Sides::new().top(5.0).right(10.0).bottom(5.0).left(10.0));
    let press = b.interact(interact::press());
    b.insert(
        Rectangle::new()
            .background(if press.active {
                PRESSED
            } else if press.hovered {
                HOVER
            } else if selected {
                Color::rgba(0xf2, 0xc8, 0x1e, 0x30)
            } else {
                BACKGROUND
            })
            .border(Border::solid(
                1.5,
                if selected {
                    ACCENT
                } else {
                    Color::rgb(0x3a, 0x3e, 0x48)
                },
            ))
            .radius(BorderRadius::uniform(6.0)),
    );
    b.child()
        .insert(Text::new(label).style(TextStyle::new().size(14.0).color(TEXT)));
    press.clicked
}

/// The background of a menu.
fn menu_frame() -> Rectangle<'static> {
    Rectangle::new()
        .background(MENU)
        .border(Border::solid(1.5, ACCENT))
        .radius(BorderRadius::uniform(8.0))
}

/// A single-line text field `width` wide; returns whether Enter was pressed.
fn field(ui: Ui<'_>, field: &mut Field, width: f32, placeholder: &str) -> bool {
    let mut f = ui.layout(flex::column()).pad(1.5);
    f.insert(
        Rectangle::new()
            .background(BACKGROUND)
            .border(Border::solid(1.5, Color::rgb(0x3a, 0x3e, 0x48)))
            .radius(BorderRadius::uniform(6.0)),
    );
    f.child()
        .item(flex::item().width(Sizing::fixed(width)))
        .build(
            TextInput::new(&mut field.state, &mut field.value)
                .style(TextStyle::new().size(14.0).color(TEXT))
                .padding(Sides::new().top(5.0).right(8.0).bottom(5.0).left(8.0))
                .placeholder(placeholder)
                .placeholder_color(MUTED)
                .cursor_background(ACCENT),
        )
        .submitted
}

/// A label `width` wide, for the rows of a menu.
fn label(ui: Ui<'_>, text: &str, width: f32) {
    let mut l = ui.layout(flex::column());
    l.child()
        .item(flex::item().width(Sizing::fixed(width)))
        .insert(Text::new(text).style(TextStyle::new().size(13.0).color(MUTED)));
}

/// The trigger of a menu: its label in a frame.
fn menu_trigger(ui: Ui<'_>, text: &str, hovered: bool) {
    let mut t = ui
        .layout(flex::column())
        .pad(Sides::new().top(4.0).right(8.0).bottom(4.0).left(8.0));
    t.insert(
        Rectangle::new()
            .background(if hovered { HOVER } else { BACKGROUND })
            .border(Border::solid(1.0, Color::rgb(0x3a, 0x3e, 0x48)))
            .radius(BorderRadius::uniform(6.0)),
    );
    t.child()
        .insert(Text::new(text).style(TextStyle::new().size(14.0).color(TEXT)));
}

/// `~/` at the start of a path stands for the home directory.
fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

/// Shared state handed to [`App::new`] by [`run`].
static HANDOFF: Mutex<Option<Launch>> = Mutex::new(None);

impl App {
    /// A RESET or BOOT button was clicked or is held.
    fn button(&mut self, label: &str, clicked: bool, active: bool) {
        match label {
            "RESET" if clicked => {
                let _ = self.commands.send(Command::Reset);
            }
            "BOOT" if active != self.boot_pressed => {
                self.boot_pressed = active;
                let _ = self.commands.send(Command::BootButton(active));
            }
            _ => {}
        }
    }

    /// Runs `choice` instead of the current one.
    fn switch(&mut self, choice: Choice) {
        choice.save();
        let previous = SESSION.lock().unwrap().take();
        let session = Session::start(&choice, self.proxy.clone(), previous);
        self.snapshot = session.snapshot.clone();
        self.commands = session.commands.clone();
        *SESSION.lock().unwrap() = Some(session);
        self.display = None;
        self.boot_pressed = false;
        self.logs_scroll = ScrollState::new();
        self.seen_logs = 0;
        self.choice = choice;
    }

    /// Applies what the settings menu asked for.
    fn apply(&mut self, setting: Setting) {
        match setting {
            Setting::Restart(rom, mac) => {
                let choice = Choice {
                    rom,
                    mac,
                    ..self.choice.clone()
                };
                self.switch(choice);
            }
            Setting::Persist(on) => {
                self.choice.persist = on;
                self.choice.save();
                let _ = self.commands.send(Command::Persist(on));
            }
            Setting::SaveDisplay(path) => {
                let _ = self.commands.send(Command::SaveDisplay(path));
            }
        }
    }

    /// The board menu: one row per family, a button per board number.
    fn board_menu(&mut self, ui: Ui<'_>) -> Option<&'static Spec> {
        let current = self.choice.board.number;
        let title = format!("{} ▾", self.choice.board.name());
        let picked = ui.build(
            Popover::new(&mut self.board_menu)
                .y(6.0)
                .trigger(|ui, press, open| {
                    let mut t = ui.layout(flex::column()).pad(Sides::new().right(8.0));
                    if press.hovered || open {
                        t.insert(
                            Rectangle::new()
                                .background(HOVER)
                                .radius(BorderRadius::uniform(6.0)),
                        );
                    }
                    t.child()
                        .insert(Text::new(&title).style(TextStyle::new().size(28.0).color(ACCENT)));
                })
                .content(|ui: Ui<'_>| {
                    let mut menu = ui.layout(flex::column().gap(6.0)).pad(12.0);
                    menu.insert(menu_frame());
                    let mut picked = None;
                    let mut families: Vec<&str> = Vec::new();
                    for b in spec::BOARDS {
                        if !families.contains(&b.family) {
                            families.push(b.family);
                        }
                    }
                    for family in families {
                        let mut row = menu
                            .child()
                            .layout(flex::row().gap(6.0).align(Align::Center));
                        row.child().build(|ui: Ui<'_>| label(ui, family, 110.0));
                        for b in spec::BOARDS.iter().filter(|b| b.family == family) {
                            let number = b.number.to_string();
                            if row
                                .child()
                                .build(|ui: Ui<'_>| button(ui, &number, b.number == current))
                            {
                                picked = Some(b);
                            }
                        }
                    }
                    picked
                }),
        );
        let picked = picked.flatten();
        if picked.is_some() {
            self.board_menu.open = false;
        }
        picked
    }

    /// The firmware menu: the two CI builds with their branch, or a file and
    /// its ELF.
    fn firmware_menu(&mut self, ui: Ui<'_>, text: &str) -> Option<Source> {
        let current = &self.choice.source;
        let (is_rs, is_idf, is_file) = (
            matches!(current, Source::EspMinerRs { .. }),
            matches!(current, Source::EspMiner { .. }),
            matches!(current, Source::File { .. }),
        );
        let text = format!("{text} ▾");
        let (rs_branch, idf_branch, file, elf) = (
            &mut self.rs_branch,
            &mut self.idf_branch,
            &mut self.file,
            &mut self.elf,
        );
        let picked = ui.build(
            Popover::new(&mut self.firmware_menu)
                .y(6.0)
                .trigger(|ui, press, open| menu_trigger(ui, &text, press.hovered || open))
                .content(|ui: Ui<'_>| {
                    let mut menu = ui.layout(flex::column().gap(10.0)).pad(14.0);
                    menu.insert(menu_frame());
                    let mut picked = None;
                    let rows: [(&str, bool, &str, &mut Field, f32, &str); 3] = [
                        ("esp-miner-rs CI", is_rs, "branch", rs_branch, 160.0, firmware::RS_BRANCH),
                        ("ESP-Miner CI", is_idf, "branch", idf_branch, 160.0, firmware::IDF_BRANCH),
                        ("Flash image", is_file, "file", file, 380.0, "~/esp-miner-rs-bitaxe-601.bin"),
                    ];
                    let mut file_submitted = false;
                    for (i, (name, selected, what, value, width, placeholder)) in
                        rows.into_iter().enumerate()
                    {
                        let mut row = menu
                            .child()
                            .layout(flex::row().gap(8.0).align(Align::Center));
                        let clicked = row
                            .child()
                            .item(flex::item().width(Sizing::fixed(150.0)))
                            .build(|ui: Ui<'_>| button(ui, name, selected));
                        row.child().build(|ui: Ui<'_>| label(ui, what, 48.0));
                        let submitted = row
                            .child()
                            .build(|ui: Ui<'_>| field(ui, value, width, placeholder));
                        if !(clicked || submitted) {
                            continue;
                        }
                        let text = value.value.trim().to_string();
                        picked = match i {
                            0 => Some(Source::EspMinerRs {
                                branch: if text.is_empty() { firmware::RS_BRANCH.into() } else { text },
                            }),
                            1 => Some(Source::EspMiner {
                                branch: if text.is_empty() { firmware::IDF_BRANCH.into() } else { text },
                            }),
                            _ => {
                                file_submitted = true;
                                None
                            }
                        };
                    }
                    let elf_submitted = {
                        let mut row = menu
                            .child()
                            .layout(flex::row().gap(8.0).align(Align::Center));
                        row.child().build(|ui: Ui<'_>| label(ui, "", 150.0));
                        row.child().build(|ui: Ui<'_>| label(ui, "ELF", 48.0));
                        row.child().build(|ui: Ui<'_>| {
                            field(ui, elf, 380.0, "beside the image, with the .elf extension")
                        })
                    };
                    if (file_submitted || elf_submitted) && !file.value.trim().is_empty() {
                        let flash = expand_home(file.value.trim());
                        let elf = match elf.value.trim() {
                            "" => firmware::elf_beside(&flash),
                            path => Some(expand_home(path)),
                        };
                        picked = Some(Source::File { flash, elf });
                    }
                    for note in [
                        "Click a source, or press Enter in its field, to load it.",
                        "A CI build is the newest of the branch; downloading it needs a GitHub token",
                        "(GH_TOKEN, or gh auth login). Builds stay in ~/.cache/bitaxe-emu.",
                    ] {
                        menu.child()
                            .insert(Text::new(note).style(TextStyle::new().size(12.0).color(MUTED)));
                    }
                    picked
                }),
        );
        let picked = picked.flatten();
        if picked.is_some() {
            self.firmware_menu.open = false;
        }
        picked
    }

    /// The settings menu: the chip's ROM image and MAC address, writing the
    /// flash back, and saving the display.
    fn settings_menu(&mut self, ui: Ui<'_>) -> Option<Setting> {
        let persist = self.choice.persist;
        let (rom, mac, png) = (&mut self.rom, &mut self.mac, &mut self.png);
        let asked = ui.build(
            Popover::new(&mut self.settings_menu)
                .y(6.0)
                .trigger(|ui, press, open| menu_trigger(ui, "Settings ▾", press.hovered || open))
                .content(|ui: Ui<'_>| {
                    let mut menu = ui.layout(flex::column().gap(10.0)).pad(14.0);
                    menu.insert(menu_frame());
                    let mut asked = None;

                    let rom_entered = {
                        let mut row = menu
                            .child()
                            .layout(flex::row().gap(8.0).align(Align::Center));
                        row.child()
                            .build(|ui: Ui<'_>| label(ui, "ROM image", 110.0));
                        row.child()
                            .build(|ui: Ui<'_>| field(ui, rom, 420.0, esp32s3_emu::DEFAULT_ROM))
                    };
                    let (mac_entered, restart) = {
                        let mut row = menu
                            .child()
                            .layout(flex::row().gap(8.0).align(Align::Center));
                        row.child()
                            .build(|ui: Ui<'_>| label(ui, "MAC address", 110.0));
                        let entered = row
                            .child()
                            .build(|ui: Ui<'_>| field(ui, mac, 170.0, "7c:df:a1:0b:17:ae"));
                        let restart = row
                            .child()
                            .build(|ui: Ui<'_>| button(ui, "Restart the chip with them", false));
                        (entered, restart)
                    };
                    if rom_entered || mac_entered || restart {
                        let rom_path = match rom.value.trim() {
                            "" => PathBuf::from(esp32s3_emu::DEFAULT_ROM),
                            path => expand_home(path),
                        };
                        match firmware::parse_mac(&mac.value) {
                            Some(mac) => asked = Some(Setting::Restart(rom_path, mac)),
                            None => mac.value = "write it 7c:df:a1:0b:17:ae".into(),
                        }
                    }

                    {
                        let mut row = menu
                            .child()
                            .layout(flex::row().gap(8.0).align(Align::Center));
                        row.child().build(|ui: Ui<'_>| label(ui, "Flash", 110.0));
                        let toggle = if persist {
                            "Written back to its file when the emulator stops"
                        } else {
                            "Not written back to its file"
                        };
                        if row.child().build(|ui: Ui<'_>| button(ui, toggle, persist)) {
                            asked = Some(Setting::Persist(!persist));
                        }
                    }

                    let (png_entered, save) = {
                        let mut row = menu
                            .child()
                            .layout(flex::row().gap(8.0).align(Align::Center));
                        row.child().build(|ui: Ui<'_>| label(ui, "Display", 110.0));
                        let entered = row
                            .child()
                            .build(|ui: Ui<'_>| field(ui, png, 320.0, "~/bitaxe-display.png"));
                        let save = row
                            .child()
                            .build(|ui: Ui<'_>| button(ui, "Save as PNG", false));
                        (entered, save)
                    };
                    if png_entered || save {
                        let path = match png.value.trim() {
                            "" => "~/bitaxe-display.png",
                            path => path,
                        };
                        asked = Some(Setting::SaveDisplay(expand_home(path)));
                    }
                    asked
                }),
        );
        let asked = asked.flatten();
        if matches!(asked, Some(Setting::Restart(..))) {
            self.settings_menu.open = false;
        }
        asked
    }
}

impl Application for App {
    type Input = Wake;
    type Graphics = cpu::Backend;

    fn new(proxy: EventLoopProxy<Wake>, _: Scope<Self>, _: &mut Ctx) -> Self {
        let launch = HANDOFF.lock().unwrap().take().expect("emulator handoff");
        let choice = launch.choice;
        let session = Session::start(&choice, proxy.clone(), None);
        let branch =
            |default: &str, own: Option<&String>| Field::new(own.map_or(default, |b| b.as_str()));
        let (rs_branch, idf_branch) = match &choice.source {
            Source::EspMinerRs { branch: b } => (
                branch(firmware::RS_BRANCH, Some(b)),
                branch(firmware::IDF_BRANCH, None),
            ),
            Source::EspMiner { branch: b } => (
                branch(firmware::RS_BRANCH, None),
                branch(firmware::IDF_BRANCH, Some(b)),
            ),
            Source::File { .. } => (
                branch(firmware::RS_BRANCH, None),
                branch(firmware::IDF_BRANCH, None),
            ),
        };
        let (file, elf) = match &choice.source {
            Source::File { flash, elf } => (
                Field::new(flash.display().to_string()),
                Field::new(
                    elf.as_ref()
                        .map(|e| e.display().to_string())
                        .unwrap_or_default(),
                ),
            ),
            _ => (Field::default(), Field::default()),
        };
        let app = App {
            proxy,
            snapshot: session.snapshot.clone(),
            commands: session.commands.clone(),
            rom: Field::new(choice.rom.display().to_string()),
            mac: Field::new(firmware::format_mac(&choice.mac)),
            png: Field::new(
                launch
                    .display_png
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            ),
            choice,
            picture: None,
            display: None,
            boot_pressed: false,
            fan_angles: Vec::new(),
            last_frame: Instant::now(),
            board_menu: PopoverState::new(),
            firmware_menu: PopoverState::new(),
            settings_menu: PopoverState::new(),
            rs_branch,
            idf_branch,
            file,
            elf,
            logs_scroll: ScrollState::new(),
            seen_logs: 0,
        };
        *SESSION.lock().unwrap() = Some(session);
        app
    }

    fn input(&mut self, _: Wake) {}

    fn build(&mut self, mut ui: Ui<'_>) {
        let spec = self.choice.board;
        if self
            .picture
            .as_ref()
            .is_none_or(|p| p.spec.number != spec.number)
        {
            let (rgba, w, h) = decode_png(spec.art.png);
            let image = ui
                .context_mut()
                .create_image(ImageData::new(
                    ImagePixels::Owned(rgba.into_boxed_slice()),
                    ImageFormat::Rgba8,
                    w,
                    h,
                ))
                .expect("board image");
            self.picture = Some(Picture {
                spec,
                image,
                size: (w as f32, h as f32),
                scale: board_scale(w as f32, h as f32),
            });
            self.fan_angles = vec![0.0; spec.art.fans.len()];
        }
        let snap = self.snapshot.lock().unwrap().clone();
        let dt = self.last_frame.elapsed().as_secs_f32().min(0.1);
        self.last_frame = Instant::now();
        // The rotors turn at a fraction of their speed, to stay visible on screen.
        for (i, angle) in self.fan_angles.iter_mut().enumerate() {
            let rpm = snap
                .fan_rpm
                .get(i)
                .or(snap.fan_rpm.first())
                .copied()
                .unwrap_or(0.0);
            *angle =
                (*angle + rpm / 60.0 * std::f32::consts::TAU * 0.04 * dt) % std::f32::consts::TAU;
        }
        let art = spec.art;
        let picture = self.picture.as_ref().expect("board picture");
        let scale = picture.scale;
        let mut new_choice = None;
        let setting;

        let mut root = ui.layout(flex::row().gap(GAP)).pad(GAP);
        root.insert(Rectangle::new().background(BACKGROUND));

        // The board.
        let (bw, bh) = (picture.size.0 * scale, picture.size.1 * scale);
        let mut clicks = Vec::new();
        {
            let mut board = root.child().item(flex::item().fixed(bw, bh)).layout(());
            board.insert(Image::new(&picture.image).sampling(ImageSampling::Bilinear));

            let frame = &snap.display;
            if self
                .display
                .as_ref()
                .is_none_or(|(g, _)| *g != frame.generation)
                && !frame.rgba.is_empty()
            {
                let img = display_image(board.context_mut(), frame, art.display_rotation);
                self.display = Some((frame.generation, img));
            }
            if let Some((_, img)) = &self.display {
                let (w, h) = (art.display_size.0 * scale, art.display_size.1 * scale);
                let sampling = if frame.width > 200 {
                    ImageSampling::Bilinear
                } else {
                    ImageSampling::Nearest
                };
                board
                    .child()
                    .place(
                        NodeTarget::Parent,
                        absolute::place()
                            .x(art.display_center.0 * scale - w / 2.0)
                            .y(art.display_center.1 * scale - h / 2.0)
                            .width(absolute::Sizing::fixed(w))
                            .height(absolute::Sizing::fixed(h)),
                    )
                    .insert(Image::new(img).sampling(sampling));
            }

            for (&(center, size), &angle) in art.fans.iter().zip(&self.fan_angles) {
                let fan = size * scale;
                board
                    .child()
                    .place(
                        NodeTarget::Parent,
                        absolute::place()
                            .x(center.0 * scale - fan / 2.0)
                            .y(center.1 * scale - fan / 2.0)
                            .width(absolute::Sizing::fixed(fan))
                            .height(absolute::Sizing::fixed(fan)),
                    )
                    .insert(Fan { angle });
            }

            for (label, pos) in [("RESET", art.reset), ("BOOT", art.boot)] {
                let Some(pos) = pos else { continue };
                let d = BUTTON_SIZE * scale;
                let mut button = board.child().place(
                    NodeTarget::Parent,
                    absolute::place()
                        .x(pos.0 * scale - d / 2.0)
                        .y(pos.1 * scale - d / 2.0)
                        .width(absolute::Sizing::fixed(d))
                        .height(absolute::Sizing::fixed(d)),
                );
                let press = button.interact(interact::press());
                if press.hovered || press.active {
                    button.insert(
                        Rectangle::new()
                            .background(if press.active { PRESSED } else { HOVER })
                            .border(Border::solid(2.0, ACCENT))
                            .radius(BorderRadius::uniform(d / 2.0)),
                    );
                }
                clicks.push((label, press.clicked, press.active));
            }
        }

        // Menus, telemetry and logs.
        let mut side = root
            .child()
            .item(flex::item().grow())
            .layout(flex::column().gap(12.0));
        {
            let mut header = side.child().layout(flex::column().gap(6.0)).pad(14.0);
            header.insert(
                Rectangle::new()
                    .background(PANEL)
                    .radius(BorderRadius::uniform(8.0)),
            );
            if let Some(board) = header.child().build(|ui: Ui<'_>| self.board_menu(ui)) {
                new_choice = Some(Choice {
                    board,
                    ..self.choice.clone()
                });
            }
            let mut line = header
                .child()
                .layout(flex::row().gap(10.0).align(Align::Center));
            let firmware = snap.firmware.clone();
            if let Some(source) = line
                .child()
                .build(|ui: Ui<'_>| self.firmware_menu(ui, &firmware))
            {
                let board = match &source {
                    Source::File { flash, .. } => spec::from_file_name(flash).unwrap_or(spec),
                    _ => spec,
                };
                new_choice = Some(Choice {
                    board,
                    source,
                    ..self.choice.clone()
                });
            }
            setting = line.child().build(|ui: Ui<'_>| self.settings_menu(ui));
            let info = format!(
                "{:.1} s  ·  {:.2}× real time  ·  {:.0} MIPS",
                snap.seconds, snap.speed, snap.mips
            );
            line.child()
                .insert(Text::new(&info).style(TextStyle::new().size(14.0).color(MUTED)));
        }
        {
            let mut stats = side.child().layout(flex::column().gap(6.0)).pad(14.0);
            stats.insert(
                Rectangle::new()
                    .background(PANEL)
                    .radius(BorderRadius::uniform(8.0)),
            );
            let vcore = match snap.vcore {
                Some(v) if spec.domains > 1 => format!(
                    "{:.3} V ({} domains of {:.3} V)",
                    v,
                    spec.domains,
                    v / spec.domains as f32
                ),
                Some(v) => format!("{v:.3} V"),
                None => "off".into(),
            };
            let temps = snap
                .chip_temps
                .iter()
                .map(|t| format!("{t:.1}"))
                .collect::<Vec<_>>()
                .join(" / ");
            let fans = snap
                .fan_rpm
                .iter()
                .zip(&snap.fan_duty)
                .map(|(rpm, duty)| format!("{rpm:.0} RPM ({:.0} %)", duty * 100.0))
                .collect::<Vec<_>>()
                .join(", ");
            let chips = if spec.chips > 1 {
                format!("{} × {}", spec.chips, spec.asic.name())
            } else {
                spec.asic.name().to_string()
            };
            let mut rows = vec![
                ("Core voltage".to_string(), vcore),
                (
                    "Input".to_string(),
                    format!(
                        "{:.2} V, {:.2} A, {:.1} W",
                        snap.input_v,
                        snap.input_a,
                        snap.input_v * snap.input_a
                    ),
                ),
                (
                    "ASIC power".to_string(),
                    format!("{:.2} W", snap.asic_power),
                ),
                ("ASIC temperature".to_string(), format!("{temps} °C")),
            ];
            if let Some(t) = snap.vr_temp {
                rows.push(("Regulator temperature".to_string(), format!("{t:.1} °C")));
            }
            rows.push((
                if snap.fan_rpm.len() > 1 {
                    "Fans"
                } else {
                    "Fan"
                }
                .to_string(),
                fans,
            ));
            rows.push((
                chips,
                if snap.asic_running {
                    format!(
                        "{:.0} MHz, {} jobs, {} nonces",
                        snap.freq_mhz, snap.jobs, snap.nonces
                    )
                } else {
                    "in reset".into()
                },
            ));
            for (name, value) in &rows {
                let mut row = stats.child().layout(flex::row().gap(8.0));
                row.child()
                    .item(flex::item().fixed(190.0, 20.0))
                    .insert(Text::new(name).style(TextStyle::new().size(15.0).color(MUTED)));
                row.child().insert(
                    Text::new(value).style(TextStyle::new().size(15.0).weight(600).color(TEXT)),
                );
            }
            // Buttons the board lacks: the chip's EN and IO0 lines.
            let missing: Vec<&str> = [("RESET", art.reset), ("BOOT", art.boot)]
                .into_iter()
                .filter(|(_, pos)| pos.is_none())
                .map(|(label, _)| label)
                .collect();
            if !missing.is_empty() {
                let mut row = stats.child().layout(flex::row().gap(8.0));
                for label in missing {
                    let mut button = row
                        .child()
                        .item(flex::item().fixed(96.0, 28.0))
                        .layout(flex::column())
                        .pad(5.0);
                    let press = button.interact(interact::press());
                    button.insert(
                        Rectangle::new()
                            .background(if press.active {
                                PRESSED
                            } else if press.hovered {
                                HOVER
                            } else {
                                BACKGROUND
                            })
                            .border(Border::solid(1.5, ACCENT))
                            .radius(BorderRadius::uniform(6.0)),
                    );
                    button
                        .child()
                        .insert(Text::new(label).style(TextStyle::new().size(13.0).color(TEXT)));
                    clicks.push((label, press.clicked, press.active));
                }
            }
            stats.child().insert(
                Text::new("Click RESET to restart the chip. Hold BOOT to press the button (next screen, 2 s: access point).")
                    .style(TextStyle::new().size(12.0).color(MUTED)),
            );
        }
        {
            let mut logs = side
                .child()
                .item(flex::item().grow())
                .layout(flex::column().gap(2.0))
                .pad(12.0);
            logs.insert(
                Rectangle::new()
                    .background(Color::rgb(0x0c, 0x0d, 0x10))
                    .radius(BorderRadius::uniform(8.0)),
            );
            logs.child().insert(
                Text::new("Firmware logs").style(TextStyle::new().size(14.0).color(ACCENT)),
            );
            // Follow new lines while the view is at the end.
            let scroll = &mut self.logs_scroll;
            if snap.log_count != self.seen_logs
                && scroll.offset >= scroll.maximum_offset() - LOG_LINE / 2.0
            {
                scroll.scroll_to(f32::MAX, Animation::default(), AnimationOptions::new());
            }
            self.seen_logs = snap.log_count;
            logs.child().item(flex::item().grow()).build(
                UniformList::new(scroll, snap.logs.iter())
                    .item_extent(LOG_LINE)
                    .scroll_speed(2.0)
                    .scrollbar_thickness(6.0)
                    .minimum_thumb_extent(24.0)
                    .clip(BoundsClip)
                    .build_scrollbar(
                        |mut ui: Ui<'_>, line: &String| {
                            let color = if line.starts_with("ERROR") {
                                Color::rgb(0xff, 0x6b, 0x6b)
                            } else if line.starts_with("WARN") {
                                Color::rgb(0xff, 0xc1, 0x4d)
                            } else {
                                TEXT
                            };
                            let short: String = line.chars().take(400).collect();
                            ui.insert(
                                Text::new(&short)
                                    .style(TextStyle::new().font(MONO).size(12.0).color(color)),
                            );
                        },
                        |mut ui: Ui<'_>| {
                            ui.insert(
                                Rectangle::new()
                                    .background(Color::rgb(0x1a, 0x1c, 0x22))
                                    .radius(BorderRadius::uniform(3.0)),
                            );
                        },
                        |mut ui: Ui<'_>, active: bool| {
                            ui.insert(
                                Rectangle::new()
                                    .background(if active { ACCENT } else { MUTED })
                                    .radius(BorderRadius::uniform(3.0)),
                            );
                        },
                    ),
            );
        }
        for (label, clicked, active) in clicks {
            self.button(label, clicked, active);
        }
        if let Some(choice) = new_choice {
            self.switch(choice);
        }
        if let Some(setting) = setting {
            self.apply(setting);
        }
    }
}

/// Scale of a board picture of `width` × `height` pixels in the window.
fn board_scale(width: f32, height: f32) -> f32 {
    (BOARD_HEIGHT / height).min(BOARD_WIDTH / width).min(1.0)
}

/// Opens the window, runs the sessions next to it, and stops the last one
/// when the window closes.
pub fn run(launch: Launch) {
    let text = TextSystem::new(
        TextConfig {
            fonts: vec![
                FontStack {
                    id: FontId::default(),
                    families: vec![Family::embedded([FontData::Shared(FONT.to_vec().into())])],
                },
                FontStack {
                    id: MONO,
                    families: vec![Family::embedded([FontData::Shared(
                        FONT_MONO.to_vec().into(),
                    )])],
                },
            ],
            text_cache_capacity: 1024 * 1024,
            layout_cache_capacity: 2 * 1024 * 1024,
        },
        blit_text_parley::Backend::new(),
    )
    .expect("text system");
    let graphics = cpu::Backend::new(text, cpu::Config::default());

    // Room for the widest and the tallest board picture.
    let (mut width, mut height) = (0.0f32, 0.0f32);
    for b in spec::BOARDS {
        let (w, h) = png_size(b.art.png);
        let scale = board_scale(w as f32, h as f32);
        width = width.max(w as f32 * scale);
        height = height.max(h as f32 * scale);
    }
    *HANDOFF.lock().unwrap() = Some(launch);
    blit_desktop::run::<App>(Config {
        title: "Bitaxe emulator".into(),
        width: (width + SIDE_WIDTH + 3.0 * GAP) as u32,
        height: ((height + 2.0 * GAP) as u32).max(940),
        graphics,
    })
    .expect("window");
    if let Some(session) = SESSION.lock().unwrap().take() {
        session.stop();
    }
}
