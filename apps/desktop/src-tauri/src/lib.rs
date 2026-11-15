use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

struct Rect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    visible: bool,
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
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShareBody {
    width: u32,
    height: u32,
    fps: u32,
    bitrate_kbps: u32,
}

#[derive(Deserialize)]
struct MuteBody {
    muted: bool,
}

#[derive(Deserialize)]
struct RectBody {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    visible: bool,
}

#[derive(Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum NoteEvent {
    Status { text: String },
    Joined,
    Left,
    Sharing { on: bool },
}

fn map_note(note: native_sender::EngineNote) -> NoteEvent {
    match note {
        native_sender::EngineNote::Status(text) => NoteEvent::Status { text },
        native_sender::EngineNote::Joined => NoteEvent::Joined,
        native_sender::EngineNote::Left => NoteEvent::Left,
        native_sender::EngineNote::Sharing(on) => NoteEvent::Sharing { on },
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
    with_engine(&state, |engine| {
        engine.join(native_sender::CallRequest {
            name: body.name,
            room: body.room,
            invite: body.invite,
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
fn stage_rect(state: State<DesktopState>, body: RectBody) -> Result<(), String> {
    let mut rect = state.rect.lock().map_err(|_| "the stage is unavailable".to_string())?;
    *rect = Rect {
        x: body.x,
        y: body.y,
        width: body.width,
        height: body.height,
        visible: body.visible,
    };
    Ok(())
}

fn share_request(body: ShareBody) -> native_sender::ShareRequest {
    native_sender::ShareRequest {
        width: body.width,
        height: body.height,
        fps: body.fps,
        bitrate_kbps: body.bitrate_kbps,
    }
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

#[cfg(target_os = "macos")]
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
            existing: *mut c_void,
            x: f64,
            y: f64,
            w: f64,
            h: f64,
            visible: i32,
        ) -> *mut c_void;
    }

    let Some(window) = handle.get_webview_window("main") else {
        return;
    };
    let Ok(ns_window) = window.ns_window() else {
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
            visible: rect.visible,
        }
    };
    if rect.width < 2.0 || rect.height < 2.0 {
        return;
    }
    SURFACE.with(|slot| {
        let mut surface = slot.borrow_mut();
        let view = unsafe {
            dcmax_stage_place(
                ns_window.cast(),
                surface.view,
                rect.x,
                rect.y,
                rect.width,
                rect.height,
                i32::from(rect.visible),
            )
        };
        surface.view = view;
        if surface.presenter.is_none() && !view.is_null() {
            match Presenter::attach(view) {
                Ok(presenter) => surface.presenter = Some(presenter),
                Err(err) => eprintln!("picture surface: {err}"),
            }
        }
        if !rect.visible {
            return;
        }
        let Some(presenter) = surface.presenter.as_ref() else {
            return;
        };
        let state = handle.state::<DesktopState>();
        let Ok(engine) = state.engine.lock() else {
            return;
        };
        let Some(engine) = engine.as_ref() else {
            return;
        };
        engine.with_picture(|picture| {
            if let Picture::Nv12 { buffer, full_range } = picture {
                let _ = presenter.draw_nv12(
                    buffer.as_ptr().cast(),
                    *full_range,
                    Pointer {
                        visible: false,
                        x: 0.0,
                        y: 0.0,
                    },
                );
            }
        });
    });
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
                visible: false,
            }),
        })
        .invoke_handler(tauri::generate_handler![
            join,
            leave,
            share,
            stop_share,
            tune,
            set_muted,
            stage_rect
        ])
        .setup(|app| {
            let engine = native_sender::Engine::start().map_err(|err| err.to_string())?;
            *app.state::<DesktopState>()
                .engine
                .lock()
                .map_err(|_| "the call runtime is unavailable".to_string())? = Some(engine);
            let handle = app.handle().clone();
            watch_notes(handle.clone());
            #[cfg(target_os = "macos")]
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
            }
        });
}
