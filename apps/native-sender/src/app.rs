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
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const WORKER: &str = "https://dcmax-token.soniavyukt.workers.dev";

struct Frame {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
    /// When this frame should hit the screen, on the sender's clock.
    due: Instant,
}

/// Frames wait here in order. The window shows each one when it is due.
struct Stage {
    frames: Mutex<VecDeque<Frame>>,
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
        frames: Mutex::new(VecDeque::new()),
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
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([860.0, 560.0])
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
    status: String,
    picture: Option<egui::TextureHandle>,
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
        Self {
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
            status: "You and the other person each open MyCord and join the same room.".to_string(),
            picture: None,
        }
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
        if self.joined {
            self.call_screen(ctx);
        } else {
            self.lobby(ctx);
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
            if let Ok(mut frames) = self.stage.frames.lock() {
                frames.clear();
            }
        }
        let due = {
            let Ok(mut frames) = self.stage.frames.lock() else {
                return;
            };
            let now = Instant::now();
            if frames.front().is_some_and(|frame| frame.due <= now) {
                frames.pop_front()
            } else {
                frames.front().map(|frame| frame.due)
                    .map(|due| ctx.request_repaint_after(due.saturating_duration_since(now)));
                None
            }
        };
        if let Some(frame) = due {
            let pixels = frame.width.saturating_mul(frame.height).saturating_mul(4);
            if frame.width == 0 || frame.height == 0 || frame.rgba.len() != pixels {
                note_log(&format!(
                    "rejected a picture frame {}x{} ({} bytes)",
                    frame.width,
                    frame.height,
                    frame.rgba.len()
                ));
                ctx.request_repaint();
                return;
            }
            let image = egui::ColorImage::from_rgba_unmultiplied([frame.width, frame.height], &frame.rgba);
            if let Some(texture) = &mut self.picture {
                texture.set(image, egui::TextureOptions::LINEAR);
            } else {
                self.picture = Some(ctx.load_texture("picture", image, egui::TextureOptions::LINEAR));
            }
            ctx.request_repaint();
        } else if self.joined {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }

