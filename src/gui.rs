//! The window: the front of the board with its display, its RESET and BOOT
//! buttons and its fans, next to the board telemetry and the firmware logs
//! (BLIT).

use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use blit::{Atom, AtomCx, Constraints, LogicalRect, NodeTarget, Size};
use blit_desktop::{Application, Config, DesktopContext, EventLoopProxy, Scope, cpu};
use blit_gui::color::Color;
use blit_gui::image::{Image, ImageData, ImageFormat, ImageHandle, ImagePixels, ImageSampling};
use blit_gui::mesh::{Mesh, MeshVertex};
use blit_gui::rectangle::Rectangle;
use blit_gui::style::{Border, BorderRadius};
use blit_gui::text::system::{FontStack, TextConfig, TextSystem};
use blit_gui::text::{FontId, Text, TextStyle};
use blit_gui_widgets::interact;
use blit_gui_widgets::layout::{absolute, flex};
use blit_text::{Family, FontData};

use crate::board::spec::Spec;
use crate::emu::{Command, Emulator, Frame, Snapshot};

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

const BACKGROUND: Color = Color::rgb(0x12, 0x14, 0x18);
const PANEL: Color = Color::rgb(0x1c, 0x1f, 0x26);
const TEXT: Color = Color::rgb(0xe6, 0xe8, 0xee);
const MUTED: Color = Color::rgb(0x8c, 0x93, 0xa3);
const ACCENT: Color = Color::rgb(0xf2, 0xc8, 0x1e);

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

struct App {
    spec: &'static Spec,
    snapshot: Arc<Mutex<Snapshot>>,
    commands: Sender<Command>,
    board: ImageHandle,
    board_size: (f32, f32),
    /// Pixels of the window per pixel of the board picture.
    scale: f32,
    display: Option<(u64, ImageHandle)>,
    boot_pressed: bool,
    fan_angles: Vec<f32>,
    last_frame: Instant,
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

/// Shared state handed to [`App::new`] by [`run`].
type Handoff = (
    &'static Spec,
    Arc<Mutex<Snapshot>>,
    Sender<Command>,
    Sender<EventLoopProxy<Wake>>,
);
static HANDOFF: Mutex<Option<Handoff>> = Mutex::new(None);

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
}

impl Application for App {
    type Input = Wake;
    type Graphics = cpu::Backend;

    fn new(proxy: EventLoopProxy<Wake>, _: Scope<Self>, context: &mut Ctx) -> Self {
        let (spec, snapshot, commands, proxy_tx) =
            HANDOFF.lock().unwrap().take().expect("emulator handoff");
        let _ = proxy_tx.send(proxy);
        let (rgba, w, h) = decode_png(spec.art.png);
        let board = context
            .create_image(ImageData::new(
                ImagePixels::Owned(rgba.into_boxed_slice()),
                ImageFormat::Rgba8,
                w,
                h,
            ))
            .expect("board image");
        App {
            spec,
            snapshot,
            commands,
            board,
            board_size: (w as f32, h as f32),
            scale: board_scale(w as f32, h as f32),
            display: None,
            boot_pressed: false,
            fan_angles: vec![0.0; spec.art.fans.len()],
            last_frame: Instant::now(),
        }
    }

    fn input(&mut self, _: Wake) {}

