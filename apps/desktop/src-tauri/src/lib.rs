use serde::Deserialize;
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

struct Rect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    radius: f64,
    visible: bool,
    holes: Vec<[f64; 4]>,
}

struct DesktopState {
    engine: Mutex<Option<native_sender::Engine>>,
    rect: Mutex<Rect>,
}

#[derive(Deserialize)]
struct JoinBody {
    name: String,
    room: String,
    invite: Option<String>,
    #[serde(default)]
    session: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShareBody {
    width: u32,
    height: u32,
    fps: u32,
    bitrate_kbps: u32,
    #[serde(default)]
    display_id: Option<u32>,
}

#[derive(Deserialize)]
struct MuteBody {
    muted: bool,
}

#[derive(Deserialize)]
struct GainBody {
    percent: u32,
}

#[derive(Deserialize)]
struct HoleBody {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Deserialize)]
struct RectBody {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    visible: bool,
    #[serde(default)]
    radius: f64,
    #[serde(default)]
    holes: Vec<HoleBody>,
}

#[derive(Deserialize)]
struct FullBody {
    on: bool,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Profile {
    name: String,
    room: String,
    invite: String,
    resolution: usize,
    fps: u32,
    bitrate_kbps: u32,
    dark: bool,
    voice: u32,
    screen: u32,
    input: String,
    output: String,
    noise: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProfileBody {
    name: String,
    room: String,
    invite: String,
    resolution: usize,
    fps: u32,
    bitrate_kbps: u32,
    dark: bool,
    voice: u32,
    screen: u32,
    input: String,
    output: String,
    #[serde(default = "noise_on")]
    noise: bool,
}

fn noise_on() -> bool {
    true
}

#[derive(Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum NoteEvent {
    Status { text: String },
    Joined,
    Left,
    Sharing { on: bool },
    Watching { on: bool },
    People { people: Vec<PersonEvent> },
}

fn emit_watching(handle: &AppHandle, on: bool) {
    thread_local! {
        static LAST: Cell<Option<bool>> = const { Cell::new(None) };
    }
    let changed = LAST.with(|slot| {
        if slot.get() == Some(on) {
            return false;
        }
        slot.set(Some(on));
        true
    });
    if changed {
        let _ = handle.emit("engine", NoteEvent::Watching { on });
    }
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PersonEvent {
    name: String,
    local: bool,
    speaking: bool,
    muted: bool,
}

fn map_note(note: native_sender::EngineNote) -> NoteEvent {
    match note {
        native_sender::EngineNote::Status(text) => NoteEvent::Status { text },
        native_sender::EngineNote::Joined => NoteEvent::Joined,
        native_sender::EngineNote::Left => NoteEvent::Left,
        native_sender::EngineNote::Sharing(on) => NoteEvent::Sharing { on },
        native_sender::EngineNote::People(people) => NoteEvent::People {
            people: people
                .into_iter()
                .map(|person| PersonEvent {
                    name: person.name,
                    local: person.local,
                    speaking: person.speaking,
                    muted: person.muted,
                })
                .collect(),
        },
    }
}

fn with_engine<T>(
    state: &DesktopState,
    call: impl FnOnce(&native_sender::Engine) -> anyhow::Result<T>,
) -> Result<T, String> {
    let guard = state.engine.lock().map_err(|_| "the call runtime is unavailable".to_string())?;
    let engine = guard.as_ref().ok_or_else(|| "the call runtime is not running".to_string())?;
    call(engine).map_err(|err| err.to_string())
}

#[tauri::command]
fn join(state: State<DesktopState>, body: JoinBody) -> Result<(), String> {
    let stored = load_profile();
    native_sender::set_noise_suppression(stored.noise);
    let _ = native_sender::choose_input(&stored.input);
    let _ = native_sender::choose_output(&stored.output);
    let _ = native_sender::prepare_voice();
    with_engine(&state, |engine| {
        engine.join(native_sender::CallRequest {
            name: body.name,
            room: body.room,
            invite: body.invite,
            session: body.session,
        })
    })
}

#[tauri::command]
fn leave(state: State<DesktopState>) -> Result<(), String> {
    with_engine(&state, |engine| engine.leave())
}

#[tauri::command]
fn share(state: State<DesktopState>, body: ShareBody) -> Result<(), String> {
    with_engine(&state, |engine| engine.share(share_request(body)))
}

#[tauri::command]
fn stop_share(state: State<DesktopState>) -> Result<(), String> {
    with_engine(&state, |engine| engine.stop_share())
}

#[tauri::command]
fn tune(state: State<DesktopState>, body: ShareBody) -> Result<(), String> {
    with_engine(&state, |engine| engine.tune(share_request(body)))
}

#[tauri::command]
fn set_muted(state: State<DesktopState>, body: MuteBody) -> Result<(), String> {
    with_engine(&state, |engine| engine.set_muted(body.muted))
}

#[tauri::command]
fn set_voice(state: State<DesktopState>, body: GainBody) -> Result<(), String> {
    with_engine(&state, |engine| engine.set_voice(body.percent))
}

#[tauri::command]
fn set_screen(state: State<DesktopState>, body: GainBody) -> Result<(), String> {
    with_engine(&state, |engine| engine.set_screen(body.percent))
}

#[derive(Clone, serde::Serialize)]
struct Gear {
    id: String,
    name: String,
}

#[derive(Deserialize)]
struct DeviceBody {
    id: String,
}

#[tauri::command]
fn inputs() -> Result<Vec<Gear>, String> {
    native_sender::audio_inputs()
        .map(|list| list.into_iter().map(|device| Gear { id: device.id, name: device.name }).collect())
        .map_err(|err| err.to_string())
}

#[tauri::command]
fn outputs() -> Result<Vec<Gear>, String> {
    native_sender::audio_outputs()
        .map(|list| list.into_iter().map(|device| Gear { id: device.id, name: device.name }).collect())
        .map_err(|err| err.to_string())
}

#[tauri::command]
fn set_input(body: DeviceBody) -> Result<(), String> {
    native_sender::choose_input(&body.id).map_err(|err| err.to_string())
}

#[tauri::command]
fn set_output(body: DeviceBody) -> Result<(), String> {
    native_sender::choose_output(&body.id).map_err(|err| err.to_string())
}

#[derive(Deserialize)]
struct NoiseBody {
    on: bool,
}

#[tauri::command]
fn set_noise(body: NoiseBody) -> Result<(), String> {
    native_sender::set_noise_suppression(body.on);
    Ok(())
}

#[tauri::command]
fn set_fullscreen(app: AppHandle, body: FullBody) -> Result<(), String> {
    let window = app.get_webview_window("main").ok_or("the window is unavailable")?;
    window.set_fullscreen(body.on).map_err(|err| err.to_string())
}

#[tauri::command]
fn profile() -> Profile {
    load_profile()
}

#[tauri::command]
fn remember(body: ProfileBody) -> Result<(), String> {
    save_profile(&body);
    Ok(())
}

#[derive(Deserialize)]
struct AccountBody {
    action: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    login: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    day: String,
    #[serde(default)]
    month: String,
    #[serde(default)]
    year: String,
    #[serde(default)]
    session: String,
}

#[tauri::command]
fn account(body: AccountBody) -> Result<native_sender::AccountSession, String> {
    let payload = serde_json::json!({
        "action": body.action,
        "email": body.email,
        "username": body.username,
        "login": body.login,
        "password": body.password,
        "day": body.day,
        "month": body.month,
        "year": body.year,
        "session": body.session,
    });
    native_sender::account_blocking(&payload).map_err(|err| err.to_string())
}

#[tauri::command]
fn stage_rect(state: State<DesktopState>, body: RectBody) -> Result<(), String> {
    let mut rect = state.rect.lock().map_err(|_| "the stage is unavailable".to_string())?;
    *rect = Rect {
        x: body.x,
        y: body.y,
        width: body.width,
        height: body.height,
        radius: body.radius,
        visible: body.visible,
        holes: body
            .holes
            .into_iter()
            .map(|hole| [hole.x, hole.y, hole.width, hole.height])
            .collect(),
    };
    Ok(())
}

fn share_request(body: ShareBody) -> native_sender::ShareRequest {
    native_sender::ShareRequest {
        width: body.width,
        height: body.height,
        fps: body.fps,
        bitrate_kbps: body.bitrate_kbps,
        display_id: body.display_id,
    }
}

#[derive(Clone, serde::Serialize)]
struct ScreenInfo {
    id: u32,
    label: String,
    width: u32,
    height: u32,
}

#[tauri::command]
fn screens() -> Result<Vec<ScreenInfo>, String> {
    native_sender::screens()
        .map(|list| {
            list.into_iter()
                .map(|screen| ScreenInfo {
                    id: screen.id,
                    label: screen.label,
                    width: screen.width,
                    height: screen.height,
                })
                .collect()
        })
        .map_err(|err| err.to_string())
}

fn watch_notes(handle: AppHandle) {
    std::thread::Builder::new()
        .name("mycord-notes".to_string())
        .spawn(move || loop {
            let note = {
                let state = handle.state::<DesktopState>();
                let guard = state.engine.lock().ok();
                guard.and_then(|engine| engine.as_ref().and_then(|engine| engine.poll()))
            };
            if let Some(note) = note {
                let _ = handle.emit("engine", map_note(note));
            }
            std::thread::sleep(Duration::from_millis(40));
        })
        .ok();
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn watch_picture(handle: AppHandle) {
    static QUEUED: AtomicBool = AtomicBool::new(false);
    std::thread::Builder::new()
        .name("mycord-present".to_string())
        .spawn(move || loop {
            if QUEUED
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                let draw = handle.clone();
                let present = draw.clone();
                if draw
                    .run_on_main_thread(move || {
                        present_on_main(&present);
                        QUEUED.store(false, Ordering::Release);
                    })
                    .is_err()
                {
                    QUEUED.store(false, Ordering::Release);
                }
            }
            std::thread::sleep(Duration::from_millis(16));
        })
        .ok();
}

#[cfg(target_os = "macos")]
fn present_on_main(handle: &AppHandle) {
    use native_sender::{Picture, Pointer, Presenter};
    use std::cell::RefCell;
    use std::ffi::c_void;

    struct Surface {
        view: *mut c_void,
        presenter: Option<Presenter>,
    }
    thread_local! {
        static SURFACE: RefCell<Surface> = RefCell::new(Surface { view: std::ptr::null_mut(), presenter: None });
    }
    extern "C" {
        fn dcmax_stage_place(
            ns_window: *mut c_void,
            ns_content: *mut c_void,
            existing: *mut c_void,
            x: f64,
            y: f64,
            w: f64,
            h: f64,
            visible: i32,
        ) -> *mut c_void;
        fn dcmax_stage_holes(view: *mut c_void, xywh: *const f64, count: i32, radius: f64);
    }

    let Some(window) = handle.get_webview_window("main") else {
        return;
    };
    let Ok(ns_window) = window.ns_window() else {
        return;
    };
    let Ok(ns_content) = window.ns_view() else {
        return;
    };
    let rect = {
        let state = handle.state::<DesktopState>();
        let Ok(rect) = state.rect.lock() else {
            return;
        };
        Rect {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
            radius: rect.radius,
            visible: rect.visible,
            holes: rect.holes.clone(),
        }
    };
    // `showing` means a frame has been drawn and should stay up. `armed` means
    // the child window was already on screen on a previous tick. Metal will
    // not give a drawable for a window that was just created, and taking the
    // frame then discards it.
    thread_local! {
        static SHOWING: Cell<bool> = const { Cell::new(false) };
        static ARMED: Cell<bool> = const { Cell::new(false) };
    }
    SURFACE.with(|slot| {
        let mut surface = slot.borrow_mut();
        let place = |view: *mut c_void, on: bool| unsafe {
            dcmax_stage_place(
                ns_window.cast(),
                ns_content.cast(),
                view,
                rect.x,
                rect.y,
                rect.width.max(2.0),
                rect.height.max(2.0),
                i32::from(on && rect.width >= 2.0 && rect.height >= 2.0),
            )
        };
        if !rect.visible {
            surface.view = place(surface.view, false);
            SHOWING.with(|slot| slot.set(false));
            ARMED.with(|slot| slot.set(false));
            return;
        }
        let state = handle.state::<DesktopState>();
        let Ok(engine) = state.engine.lock() else {
            return;
        };
        let Some(engine) = engine.as_ref() else {
            return;
        };
        if engine.picture_ended() {
            SHOWING.with(|slot| slot.set(false));
            ARMED.with(|slot| slot.set(false));
        }
        let showing = SHOWING.with(|slot| slot.get());
        let want = showing || engine.picture_ready();
        let view = place(surface.view, want);
        surface.view = view;
        let punch = |view: *mut c_void| {
            if view.is_null() {
                return;
            }
            let packed: Vec<f64> = rect.holes.iter().flat_map(|hole| hole.iter().copied()).collect();
            let ptr = if packed.is_empty() { std::ptr::null() } else { packed.as_ptr() };
            unsafe { dcmax_stage_holes(view, ptr, (packed.len() / 4) as i32, rect.radius) };
        };
        if surface.presenter.is_none() && !view.is_null() {
            match Presenter::attach(view) {
                Ok(presenter) => surface.presenter = Some(presenter),
                Err(err) => eprintln!("picture surface: {err}"),
            }
        }
        if !want || surface.presenter.is_none() {
            ARMED.with(|slot| slot.set(false));
            punch(surface.view);
            return;
        }
        let armed = ARMED.with(|slot| slot.get());
        if !armed {
            ARMED.with(|slot| slot.set(true));
            punch(surface.view);
            return;
        }
        let Some(presenter) = surface.presenter.as_ref() else {
            punch(surface.view);
            return;
        };
        let drew = engine
            .with_picture(|picture| match picture {
                Picture::Nv12 { buffer, full_range } => {
                    presenter.draw_nv12(
                        buffer.as_ptr().cast(),
                        *full_range,
                        Pointer {
                            visible: false,
                            x: 0.0,
                            y: 0.0,
                        },
                    ) == 0
                }
                Picture::Rgba { .. } => false,
            })
            .unwrap_or(false);
        if drew {
            SHOWING.with(|slot| slot.set(true));
        }
        punch(surface.view);
    });
    emit_watching(handle, SHOWING.with(|slot| slot.get()));
}

#[cfg(target_os = "windows")]
fn present_on_main(handle: &AppHandle) {
    use native_sender::Picture;
    use std::cell::RefCell;
    use std::ffi::c_void;

    struct Surface {
        window: *mut c_void,
    }
    thread_local! {
        static SURFACE: RefCell<Surface> = RefCell::new(Surface { window: std::ptr::null_mut() });
        static SHOWING: Cell<bool> = const { Cell::new(false) };
        static ARMED: Cell<bool> = const { Cell::new(false) };
    }
    extern "C" {
        fn dcmax_stage_place(
            owner: *mut c_void,
            existing: *mut c_void,
            x: f64,
            y: f64,
            w: f64,
            h: f64,
            visible: i32,
        ) -> *mut c_void;
        fn dcmax_stage_ready(window: *mut c_void) -> i32;
        fn dcmax_stage_draw_nv12(
            window: *mut c_void,
            y: *const u8,
            uv: *const u8,
            width: i32,
            height: i32,
            full_range: i32,
        ) -> i32;
        fn dcmax_stage_holes(window: *mut c_void, xywh: *const f64, count: i32, radius: f64);
    }

    let Some(window) = handle.get_webview_window("main") else {
        return;
    };
    let Ok(hwnd) = window.hwnd() else {
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0).max(1.0);
    let rect = {
        let state = handle.state::<DesktopState>();
        let Ok(rect) = state.rect.lock() else {
            return;
        };
        Rect {
            x: rect.x * scale,
            y: rect.y * scale,
            width: rect.width * scale,
            height: rect.height * scale,
            radius: rect.radius * scale,
            visible: rect.visible,
            holes: rect
                .holes
                .iter()
                .map(|hole| [hole[0] * scale, hole[1] * scale, hole[2] * scale, hole[3] * scale])
                .collect(),
        }
    };
    SURFACE.with(|slot| {
        let mut surface = slot.borrow_mut();
        let place = |existing: *mut c_void, on: bool| unsafe {
            dcmax_stage_place(
                hwnd.0,
                existing,
                rect.x,
                rect.y,
                rect.width.max(2.0),
                rect.height.max(2.0),
                i32::from(on && rect.width >= 2.0 && rect.height >= 2.0),
            )
        };
        if !rect.visible {
            surface.window = place(surface.window, false);
            SHOWING.with(|slot| slot.set(false));
            ARMED.with(|slot| slot.set(false));
            return;
        }
        let state = handle.state::<DesktopState>();
        let Ok(engine) = state.engine.lock() else {
            return;
        };
        let Some(engine) = engine.as_ref() else {
            return;
        };
        if engine.picture_ended() {
            SHOWING.with(|slot| slot.set(false));
            ARMED.with(|slot| slot.set(false));
        }
        let showing = SHOWING.with(|slot| slot.get());
        let want = showing || engine.picture_ready();
        surface.window = place(surface.window, want);
        let punch = |window: *mut c_void| {
            if window.is_null() {
                return;
            }
            let packed: Vec<f64> = rect.holes.iter().flat_map(|hole| hole.iter().copied()).collect();
            let ptr = if packed.is_empty() { std::ptr::null() } else { packed.as_ptr() };
            unsafe { dcmax_stage_holes(window, ptr, (packed.len() / 4) as i32, rect.radius) };
        };
        if !want || surface.window.is_null() {
            ARMED.with(|slot| slot.set(false));
            punch(surface.window);
            return;
        }
        let armed = ARMED.with(|slot| slot.get());
        if !armed {
            ARMED.with(|slot| slot.set(true));
            punch(surface.window);
            return;
        }
        if unsafe { dcmax_stage_ready(surface.window) } == 0 {
            punch(surface.window);
            return;
        }
        let view = surface.window;
        let drew = engine
            .with_picture(|picture| match picture {
                Picture::Nv12Bytes { width, height, y, uv, full_range } => unsafe {
                    dcmax_stage_draw_nv12(view, y.as_ptr(), uv.as_ptr(), *width, *height, i32::from(*full_range)) == 0
                },
                Picture::Rgba { .. } => false,
            })
            .unwrap_or(false);
        if drew {
            SHOWING.with(|slot| slot.set(true));
        }
        punch(surface.window);
    });
    emit_watching(handle, SHOWING.with(|slot| slot.get()));
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(DesktopState {
            engine: Mutex::new(None),
            rect: Mutex::new(Rect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
                radius: 0.0,
                visible: false,
                holes: Vec::new(),
            }),
        })
        .invoke_handler(tauri::generate_handler![
            join,
            leave,
            share,
            stop_share,
            tune,
            set_muted,
            set_voice,
            set_screen,
            inputs,
            outputs,
            set_input,
            set_output,
            set_noise,
            set_fullscreen,
            profile,
            remember,
            account,
            stage_rect,
            screens
        ])
        .setup(|app| {
            let engine = native_sender::Engine::start().map_err(|err| err.to_string())?;
            *app.state::<DesktopState>()
                .engine
                .lock()
                .map_err(|_| "the call runtime is unavailable".to_string())? = Some(engine);
            let handle = app.handle().clone();
            watch_notes(handle.clone());
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            watch_picture(handle);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("MyCord failed to start")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                if let Some(state) = app.try_state::<DesktopState>() {
                    if let Ok(engine) = state.engine.lock() {
                        if let Some(engine) = engine.as_ref() {
                            let _ = engine.leave();
                        }
                    }
                }
                // Leave is handled on the call thread. Give that thread time
                // to close the room before this process exits.
                std::thread::sleep(Duration::from_millis(400));
            }
        });
}

