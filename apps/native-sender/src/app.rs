//! The window people open. No flags: a name, a room code, then voice and the
//! native picture. The command-line tools stay available when arguments are set.

use crate::audio::{decode_packet, decoder as opus_decoder, AudioPlayer};
use crate::config::{AppConfig, CaptureBackend, EncoderBackend, TargetPlatform};
use crate::direct::{self, TOPIC};
use crate::encoder::ScreenEncoder;
use crate::h264::{unix_us_now, MAX_FRAME_AGE};
use crate::picture::{accept_picture, listen, FrameReader, PictureSender};
use anyhow::{anyhow, Context, Result};
use eframe::egui;
use livekit::options::{
    DegradationPreference, TrackPublishOptions, VideoCodec, VideoEncoderBackend, VideoEncoding,
};
use livekit::prelude::*;
use livekit::webrtc::video_frame::{EncodedFrameType, EncodedVideoCodec, EncodedVideoFrame};
use livekit::webrtc::video_source::{native::NativeVideoSource, RtcVideoSource, VideoResolution};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const WORKER: &str = "https://dcmax-token.soniavyukt.workers.dev";

struct Frame {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

/// The newest picture only. A queue of older frames is what made the share
/// play several seconds behind live video.
struct Stage {
    latest: Mutex<Option<Frame>>,
    ended: AtomicBool,
}

enum Cmd {
    Join(CallSettings),
    Share(PictureSettings),
    StopShare,
    Mute(bool),
    Leave,
}

struct CallSettings {
    name: String,
    room: String,
    invite: Option<String>,
}

struct PictureSettings {
    width: u32,
    height: u32,
    fps: u32,
    bitrate_kbps: u32,
}

enum Note {
    Status(String),
    Joined,
    Left,
    Sharing(bool),
}

struct Joined {
    room: Room,
    events: tokio::sync::mpsc::UnboundedReceiver<RoomEvent>,
    mic: LocalAudioTrack,
    _audio: PlatformAudio,
    fan: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    host: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>>,
    stop_share: Option<tokio::sync::watch::Sender<bool>>,
    share: Option<tokio::task::JoinHandle<()>>,
    playback_stop: Arc<AtomicBool>,
}

pub fn run() -> Result<()> {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (note_tx, note_rx) = std::sync::mpsc::channel();
    let stage = Arc::new(Stage {
        latest: Mutex::new(None),
        ended: AtomicBool::new(false),
    });
    let stage_thread = Arc::clone(&stage);
    std::thread::Builder::new()
        .name("mycord".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(err) => {
                    let _ = note_tx.send(Note::Status(format!("Could not start the call runtime: {err}")));
                    return;
                }
            };
            runtime.block_on(session(cmd_rx, note_tx, stage_thread));
        })
        .context("failed to start the call thread")?;

    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([440.0, 660.0])
            .with_min_inner_size([420.0, 620.0])
            .with_decorations(false)
            .with_title("MyCord"),
        ..Default::default()
    };
    eframe::run_native(
        "MyCord",
        native,
        Box::new(move |cc| Ok(Box::new(MyCord::new(cc, cmd_tx, note_rx, stage)))),
    )
    .map_err(|err| anyhow!("{err}"))
}

struct MyCord {
    cmd: tokio::sync::mpsc::UnboundedSender<Cmd>,
    notes: std::sync::mpsc::Receiver<Note>,
    stage: Arc<Stage>,
    name: String,
    room: String,
    invite: String,
    resolution: usize,
    fps: u32,
    bitrate_kbps: u32,
    joined: bool,
    joining: bool,
    leaving: bool,
    sharing: bool,
    muted: bool,
    fullscreen: bool,
    dark: bool,
    window_for_call: bool,
    status: String,
    picture: Option<egui::TextureHandle>,
    shown: Instant,
    pointer_at: Option<egui::Pos2>,
    controls_until: Instant,
}

const RESOLUTIONS: &[(u32, u32, &str)] = &[
    (1280, 720, "1280 × 720"),
    (1920, 1080, "1920 × 1080"),
    (2560, 1440, "2560 × 1440"),
];
const RATES: &[u32] = &[30, 60];
const BITRATES: &[(u32, &str)] = &[
    (8_000, "8 Mbps"),
    (12_000, "12 Mbps"),
    (20_000, "20 Mbps"),
    (40_000, "40 Mbps"),
    (80_000, "80 Mbps"),
    (100_000, "100 Mbps"),
];

impl MyCord {
    fn new(
        cc: &eframe::CreationContext<'_>,
        cmd: tokio::sync::mpsc::UnboundedSender<Cmd>,
        notes: std::sync::mpsc::Receiver<Note>,
        stage: Arc<Stage>,
    ) -> Self {
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = egui::Color32::from_rgb(20, 18, 14);
        visuals.window_fill = egui::Color32::from_rgb(20, 18, 14);
        visuals.extreme_bg_color = egui::Color32::from_rgb(12, 11, 9);
        visuals.override_text_color = Some(egui::Color32::from_rgb(243, 239, 230));
        cc.egui_ctx.set_visuals(visuals);
        let saved = remember();
        let app = Self {
            cmd,
            notes,
            stage,
            name: saved.name,
            room: saved.room,
            invite: saved.invite,
            resolution: saved.resolution,
            fps: saved.fps,
            bitrate_kbps: saved.bitrate,
            joined: false,
            joining: false,
            leaving: false,
            sharing: false,
            muted: false,
            fullscreen: false,
            dark: saved.dark,
            window_for_call: false,
            status: String::new(),
            picture: None,
            shown: Instant::now(),
            pointer_at: None,
            controls_until: Instant::now(),
        };
        app
    }

    fn picture_settings(&self) -> PictureSettings {
        let (width, height, _) = RESOLUTIONS[self.resolution.min(RESOLUTIONS.len() - 1)];
        PictureSettings {
            width,
            height,
            fps: self.fps,
            bitrate_kbps: self.bitrate_kbps,
        }
    }

    fn send(&mut self, cmd: Cmd) {
        if self.cmd.send(cmd).is_err() {
            self.status = "The call thread stopped.".to_string();
        }
    }
}

impl eframe::App for MyCord {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump_with(ctx);
        self.keys(ctx);
        self.fit_window(ctx);
        let theme = self.theme(ctx);
        if !self.fullscreen {
            resize_borders(ctx);
        }
        if self.fullscreen {
            self.fullscreen_stage(ctx, &theme);
        } else if self.joined {
            self.call_screen(ctx, &theme);
        } else {
            self.lobby(ctx, &theme);
        }
    }
}

impl MyCord {
    fn keys(&mut self, ctx: &egui::Context) {
        if let Some(on) = ctx.input(|input| input.viewport().fullscreen) {
            self.fullscreen = on;
        }
        let escape = ctx.input(|input| input.key_pressed(egui::Key::Escape));
        let toggle = ctx.input(|input| input.key_pressed(egui::Key::F));
        if self.fullscreen && escape {
            self.set_fullscreen(ctx, false);
        } else if self.joined && toggle {
            self.set_fullscreen(ctx, !self.fullscreen);
        }
    }