    fn build(&mut self, ui: Ui<'_>) {
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
        let art = self.spec.art;
        let scale = self.scale;

        let mut root = ui.layout(flex::row().gap(GAP)).pad(GAP);
        root.insert(Rectangle::new().background(BACKGROUND));

        // The board.
        let (bw, bh) = (self.board_size.0 * scale, self.board_size.1 * scale);
        {
            let mut board = root.child().item(flex::item().fixed(bw, bh)).layout(());
            board.insert(Image::new(&self.board).sampling(ImageSampling::Bilinear));

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
                            .background(if press.active {
                                Color::rgba(0xf2, 0xc8, 0x1e, 0x60)
                            } else {
                                Color::rgba(0xff, 0xff, 0xff, 0x28)
                            })
                            .border(Border::solid(2.0, ACCENT))
                            .radius(BorderRadius::uniform(d / 2.0)),
                    );
                }
                self.button(label, press.clicked, press.active);
            }
        }

        // Telemetry and logs.
        let mut side = root
            .child()
            .item(flex::item().grow())
            .layout(flex::column().gap(12.0));
        {
            let mut header = side.child().layout(flex::column().gap(4.0)).pad(14.0);
            header.insert(
                Rectangle::new()
                    .background(PANEL)
                    .radius(BorderRadius::uniform(8.0)),
            );
            let title = self.spec.name();
            header
                .child()
                .insert(Text::new(&title).style(TextStyle::new().size(28.0).color(ACCENT)));
            let line = format!(
                "esp-miner-rs on the emulated ESP32-S3  ·  {:.1} s  ·  {:.2}× real time  ·  {:.0} MIPS",
                snap.seconds, snap.speed, snap.mips
            );
            header
                .child()
                .insert(Text::new(&line).style(TextStyle::new().size(14.0).color(MUTED)));
        }
        {
            let mut stats = side.child().layout(flex::column().gap(6.0)).pad(14.0);
            stats.insert(
                Rectangle::new()
                    .background(PANEL)
                    .radius(BorderRadius::uniform(8.0)),
            );
            let spec = self.spec;
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
                                Color::rgba(0xf2, 0xc8, 0x1e, 0x60)
                            } else if press.hovered {
                                Color::rgba(0xff, 0xff, 0xff, 0x28)
                            } else {
                                BACKGROUND
                            })
                            .border(Border::solid(1.5, ACCENT))
                            .radius(BorderRadius::uniform(6.0)),
                    );
                    button
                        .child()
                        .insert(Text::new(label).style(TextStyle::new().size(13.0).color(TEXT)));
                    self.button(label, press.clicked, press.active);
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
            let visible = 34;
            let skip = snap.logs.len().saturating_sub(visible);
            for line in snap.logs.iter().skip(skip) {
                let short: String = line.chars().take(110).collect();
                let color = if line.starts_with("ERROR") {
                    Color::rgb(0xff, 0x6b, 0x6b)
                } else if line.starts_with("WARN") {
                    Color::rgb(0xff, 0xc1, 0x4d)
                } else {
                    TEXT
                };
                logs.child().insert(
                    Text::new(&short).style(TextStyle::new().font(MONO).size(12.0).color(color)),
                );
            }
        }
    }
}

/// Scale of a board picture of `width` × `height` pixels in the window.
fn board_scale(width: f32, height: f32) -> f32 {
    (BOARD_HEIGHT / height).min(BOARD_WIDTH / width).min(1.0)
}

/// Opens the window and runs the emulator next to it until the window closes.
pub fn run(mut emulator: Emulator) {
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

    let (commands_tx, commands_rx) = channel();
    let (proxy_tx, proxy_rx) = channel::<EventLoopProxy<Wake>>();
    let spec = emulator.board.spec;
    *HANDOFF.lock().unwrap() = Some((spec, emulator.snapshot(), commands_tx.clone(), proxy_tx));
    let (_, w, h) = decode_png(spec.art.png);
    let scale = board_scale(w as f32, h as f32);
    let width = (w as f32 * scale + SIDE_WIDTH + 3.0 * GAP) as u32;
    let height = ((h as f32 * scale + 2.0 * GAP) as u32).max(940);

    let worker = std::thread::spawn(move || {
        let Ok(proxy) = proxy_rx.recv() else { return };
        let mut last = Instant::now();
        emulator.run_realtime(commands_rx, || {
            // About 60 frames per second.
            if last.elapsed().as_millis() >= 16 {
                last = Instant::now();
                let _ = proxy.send_event(Wake);
            }
        });
        emulator.board.save_flash();
    });

    blit_desktop::run::<App>(Config {
        title: format!("{} emulator", spec.name()),
        width,
        height,
        graphics,
    })
    .expect("window");
    let _ = commands_tx.send(Command::Quit);
    let _ = worker.join();
}
