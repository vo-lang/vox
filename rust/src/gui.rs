//! Guest GUI VM management for native mode.
//!
//! Architecture:
//!
//! ```text
//!   ┌──────────────┐         GuestEvent::Ide         ┌───────────────┐
//!   │  Tauri thread │ ──────────────────────────────→ │  Guest thread │
//!   │               │ ←─── render_tx (sync reply) ──── │  (VM + loop)  │
//!   │ send_gui_event│                                  │               │
//!   └──────────────┘         GuestEvent::Platform    │               │
//!   ┌──────────────┐ ──────────────────────────────→ │               │
//!   │ Timer threads │                                  │               │
//!   │ (game loop,  │         push_tx (async render)  │               │
//!   │  timeout,    │ ←──────────────────────────────── │               │
//!   │  interval)   │                                  └───────────────┘
//!   └──────────────┘
//! ```
//!
//! Two kinds of events feed into the guest thread:
//! - **Ide events**: from `send_gui_event`, request-response. Caller blocks on `render_tx`.
//! - **Platform events**: from timer/game-loop threads, fire-and-forget.
//!   Render output goes to `push_tx` for the IDE to pick up asynchronously.
//!
//! This separation ensures `send_gui_event` always gets back its own render,
//! and platform-driven renders don't get lost or misrouted.

#![cfg(not(target_arch = "wasm32"))]

use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use vo_engine::CompileOutput;
use vo_vm::vm::Vm;

// =============================================================================
// Vogui event IDs (must match vogui/event.vo and vogui/canvas.vo)
// =============================================================================

const EVENT_ID_TIMER: i32 = -1;
const EVENT_ID_ANIM_FRAME: i32 = -4;
const EVENT_ID_GAME_LOOP: i32 = -5;

// =============================================================================
// GuestEvent — tagged union for the unified event channel
// =============================================================================

enum GuestEvent {
    /// Event from IDE (click, key, etc). Guest must reply on `render_tx`.
    Ide { handler_id: i32, payload: String },
    /// Event from platform (timer, game loop, anim frame). Render pushed via `push_tx`.
    Platform { handler_id: i32, payload: String },
    /// Shutdown signal. Guest thread should exit.
    Shutdown,
}

// =============================================================================
// NativeGuiPlatform — per-session timer/game-loop management
// =============================================================================

struct NativeGuiPlatform {
    event_tx: mpsc::Sender<GuestEvent>,
    cancels: Mutex<HashMap<i32, mpsc::Sender<()>>>,
}

impl NativeGuiPlatform {
    fn new(event_tx: mpsc::Sender<GuestEvent>) -> Self {
        Self {
            event_tx,
            cancels: Mutex::new(HashMap::new()),
        }
    }

    fn spawn_oneshot(&self, id: i32, delay: Duration, handler_id: i32, payload: String) {
        let tx = self.event_tx.clone();
        let (cancel_tx, cancel_rx) = mpsc::channel();
        self.cancels.lock().unwrap().insert(id, cancel_tx);

        std::thread::spawn(move || {
            match cancel_rx.recv_timeout(delay) {
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let _ = tx.send(GuestEvent::Platform { handler_id, payload });
                }
                _ => {}
            }
        });
    }

    fn spawn_repeating(&self, id: i32, interval: Duration, handler_id: i32, make_payload: Box<dyn Fn() -> String + Send>) {
        let tx = self.event_tx.clone();
        let (cancel_tx, cancel_rx) = mpsc::channel();
        self.cancels.lock().unwrap().insert(id, cancel_tx);

        std::thread::spawn(move || {
            loop {
                match cancel_rx.recv_timeout(interval) {
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if tx.send(GuestEvent::Platform { handler_id, payload: make_payload() }).is_err() {
                            return;
                        }
                    }
                    _ => return,
                }
            }
        });
    }

    fn cancel(&self, id: i32) {
        if let Some(cancel_tx) = self.cancels.lock().unwrap().remove(&id) {
            let _ = cancel_tx.send(());
        }
    }

}

impl vogui::VoguiPlatform for NativeGuiPlatform {
    fn start_timeout(&self, id: i32, ms: i32) {
        let payload = format!("{{\"Id\":{}}}", id);
        self.spawn_oneshot(id, Duration::from_millis(ms.max(0) as u64), EVENT_ID_TIMER, payload);
    }

    fn clear_timeout(&self, id: i32) {
        self.cancel(id);
    }

    fn start_interval(&self, id: i32, ms: i32) {
        let interval = Duration::from_millis(ms.max(1) as u64);
        let timer_id = id;
        self.spawn_repeating(id, interval, EVENT_ID_TIMER, Box::new(move || {
            format!("{{\"Id\":{}}}", timer_id)
        }));
    }

    fn clear_interval(&self, id: i32) {
        self.cancel(id);
    }