    fn set_fullscreen(&mut self, ctx: &egui::Context, on: bool) {
        self.fullscreen = on;
        if on {
            self.controls_until = Instant::now() + Duration::from_millis(2800);
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
    }

    fn pump_with(&mut self, ctx: &egui::Context) {
        while let Ok(note) = self.notes.try_recv() {
            match note {
                Note::Status(text) => {
                    self.joining = false;
                    self.status = text;
                }
                Note::Joined => {
                    self.joining = false;
                    self.leaving = false;
                    self.joined = true;
                    self.status = format!("In {}", self.room);
                }
                Note::Left => {
                    self.joined = false;
                    self.joining = false;
                    self.leaving = false;
                    self.sharing = false;
                    self.muted = false;
                    self.picture = None;
                    if self.fullscreen {
                        self.set_fullscreen(ctx, false);
                    }
                    self.status = "You left the room.".to_string();
                }
                Note::Sharing(on) => {
                    self.sharing = on;
                    if on {
                        self.status = "Sharing your screen.".to_string();
                    } else if self.joined {
                        self.status = format!("In {}", self.room);
                    }
                }
            }
        }
        if self.stage.ended.swap(false, Ordering::AcqRel) {
            self.picture = None;
            if let Ok(mut latest) = self.stage.latest.lock() {
                *latest = None;
            }
        }
        // One upload per refresh. Anything newer that arrived in between
        // replaces this frame, so a burst cannot pile up into a delay.
        let ready = self.picture.is_none() || self.shown.elapsed() >= Duration::from_millis(12);
        if ready {
            if let Some(frame) = take_frame(&self.stage) {
                let pixels = frame.width.saturating_mul(frame.height).saturating_mul(4);
                if frame.width == 0 || frame.height == 0 || frame.rgba.len() != pixels {
                    note_log(&format!(
                        "rejected a picture frame {}x{} ({} bytes)",
                        frame.width,
                        frame.height,
                        frame.rgba.len()
                    ));
                } else {
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [frame.width, frame.height],
                        &frame.rgba,
                    );
                    if let Some(texture) = &mut self.picture {
                        texture.set(image, egui::TextureOptions::LINEAR);
                    } else {
                        self.picture =
                            Some(ctx.load_texture("picture", image, egui::TextureOptions::LINEAR));
                    }
                    self.shown = Instant::now();
                }
            }
        }
        if self.joined {
            ctx.request_repaint_after(Duration::from_millis(8));
        }
    }