    fn lobby(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            let top = (ui.available_height() * 0.14).clamp(28.0, 96.0);
            ui.add_space(top);
            ui.vertical_centered(|ui| {
                ui.set_width(420.0);
                ui.label(egui::RichText::new("MyCord").size(18.0).color(egui::Color32::from_rgb(224, 168, 92)));
                ui.add_space(8.0);
                ui.label(egui::RichText::new("Open a room").size(32.0).strong());
                ui.add_space(6.0);
                ui.label("The other person opens MyCord, types this room code, and joins. Voice and the screen stay in this window.");
                ui.add_space(22.0);
                field(ui, "Your name", &mut self.name, "Ava");
                ui.add_space(10.0);
                field(ui, "Room code", &mut self.room, "mycord-room");
                ui.add_space(10.0);
                field(ui, "Invite, if this room uses one", &mut self.invite, "Leave blank if you have none");
                ui.add_space(18.0);
                self.picture_pickers(ui);
                ui.add_space(18.0);
                let label = if self.joining { "Joining…" } else { "Join room" };
                let join = egui::Button::new(label).fill(egui::Color32::from_rgb(224, 168, 92)).min_size(egui::vec2(420.0, 40.0));
                if ui.add_enabled(!self.joining, join).clicked() {
                    self.join();
                }
                ui.add_space(12.0);
                ui.label(egui::RichText::new(&self.status).color(egui::Color32::from_rgb(156, 148, 134)));
            });
        });
    }

    fn call_screen(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("bar").exact_height(52.0).show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(egui::RichText::new("MyCord").strong().color(egui::Color32::from_rgb(224, 168, 92)));
                ui.add_space(12.0);
                ui.label(egui::RichText::new(&self.room).size(16.0));
                ui.add_space(12.0);
                ui.label(egui::RichText::new(&self.status).color(egui::Color32::from_rgb(156, 148, 134)));
            });
        });
        egui::TopBottomPanel::bottom("dock").exact_height(108.0).show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let mute = if self.muted { "Unmute" } else { "Mute" };
                if dock_button(ui, mute, self.muted).clicked() {
                    self.muted = !self.muted;
                    self.send(Cmd::Mute(self.muted));
                }
                let share = if self.sharing { "Stop sharing" } else { "Share screen" };
                if dock_button(ui, share, self.sharing).clicked() {
                    if self.sharing {
                        self.sharing = false;
                        self.send(Cmd::StopShare);
                    } else {
                        self.send(Cmd::Share(self.picture_settings()));
                    }
                }
                let full = if self.fullscreen { "Exit full screen" } else { "Full screen" };
                if dock_button(ui, full, self.fullscreen).clicked() {
                    self.set_fullscreen(ctx, !self.fullscreen);
                }
                let leave = if self.leaving { "Leaving…" } else { "Leave" };
                let leave_button = egui::Button::new(leave)
                    .fill(egui::Color32::from_rgb(227, 106, 85))
                    .min_size(egui::vec2(120.0, 40.0));
                if ui.add_enabled(!self.leaving, leave_button).clicked() {
                    self.leaving = true;
                    self.status = "Leaving the room…".to_string();
                    self.send(Cmd::Leave);
                }
            });
            ui.add_space(8.0);
            self.picture_pickers(ui);
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            let rect = ui.max_rect();
            ui.painter().rect_filled(rect, 0.0, egui::Color32::from_rgb(12, 11, 9));
            if let Some(texture) = &self.picture {
                let available = rect.size();
                let sized = texture.size_vec2();
                let scale = (available.x / sized.x).min(available.y / sized.y);
                let size = sized * scale;
                let image_rect = egui::Rect::from_center_size(rect.center(), size);
                ui.put(image_rect, egui::Image::new((texture.id(), size)));
            } else {
                ui.allocate_ui_at_rect(rect, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(rect.height() * 0.38);
                        ui.label(egui::RichText::new("Waiting for a screen").size(22.0));
                        ui.add_space(6.0);
                        ui.label("When someone presses Share screen, it shows here. Yours shows on their window.");
                    });
                });
            }
        });
    }

    fn picture_pickers(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let mut resolution = self.resolution;
            egui::ComboBox::from_label("Resolution")
                .selected_text(RESOLUTIONS[resolution].2)
                .show_ui(ui, |ui| {
                    for (index, (_, _, label)) in RESOLUTIONS.iter().enumerate() {
                        ui.selectable_value(&mut resolution, index, *label);
                    }
                });
            self.resolution = resolution;
            let mut fps = self.fps;
            egui::ComboBox::from_label("Refresh")
                .selected_text(format!("{fps} Hz"))
                .show_ui(ui, |ui| {
                    for rate in RATES {
                        ui.selectable_value(&mut fps, *rate, format!("{rate} Hz"));
                    }
                });
            self.fps = fps;
            let mut bitrate = self.bitrate_kbps;
            let bitrate_label = BITRATES
                .iter()
                .find(|(kbps, _)| *kbps == bitrate)
                .map(|(_, label)| *label)
                .unwrap_or("Custom");
            egui::ComboBox::from_label("Bitrate")
                .selected_text(bitrate_label)
                .show_ui(ui, |ui| {
                    for (kbps, label) in BITRATES {
                        ui.selectable_value(&mut bitrate, *kbps, *label);
                    }
                });
            self.bitrate_kbps = bitrate;
        });
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
        store(&name, &room, invite.as_deref(), self.resolution, self.fps, self.bitrate_kbps);
        self.joining = true;
        self.status = "Joining…".to_string();
        self.send(Cmd::Join(CallSettings { name, room, invite }));
    }
}