    fn navigate(&self, _path: &str) {}
    fn get_current_path(&self) -> String { "/".to_string() }

    fn start_anim_frame(&self, id: i32) {
        let payload = format!("{{\"Id\":{}}}", id);
        self.spawn_oneshot(id, Duration::from_millis(16), EVENT_ID_ANIM_FRAME, payload);
    }

    fn cancel_anim_frame(&self, id: i32) {
        self.cancel(id);
    }

    fn start_game_loop(&self, id: i32) {
        let tx = self.event_tx.clone();
        let (cancel_tx, cancel_rx) = mpsc::channel();
        self.cancels.lock().unwrap().insert(id, cancel_tx);

        std::thread::spawn(move || {
            let target_frame = Duration::from_micros(16_667); // ~60fps
            let mut last = Instant::now();

            loop {
                let now = Instant::now();
                let elapsed = now.duration_since(last);
                if elapsed < target_frame {
                    match cancel_rx.recv_timeout(target_frame - elapsed) {
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        _ => return,
                    }
                }
                let now = Instant::now();
                let dt_ms = now.duration_since(last).as_secs_f64() * 1000.0;
                last = now;

                let payload = format!("{{\"Dt\":{:.3}}}", dt_ms);
                if tx.send(GuestEvent::Platform {
                    handler_id: EVENT_ID_GAME_LOOP,
                    payload,
                }).is_err() {
                    return;
                }
            }
        });
    }

    fn stop_game_loop(&self, id: i32) {
        self.cancel(id);
    }
}

// =============================================================================
// GuestHandle — public handle held by the IDE layer
// =============================================================================

pub struct GuestHandle {
    event_tx: mpsc::Sender<GuestEvent>,
    /// Synchronous render replies for IDE-originated events.
    render_rx: mpsc::Receiver<Result<Vec<u8>, String>>,
}

impl Drop for GuestHandle {
    fn drop(&mut self) {
        let _ = self.event_tx.send(GuestEvent::Shutdown);
    }
}

impl GuestHandle {
    /// Send an IDE event and wait for the corresponding render.
    pub fn send_event(&self, handler_id: i32, payload: &str) -> Result<Vec<u8>, String> {
        self.event_tx
            .send(GuestEvent::Ide { handler_id, payload: payload.to_string() })
            .map_err(|_| "guest VM stopped".to_string())?;

        self.render_rx
            .recv()
            .map_err(|_| "guest VM stopped".to_string())?
            .map_err(|e| format!("guest event failed: {}", e))
    }
}

/// Receiver for platform-driven render updates (game loop, timers, anim frames).
/// Stores the channel in a Mutex so the type is Sync (required by Tauri AppState).
/// Designed to be stored independently from GuestHandle so polling doesn't
/// contend with the guest handle lock.
pub struct PushReceiver {
    rx: Mutex<mpsc::Receiver<Vec<u8>>>,
}

impl PushReceiver {
    /// Drain all pending renders, returning only the latest one.
    /// Intermediate frames are discarded (natural back-pressure).
    pub fn poll(&self) -> Option<Vec<u8>> {
        let rx = self.rx.lock().unwrap();
        let mut latest = None;
        while let Ok(bytes) = rx.try_recv() {
            if !bytes.is_empty() {
                latest = Some(bytes);
            }
        }
        latest
    }
}

// =============================================================================
// Guest Handle Storage (indexed by module id)
// =============================================================================

static GUEST_HANDLES: Mutex<Vec<Option<GuestHandle>>> = Mutex::new(Vec::new());

pub fn store_guest_handle(handle: GuestHandle) -> i64 {
    let mut handles = GUEST_HANDLES.lock().unwrap();
    for (i, slot) in handles.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(handle);
            return i as i64;
        }
    }
    let id = handles.len();
    handles.push(Some(handle));
    id as i64
}

pub fn with_guest_handle<F, R>(id: i64, f: F) -> Option<R>
where
    F: FnOnce(&mut GuestHandle) -> R,
{
    let mut handles = GUEST_HANDLES.lock().unwrap();
    let idx = id as usize;
    handles.get_mut(idx).and_then(|s| s.as_mut()).map(f)
}

pub fn take_guest_handle(id: i64) -> Option<GuestHandle> {
    let mut handles = GUEST_HANDLES.lock().unwrap();
    let idx = id as usize;
    if idx < handles.len() {
        handles[idx].take()
    } else {
        None
    }
}

// =============================================================================
// Module id -> guest id mapping
// =============================================================================

static MODULE_GUEST_MAP: Mutex<Vec<Option<i64>>> = Mutex::new(Vec::new());

pub fn set_module_guest(module_id: i64, guest_id: i64) {
    let mut map = MODULE_GUEST_MAP.lock().unwrap();
    let idx = module_id as usize;
    while map.len() <= idx {
        map.push(None);
    }
    map[idx] = Some(guest_id);
}