    fn fit_window(&mut self, ctx: &egui::Context) {
        if self.fullscreen || self.joined == self.window_for_call {
            return;
        }
        self.window_for_call = self.joined;
        let (size, min) = if self.joined {
            (egui::vec2(1080.0, 720.0), egui::vec2(860.0, 560.0))
        } else {
            (egui::vec2(440.0, 660.0), egui::vec2(420.0, 620.0))
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(min));
        if let Some(monitor) = ctx.input(|input| input.viewport().monitor_size) {
            if monitor.x > size.x && monitor.y > size.y {
                let origin = egui::pos2(
                    ((monitor.x - size.x) / 2.0).max(0.0),
                    ((monitor.y - size.y) / 2.0).max(24.0),
                );
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(origin));
            }
        }
    }

    fn theme(&self, ctx: &egui::Context) -> Theme {
        let blend = ctx.animate_bool_with_time(egui::Id::new("mycord-theme"), self.dark, 0.32);
        let theme = Theme::blend(blend);
        let mut visuals = if blend < 0.5 {
            egui::Visuals::light()
        } else {
            egui::Visuals::dark()
        };
        visuals.panel_fill = theme.bg;
        visuals.window_fill = theme.raised;
        visuals.extreme_bg_color = theme.field;
        visuals.override_text_color = Some(theme.text);
        visuals.widgets.noninteractive.fg_stroke.color = theme.text;
        visuals.widgets.inactive.bg_fill = theme.field;
        visuals.widgets.inactive.weak_bg_fill = theme.field;
        visuals.widgets.inactive.fg_stroke.color = theme.text;
        visuals.widgets.hovered.bg_fill = theme.pill_hover;
        visuals.widgets.hovered.weak_bg_fill = theme.pill_hover;
        visuals.widgets.hovered.fg_stroke.color = theme.text;
        visuals.widgets.active.bg_fill = theme.pill;
        visuals.widgets.active.fg_stroke.color = theme.text;
        visuals.widgets.open.bg_fill = theme.raised;
        visuals.widgets.open.fg_stroke.color = theme.text;
        visuals.selection.bg_fill = theme.accent;
        visuals.selection.stroke.color = theme.accent_ink;
        visuals.window_corner_radius = egui::CornerRadius::same(12);
        visuals.menu_corner_radius = egui::CornerRadius::same(10);
        ctx.set_visuals(visuals);
        theme
    }

    fn toggle_theme(&mut self) {
        self.dark = !self.dark;
        self.persist();
    }

    fn persist(&self) {
        let trimmed = self.invite.trim();
        let invite = if trimmed.is_empty() { None } else { Some(trimmed) };
        store(
            &self.name,
            &self.room,
            invite,
            self.resolution,
            self.fps,
            self.bitrate_kbps,
            self.dark,
        );
    }

    fn lobby(&mut self, ctx: &egui::Context, theme: &Theme) {
        let frame = egui::Frame::new().fill(theme.bg);
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            window_edge(ui, theme);
            let action = title_bar(ui, theme, self.dark, |ui| {
                let center = egui::pos2(ui.ctx().screen_rect().center().x, ui.max_rect().center().y);
                ui.painter().text(
                    center,
                    egui::Align2::CENTER_CENTER,
                    "MyCord",
                    egui::FontId::proportional(13.0),
                    theme.muted,
                );
            });
            self.apply_bar(ctx, action);
            ui.add_space(18.0);
            ui.vertical_centered(|ui| {
                ui.set_width(360.0);
                brand_row(ui, theme);
                ui.add_space(18.0);
                ui.label(egui::RichText::new("Open a room").size(34.0).strong().color(theme.text));
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("The other person opens MyCord, enters this room code, and joins. Voice and screen sharing stay right here.")
                        .size(13.0)
                        .color(theme.muted),
                );
                ui.add_space(18.0);
                field(ui, theme, "Your name", &mut self.name, "Ava");
                ui.add_space(12.0);
                field(ui, theme, "Room code", &mut self.room, "mycord-room");
                ui.add_space(12.0);
                field(ui, theme, "Invite, if this room uses one", &mut self.invite, "Leave blank if you have none");
                ui.add_space(16.0);
                self.picture_pickers(ui, theme);
                ui.add_space(18.0);
                let label = if self.joining { "Joining…" } else { "Join room" };
                if !self.joining && lime_button(ui, theme, label, true).clicked() {
                    self.join();
                }
                ui.add_space(12.0);
                if self.status.is_empty() {
                    status_line(ui, theme, "Ready to join. No account needed.", false);
                } else {
                    let joining = self.status.starts_with("Joining");
                    status_line(ui, theme, &self.status, !joining);
                }
            });
        });
    }

    fn call_screen(&mut self, ctx: &egui::Context, theme: &Theme) {
        let frame = egui::Frame::new().fill(theme.bg);
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            window_edge(ui, theme);
            let room = self.room.clone();
            let status = if self.status.is_empty() {
                format!("In {}", self.room)
            } else {
                self.status.clone()
            };
            let action = title_bar(ui, theme, self.dark, |ui| {
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add_space(6.0);
                    brand_mark(ui, theme, 13.0);
                    ui.add_space(10.0);
                    ui.label(egui::RichText::new("|").color(theme.line));
                    ui.add_space(10.0);
                    ui.label(egui::RichText::new(room).size(14.0).color(theme.text));
                    ui.add_space(12.0);
                    dot(ui, theme.dot, 4.0);
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(status).size(13.0).color(theme.muted));
                });
            });
            self.apply_bar(ctx, action);
            let bottom_pad = 22.0;
            let dock_height = 150.0;
            let stage = egui::Rect::from_min_size(
                egui::pos2(ui.max_rect().left() + 16.0, ui.cursor().top() + 8.0),
                egui::vec2(
                    ui.available_width() - 32.0,
                    (ui.available_height() - dock_height - bottom_pad).max(120.0),
                ),
            );
            self.stage_view(ui, theme, stage, 16);
            ui.add_space(stage.height() + 16.0);
            self.dock(ui, ctx, theme);
            ui.add_space(bottom_pad);
        });
    }

    fn fullscreen_stage(&mut self, ctx: &egui::Context, theme: &Theme) {
        let frame = egui::Frame::new().fill(theme.stage);
        let rect = egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            let rect = ui.max_rect();
            self.stage_view(ui, theme, rect, 0);
            rect
        });
        let rect = rect.inner;
        let bar = egui::Rect::from_min_max(
            egui::pos2(rect.left(), rect.bottom() - 84.0),
            rect.right_bottom(),
        );
        let fade = self.chrome_fade(ctx, bar);
        if fade <= 0.04 {
            return;
        }
        let hint = egui::Rect::from_center_size(
            egui::pos2(rect.center().x, rect.top() + 36.0),
            egui::vec2(250.0, 36.0),
        );
        let mut painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("fs-hint-layer")));
        painter.set_opacity(fade);
        painter.rect_filled(hint, 18.0, mix(theme.raised, egui::Color32::BLACK, 0.25));
        painter.text(
            hint.center(),
            egui::Align2::CENTER_CENTER,
            "Press  Esc  to exit full screen",
            egui::FontId::proportional(13.0),
            theme.text,
        );
        egui::Area::new(egui::Id::new("fs-dock"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(rect.left(), rect.bottom() - 68.0))
            .show(ctx, |ui| {
                ui.set_width(rect.width());
                ui.set_opacity(fade);
                self.pills(ui, ctx, theme);
            });
    }

    fn chrome_fade(&mut self, ctx: &egui::Context, bar: egui::Rect) -> f32 {
        let pointer = ctx.input(|input| input.pointer.hover_pos());
        let moved = match (pointer, self.pointer_at) {
            (Some(now), Some(previous)) => now.distance(previous) > 2.0,
            (Some(_), None) => true,
            _ => false,
        };
        if let Some(pos) = pointer {
            self.pointer_at = Some(pos);
        }
        let over_bar = pointer.is_some_and(|pos| bar.contains(pos));
        if moved || over_bar {
            self.controls_until = Instant::now() + Duration::from_millis(2800);
        }
        let show = Instant::now() < self.controls_until;
        ctx.animate_bool_with_time(egui::Id::new("fs-chrome"), show, 0.24)
    }

    fn stage_view(&mut self, ui: &mut egui::Ui, theme: &Theme, rect: egui::Rect, radius: u8) {
        ui.painter().rect_filled(rect, radius, theme.stage);
        if let Some(texture) = &self.picture {
            let inset = if radius == 0 { 0.0 } else { 8.0 };
            let available = rect.shrink(inset);
            let sized = texture.size_vec2();
            let scale = (available.width() / sized.x).min(available.height() / sized.y);
            let size = sized * scale;
            let image_rect = egui::Rect::from_center_size(available.center(), size);
            ui.put(
                image_rect,
                egui::Image::new((texture.id(), size)).corner_radius(egui::CornerRadius::same(radius.saturating_sub(2))),
            );
        } else {
            let icon = egui::Rect::from_center_size(rect.center() - egui::vec2(0.0, 42.0), egui::vec2(42.0, 34.0));
            let icon_color = if theme.text.r() > 180 { theme.accent } else { theme.text };
            glyph(ui.painter(), icon, Glyph::Monitor, icon_color);
            ui.painter().text(
                rect.center() + egui::vec2(0.0, 8.0),
                egui::Align2::CENTER_CENTER,
                "Waiting for a screen",
                egui::FontId::proportional(22.0),
                theme.text,
            );
            ui.painter().text(
                rect.center() + egui::vec2(0.0, 36.0),
                egui::Align2::CENTER_CENTER,
                "When someone presses Share screen,",
                egui::FontId::proportional(13.0),
                theme.muted,
            );
            ui.painter().text(
                rect.center() + egui::vec2(0.0, 54.0),
                egui::Align2::CENTER_CENTER,
                "it shows here.",
                egui::FontId::proportional(13.0),
                theme.muted,
            );
        }
    }

    fn dock(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, theme: &Theme) {
        self.pills(ui, ctx, theme);
        ui.add_space(16.0);
        self.picture_pickers(ui, theme);
    }

    fn pills(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, theme: &Theme) {
        let mute = if self.muted { "Unmute" } else { "Mute" };
        let share = if self.sharing { "Stop sharing" } else { "Share screen" };
        let full = if self.fullscreen { "Exit" } else { "Full screen" };
        let leave = if self.leaving { "Leaving…" } else { "Leave" };
        let gap = 10.0;
        let widths = [
            pill_width(ui, mute),
            pill_width(ui, share),
            pill_width(ui, full),
            pill_width(ui, leave),
        ];
        let total = widths.iter().sum::<f32>() + gap * 3.0;
        ui.horizontal(|ui| {
            ui.add_space(((ui.available_width() - total) / 2.0).max(0.0));
            if pill(ui, theme, "mute", mute, Glyph::Mic, Pill::Plain, 0.0).clicked() {
                self.muted = !self.muted;
                self.send(Cmd::Mute(self.muted));
            }
            ui.add_space(gap);
            let live = ctx.animate_bool_with_time(egui::Id::new("share-live"), self.sharing, 0.2);
            if pill(ui, theme, "share", share, Glyph::Share, Pill::Live, live).clicked() {
                if self.sharing {
                    self.sharing = false;
                    self.send(Cmd::StopShare);
                } else {
                    self.send(Cmd::Share(self.picture_settings()));
                }
            }
            ui.add_space(gap);
            if pill(ui, theme, "full", full, Glyph::Expand, Pill::Plain, 0.0).clicked() {
                self.set_fullscreen(ctx, !self.fullscreen);
            }
            ui.add_space(gap);
            if pill(ui, theme, "leave", leave, Glyph::Leave, Pill::Leave, 1.0).clicked() && !self.leaving {
                self.leaving = true;
                self.status = "Leaving the room…".to_string();
                self.send(Cmd::Leave);
            }
        });
    }

    fn apply_bar(&mut self, ctx: &egui::Context, action: BarClick) {
        match action {
            BarClick::None => {}
            BarClick::Close => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            BarClick::Minimize => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
            BarClick::Zoom => {
                if self.joined {
                    self.set_fullscreen(ctx, !self.fullscreen);
                }
            }
            BarClick::Theme => self.toggle_theme(),
        }
    }

    fn picture_pickers(&mut self, ui: &mut egui::Ui, theme: &Theme) {
        let before = (self.resolution, self.fps, self.bitrate_kbps);
        let gap = 12.0;
        let full = ui.available_width();
        let group = if full < 520.0 { full } else { 468.0 };
        let column = ((group - gap * 2.0) / 3.0).max(108.0);
        let total = column * 3.0 + gap * 2.0;
        ui.horizontal(|ui| {
            ui.add_space(((ui.available_width() - total) / 2.0).max(0.0));
            let mut resolution = self.resolution;
            choice(ui, theme, "resolution", "Resolution", RESOLUTIONS[resolution].2, column, |ui| {
                for (index, (_, _, label)) in RESOLUTIONS.iter().enumerate() {
                    ui.selectable_value(&mut resolution, index, *label);
                }
            });
            self.resolution = resolution;
            ui.add_space(gap);
            let mut fps = self.fps;
            choice(ui, theme, "refresh", "Refresh", &format!("{fps} Hz"), column, |ui| {
                for rate in RATES {
                    ui.selectable_value(&mut fps, *rate, format!("{rate} Hz"));
                }
            });
            self.fps = fps;
            ui.add_space(gap);
            let mut bitrate = self.bitrate_kbps;
            let bitrate_label = BITRATES
                .iter()
                .find(|(kbps, _)| *kbps == bitrate)
                .map(|(_, label)| *label)
                .unwrap_or("Custom");
            choice(ui, theme, "bitrate", "Bitrate", bitrate_label, column, |ui| {
                for (kbps, label) in BITRATES {
                    ui.selectable_value(&mut bitrate, *kbps, *label);
                }
            });
            self.bitrate_kbps = bitrate;
        });
        if before != (self.resolution, self.fps, self.bitrate_kbps) {
            self.persist();
        }
    }

    fn join(&mut self) {
        let name = self.name.trim().to_string();
        let room = self.room.trim().to_string();
        if name.len() < 2 {
            self.status = "Enter your name.".to_string();
            return;
        }
        if room.len() < 2 {
            self.status = "Enter a room code.".to_string();
            return;
        }
        let invite = {
            let trimmed = self.invite.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        };
        store(
            &name,
            &room,
            invite.as_deref(),
            self.resolution,
            self.fps,
            self.bitrate_kbps,
            self.dark,
        );
        self.joining = true;
        self.status = "Joining…".to_string();
        self.send(Cmd::Join(CallSettings { name, room, invite }));
    }
}