fn profile_path() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).ok()?;
    Some(std::path::PathBuf::from(home).join(".mycord"))
}

fn load_profile() -> Profile {
    let mut profile = Profile {
        name: String::new(),
        room: "mycord-room".to_string(),
        invite: String::new(),
        resolution: 1,
        fps: 60,
        bitrate_kbps: 40_000,
        dark: false,
        voice: 100,
        screen: 100,
        input: String::new(),
        output: String::new(),
        noise: true,
    };
    let Some(path) = profile_path() else {
        return profile;
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return profile;
    };
    let mut lines = text.lines();
    if let Some(name) = lines.next() {
        profile.name = name.to_string();
    }
    if let Some(room) = lines.next() {
        if !room.is_empty() {
            profile.room = room.to_string();
        }
    }
    if let Some(invite) = lines.next() {
        profile.invite = invite.to_string();
    }
    if let Some(resolution) = lines.next().and_then(|line| line.parse().ok()) {
        if resolution < 3 {
            profile.resolution = resolution;
        }
    }
    if let Some(fps) = lines.next().and_then(|line| line.parse().ok()) {
        if fps == 30 || fps == 60 {
            profile.fps = fps;
        }
    }
    if let Some(bitrate) = lines.next().and_then(|line| line.parse().ok()) {
        if matches!(bitrate, 8000 | 12000 | 20000 | 40000 | 80000 | 100000) {
            profile.bitrate_kbps = bitrate;
        }
    }
    if let Some(theme) = lines.next() {
        profile.dark = theme == "dark";
    }
    if let Some(voice) = lines.next().and_then(|line| line.parse().ok()) {
        profile.voice = gain_percent(voice);
    }
    if let Some(screen) = lines.next().and_then(|line| line.parse().ok()) {
        profile.screen = gain_percent(screen);
    }
    if let Some(input) = lines.next() {
        profile.input = input.to_string();
    }
    if let Some(output) = lines.next() {
        profile.output = output.to_string();
    }
    if let Some(noise) = lines.next() {
        profile.noise = noise != "0";
    }
    profile
}

fn gain_percent(value: u32) -> u32 {
    value.min(200)
}

fn save_profile(body: &ProfileBody) {
    let Some(path) = profile_path() else {
        return;
    };
    let resolution = body.resolution.min(2);
    let fps = if body.fps == 30 { 30 } else { 60 };
    let bitrate = if matches!(body.bitrate_kbps, 8000 | 12000 | 20000 | 40000 | 80000 | 100000) {
        body.bitrate_kbps
    } else {
        40_000
    };
    let theme = if body.dark { "dark" } else { "light" };
    let voice = gain_percent(body.voice);
    let screen = gain_percent(body.screen);
    let noise = if body.noise { "1" } else { "0" };
    let text = format!(
        "{}\n{}\n{}\n{resolution}\n{fps}\n{bitrate}\n{theme}\n{voice}\n{screen}\n{}\n{}\n{noise}\n",
        body.name.trim(),
        body.room.trim(),
        body.invite.trim(),
        body.input.trim(),
        body.output.trim()
    );
    let _ = std::fs::write(path, text);
}
