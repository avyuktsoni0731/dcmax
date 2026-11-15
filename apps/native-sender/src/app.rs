//! The window people open. No flags: a name, a room code, then voice and the
//! native picture. The command-line tools stay available when arguments are set.

use crate::adapt::PictureAdapt;
use crate::audio::{decode_packet, decoder as opus_decoder, AudioPlayer, PlayClock};
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
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const WORKER: &str = "https://dcmax-token.soniavyukt.workers.dev";
/// One keyframe request while the picture pipe is behind. A request on every
/// skipped frame turned the stall into a run of the largest packets.
const KEY_REQUEST_GAP: Duration = Duration::from_millis(800);

struct Frame {
    width: usize,
    height: usize,
    pixels: Arc<crate::present_gl::Picture>,
}

/// A few frames, released on the same clock as the desktop audio.
/// Ten frames is about 160 ms at 60 fps. A burst older than that is
/// trimmed so a stall cannot replay itself.
struct Stage {
    frames: Mutex<VecDeque<TimedFrame>>,
    ended: AtomicBool,
    clock: Arc<PlayClock>,
    stats: PictureStats,
}

/// Counters for one watched picture. The play thread and the window both
/// update them; a once-a-second line reports the deltas.
struct PictureStats {
    received: AtomicU64,
    decoded: AtomicU64,
    decode_fail: AtomicU64,
    shown: AtomicU64,
    queue_drop: AtomicU64,
    late_drop: AtomicU64,
    audio: AtomicU64,
    video_bytes: AtomicU64,
    audio_bytes: AtomicU64,
}

struct RateSample {
    received: u64,
    decoded: u64,
    decode_fail: u64,
    shown: u64,
    queue_drop: u64,
    late_drop: u64,
    audio: u64,
    video_bytes: u64,
    audio_bytes: u64,
}

struct TimedFrame {
    timestamp_us: i64,
    frame: Frame,
}

enum Cmd {
    Join(CallSettings),
    Share(PictureSettings),
    StopShare,
    Tune(PictureSettings),
    Mute(bool),
    Leave,
}

struct CallSettings {
    name: String,
    room: String,
    invite: Option<String>,
}

#[derive(Clone, Copy)]
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
    tune: Option<tokio::sync::watch::Sender<PictureSettings>>,
    share_size: Option<(u32, u32)>,
    share: Option<tokio::task::JoinHandle<()>>,
    playback_stop: Arc<AtomicBool>,
}