struct Theme {
    bg: egui::Color32,
    raised: egui::Color32,
    text: egui::Color32,
    muted: egui::Color32,
    faint: egui::Color32,
    line: egui::Color32,
    field: egui::Color32,
    pill: egui::Color32,
    pill_hover: egui::Color32,
    accent: egui::Color32,
    accent_hover: egui::Color32,
    accent_ink: egui::Color32,
    leave_bg: egui::Color32,
    leave_fg: egui::Color32,
    stage: egui::Color32,
    dot: egui::Color32,
}

impl Theme {
    fn blend(t: f32) -> Self {
        const LIGHT: Theme = Theme {
            bg: egui::Color32::from_rgb(255, 255, 255),
            raised: egui::Color32::from_rgb(255, 255, 255),
            text: egui::Color32::from_rgb(22, 22, 22),
            muted: egui::Color32::from_rgb(110, 110, 110),
            faint: egui::Color32::from_rgb(168, 168, 168),
            line: egui::Color32::from_rgb(230, 230, 230),
            field: egui::Color32::from_rgb(255, 255, 255),
            pill: egui::Color32::from_rgb(244, 244, 244),
            pill_hover: egui::Color32::from_rgb(232, 232, 232),
            accent: egui::Color32::from_rgb(198, 255, 46),
            accent_hover: egui::Color32::from_rgb(214, 255, 92),
            accent_ink: egui::Color32::from_rgb(22, 22, 22),
            leave_bg: egui::Color32::from_rgb(255, 232, 229),
            leave_fg: egui::Color32::from_rgb(214, 84, 72),
            stage: egui::Color32::from_rgb(243, 243, 243),
            dot: egui::Color32::from_rgb(61, 196, 86),
        };
        const DARK: Theme = Theme {
            bg: egui::Color32::from_rgb(22, 22, 22),
            raised: egui::Color32::from_rgb(28, 28, 28),
            text: egui::Color32::from_rgb(245, 245, 245),
            muted: egui::Color32::from_rgb(160, 160, 160),
            faint: egui::Color32::from_rgb(120, 120, 120),
            line: egui::Color32::from_rgb(48, 48, 48),
            field: egui::Color32::from_rgb(36, 36, 36),
            pill: egui::Color32::from_rgb(42, 42, 42),
            pill_hover: egui::Color32::from_rgb(58, 58, 58),
            accent: egui::Color32::from_rgb(198, 255, 46),
            accent_hover: egui::Color32::from_rgb(214, 255, 92),
            accent_ink: egui::Color32::from_rgb(22, 22, 22),
            leave_bg: egui::Color32::from_rgb(92, 42, 38),
            leave_fg: egui::Color32::from_rgb(255, 214, 208),
            stage: egui::Color32::from_rgb(14, 14, 14),
            dot: egui::Color32::from_rgb(182, 255, 60),
        };
        Self {
            bg: mix(LIGHT.bg, DARK.bg, t),
            raised: mix(LIGHT.raised, DARK.raised, t),
            text: mix(LIGHT.text, DARK.text, t),
            muted: mix(LIGHT.muted, DARK.muted, t),
            faint: mix(LIGHT.faint, DARK.faint, t),
            line: mix(LIGHT.line, DARK.line, t),
            field: mix(LIGHT.field, DARK.field, t),
            pill: mix(LIGHT.pill, DARK.pill, t),
            pill_hover: mix(LIGHT.pill_hover, DARK.pill_hover, t),
            accent: mix(LIGHT.accent, DARK.accent, t),
            accent_hover: mix(LIGHT.accent_hover, DARK.accent_hover, t),
            accent_ink: mix(LIGHT.accent_ink, DARK.accent_ink, t),
            leave_bg: mix(LIGHT.leave_bg, DARK.leave_bg, t),
            leave_fg: mix(LIGHT.leave_fg, DARK.leave_fg, t),
            stage: mix(LIGHT.stage, DARK.stage, t),
            dot: mix(LIGHT.dot, DARK.dot, t),
        }
    }
}

fn mix(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |from: u8, to: u8| (f32::from(from) + (f32::from(to) - f32::from(from)) * t).round() as u8;
    egui::Color32::from_rgb(ch(a.r(), b.r()), ch(a.g(), b.g()), ch(a.b(), b.b()))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BarClick {
    None,
    Close,
    Minimize,
    Zoom,
    Theme,
}

#[derive(Clone, Copy)]
enum Pill {
    Plain,
    Live,
    Leave,
}

#[derive(Clone, Copy)]
enum Glyph {
    Mic,
    Share,
    Expand,
    Leave,
    Monitor,
    Sun,
}

fn title_bar(ui: &mut egui::Ui, theme: &Theme, dark: bool, body: impl FnOnce(&mut egui::Ui)) -> BarClick {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 46.0), egui::Sense::hover());
    let mut action = BarClick::None;
    let y = rect.center().y;
    let mut x = rect.left() + 18.0;
    let lights = [
        (0, egui::Color32::from_rgb(255, 95, 87), BarClick::Close),
        (1, egui::Color32::from_rgb(254, 188, 46), BarClick::Minimize),
        (2, egui::Color32::from_rgb(40, 200, 64), BarClick::Zoom),
    ];
    for (index, color, click) in lights {
        let spot = egui::Rect::from_center_size(egui::pos2(x, y), egui::vec2(16.0, 16.0));
        if traffic_light(ui, spot, index, color) {
            action = click;
        }
        x += 20.0;
    }
    let theme_rect = egui::Rect::from_center_size(egui::pos2(rect.right() - 24.0, y), egui::vec2(28.0, 28.0));
    if theme_button(ui, theme_rect, theme, dark) {
        action = BarClick::Theme;
    }
    let content = egui::Rect::from_min_max(
        egui::pos2(x + 8.0, rect.top()),
        egui::pos2(theme_rect.left() - 8.0, rect.bottom()),
    );
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(content), body);
    let drag = ui.interact(content, egui::Id::new("title-drag"), egui::Sense::drag());
    if drag.drag_started() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    action
}

fn traffic_light(ui: &mut egui::Ui, rect: egui::Rect, index: u8, color: egui::Color32) -> bool {
    let id = egui::Id::new(("traffic", index));
    let response = ui.interact(rect, id, egui::Sense::click());
    let hover = ui.ctx().animate_bool_with_time(id, response.hovered(), 0.14);
    let radius = 6.0 + hover * 1.4;
    ui.painter().circle_filled(rect.center(), radius, color);
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.clicked()
}