fn field(ui: &mut egui::Ui, label: &str, text: &mut String, hint: &str) {
    ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
        ui.set_width(420.0);
        ui.label(label);
        ui.add_sized([420.0, 32.0], egui::TextEdit::singleline(text).hint_text(hint));
    });
}

fn dock_button(ui: &mut egui::Ui, label: &str, on: bool) -> egui::Response {
    let mut button = egui::Button::new(label).min_size(egui::vec2(140.0, 40.0));
    if on {
        button = button.fill(egui::Color32::from_rgb(224, 168, 92));
    }
    ui.add(button)
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
                    picture.offer_audio(&audio.payload, audio.timestamp_us);
                } else {
                    audio_open = false;
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
    let player = AudioPlayer::start(Arc::new(AtomicBool::new(false))).ok();
    let mut opus = opus_decoder().ok();
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
        let mut pace = Pace::new();
        let mut decoder = PictureDecoder::new();
        let mut saw_video = false;
        note_log("picture connected");
        loop {
            if stop.load(Ordering::Acquire) {
                return;
            }
            match reader.read_frame() {
                Ok(Some(frame)) if frame.audio => {
                    if let (Some(player), Some(opus)) = (player.as_ref(), opus.as_mut()) {
                        if let Ok(pcm) = decode_packet(opus, &frame.payload) {
                            player.push(&pcm);
                        }
                    }
                }
                Ok(Some(frame)) => {
                    if let Some(mut decoded) = decoder.decode(&frame.payload) {
                        if !saw_video {
                            saw_video = true;
                            note_log(&format!("picture {}x{}", decoded.width, decoded.height));
                        }
                        decoded.due = pace.due(frame.timestamp_us);
                        enqueue(&stage, decoded);
                    }
                }
                Ok(None) => {
                    // The sharer closed the picture. A quiet socket is not the
                    // end: the next frame is still this same connection.
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

struct Pace {
    origin_us: i64,
    origin: Instant,
    started: bool,
}

impl Pace {
    fn new() -> Self {
        Self {
            origin_us: 0,
            origin: Instant::now(),
            started: false,
        }
    }

    /// Two frames of hold, then each frame keeps the gap the sender captured.
    fn due(&mut self, timestamp_us: i64) -> Instant {
        const HOLD: Duration = Duration::from_millis(34);
        let now = Instant::now();
        if !self.started || timestamp_us <= self.origin_us {
            self.started = true;
            self.origin_us = timestamp_us;
            self.origin = now;
            return now + HOLD;
        }
        let elapsed = Duration::from_micros(timestamp_us.saturating_sub(self.origin_us) as u64);
        self.origin + elapsed + HOLD
    }
}

fn enqueue(stage: &Stage, frame: Frame) {
    const CAP: usize = 8;
    let Ok(mut frames) = stage.frames.lock() else {
        return;
    };
    if frames.len() >= CAP {
        // The window is more than eight frames behind. Releasing the oldest
        // keeps the delay from growing; every newer frame is still shown.
        frames.pop_front();
    }
    frames.push_back(frame);
}

fn finish_stream(stage: &Stage) {
    if let Ok(mut frames) = stage.frames.lock() {
        frames.clear();
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
                due: Instant::now(),
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
                due: Instant::now(),
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
}

fn note_log(line: &str) {
    let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) else {
        return;
    };
    let path = std::path::PathBuf::from(home).join(".mycord.log");
    let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|time| time.as_secs())
        .unwrap_or(0);
    use std::io::Write;
    let _ = writeln!(file, "{secs} {line}");
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
    saved
}

fn store(name: &str, room: &str, invite: Option<&str>, resolution: usize, fps: u32, bitrate: u32) {
    let Some(path) = settings_path() else {
        return;
    };
    let body = format!(
        "{name}\n{room}\n{}\n{resolution}\n{fps}\n{bitrate}\n",
        invite.unwrap_or("")
    );
    let _ = std::fs::write(path, body);
}