pub fn get_module_guest(module_id: i64) -> Option<i64> {
    let map = MODULE_GUEST_MAP.lock().unwrap();
    let idx = module_id as usize;
    map.get(idx).and_then(|v| *v)
}

pub fn clear_module_guest(module_id: i64) {
    let mut map = MODULE_GUEST_MAP.lock().unwrap();
    let idx = module_id as usize;
    if idx < map.len() {
        map[idx] = None;
    }
}

// =============================================================================
// run_gui: start a guest VM thread, return initial render + handle
// =============================================================================

pub fn run_gui(output: CompileOutput) -> Result<(Vec<u8>, GuestHandle, Arc<PushReceiver>), String> {
    let (event_tx, event_rx) = mpsc::channel::<GuestEvent>();
    let (render_tx, render_rx) = mpsc::sync_channel::<Result<Vec<u8>, String>>(1);
    let (push_tx, push_rx) = mpsc::channel::<Vec<u8>>();

    let platform_tx = event_tx.clone();
    std::thread::spawn(move || {
        run_gui_thread(output, render_tx, push_tx, event_rx, platform_tx);
    });

    // Wait for initial render
    let initial = render_rx
        .recv()
        .map_err(|e| format!("guest thread died: {}", e))?
        .map_err(|e| format!("guest init failed: {}", e))?;

    let handle = GuestHandle { event_tx, render_rx };
    let push = Arc::new(PushReceiver { rx: Mutex::new(push_rx) });
    Ok((initial, handle, push))
}

// =============================================================================
// Guest thread body
// =============================================================================

fn build_gui_vm(output: CompileOutput) -> Result<Vm, String> {
    let ext_loader = if output.extensions.is_empty() {
        None
    } else {
        use vo_runtime::ext_loader::ExtensionLoader;
        let mut loader = ExtensionLoader::new();
        for manifest in &output.extensions {
            loader.load(&manifest.native_path, &manifest.name)
                .map_err(|e| format!("failed to load extension '{}': {}", manifest.name, e))?;
        }
        Some(loader)
    };

    let mut vm = Vm::new();
    vm.load_with_extensions(output.module, ext_loader.as_ref());
    Ok(vm)
}

fn run_gui_thread(
    output: CompileOutput,
    render_tx: mpsc::SyncSender<Result<Vec<u8>, String>>,
    push_tx: mpsc::Sender<Vec<u8>>,
    event_rx: mpsc::Receiver<GuestEvent>,
    platform_tx: mpsc::Sender<GuestEvent>,
) {
    // Install per-session platform so vogui externs route back to this thread.
    vogui::set_platform(Box::new(NativeGuiPlatform::new(platform_tx)));

    let mut vm = match build_gui_vm(output) {
        Ok(vm) => vm,
        Err(e) => {
            let _ = render_tx.send(Err(e));
            return;
        }
    };

    // Run until vogui app blocks on waitForEvent().
    vm.clear_host_output();
    if let Err(e) = vm.run() {
        let _ = render_tx.send(Err(format!("{:?}", e)));
        return;
    }

    let bytes = vm.take_host_output().unwrap_or_default();
    let _ = render_tx.send(Ok(bytes));

    // Main event loop
    while let Ok(event) = event_rx.recv() {
        match event {
            GuestEvent::Shutdown => {
                vogui::clear_platform();
                break;
            }
            GuestEvent::Ide { handler_id, payload } => {
                match dispatch_event(&mut vm, handler_id, &payload) {
                    Ok(bytes) => { let _ = render_tx.send(Ok(bytes)); }
                    Err(e) => {
                        let _ = render_tx.send(Err(e));
                        return;
                    }
                }
            }
            GuestEvent::Platform { handler_id, payload } => {
                match dispatch_event(&mut vm, handler_id, &payload) {
                    Ok(bytes) => {
                        if !bytes.is_empty() {
                            let _ = push_tx.send(bytes);
                        }
                    }
                    Err(e) => {
                        eprintln!("guest VM error on platform event: {}", e);
                        return;
                    }
                }
            }
        }
    }
}

/// Process one event through the VM. Returns render bytes on success.
fn dispatch_event(vm: &mut Vm, handler_id: i32, payload: &str) -> Result<Vec<u8>, String> {
    vm.clear_host_output();
    vo_runtime::output::clear_output();

    let pending = vm.scheduler.take_pending_host_events();
    let token = match pending.first() {
        Some(ev) => ev.token,
        None => return Err("Main fiber not waiting for events".to_string()),
    };

    let mut data = Vec::with_capacity(4 + payload.len());
    data.extend_from_slice(&handler_id.to_le_bytes());
    data.extend_from_slice(payload.as_bytes());
    vm.wake_host_event_with_data(token, data);

    vm.run_scheduled()
        .map_err(|e| format!("{:?}", e))?;

    Ok(vm.take_host_output().unwrap_or_default())
}