fn theme_button(ui: &mut egui::Ui, rect: egui::Rect, theme: &Theme, dark: bool) -> bool {
    let id = egui::Id::new("theme-toggle");
    let response = ui.interact(rect, id, egui::Sense::click());
    let hover = ui.ctx().animate_bool_with_time(id, response.hovered(), 0.16);
    let fill = mix(theme.pill, theme.pill_hover, hover);
    ui.painter().circle_filled(rect.center(), 13.0 + hover, fill);
    let icon = rect.shrink(7.0);
    if dark {
        glyph(ui.painter(), icon, Glyph::Sun, theme.text);
    } else {
        let center = icon.center();
        ui.painter().circle_filled(center, 5.4, theme.text);
        ui.painter().circle_filled(center + egui::vec2(2.8, -1.4), 4.6, fill);
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.clicked()
}

fn window_edge(ui: &mut egui::Ui, theme: &Theme) {
    let rect = ui.max_rect().shrink(0.5);
    ui.painter().rect_stroke(
        rect,
        0,
        egui::Stroke::new(1.0_f32, theme.line),
        egui::StrokeKind::Outside,
    );
}

fn resize_borders(ctx: &egui::Context) {
    let rect = ctx.screen_rect();
    let edges = [
        (egui::Rect::from_min_max(rect.left_top(), egui::pos2(rect.left() + 5.0, rect.bottom())), egui::ResizeDirection::West, egui::CursorIcon::ResizeHorizontal),
        (egui::Rect::from_min_max(egui::pos2(rect.right() - 5.0, rect.top()), rect.right_bottom()), egui::ResizeDirection::East, egui::CursorIcon::ResizeHorizontal),
        (egui::Rect::from_min_max(rect.left_top(), egui::pos2(rect.right(), rect.top() + 5.0)), egui::ResizeDirection::North, egui::CursorIcon::ResizeVertical),
        (egui::Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - 5.0), rect.right_bottom()), egui::ResizeDirection::South, egui::CursorIcon::ResizeVertical),
    ];
    egui::Area::new(egui::Id::new("window-edges"))
        .order(egui::Order::Foreground)
        .fixed_pos(egui::Pos2::ZERO)
        .interactable(true)
        .show(ctx, |ui| {
            for (index, (edge, direction, cursor)) in edges.into_iter().enumerate() {
                let response = ui.interact(edge, egui::Id::new(("edge", index)), egui::Sense::drag());
                if response.hovered() {
                    ui.ctx().set_cursor_icon(cursor);
                }
                if response.drag_started() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::BeginResize(direction));
                }
            }
        });
}

fn brand_row(ui: &mut egui::Ui, theme: &Theme) {
    ui.horizontal(|ui| {
        let width = ui.available_width();
        ui.add_space((width - 92.0).max(0.0) / 2.0);
        brand_mark(ui, theme, 16.0);
    });
}

fn brand_mark(ui: &mut egui::Ui, theme: &Theme, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(18.0, size + 4.0), egui::Sense::hover());
    let painter = ui.painter();
    let color = theme.accent;
    for shift in [-4.5, 3.5] {
        painter.line_segment(
            [
                egui::pos2(rect.center().x + shift - 2.2, rect.bottom() - 2.0),
                egui::pos2(rect.center().x + shift + 2.2, rect.top() + 2.0),
            ],
            egui::Stroke::new(3.2_f32, color),
        );
    }
    ui.add_space(6.0);
    ui.label(egui::RichText::new("MyCord").size(size).strong().color(theme.text));
}

fn field(ui: &mut egui::Ui, theme: &Theme, label: &str, text: &mut String, hint: &str) {
    ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
        ui.set_width(ui.available_width());
        ui.label(egui::RichText::new(label).size(12.0).color(theme.muted));
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(theme.field)
            .stroke(egui::Stroke::new(1.0_f32, theme.line))
            .corner_radius(10)
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::singleline(text)
                        .hint_text(egui::RichText::new(hint).color(theme.faint))
                        .frame(false)
                        .desired_width(f32::INFINITY)
                        .text_color(theme.text),
                );
            });
    });
}

fn choice(
    ui: &mut egui::Ui,
    theme: &Theme,
    id: &str,
    label: &str,
    selected: &str,
    width: f32,
    body: impl FnOnce(&mut egui::Ui),
) {
    ui.vertical(|ui| {
        ui.set_width(width);
        ui.label(egui::RichText::new(label).size(11.0).color(theme.muted));
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(theme.field)
            .stroke(egui::Stroke::new(1.0_f32, theme.line))
            .corner_radius(10)
            .inner_margin(egui::Margin::symmetric(8, 4))
            .show(ui, |ui| {
                egui::ComboBox::from_id_salt(id)
                    .selected_text(egui::RichText::new(selected).size(13.0).color(theme.text))
                    .width((width - 18.0).max(72.0))
                    .icon(|ui, rect, visuals, _, _| {
                        let center = rect.center();
                        let stroke = egui::Stroke::new(1.5_f32, visuals.fg_stroke.color);
                        ui.painter().line_segment(
                            [center + egui::vec2(-3.5, -1.0), center + egui::vec2(0.0, 2.2)],
                            stroke,
                        );
                        ui.painter().line_segment(
                            [center + egui::vec2(0.0, 2.2), center + egui::vec2(3.5, -1.0)],
                            stroke,
                        );
                    })
                    .show_ui(ui, body);
            });
    });
}

fn lime_button(ui: &mut egui::Ui, theme: &Theme, label: &str, enabled: bool) -> egui::Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 44.0), egui::Sense::click());
    let hover = ui.ctx().animate_bool_with_time(response.id, response.hovered() && enabled, 0.16);
    let fill = mix(theme.accent, theme.accent_hover, hover);
    let fill = if enabled { fill } else { mix(fill, theme.pill, 0.45) };
    let pressed = response.is_pointer_button_down_on() && enabled;
    let scale = if pressed { 0.985 } else { 1.0 };
    let painted = egui::Rect::from_center_size(rect.center(), rect.size() * scale);
    ui.painter().rect_filled(painted, 22.0, fill);
    let slide = hover * 4.0;
    let font = egui::FontId::proportional(15.0);
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font.clone(), theme.accent_ink);
    let gap = 10.0;
    let arrow = 12.0;
    let total = galley.rect.width() + gap + arrow;
    let left = painted.center().x - total / 2.0 + slide;
    let y = painted.center().y;
    ui.painter().text(
        egui::pos2(left + galley.rect.width() / 2.0, y),
        egui::Align2::CENTER_CENTER,
        label,
        font,
        theme.accent_ink,
    );
    let tail = egui::pos2(left + galley.rect.width() + gap, y);
    let tip = tail + egui::vec2(arrow, 0.0);
    let stroke = egui::Stroke::new(1.6_f32, theme.accent_ink);
    ui.painter().line_segment([tail, tip], stroke);
    ui.painter().line_segment([tip, tip + egui::vec2(-4.5, -3.5)], stroke);
    ui.painter().line_segment([tip, tip + egui::vec2(-4.5, 3.5)], stroke);
    if response.hovered() && enabled {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if enabled { response } else { response.on_hover_text("") }
}

fn pill(
    ui: &mut egui::Ui,
    theme: &Theme,
    salt: &str,
    label: &str,
    icon: Glyph,
    kind: Pill,
    live: f32,
) -> egui::Response {
    let font = egui::FontId::proportional(13.5);
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font.clone(), theme.text);
    let width = 16.0 + 16.0 + 8.0 + galley.rect.width() + 16.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 36.0), egui::Sense::click());
    let id = response.id.with(salt);
    let hover = ui.ctx().animate_bool_with_time(id, response.hovered(), 0.16);
    let (idle, hot, fg) = match kind {
        Pill::Plain => (theme.pill, theme.pill_hover, theme.text),
        Pill::Live => (mix(theme.pill, theme.accent, live), mix(theme.pill_hover, theme.accent_hover, live), mix(theme.text, theme.accent_ink, live)),
        Pill::Leave => (theme.leave_bg, mix(theme.leave_bg, theme.leave_fg, 0.12), theme.leave_fg),
    };
    let fill = mix(idle, hot, hover);
    let pressed = response.is_pointer_button_down_on();
    let scale = if pressed { 0.97 } else { 1.0 };
    let painted = egui::Rect::from_center_size(rect.center(), rect.size() * scale);
    ui.painter().rect_filled(painted, 18.0, fill);
    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(painted.left() + 12.0, painted.center().y - 8.0),
        egui::vec2(16.0, 16.0),
    );
    glyph(ui.painter(), icon_rect, icon, fg);
    ui.painter().text(
        egui::pos2(icon_rect.right() + 8.0 + galley.rect.width() / 2.0, painted.center().y),
        egui::Align2::CENTER_CENTER,
        label,
        font,
        fg,
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

fn pill_width(ui: &egui::Ui, label: &str) -> f32 {
    let font = egui::FontId::proportional(13.5);
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, egui::Color32::WHITE);
    16.0 + 16.0 + 8.0 + galley.rect.width() + 16.0
}