pub fn run() -> Result<()> {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (note_tx, note_rx) = std::sync::mpsc::channel();
    let stage = Arc::new(Stage {
        frames: Mutex::new(VecDeque::new()),
        ended: AtomicBool::new(false),
        clock: PlayClock::new(),
        stats: PictureStats::new(),
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
    frame: Option<Arc<Frame>>,
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
            frame: None,
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
                    self.frame = None;
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
            self.frame = None;
            if let Ok(mut frames) = self.stage.frames.lock() {
                frames.clear();
            }
        }
        // One frame per display refresh, in order. The gpu draw is cheap
        // enough to make that refresh, so a 60 fps stream stays 60 fps.
        if let Some(frame) = take_frame(&self.stage) {
            if frame.width == 0 || frame.height == 0 {
                note_log("rejected a picture frame with no size");
            } else {
                self.frame = Some(Arc::new(frame));
                self.stage.stats.shown.fetch_add(1, Ordering::Relaxed);
            }
        }
        if self.joined {
            ctx.request_repaint();
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
            let bottom_pad = 16.0;
            let dock_height = 160.0;
            let top_space = ui.cursor().top();
            let available_height = ui.max_rect().bottom() - top_space;
            let stage_height = (available_height - dock_height - bottom_pad).max(200.0);
            let stage = egui::Rect::from_min_size(
                egui::pos2(ui.max_rect().left() + 16.0, top_space + 8.0),
                egui::vec2(
                    ui.available_width() - 32.0,
                    stage_height,
                ),
            );
            self.stage_view(ui, theme, stage, 16);
            // stage_view paints into `stage` and does not move the cursor,
            // so the dock used to land on top of "Waiting for a screen"
            // and, once a picture existed, below the window.
            ui.allocate_rect(stage, egui::Sense::hover());
            ui.add_space(12.0);
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
        if let Some(frame) = &self.frame {
            let inset = if radius == 0 { 0.0 } else { 8.0 };
            let available = rect.shrink(inset);
            let scale = (available.width() / frame.width as f32).min(available.height() / frame.height as f32);
            let size = egui::vec2(frame.width as f32 * scale, frame.height as f32 * scale);
            let image_rect = egui::Rect::from_center_size(available.center(), size);
            let shown = Arc::clone(frame);
            ui.painter().add(egui::PaintCallback {
                rect: image_rect,
                callback: Arc::new(eframe::egui_glow::CallbackFn::new(move |_info, painter| {
                    crate::present_gl::draw(painter.gl(), &shown.pixels);
                })),
            });
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
            if self.joined {
                self.send(Cmd::Tune(self.picture_settings()));
            }
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
                    Cmd::Tune(picture) => {
                        if let Some(call) = call.as_mut() {
                            let restart = call.share.is_some()
                                && call.share_size.is_some_and(|size| size != (picture.width, picture.height));
                            if restart {
                                stop_share(call).await;
                                start_share(call, picture, &notes).await;
                            } else if let Some(tune) = &call.tune {
                                let _ = tune.send(picture);
                            }
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
        tune: None,
        share_size: None,
        share: None,
        playback_stop,
    })
}

async fn start_share(call: &mut Joined, picture: PictureSettings, notes: &std::sync::mpsc::Sender<Note>) {
    if call.share.is_some() {
        return;
    }
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let (tune_tx, tune_rx) = tokio::sync::watch::channel(picture);
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
    call.tune = Some(tune_tx);
    call.share_size = Some((picture.width, picture.height));
    call.share = Some(tokio::spawn(async move {
        let result = share_loop(config, participant, host_rx, stop_rx, tune_rx).await;
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
    call.tune = None;
    call.share_size = None;
}

async fn share_loop(
    config: AppConfig,
    participant: LocalParticipant,
    incoming: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    mut stop: tokio::sync::watch::Receiver<bool>,
    mut tune: tokio::sync::watch::Receiver<PictureSettings>,
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
    let mut last_key = Instant::now() - KEY_REQUEST_GAP;
    let mut adapt = PictureAdapt::new(config.bitrate_kbps, config.target_fps);
    let mut last_timestamp_us = 0i64;
    let mut audio_open = true;
    let mut audio_noted = false;
    let mut window = Instant::now();
    let mut encoded = 0u64;
    let mut video_bytes = 0u64;
    let mut audio_pkts = 0u64;
    let mut audio_bytes = 0u64;
    let mut late = 0u64;
    let mut keys = 0u64;
    let mut last_sent = picture.sent();
    let mut last_skipped = picture.skipped();
    note_log(&format!(
        "share started: {}x{} target {}fps cap {}kbps",
        config.width, config.height, config.target_fps, config.bitrate_kbps
    ));
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
            changed = tune.changed() => {
                if changed.is_ok() {
                    let next = *tune.borrow();
                    adapt.set_ceiling(next.bitrate_kbps);
                    adapt.set_target_fps(next.fps);
                    let pace = adapt.pace();
                    encoder.set_bitrate_bps(pace.bps);
                    encoder.set_fps(pace.fps);
                    note_log(&format!(
                        "share retune: {}kbps {}fps",
                        pace.bps / 1000,
                        pace.fps
                    ));
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
                encoded += 1;
                video_bytes += unit.payload.len() as u64;
                if unit.keyframe {
                    keys += 1;
                }
                if picture.take_keyframe() {
                    request_key(&encoder, &mut last_key);
                }
                picture.offer(&unit.payload, unit.keyframe, timestamp_us, unit.cursor);
                if unit.captured_at.elapsed() > MAX_FRAME_AGE {
                    late += 1;
                    need_key = true;
                    request_key(&encoder, &mut last_key);
                    continue;
                }
                if need_key && !unit.keyframe {
                    request_key(&encoder, &mut last_key);
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
                    request_key(&encoder, &mut last_key);
                }
                if window.elapsed() >= Duration::from_secs(1) {
                    apply_pace(
                        &encoder,
                        &mut adapt,
                        &picture,
                        window.elapsed(),
                        &mut encoded,
                        &mut video_bytes,
                        &mut audio_pkts,
                        &mut audio_bytes,
                        &mut late,
                        &mut keys,
                        &mut last_sent,
                        &mut last_skipped,
                    );
                    window = Instant::now();
                }
            }
            audio = encoder.audio.recv(), if audio_open => {
                if let Some(audio) = audio {
                    if !audio_noted {
                        audio_noted = true;
                        note_log("desktop audio captured");
                    }
                    audio_pkts += 1;
                    audio_bytes += audio.payload.len() as u64;
                    picture.offer_audio(&audio.payload, audio.timestamp_us);
                } else {
                    audio_open = false;
                    note_log("desktop audio capture ended");
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(200)) => {
                if window.elapsed() >= Duration::from_secs(1) {
                    apply_pace(
                        &encoder,
                        &mut adapt,
                        &picture,
                        window.elapsed(),
                        &mut encoded,
                        &mut video_bytes,
                        &mut audio_pkts,
                        &mut audio_bytes,
                        &mut late,
                        &mut keys,
                        &mut last_sent,
                        &mut last_skipped,
                    );
                    window = Instant::now();
                }
            }
        }
    }
    encoder.stop();
    let _ = participant.unpublish_track(&publication.sid()).await;
    drop(picture);
    Ok(())
}

fn request_key(encoder: &ScreenEncoder, last_key: &mut Instant) {
    if last_key.elapsed() >= KEY_REQUEST_GAP {
        encoder.request_keyframe();
        *last_key = Instant::now();
    }
}

fn apply_pace(
    encoder: &ScreenEncoder,
    adapt: &mut PictureAdapt,
    picture: &PictureSender,
    elapsed: Duration,
    encoded: &mut u64,
    video_bytes: &mut u64,
    audio_pkts: &mut u64,
    audio_bytes: &mut u64,
    late: &mut u64,
    keys: &mut u64,
    last_sent: &mut u64,
    last_skipped: &mut u64,
) {
    let sent = picture.sent().saturating_sub(*last_sent);
    let skipped = picture.skipped().saturating_sub(*last_skipped);
    let pace = adapt.observe(*encoded, sent, skipped);
    if pace.changed {
        encoder.set_bitrate_bps(pace.bps);
        encoder.set_fps(pace.fps);
        note_log(&format!(
            "share pace: {} {}kbps {}fps",
            pace.reason,
            pace.bps / 1000,
            pace.fps
        ));
    }
    log_share(
        picture,
        elapsed,
        encoded,
        video_bytes,
        audio_pkts,
        audio_bytes,
        late,
        keys,
        last_sent,
        last_skipped,
        pace.bps,
        pace.fps,
    );
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
    let player = match AudioPlayer::start_synced(Arc::new(AtomicBool::new(false)), Arc::clone(&stage.clock)) {
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
        let mut previous = RateSample::default();
        let mut clock_ms = 0i64;
        note_log("picture connected");
        loop {
            if stop.load(Ordering::Acquire) {
                return;
            }
            if logged.elapsed() >= Duration::from_secs(1) {
                log_watch(&stage, logged.elapsed(), &mut previous, clock_ms);
                logged = Instant::now();
            }
            match reader.read_frame() {
                Ok(Some(frame)) if frame.audio => {
                    stage.stats.audio.fetch_add(1, Ordering::Relaxed);
                    stage.stats.audio_bytes.fetch_add(frame.payload.len() as u64, Ordering::Relaxed);
                    if let (Some(player), Some(opus)) = (player.as_ref(), opus.as_mut()) {
                        match decode_packet(opus, &frame.payload) {
                            Ok(pcm) => {
                                if !heard {
                                    heard = true;
                                    note_log("desktop audio playing");
                                }
                                player.push_at(&pcm, frame.timestamp_us);
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
                    stage.stats.received.fetch_add(1, Ordering::Relaxed);
                    stage.stats.video_bytes.fetch_add(frame.payload.len() as u64, Ordering::Relaxed);
                    let now_us = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|time| time.as_micros() as i64)
                        .unwrap_or(frame.timestamp_us);
                    clock_ms = now_us.saturating_sub(frame.timestamp_us) / 1000;
                    if let Some(decoded) = decoder.decode(&frame.payload) {
                        stage.stats.decoded.fetch_add(1, Ordering::Relaxed);
                        if !saw_video {
                            saw_video = true;
                            note_log(&format!("picture {}x{}", decoded.width, decoded.height));
                        }
                        store_frame(&stage, decoded, frame.timestamp_us);
                    } else {
                        stage.stats.decode_fail.fetch_add(1, Ordering::Relaxed);
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

fn store_frame(stage: &Stage, frame: Frame, timestamp_us: i64) {
    stage.clock.note_video(timestamp_us);
    let Ok(mut frames) = stage.frames.lock() else {
        return;
    };
    frames.push_back(TimedFrame { timestamp_us, frame });
    let mut dropped = 0u64;
    while frames.len() > 10 {
        frames.pop_front();
        dropped += 1;
    }
    if dropped > 0 {
        stage.stats.queue_drop.fetch_add(dropped, Ordering::Relaxed);
    }
}

fn take_frame(stage: &Stage) -> Option<Frame> {
    let now = stage.clock.now_us()?;
    let waiting = stage.frames.lock().ok()?.len();
    // A full queue of "not yet" frames means the clock is behind the pictures
    // we still have. The next decode would delete the oldest. Show it.
    if waiting >= 8 {
        if let Some(timestamp_us) = stage.frames.lock().ok().and_then(|frames| frames.front().map(|frame| frame.timestamp_us)) {
            if timestamp_us > now {
                stage.clock.catch_up(timestamp_us);
            }
        }
    }
    let now = stage.clock.now_us()?;
    let mut frames = stage.frames.lock().ok()?;
    let mut dropped = 0u64;
    // Play frames in order. Drop only when the queued pictures span more
    // than half a second, which is a backlog rather than a smooth buffer.
    while frames.len() > 3 {
        let oldest = frames.front().map(|frame| frame.timestamp_us).unwrap_or(0);
        let newest = frames.back().map(|frame| frame.timestamp_us).unwrap_or(0);
        if newest.saturating_sub(oldest) <= 500_000 {
            break;
        }
        frames.pop_front();
        dropped += 1;
    }
    if dropped > 0 {
        stage.stats.late_drop.fetch_add(dropped, Ordering::Relaxed);
    }
    if frames.front().is_some_and(|frame| frame.timestamp_us <= now || frames.len() >= 8) {
        return frames.pop_front().map(|timed| timed.frame);
    }
    None
}

#[cfg(test)]
fn queued(stage: &Stage) -> usize {
    stage.frames.lock().map(|frames| frames.len()).unwrap_or(0)
}

fn finish_stream(stage: &Stage) {
    if let Ok(mut frames) = stage.frames.lock() {
        frames.clear();
    }
    stage.clock.reset();
    stage.ended.store(true, Ordering::Release);
}

struct PictureDecoder {
    #[cfg(target_os = "macos")]
    inner: crate::decode_macos::Decoder,
    #[cfg(target_os = "windows")]
    inner: crate::decode_windows::Decoder,
}

impl PictureDecoder {
    fn new() -> Self {
        Self {
            #[cfg(target_os = "macos")]
            inner: crate::decode_macos::Decoder::new(),
            #[cfg(target_os = "windows")]
            inner: crate::decode_windows::Decoder::new(),
        }
    }

    fn decode(&mut self, payload: &[u8]) -> Option<Frame> {
        #[cfg(target_os = "macos")]
        {
            let decoded = self.inner.decode(payload)?;
            let width = decoded.buffer.width();
            let height = decoded.buffer.height();
            if width == 0 || height == 0 {
                return None;
            }
            return Some(Frame {
                width,
                height,
                pixels: Arc::new(crate::present_gl::Picture::Nv12 {
                    buffer: decoded.buffer,
                    full_range: decoded.full_range,
                }),
            });
        }
        #[cfg(target_os = "windows")]
        {
            let picture = self.inner.decode(payload)?;
            let (width, height) = match &picture {
                crate::present_gl::Picture::Nv12Bytes { width, height, .. } => (*width as usize, *height as usize),
                crate::present_gl::Picture::Rgba { width, height, .. } => (*width as usize, *height as usize),
            };
            if width == 0 || height == 0 {
                return None;
            }
            return Some(Frame {
                width,
                height,
                pixels: Arc::new(picture),
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

impl PictureStats {
    fn new() -> Self {
        Self {
            received: AtomicU64::new(0),
            decoded: AtomicU64::new(0),
            decode_fail: AtomicU64::new(0),
            shown: AtomicU64::new(0),
            queue_drop: AtomicU64::new(0),
            late_drop: AtomicU64::new(0),
            audio: AtomicU64::new(0),
            video_bytes: AtomicU64::new(0),
            audio_bytes: AtomicU64::new(0),
        }
    }
}

impl Default for RateSample {
    fn default() -> Self {
        Self {
            received: 0,
            decoded: 0,
            decode_fail: 0,
            shown: 0,
            queue_drop: 0,
            late_drop: 0,
            audio: 0,
            video_bytes: 0,
            audio_bytes: 0,
        }
    }
}

fn sample_stats(stage: &Stage) -> RateSample {
    let stats = &stage.stats;
    RateSample {
        received: stats.received.load(Ordering::Relaxed),
        decoded: stats.decoded.load(Ordering::Relaxed),
        decode_fail: stats.decode_fail.load(Ordering::Relaxed),
        shown: stats.shown.load(Ordering::Relaxed),
        queue_drop: stats.queue_drop.load(Ordering::Relaxed),
        late_drop: stats.late_drop.load(Ordering::Relaxed),
        audio: stats.audio.load(Ordering::Relaxed),
        video_bytes: stats.video_bytes.load(Ordering::Relaxed),
        audio_bytes: stats.audio_bytes.load(Ordering::Relaxed),
    }
}

fn per_sec(count: u64, secs: f64) -> f64 {
    count as f64 / secs
}

fn log_share(
    picture: &PictureSender,
    elapsed: Duration,
    encoded: &mut u64,
    video_bytes: &mut u64,
    audio_pkts: &mut u64,
    audio_bytes: &mut u64,
    late: &mut u64,
    keys: &mut u64,
    last_sent: &mut u64,
    last_skipped: &mut u64,
    pace_bps: u64,
    pace_fps: u32,
) {
    let secs = elapsed.as_secs_f64().max(0.001);
    let sent = picture.sent();
    let skipped = picture.skipped();
    note_log(&format!(
        "share: encoded={:.0} sent={:.0} skipped={:.0} late={} key={} audio={:.0} video_kbps={:.0} audio_kbps={:.0} pace_kbps={} pace_fps={}",
        per_sec(*encoded, secs),
        per_sec(sent.saturating_sub(*last_sent), secs),
        per_sec(skipped.saturating_sub(*last_skipped), secs),
        *late,
        *keys,
        per_sec(*audio_pkts, secs),
        per_sec(*video_bytes * 8 / 1000, secs),
        per_sec(*audio_bytes * 8 / 1000, secs),
        pace_bps / 1000,
        pace_fps,
    ));
    *encoded = 0;
    *video_bytes = 0;
    *audio_pkts = 0;
    *audio_bytes = 0;
    *late = 0;
    *keys = 0;
    *last_sent = sent;
    *last_skipped = skipped;
}

fn log_watch(stage: &Stage, elapsed: Duration, previous: &mut RateSample, clock_ms: i64) {
    let secs = elapsed.as_secs_f64().max(0.001);
    let now = sample_stats(stage);
    let held = stage.frames.lock().map(|frames| frames.len()).unwrap_or(0);
    note_log(&format!(
        "watch: received={:.0} decoded={:.0} shown={:.0} queue_drop={} late_drop={} decode_fail={} audio={:.0} video_kbps={:.0} audio_kbps={:.0} held={} clock_ms={}",
        per_sec(now.received.saturating_sub(previous.received), secs),
        per_sec(now.decoded.saturating_sub(previous.decoded), secs),
        per_sec(now.shown.saturating_sub(previous.shown), secs),
        now.queue_drop.saturating_sub(previous.queue_drop),
        now.late_drop.saturating_sub(previous.late_drop),
        now.decode_fail.saturating_sub(previous.decode_fail),
        per_sec(now.audio.saturating_sub(previous.audio), secs),
        per_sec(now.video_bytes.saturating_sub(previous.video_bytes) * 8 / 1000, secs),
        per_sec(now.audio_bytes.saturating_sub(previous.audio_bytes) * 8 / 1000, secs),
        held,
        clock_ms,
    ));
    *previous = now;
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
    fn the_stage_waits_for_its_clock_and_drops_a_backlog() {
        let stage = Stage {
            frames: Mutex::new(VecDeque::new()),
            ended: AtomicBool::new(false),
            clock: PlayClock::new(),
            stats: PictureStats::new(),
        };
        store_frame(
            &stage,
            Frame {
                width: 1,
                height: 1,
                pixels: Arc::new(crate::present_gl::Picture::Rgba {
                    width: 1,
                    height: 1,
                    pixels: vec![1, 0, 0, 255],
                }),
            },
            5_000_000,
        );
        assert!(take_frame(&stage).is_none(), "a frame stays held until it is due");
        for index in 0..12u8 {
            store_frame(
                &stage,
                Frame {
                    width: 1,
                    height: 1,
                    pixels: Arc::new(crate::present_gl::Picture::Rgba {
                        width: 1,
                        height: 1,
                        pixels: vec![index, 0, 0, 255],
                    }),
                },
                8_000_000 + i64::from(index) * 16_000,
            );
        }
        assert!(queued(&stage) <= 10, "a burst must not rebuild a long delay");
    }
}