fn status_line(ui: &mut egui::Ui, theme: &Theme, text: &str, warn: bool) {
    ui.horizontal(|ui| {
        let width = ui.available_width();
        ui.add_space((width - 280.0).max(0.0) / 2.0);
        if !warn {
            dot(ui, theme.dot, 4.0);
            ui.add_space(6.0);
        }
        let color = if warn { theme.leave_fg } else { theme.muted };
        ui.label(egui::RichText::new(text).size(12.0).color(color));
    });
}

fn dot(ui: &mut egui::Ui, color: egui::Color32, radius: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(radius * 2.0, radius * 2.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), radius, color);
}

fn glyph(painter: &egui::Painter, rect: egui::Rect, glyph: Glyph, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.6_f32, color);
    match glyph {
        Glyph::Mic => {
            let head = egui::Rect::from_center_size(rect.center() - egui::vec2(0.0, 2.0), egui::vec2(6.0, 9.0));
            painter.rect_stroke(head, 3.0, stroke, egui::StrokeKind::Middle);
            painter.line_segment([head.center_bottom(), head.center_bottom() + egui::vec2(0.0, 4.0)], stroke);
            painter.line_segment([
                head.center_bottom() + egui::vec2(-4.0, 4.0),
                head.center_bottom() + egui::vec2(4.0, 4.0),
            ], stroke);
        }
        Glyph::Share => {
            let screen = egui::Rect::from_center_size(rect.center() + egui::vec2(0.0, 1.0), egui::vec2(12.0, 8.5));
            painter.rect_stroke(screen, 1.5, stroke, egui::StrokeKind::Inside);
            let tip = egui::pos2(screen.center().x, screen.top() + 1.6);
            let tail = egui::pos2(screen.center().x, screen.bottom() - 1.6);
            painter.line_segment([tail, tip], stroke);
            painter.line_segment([tip, tip + egui::vec2(-2.8, 2.8)], stroke);
            painter.line_segment([tip, tip + egui::vec2(2.8, 2.8)], stroke);
        }
        Glyph::Expand => {
            let a = rect.left_top() + egui::vec2(1.0, 1.0);
            let b = rect.right_bottom() - egui::vec2(1.0, 1.0);
            painter.line_segment([a, a + egui::vec2(5.0, 0.0)], stroke);
            painter.line_segment([a, a + egui::vec2(0.0, 5.0)], stroke);
            painter.line_segment([b, b - egui::vec2(5.0, 0.0)], stroke);
            painter.line_segment([b, b - egui::vec2(0.0, 5.0)], stroke);
            painter.line_segment([egui::pos2(b.x, a.y), egui::pos2(b.x - 5.0, a.y)], stroke);
            painter.line_segment([egui::pos2(b.x, a.y), egui::pos2(b.x, a.y + 5.0)], stroke);
            painter.line_segment([egui::pos2(a.x, b.y), egui::pos2(a.x + 5.0, b.y)], stroke);
            painter.line_segment([egui::pos2(a.x, b.y), egui::pos2(a.x, b.y - 5.0)], stroke);
        }
        Glyph::Leave => {
            painter.line_segment([rect.left_center() + egui::vec2(2.0, -5.0), rect.left_center() + egui::vec2(2.0, 5.0)], stroke);
            painter.line_segment([rect.left_center() + egui::vec2(2.0, -5.0), rect.center() + egui::vec2(-1.0, -5.0)], stroke);
            painter.line_segment([rect.left_center() + egui::vec2(2.0, 5.0), rect.center() + egui::vec2(-1.0, 5.0)], stroke);
            painter.line_segment([rect.center() + egui::vec2(-2.0, 0.0), rect.right_center() - egui::vec2(1.0, 0.0)], stroke);
            painter.line_segment([rect.right_center() - egui::vec2(4.0, 3.0), rect.right_center() - egui::vec2(1.0, 0.0)], stroke);
            painter.line_segment([rect.right_center() - egui::vec2(4.0, -3.0), rect.right_center() - egui::vec2(1.0, 0.0)], stroke);
        }
        Glyph::Monitor => {
            let screen = egui::Rect::from_center_size(rect.center() - egui::vec2(0.0, 3.0), egui::vec2(28.0, 18.0));
            painter.rect_stroke(screen, 3.0, egui::Stroke::new(1.8_f32, color), egui::StrokeKind::Middle);
            painter.line_segment([screen.center_bottom(), screen.center_bottom() + egui::vec2(0.0, 6.0)], egui::Stroke::new(1.8_f32, color));
            painter.line_segment([
                screen.center_bottom() + egui::vec2(-6.0, 6.0),
                screen.center_bottom() + egui::vec2(6.0, 6.0),
            ], egui::Stroke::new(1.8_f32, color));
        }
        Glyph::Sun => {
            painter.circle_filled(rect.center(), 2.6, color);
            for index in 0..8 {
                let angle = index as f32 * std::f32::consts::TAU / 8.0;
                let dir = egui::vec2(angle.cos(), angle.sin());
                painter.line_segment([rect.center() + dir * 4.6, rect.center() + dir * 6.8], stroke);
            }
        }
    }
}

async fn session(
    mut cmd: tokio::sync::mpsc::UnboundedReceiver<Cmd>,
    notes: std::sync::mpsc::Sender<Note>,
    stage: Arc<Stage>,
) {
    let mut call: Option<Joined> = None;
    loop {
        let event = async {
            match call.as_mut() {
                Some(call) => call.events.recv().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            command = cmd.recv() => {
                let Some(command) = command else { break };
                match command {
                    Cmd::Join(settings) => {
                        if call.is_some() {
                            continue;
                        }
                        match connect(&settings, &notes, Arc::clone(&stage)).await {
                            Ok(joined) => {
                                let _ = notes.send(Note::Joined);
                                call = Some(joined);
                            }
                            Err(err) => {
                                let _ = notes.send(Note::Status(err.to_string()));
                            }
                        }
                    }
                    Cmd::Share(picture) => {
                        if let Some(call) = call.as_mut() {
                            start_share(call, picture, &notes).await;
                        }
                    }
                    Cmd::StopShare => {
                        if let Some(call) = call.as_mut() {
                            stop_share(call).await;
                            let _ = notes.send(Note::Sharing(false));
                        }
                    }
                    Cmd::Mute(muted) => {
                        if let Some(call) = call.as_ref() {
                            if muted {
                                call.mic.mute();
                            } else {
                                call.mic.unmute();
                            }
                        }
                    }
                    Cmd::Leave => {
                        if let Some(mut call) = call.take() {
                            call.playback_stop.store(true, Ordering::Release);
                            stop_share(&mut call).await;
                            let _ = tokio::time::timeout(Duration::from_secs(2), call.room.close()).await;
                            let _ = notes.send(Note::Left);
                        }
                    }
                }
            }
            event = event => {
                match event {
                    Some(RoomEvent::DataReceived { payload, topic, .. }) if topic.as_deref() == Some(TOPIC) => {
                        if let Some(call) = call.as_ref() {
                            let _ = call.fan.send(payload.to_vec());
                        }
                    }
                    Some(RoomEvent::Disconnected { reason }) => {
                        if let Some(mut call) = call.take() {
                            call.playback_stop.store(true, Ordering::Release);
                            stop_share(&mut call).await;
                        }
                        let _ = notes.send(Note::Status(format!("The room disconnected ({reason:?}).")));
                        let _ = notes.send(Note::Left);
                    }
                    Some(_) => {}
                    None => {
                        call = None;
                        let _ = notes.send(Note::Left);
                    }
                }
            }
        }
    }
}

async fn connect(
    settings: &CallSettings,
    notes: &std::sync::mpsc::Sender<Note>,
    stage: Arc<Stage>,
) -> Result<Joined> {
    let config = call_config(settings, &PictureSettings {
        width: 1920,
        height: 1080,
        fps: 60,
        bitrate_kbps: 80_000,
    });
    let http = reqwest::Client::new();
    crate::api::health_check(&http, &config.api_base_url)
        .await
        .context("Could not reach the room service")?;
    let token = crate::api::fetch_token(
        &http,
        &config.api_base_url,
        &config.room_name,
        &config.identity,
        &config.client_type,
        config.invite.as_deref(),
    )
    .await
    .context("Could not join this room")?;
    let audio = PlatformAudio::new().context("Could not open the microphone")?;
    let mut options = RoomOptions::default();
    options.auto_subscribe = true;
    let (room, events) = Room::connect(&token.url, &token.token, options)
        .await
        .context("Could not connect to the room")?;
    let mic = LocalAudioTrack::create_audio_track("mic", audio.rtc_source());
    let mic_options = TrackPublishOptions {
        source: TrackSource::Microphone,
        ..Default::default()
    };
    if let Err(err) = room
        .local_participant()
        .publish_track(LocalTrack::Audio(mic.clone()), mic_options)
        .await
    {
        let _ = notes.send(Note::Status(format!(
            "Joined without a microphone ({err}). You can still share and hear the room."
        )));
    }
    let listener = listen("127.0.0.1:0".parse().expect("local address"))?;
    let playback = listener.local_addr().context("picture listener")?;
    let playback_stop = Arc::new(AtomicBool::new(false));
    let stop = Arc::clone(&playback_stop);
    std::thread::Builder::new()
        .name("picture-play".to_string())
        .spawn(move || play_picture(listener, stage, stop))
        .context("picture playback")?;
    let (fan, mut fan_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let follow_slot: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>> = Arc::new(Mutex::new(None));
    let host_slot: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>> = Arc::new(Mutex::new(None));
    let follow_for_fan = Arc::clone(&follow_slot);
    let host_for_fan = Arc::clone(&host_slot);
    tokio::spawn(async move {
        while let Some(packet) = fan_rx.recv().await {
            if let Ok(guard) = follow_for_fan.lock() {
                if let Some(tx) = guard.as_ref() {
                    let _ = tx.send(packet.clone());
                }
            }
            if let Ok(guard) = host_for_fan.lock() {
                if let Some(tx) = guard.as_ref() {
                    let _ = tx.send(packet);
                }
            }
        }
    });
    let participant = room.local_participant();
    let identity = config.identity.clone();
    let stop = Arc::clone(&playback_stop);
    let slot = Arc::clone(&follow_slot);
    tokio::spawn(async move {
        while !stop.load(Ordering::Acquire) {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            if let Ok(mut guard) = slot.lock() {
                *guard = Some(tx);
            }
            if let Err(err) = direct::follow(participant.clone(), &identity, rx, playback).await {
                eprintln!("picture direct: {err:#}");
            }
            if let Ok(mut guard) = slot.lock() {
                *guard = None;
            }
            if stop.load(Ordering::Acquire) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    });
    Ok(Joined {
        room,
        events,
        mic,
        _audio: audio,
        fan,
        host: host_slot,
        stop_share: None,
        share: None,
        playback_stop,
    })
}

async fn start_share(call: &mut Joined, picture: PictureSettings, notes: &std::sync::mpsc::Sender<Note>) {
    if call.share.is_some() {
        return;
    }
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let (host_tx, host_rx) = tokio::sync::mpsc::unbounded_channel();
    if let Ok(mut guard) = call.host.lock() {
        *guard = Some(host_tx);
    }
    let config = call_config(
        &CallSettings {
            name: call.room.local_participant().identity().to_string(),
            room: call.room.name(),
            invite: None,
        },
        &picture,
    );
    let participant = call.room.local_participant();
    let host = Arc::clone(&call.host);
    let notes_task = notes.clone();
    call.stop_share = Some(stop_tx);
    call.share = Some(tokio::spawn(async move {
        let result = share_loop(config, participant, host_rx, stop_rx).await;
        if let Ok(mut guard) = host.lock() {
            *guard = None;
        }
        if let Err(err) = result {
            let _ = notes_task.send(Note::Status(err.to_string()));
            let _ = notes_task.send(Note::Sharing(false));
        }
    }));
    let _ = notes.send(Note::Sharing(true));
}

async fn stop_share(call: &mut Joined) {
    if let Some(stop) = call.stop_share.take() {
        let _ = stop.send(true);
    }
    if let Some(task) = call.share.take() {
        let mut task = task;
        tokio::select! {
            _ = &mut task => {}
            _ = tokio::time::sleep(Duration::from_secs(1)) => task.abort(),
        }
    }
    if let Ok(mut guard) = call.host.lock() {
        *guard = None;
    }
}

async fn share_loop(
    config: AppConfig,
    participant: LocalParticipant,
    incoming: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let pipe = direct::host(participant.clone(), &config.identity, incoming)
        .await
        .context("Could not open the direct picture")?;
    let picture = PictureSender::connect(pipe).context("Could not start the picture sender")?;
    let mut encoder = ScreenEncoder::start(&config).context("Could not capture the screen")?;
    let resolution = VideoResolution {
        width: encoder.width,
        height: encoder.height,
    };
    let source = NativeVideoSource::new_encoded(resolution.clone());
    let track = LocalVideoTrack::create_video_track("screen", RtcVideoSource::Native(source.clone()));
    let options = TrackPublishOptions {
        video_codec: VideoCodec::H264,
        video_encoder: VideoEncoderBackend::PreEncoded,
        simulcast: false,
        source: TrackSource::Screenshare,
        video_encoding: Some(VideoEncoding {
            max_bitrate: u64::from(config.bitrate_kbps) * 1000,
            max_framerate: f64::from(config.target_fps),
        }),
        degradation_preference: Some(DegradationPreference::MaintainFramerateAndResolution),
        ..Default::default()
    };
    let publication = participant
        .publish_track(LocalTrack::Video(track), options)
        .await
        .context("Could not publish the screen")?;
    let mut need_key = true;
    let mut last_timestamp_us = 0i64;
    let mut audio_open = true;
    let mut audio_noted = false;
    loop {
        if *stop.borrow() {
            break;
        }
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    break;
                }
            }
            unit = encoder.units.recv() => {
                let Some(unit) = unit else { break };
                let timestamp_us = match unit.timestamp_us {
                    Some(timestamp) if timestamp > 0 => timestamp,
                    _ => {
                        let now = unix_us_now().max(last_timestamp_us.saturating_add(1));
                        last_timestamp_us = now;
                        now
                    }
                };
                last_timestamp_us = last_timestamp_us.max(timestamp_us);
                if picture.take_keyframe() {
                    encoder.request_keyframe();
                }
                picture.offer(&unit.payload, unit.keyframe, timestamp_us, unit.cursor);
                if unit.captured_at.elapsed() > MAX_FRAME_AGE {
                    need_key = true;
                    encoder.request_keyframe();
                    continue;
                }
                if need_key && !unit.keyframe {
                    encoder.request_keyframe();
                    continue;
                }
                need_key = false;
                let frame = EncodedVideoFrame {
                    codec: EncodedVideoCodec::H264,
                    payload: &unit.payload,
                    timestamp_us,
                    frame_type: if unit.keyframe { EncodedFrameType::Key } else { EncodedFrameType::Delta },
                    resolution: resolution.clone(),
                    frame_metadata: None,
                };
                let _ = source.capture_encoded_frame(&frame);
                if source.take_keyframe_request() {
                    encoder.request_keyframe();
                }
            }
            audio = encoder.audio.recv(), if audio_open => {
                if let Some(audio) = audio {
                    if !audio_noted {
                        audio_noted = true;
                        note_log("desktop audio captured");
                    }
                    picture.offer_audio(&audio.payload, audio.timestamp_us);
                } else {
                    audio_open = false;
                    note_log("desktop audio capture ended");
                }
            }
        }
    }
    encoder.stop();
    let _ = participant.unpublish_track(&publication.sid()).await;
    drop(picture);
    Ok(())
}

fn call_config(settings: &CallSettings, picture: &PictureSettings) -> AppConfig {
    let api_base_url = std::env::var("API_BASE_URL").unwrap_or_else(|_| WORKER.to_string());
    AppConfig {
        api_base_url,
        room_name: settings.room.clone(),
        identity: settings.name.clone(),
        client_type: "native_sender".to_string(),
        platform: TargetPlatform::Auto,
        dry_run: false,
        target_fps: picture.fps,
        probe_seconds: 5,
        heartbeat_seconds: 1,
        encoder_backend: if cfg!(target_os = "windows") {
            EncoderBackend::FfmpegH264Nvenc
        } else {
            EncoderBackend::Fast
        },
        capture_backend: CaptureBackend::Auto,
        width: picture.width,
        height: picture.height,
        bitrate_kbps: picture.bitrate_kbps,
        watch: false,
        picture_to: None,
        picture_listen: None,
        picture_relay: None,
        invite: settings.invite.clone(),
        picture_direct: true,
    }
}

fn play_picture(listener: std::net::TcpListener, stage: Arc<Stage>, stop: Arc<AtomicBool>) {
    let player = match AudioPlayer::start(Arc::new(AtomicBool::new(false))) {
        Ok(player) => Some(player),
        Err(err) => {
            note_log(&format!("desktop audio player: {err:#}"));
            None
        }
    };
    let mut opus = match opus_decoder() {
        Ok(decoder) => Some(decoder),
        Err(err) => {
            note_log(&format!("desktop audio decoder: {err:#}"));
            None
        }
    };
    let mut heard = false;
    while !stop.load(Ordering::Acquire) {
        let stream = match accept_picture(&listener) {
            Ok(Some(stream)) => stream,
            Ok(None) => {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            Err(_) => break,
        };
        let mut reader = FrameReader::new(stream);
        let mut decoder = PictureDecoder::new();
        let mut saw_video = false;
        let mut logged = Instant::now();
        note_log("picture connected");
        loop {
            if stop.load(Ordering::Acquire) {
                return;
            }
            match reader.read_frame() {
                Ok(Some(frame)) if frame.audio => {
                    if let (Some(player), Some(opus)) = (player.as_ref(), opus.as_mut()) {
                        match decode_packet(opus, &frame.payload) {
                            Ok(pcm) => {
                                if !heard {
                                    heard = true;
                                    note_log("desktop audio playing");
                                }
                                player.push(&pcm);
                            }
                            Err(err) if !heard => {
                                heard = true;
                                note_log(&format!("desktop audio decode: {err:#}"));
                            }
                            Err(_) => {}
                        }
                    }
                }
                Ok(Some(frame)) => {
                    if let Some(decoded) = decoder.decode(&frame.payload) {
                        if !saw_video {
                            saw_video = true;
                            note_log(&format!("picture {}x{}", decoded.width, decoded.height));
                        }
                        if logged.elapsed() >= Duration::from_secs(1) {
                            let now_us = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|time| time.as_micros() as i64)
                                .unwrap_or(frame.timestamp_us);
                            let late_ms = now_us.saturating_sub(frame.timestamp_us) / 1000;
                            note_log(&format!("picture late {late_ms}ms"));
                            logged = Instant::now();
                        }
                        store_frame(&stage, decoded);
                    }
                }
                Ok(None) => {
                    note_log("picture closed by the sharer");
                    finish_stream(&stage);
                    break;
                }
                Err(err)
                    if err.kind() == std::io::ErrorKind::TimedOut
                        || err.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(err) => {
                    note_log(&format!("picture stopped: {err}"));
                    finish_stream(&stage);
                    break;
                }
            }
        }
    }
}

fn store_frame(stage: &Stage, frame: Frame) {
    if let Ok(mut latest) = stage.latest.lock() {
        *latest = Some(frame);
    }
}

fn take_frame(stage: &Stage) -> Option<Frame> {
    stage.latest.lock().ok().and_then(|mut latest| latest.take())
}

fn finish_stream(stage: &Stage) {
    if let Ok(mut latest) = stage.latest.lock() {
        *latest = None;
    }
    stage.ended.store(true, Ordering::Release);
}

struct PictureDecoder {
    #[cfg(target_os = "macos")]
    inner: crate::decode_macos::Decoder,
    #[cfg(target_os = "windows")]
    inner: Option<openh264::decoder::Decoder>,
}

impl PictureDecoder {
    fn new() -> Self {
        Self {
            #[cfg(target_os = "macos")]
            inner: crate::decode_macos::Decoder::new(),
            #[cfg(target_os = "windows")]
            inner: openh264::decoder::Decoder::new().ok(),
        }
    }

    fn decode(&mut self, payload: &[u8]) -> Option<Frame> {
        #[cfg(target_os = "macos")]
        {
            let decoded = self.inner.decode(payload)?;
            let (width, height, rgba) = crate::decode_macos::rgba(&decoded.buffer, decoded.full_range)?;
            return Some(Frame {
                width: width as usize,
                height: height as usize,
                rgba,
            });
        }
        #[cfg(target_os = "windows")]
        {
            use openh264::formats::YUVSource;
            let yuv = self.inner.as_mut()?.decode(payload).ok()??;
            let (width, height) = yuv.dimensions();
            if width == 0 || height == 0 || width > 7680 || height > 4320 {
                return None;
            }
            let bytes = width.checked_mul(height)?.checked_mul(4)?;
            let mut rgba = vec![0u8; bytes];
            yuv.write_rgba8(&mut rgba);
            return Some(Frame {
                width,
                height,
                rgba,
            });
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let _ = payload;
            None
        }
    }
}

struct Saved {
    name: String,
    room: String,
    invite: String,
    resolution: usize,
    fps: u32,
    bitrate: u32,
    dark: bool,
}

fn note_log(line: &str) {
    crate::audio::session_log(line);
}

fn settings_path() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    Some(std::path::PathBuf::from(home).join(".mycord"))
}

fn remember() -> Saved {
    let mut saved = Saved {
        name: String::new(),
        room: "mycord-room".to_string(),
        invite: String::new(),
        resolution: 1,
        fps: 60,
        bitrate: 80_000,
        dark: false,
    };
    let Some(path) = settings_path() else {
        return saved;
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return saved;
    };
    let mut lines = text.lines();
    if let Some(name) = lines.next() {
        saved.name = name.to_string();
    }
    if let Some(room) = lines.next() {
        if !room.is_empty() {
            saved.room = room.to_string();
        }
    }
    if let Some(invite) = lines.next() {
        saved.invite = invite.to_string();
    }
    if let Some(resolution) = lines.next().and_then(|line| line.parse::<usize>().ok()) {
        if resolution < RESOLUTIONS.len() {
            saved.resolution = resolution;
        }
    }
    if let Some(fps) = lines.next().and_then(|line| line.parse::<u32>().ok()) {
        if RATES.contains(&fps) {
            saved.fps = fps;
        }
    }
    if let Some(bitrate) = lines.next().and_then(|line| line.parse::<u32>().ok()) {
        if BITRATES.iter().any(|(kbps, _)| *kbps == bitrate) {
            saved.bitrate = bitrate;
        }
    }
    if let Some(theme) = lines.next() {
        saved.dark = theme == "dark";
    }
    saved
}

fn store(
    name: &str,
    room: &str,
    invite: Option<&str>,
    resolution: usize,
    fps: u32,
    bitrate: u32,
    dark: bool,
) {
    let Some(path) = settings_path() else {
        return;
    };
    let theme = if dark { "dark" } else { "light" };
    let body = format!(
        "{name}\n{room}\n{}\n{resolution}\n{fps}\n{bitrate}\n{theme}\n",
        invite.unwrap_or("")
    );
    let _ = std::fs::write(path, body);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stage_keeps_only_the_newest_frame() {
        let stage = Stage {
            latest: Mutex::new(None),
            ended: AtomicBool::new(false),
        };
        store_frame(
            &stage,
            Frame {
                width: 1,
                height: 1,
                rgba: vec![1, 0, 0, 255],
            },
        );
        store_frame(
            &stage,
            Frame {
                width: 1,
                height: 1,
                rgba: vec![9, 0, 0, 255],
            },
        );
        let frame = take_frame(&stage).expect("a frame is waiting");
        assert_eq!(frame.rgba[0], 9);
        assert!(take_frame(&stage).is_none());
    }
}
