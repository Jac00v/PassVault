//! Screen-region QR scanning for TOTP setup codes.
//!
//! The flow is intentionally split so that overlay webviews never receive a
//! decoded secret. Rust captures a frozen frame per monitor, the overlay only
//! displays that frame and reports a selection, and decoding happens here.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Mutex,
    },
    time::{Duration, Instant},
};

use percent_encoding::percent_decode_str;
use serde::Serialize;
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use unicode_general_category::{get_general_category, GeneralCategory};
use url::Url;
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

const OVERLAY_PREFIX: &str = "qr-overlay-";
const MAIN_WINDOW_LABEL: &str = "main";
const WAIT_AFTER_HIDE: Duration = Duration::from_millis(200);
const READY_TIMEOUT: Duration = Duration::from_secs(10);
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const HARD_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_PAYLOAD_BYTES: usize = 4 * 1024;
// Bound decode memory/time; a QR code only needs a small selection, and this
// still allows a near-full 4K monitor selection.
const MAX_REGION_AREA: u64 = 12_000_000;
const MAX_DECODE_DIMENSION: u32 = 7680;
const MAX_SECRET_BYTES: usize = 128;
const MAX_LABEL_PART_CHARS: usize = 64;
const MAX_DECODE_CANDIDATES: usize = 8;
const NO_QR_MESSAGE: &str = "No QR code found in that area — try a tighter box";
const GENERIC_UNSUPPORTED_MESSAGE: &str = "That QR code isn't a TOTP setup code.";

/// A decoded TOTP setup code. `value` is the only secret-bearing field and is
/// what the composer stores in `totpSecret`.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScannedOtp {
    value: String,
    label: Option<String>,
}

impl Drop for ScannedOtp {
    fn drop(&mut self) {
        self.value.zeroize();
        if let Some(label) = &mut self.label {
            label.zeroize();
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanRegionResult {
    ok: bool,
    label: Option<String>,
    message: Option<String>,
}

impl ScanRegionResult {
    fn success(label: Option<String>) -> Self {
        Self {
            ok: true,
            label,
            message: None,
        }
    }

    fn failure(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            label: None,
            message: Some(message.into()),
        }
    }
}

struct MonitorFrame {
    width: u32,
    height: u32,
    origin_x: i32,
    origin_y: i32,
    scale_factor: f32,
    pixels: Zeroizing<Vec<u8>>,
}

struct ScanSession {
    token: Uuid,
    frames: HashMap<String, MonitorFrame>,
    overlays: Vec<WebviewWindow>,
    sender: Option<mpsc::Sender<Option<ScannedOtp>>>,
    pending: Option<ScannedOtp>,
    decoding: bool,
    ready: HashSet<String>,
    focused: bool,
    started: Instant,
    last_activity: Instant,
}

#[derive(Default)]
pub struct QrScanState {
    session: Mutex<Option<ScanSession>>,
    active: AtomicBool,
}

impl QrScanState {
    fn begin(&self, sender: mpsc::Sender<Option<ScannedOtp>>) -> Result<Uuid, String> {
        if self.active.swap(true, Ordering::SeqCst) {
            return Err("A screen scan is already in progress".to_string());
        }

        let mut guard = match self.session.lock() {
            Ok(guard) => guard,
            Err(_) => {
                self.active.store(false, Ordering::SeqCst);
                return Err(unavailable_message().to_string());
            }
        };
        if guard.is_some() {
            self.active.store(false, Ordering::SeqCst);
            return Err("A screen scan is already in progress".to_string());
        }
        let token = Uuid::new_v4();
        let now = Instant::now();
        *guard = Some(ScanSession {
            token,
            frames: HashMap::new(),
            overlays: Vec::new(),
            sender: Some(sender),
            pending: None,
            decoding: false,
            ready: HashSet::new(),
            focused: false,
            started: now,
            last_activity: now,
        });
        Ok(token)
    }

    fn take_session(&self, token: Uuid) -> Option<ScanSession> {
        // Recover from a poisoned mutex so cleanup can still restore the main
        // window if a command panicked while holding the session lock.
        let mut guard = self
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match guard.as_ref() {
            Some(session) if session.token == token => guard.take(),
            _ => None,
        }
    }
}

fn unavailable_message() -> &'static str {
    "The screen scan is unavailable right now"
}

/// Restores the main window and destroys overlay windows on every exit path,
/// including `?` returns and panics.
struct ScanCleanup {
    app: AppHandle,
    token: Uuid,
    done: bool,
}

impl ScanCleanup {
    fn finish(&mut self) {
        if self.done {
            return;
        }
        self.done = true;

        let state = self.app.state::<QrScanState>();
        let session = state.take_session(self.token);
        let Some(session) = session else {
            state.active.store(false, Ordering::SeqCst);
            return;
        };

        // Dropping the sender disconnects the orchestrator, and the remaining
        // session fields (including every captured frame) are dropped when
        // this function returns.
        let overlays = session.overlays;
        let app = self.app.clone();
        let (done_tx, done_rx) = mpsc::channel();
        if self
            .app
            .run_on_main_thread(move || {
                if let Some(main) = app.get_webview_window(MAIN_WINDOW_LABEL) {
                    let _ = main.show();
                    let _ = main.set_focus();
                }
                for overlay in &overlays {
                    let _ = overlay.destroy();
                }
                let _ = done_tx.send(());
            })
            .is_ok()
        {
            let _ = done_rx.recv_timeout(Duration::from_secs(3));
        }
        state.active.store(false, Ordering::SeqCst);
    }
}

impl Drop for ScanCleanup {
    fn drop(&mut self) {
        self.finish();
    }
}

/// Hide the vault window, freeze every monitor, show one overlay per monitor,
/// and wait for a decoded setup code or a cancellation.
fn run_scan(app: AppHandle) -> Result<Option<ScannedOtp>, String> {
    let state = app.state::<QrScanState>();
    let (result_tx, result_rx) = mpsc::channel();
    let token = state.begin(result_tx)?;
    let mut cleanup = ScanCleanup {
        app: app.clone(),
        token,
        done: false,
    };

    let main = app
        .get_webview_window(MAIN_WINDOW_LABEL)
        .ok_or_else(|| "The vault window is not available".to_string())?;
    main.hide()
        .map_err(|_| "Could not start the screen scan".to_string())?;
    std::thread::sleep(WAIT_AFTER_HIDE);

    let captures = capture_monitors(token)?;
    {
        let mut guard = state
            .session
            .lock()
            .map_err(|_| unavailable_message().to_string())?;
        let session = guard
            .as_mut()
            .filter(|session| session.token == token)
            .ok_or_else(|| "The screen scan was canceled".to_string())?;
        for (label, frame) in captures {
            session.frames.insert(label, frame);
        }
    }

    create_overlays(&app, token)?;
    if let Ok(mut guard) = state.session.lock() {
        if let Some(session) = guard.as_mut().filter(|session| session.token == token) {
            session.started = Instant::now();
            session.last_activity = Instant::now();
        }
    }
    let outcome = wait_for_result(&app, token, &result_rx);
    cleanup.finish();
    outcome
}

fn force_opaque(pixels: &mut [u8]) {
    for alpha in pixels.iter_mut().skip(3).step_by(4) {
        *alpha = 255;
    }
}

fn capture_monitors(token: Uuid) -> Result<Vec<(String, MonitorFrame)>, String> {
    let monitors = xcap::Monitor::all().map_err(|_| "Could not read the displays".to_string())?;
    if monitors.is_empty() {
        return Err("No displays were found".to_string());
    }

    let mut frames = Vec::with_capacity(monitors.len());
    for (index, monitor) in monitors.iter().enumerate() {
        let image = monitor
            .capture_image()
            .map_err(|_| "Could not capture the screen".to_string())?;
        let width = image.width();
        let height = image.height();
        if width == 0
            || height == 0
            || width > MAX_DECODE_DIMENSION
            || height > MAX_DECODE_DIMENSION
        {
            return Err("A display returned an unexpected size".to_string());
        }
        let mut pixels = image.into_raw();
        if pixels.len() != width as usize * height as usize * 4 {
            return Err("A display returned unexpected image data".to_string());
        }
        // GDI's 32-bit capture path leaves the alpha byte undefined (usually 0),
        // and canvas `putImageData` honors alpha verbatim. Force the frame
        // opaque so the overlay shows the captured desktop instead of black.
        force_opaque(&mut pixels);
        let origin_x = monitor
            .x()
            .map_err(|_| "Could not read the display layout".to_string())?;
        let origin_y = monitor
            .y()
            .map_err(|_| "Could not read the display layout".to_string())?;
        let scale_factor = monitor.scale_factor().unwrap_or(1.0).max(0.5);

        frames.push((
            overlay_label(token, index),
            MonitorFrame {
                width,
                height,
                origin_x,
                origin_y,
                scale_factor,
                pixels: Zeroizing::new(pixels),
            },
        ));
    }

    Ok(frames)
}

/// Overlay labels include the session token so a window left over from a
/// previous session can never match the frames of a newer session.
fn overlay_label(token: Uuid, index: usize) -> String {
    format!("{OVERLAY_PREFIX}{}-{index}", token.simple())
}

fn create_overlays(app: &AppHandle, token: Uuid) -> Result<(), String> {
    let specs = {
        let state = app.state::<QrScanState>();
        let guard = state
            .session
            .lock()
            .map_err(|_| unavailable_message().to_string())?;
        let session = guard
            .as_ref()
            .filter(|session| session.token == token)
            .ok_or_else(|| "The screen scan was canceled".to_string())?;
        session
            .frames
            .iter()
            .map(|(label, frame)| {
                (
                    label.clone(),
                    frame.origin_x,
                    frame.origin_y,
                    frame.width,
                    frame.height,
                    frame.scale_factor,
                )
            })
            .collect::<Vec<_>>()
    };

    let app_for_thread = app.clone();
    let (done_tx, done_rx) = mpsc::channel();
    app.run_on_main_thread(move || {
        let result = (|| -> Result<(), String> {
            for (label, origin_x, origin_y, width, height, scale_factor) in specs {
                let builder = WebviewWindowBuilder::new(
                    &app_for_thread,
                    label.clone(),
                    WebviewUrl::App("qr.html".into()),
                )
                .title("Passvault screen scan")
                .decorations(false)
                .always_on_top(true)
                .skip_taskbar(true)
                .shadow(false)
                .resizable(false)
                .visible(false)
                .focused(false)
                .background_color(tauri::utils::config::Color(0, 0, 0, 255))
                .background_throttling(tauri::utils::config::BackgroundThrottlingPolicy::Disabled)
                .inner_size(
                    f64::from(width) / f64::from(scale_factor),
                    f64::from(height) / f64::from(scale_factor),
                );

                let window = builder
                    .build()
                    .map_err(|_| "Could not open the scan overlay".to_string())?;
                if window
                    .set_position(tauri::PhysicalPosition::new(origin_x, origin_y))
                    .is_err()
                    || window
                        .set_size(tauri::PhysicalSize::new(width, height))
                        .is_err()
                {
                    let _ = window.destroy();
                    return Err("Could not position the scan overlay".to_string());
                }

                let event_app = app_for_thread.clone();
                let event_label = label.clone();
                window.on_window_event(move |event| {
                    if matches!(
                        event,
                        tauri::WindowEvent::CloseRequested { .. } | tauri::WindowEvent::Destroyed
                    ) {
                        cancel_from_overlay(&event_app, &event_label, token);
                    }
                });

                let state = app_for_thread.state::<QrScanState>();
                let mut registered = false;
                if let Ok(mut guard) = state.session.lock() {
                    if let Some(session) = guard.as_mut().filter(|session| session.token == token) {
                        session.overlays.push(window.clone());
                        registered = true;
                    }
                }
                if !registered {
                    let _ = window.destroy();
                    return Err("The screen scan was canceled".to_string());
                }
            }
            Ok(())
        })();
        let _ = done_tx.send(result);
    })
    .map_err(|_| "Could not open the scan overlay".to_string())?;

    done_rx
        .recv_timeout(Duration::from_secs(15))
        .map_err(|_| "Could not open the scan overlay".to_string())?
}

fn wait_for_result(
    app: &AppHandle,
    token: Uuid,
    result_rx: &mpsc::Receiver<Option<ScannedOtp>>,
) -> Result<Option<ScannedOtp>, String> {
    loop {
        match result_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(result) => return Ok(result),
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(None),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let state = app.state::<QrScanState>();
                let guard = state
                    .session
                    .lock()
                    .map_err(|_| unavailable_message().to_string())?;
                let Some(session) = guard.as_ref().filter(|session| session.token == token) else {
                    return Ok(None);
                };
                if session.ready.is_empty() && session.started.elapsed() >= READY_TIMEOUT {
                    return Err("The screen scan did not start in time".to_string());
                }
                if session.last_activity.elapsed() >= IDLE_TIMEOUT
                    || session.started.elapsed() >= HARD_TIMEOUT
                {
                    return Ok(None);
                }
            }
        }
    }
}

fn cancel_from_overlay(app: &AppHandle, label: &str, token: Uuid) {
    if !label.starts_with(OVERLAY_PREFIX) {
        return;
    }
    let state = app.state::<QrScanState>();
    let Ok(mut guard) = state.session.lock() else {
        return;
    };
    let Some(session) = guard.as_mut() else {
        return;
    };
    if session.token != token || !session.frames.contains_key(label) {
        return;
    }
    if let Some(sender) = session.sender.take() {
        let _ = sender.send(None);
    }
}

fn require_overlay<'a>(session: &'a ScanSession, label: &str) -> Result<&'a MonitorFrame, String> {
    if !label.starts_with(OVERLAY_PREFIX) {
        return Err(unavailable_message().to_string());
    }
    session
        .frames
        .get(label)
        .ok_or_else(|| unavailable_message().to_string())
}

#[tauri::command]
pub async fn scan_qr_from_screen(
    app: AppHandle,
    window: WebviewWindow,
) -> Result<Option<ScannedOtp>, String> {
    if window.label() != MAIN_WINDOW_LABEL {
        return Err("Screen scanning is only available from the vault window".to_string());
    }

    tauri::async_runtime::spawn_blocking(move || run_scan(app))
        .await
        .map_err(|_| "The screen scan could not be completed".to_string())?
}

#[tauri::command]
pub fn qr_overlay_frame(
    window: WebviewWindow,
    state: State<'_, QrScanState>,
) -> Result<tauri::ipc::Response, String> {
    let label = window.label();
    let guard = state
        .session
        .lock()
        .map_err(|_| unavailable_message().to_string())?;
    let session = guard
        .as_ref()
        .ok_or_else(|| "The screen scan is no longer active".to_string())?;
    let frame = require_overlay(session, label)?;

    let mut payload = Vec::with_capacity(8 + frame.pixels.len());
    payload.extend_from_slice(&frame.width.to_le_bytes());
    payload.extend_from_slice(&frame.height.to_le_bytes());
    payload.extend_from_slice(&frame.pixels);
    Ok(tauri::ipc::Response::new(payload))
}

#[tauri::command]
pub fn qr_overlay_ready(
    window: WebviewWindow,
    app: AppHandle,
    state: State<'_, QrScanState>,
) -> Result<(), String> {
    let label = window.label().to_string();
    let focus = {
        let mut guard = state
            .session
            .lock()
            .map_err(|_| unavailable_message().to_string())?;
        let session = guard
            .as_mut()
            .ok_or_else(|| "The screen scan is no longer active".to_string())?;
        let cursor = app.cursor_position().ok();
        let under_cursor = {
            let frame = require_overlay(session, &label)?;
            cursor
                .map(|position| frame_contains(frame, position.x, position.y))
                .unwrap_or(false)
        };
        session.ready.insert(label.clone());
        let already_focused = session.focused;
        session.focused = already_focused || under_cursor;
        under_cursor || !already_focused
    };

    window
        .show()
        .map_err(|_| "Could not show the scan overlay".to_string())?;
    if focus {
        let _ = window.set_focus();
    }
    Ok(())
}

fn frame_contains(frame: &MonitorFrame, x: f64, y: f64) -> bool {
    x >= f64::from(frame.origin_x)
        && y >= f64::from(frame.origin_y)
        && x < f64::from(frame.origin_x) + f64::from(frame.width)
        && y < f64::from(frame.origin_y) + f64::from(frame.height)
}

#[tauri::command]
pub fn qr_overlay_activity(
    window: WebviewWindow,
    state: State<'_, QrScanState>,
) -> Result<(), String> {
    let label = window.label();
    let mut guard = state
        .session
        .lock()
        .map_err(|_| unavailable_message().to_string())?;
    let session = guard
        .as_mut()
        .ok_or_else(|| "The screen scan is no longer active".to_string())?;
    require_overlay(session, label)?;
    session.last_activity = Instant::now();
    Ok(())
}

#[tauri::command]
pub async fn qr_scan_region(
    window: WebviewWindow,
    state: State<'_, QrScanState>,
    x: i64,
    y: i64,
    width: i64,
    height: i64,
) -> Result<ScanRegionResult, String> {
    let label = window.label().to_string();
    let (token, crop) = {
        let mut guard = state
            .session
            .lock()
            .map_err(|_| unavailable_message().to_string())?;
        let session = guard
            .as_mut()
            .ok_or_else(|| "The screen scan is no longer active".to_string())?;
        if session.decoding {
            return Ok(ScanRegionResult::failure(
                "Still reading the last selection",
            ));
        }
        let token = session.token;
        let crop_result = {
            let frame = require_overlay(session, &label)?;
            crop_luma(frame, x, y, width, height)
        };
        session.decoding = true;
        session.pending = None;
        session.last_activity = Instant::now();
        match crop_result {
            Ok(crop) => (token, crop),
            Err(message) => {
                session.decoding = false;
                return Ok(ScanRegionResult::failure(message));
            }
        }
    };

    let decoded = match tauri::async_runtime::spawn_blocking(move || decode_and_parse(&crop)).await
    {
        Ok(result) => result,
        Err(_) => {
            if let Ok(mut guard) = state.session.lock() {
                if let Some(session) = guard.as_mut().filter(|session| session.token == token) {
                    session.decoding = false;
                }
            }
            return Ok(ScanRegionResult::failure("The QR code could not be read"));
        }
    };

    let mut guard = state
        .session
        .lock()
        .map_err(|_| unavailable_message().to_string())?;
    let Some(session) = guard.as_mut().filter(|session| session.token == token) else {
        return Ok(ScanRegionResult::failure("The screen scan ended"));
    };
    session.decoding = false;
    session.last_activity = Instant::now();
    if !session.frames.contains_key(&label) {
        return Ok(ScanRegionResult::failure("The screen scan ended"));
    }

    match decoded {
        Ok(otp) => {
            let label = otp.label.clone();
            session.pending = Some(otp);
            Ok(ScanRegionResult::success(label))
        }
        Err(message) => Ok(ScanRegionResult::failure(message)),
    }
}

#[tauri::command]
pub fn qr_scan_submit(window: WebviewWindow, state: State<'_, QrScanState>) -> Result<(), String> {
    let label = window.label();
    let mut guard = state
        .session
        .lock()
        .map_err(|_| unavailable_message().to_string())?;
    let session = guard
        .as_mut()
        .ok_or_else(|| "The screen scan is no longer active".to_string())?;
    require_overlay(session, label)?;
    session.last_activity = Instant::now();

    let Some(result) = session.pending.take() else {
        return Err("Scan the QR code before adding it".to_string());
    };
    let Some(sender) = session.sender.take() else {
        return Err("The screen scan is no longer active".to_string());
    };
    let _ = sender.send(Some(result));
    Ok(())
}

#[tauri::command]
pub fn qr_scan_cancel(window: WebviewWindow, state: State<'_, QrScanState>) -> Result<(), String> {
    let label = window.label();
    let mut guard = state
        .session
        .lock()
        .map_err(|_| unavailable_message().to_string())?;
    let session = guard
        .as_mut()
        .ok_or_else(|| "The screen scan is no longer active".to_string())?;
    require_overlay(session, label)?;
    if let Some(sender) = session.sender.take() {
        let _ = sender.send(None);
    }
    Ok(())
}

struct GrayCrop {
    width: usize,
    height: usize,
    luma: Zeroizing<Vec<u8>>,
}

fn crop_luma(
    frame: &MonitorFrame,
    x: i64,
    y: i64,
    width: i64,
    height: i64,
) -> Result<GrayCrop, String> {
    if width <= 0 || height <= 0 {
        return Err("That selection is too small".to_string());
    }
    let area = (width as u64)
        .checked_mul(height as u64)
        .ok_or_else(|| "That selection is too large".to_string())?;
    if area > MAX_REGION_AREA {
        return Err("That selection is too large".to_string());
    }
    let Ok(x) = usize::try_from(x) else {
        return Err("That selection is outside the screen".to_string());
    };
    let Ok(y) = usize::try_from(y) else {
        return Err("That selection is outside the screen".to_string());
    };
    let Ok(width) = usize::try_from(width) else {
        return Err("That selection is outside the screen".to_string());
    };
    let Ok(height) = usize::try_from(height) else {
        return Err("That selection is outside the screen".to_string());
    };
    let right = x
        .checked_add(width)
        .ok_or_else(|| "That selection is outside the screen".to_string())?;
    let bottom = y
        .checked_add(height)
        .ok_or_else(|| "That selection is outside the screen".to_string())?;
    let frame_width = frame.width as usize;
    let frame_height = frame.height as usize;
    if right > frame_width || bottom > frame_height {
        return Err("That selection is outside the screen".to_string());
    }

    let mut luma = Zeroizing::new(vec![0u8; width * height]);
    for row in 0..height {
        let source_row = (y + row) * frame_width;
        let target_row = row * width;
        for column in 0..width {
            let offset = (source_row + x + column) * 4;
            let r = u32::from(frame.pixels[offset]);
            let g = u32::from(frame.pixels[offset + 1]);
            let b = u32::from(frame.pixels[offset + 2]);
            luma[target_row + column] = ((77 * r + 150 * g + 29 * b + 128) >> 8) as u8;
        }
    }

    Ok(GrayCrop {
        width,
        height,
        luma,
    })
}

fn decode_and_parse(image: &GrayCrop) -> Result<ScannedOtp, String> {
    let candidates = decode_qr_candidates(image);
    if candidates.is_empty() {
        return Err(NO_QR_MESSAGE.to_string());
    }

    let mut first_error = None;
    for candidate in &candidates {
        match parse_scanned_otp(candidate.as_str()) {
            Ok(otp) => return Ok(otp),
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }

    Err(first_error
        .unwrap_or(PayloadError::Unsupported)
        .message()
        .to_string())
}

fn decode_qr_candidates(image: &GrayCrop) -> Vec<Zeroizing<String>> {
    let mut candidates = Vec::new();
    collect_grids(image.width, image.height, &image.luma, &mut candidates);

    let inverted = Zeroizing::new(
        image
            .luma
            .iter()
            .map(|value| 255 - value)
            .collect::<Vec<u8>>(),
    );
    collect_grids(image.width, image.height, &inverted, &mut candidates);

    if let Some(threshold) = otsu_threshold(&image.luma) {
        let thresholded = Zeroizing::new(
            image
                .luma
                .iter()
                .map(|value| if *value > threshold { 255 } else { 0 })
                .collect::<Vec<u8>>(),
        );
        collect_grids(image.width, image.height, &thresholded, &mut candidates);

        if image.width <= 640 && image.height <= 640 {
            let (width, height, upscaled) = upscale(&thresholded, image.width, image.height, 2);
            collect_grids(width, height, &upscaled, &mut candidates);
        }
    }

    if image.width <= 640 && image.height <= 640 {
        let (width, height, upscaled) = upscale(&image.luma, image.width, image.height, 2);
        collect_grids(width, height, &upscaled, &mut candidates);
        let (width, height, upscaled) = upscale(&inverted, image.width, image.height, 2);
        collect_grids(width, height, &upscaled, &mut candidates);

        if image.width.max(image.height) <= 320 {
            let (width, height, upscaled) = upscale(&image.luma, image.width, image.height, 3);
            collect_grids(width, height, &upscaled, &mut candidates);
        }
    }

    candidates
}

fn collect_grids(
    width: usize,
    height: usize,
    pixels: &[u8],
    candidates: &mut Vec<Zeroizing<String>>,
) {
    if width == 0 || height == 0 || candidates.len() >= MAX_DECODE_CANDIDATES {
        return;
    }
    let mut prepared =
        rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| pixels[y * width + x]);
    for grid in prepared.detect_grids() {
        if let Ok((_metadata, content)) = grid.decode() {
            let content = Zeroizing::new(content);
            if !candidates
                .iter()
                .any(|existing| existing.as_str() == content.as_str())
            {
                candidates.push(content);
            }
            if candidates.len() >= MAX_DECODE_CANDIDATES {
                return;
            }
        }
    }
}

fn otsu_threshold(pixels: &[u8]) -> Option<u8> {
    let mut histogram = [0_u32; 256];
    for pixel in pixels {
        histogram[*pixel as usize] += 1;
    }
    let total = pixels.len() as f64;
    let total_sum: f64 = (0..256)
        .map(|index| index as f64 * f64::from(histogram[index]))
        .sum();
    let mut background_weight = 0_u32;
    let mut background_sum = 0.0;
    let mut best = 0.0;
    let mut threshold = 0_u8;
    let mut found = false;

    for (index, count) in histogram.iter().enumerate() {
        background_weight += *count;
        if background_weight == 0 {
            continue;
        }
        let foreground_weight = total - f64::from(background_weight);
        if foreground_weight <= 0.0 {
            break;
        }
        background_sum += index as f64 * f64::from(*count);
        let background_mean = background_sum / f64::from(background_weight);
        let foreground_mean = (total_sum - background_sum) / foreground_weight;
        let between = f64::from(background_weight)
            * foreground_weight
            * (background_mean - foreground_mean).powi(2);
        if between > best {
            best = between;
            threshold = index as u8;
            found = true;
        }
    }

    if found {
        Some(threshold)
    } else {
        None
    }
}

fn upscale(
    pixels: &[u8],
    width: usize,
    height: usize,
    factor: usize,
) -> (usize, usize, Zeroizing<Vec<u8>>) {
    let new_width = width * factor;
    let new_height = height * factor;
    let mut output = Zeroizing::new(vec![0u8; new_width * new_height]);
    for row in 0..new_height {
        let source_row = (row / factor) * width;
        for column in 0..new_width {
            output[row * new_width + column] = pixels[source_row + column / factor];
        }
    }
    (new_width, new_height, output)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PayloadError {
    Hotp,
    Migration,
    Unsupported,
}

impl PayloadError {
    fn message(self) -> &'static str {
        match self {
            Self::Hotp => "HOTP codes aren't supported",
            Self::Migration => {
                "This is a Google Authenticator export, not a single setup code. Add the key manually or import a CSV."
            }
            Self::Unsupported => GENERIC_UNSUPPORTED_MESSAGE,
        }
    }
}

fn parse_scanned_otp(payload: &str) -> Result<ScannedOtp, PayloadError> {
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(PayloadError::Unsupported);
    }
    if payload.chars().any(char::is_control) {
        return Err(PayloadError::Unsupported);
    }

    let payload = payload.trim();
    if payload.is_empty() {
        return Err(PayloadError::Unsupported);
    }

    if let Ok(url) = Url::parse(payload) {
        if url.scheme().eq_ignore_ascii_case("otpauth-migration") {
            return Err(PayloadError::Migration);
        }
        if url.scheme().eq_ignore_ascii_case("otpauth") {
            return match url.host_str() {
                Some(host) if host.eq_ignore_ascii_case("totp") => parse_otpauth_totp(&url),
                Some(host) if host.eq_ignore_ascii_case("hotp") => Err(PayloadError::Hotp),
                _ => Err(PayloadError::Unsupported),
            };
        }
    }
    if payload.contains("://") {
        return Err(PayloadError::Unsupported);
    }

    let secret = normalize_base32_strict(payload).ok_or(PayloadError::Unsupported)?;
    validate_storable(secret.as_str())?;
    Ok(ScannedOtp {
        value: secret.to_string(),
        label: None,
    })
}

fn parse_otpauth_totp(uri: &Url) -> Result<ScannedOtp, PayloadError> {
    if uri.port().is_some() || !uri.username().is_empty() || uri.password().is_some() {
        return Err(PayloadError::Unsupported);
    }

    let mut secret: Option<Zeroizing<String>> = None;
    let mut issuer: Option<String> = None;
    let mut algorithm = "SHA1".to_string();
    let mut digits = 6_u8;
    let mut period = 30_u64;
    let mut saw_algorithm = false;
    let mut saw_digits = false;
    let mut saw_period = false;

    for (key, value) in uri.query_pairs() {
        match key.to_ascii_lowercase().as_str() {
            "secret" => {
                if secret.is_some() || value.is_empty() {
                    return Err(PayloadError::Unsupported);
                }
                secret = Some(Zeroizing::new(value.into_owned()));
            }
            "algorithm" => {
                if saw_algorithm {
                    return Err(PayloadError::Unsupported);
                }
                saw_algorithm = true;
                algorithm = match value.to_ascii_uppercase().as_str() {
                    "SHA1" => "SHA1".to_string(),
                    "SHA256" => "SHA256".to_string(),
                    "SHA512" => "SHA512".to_string(),
                    _ => return Err(PayloadError::Unsupported),
                };
            }
            "digits" => {
                if saw_digits {
                    return Err(PayloadError::Unsupported);
                }
                saw_digits = true;
                digits = value.parse::<u8>().map_err(|_| PayloadError::Unsupported)?;
            }
            "period" => {
                if saw_period {
                    return Err(PayloadError::Unsupported);
                }
                saw_period = true;
                period = value
                    .parse::<u64>()
                    .map_err(|_| PayloadError::Unsupported)?;
            }
            "issuer" => {
                if issuer.is_some() {
                    return Err(PayloadError::Unsupported);
                }
                if !value.is_empty() {
                    issuer = Some(value.into_owned());
                }
            }
            _ => {}
        }
    }

    if !(1..=9).contains(&digits) || period == 0 || period > u64::from(u16::MAX) {
        return Err(PayloadError::Unsupported);
    }

    let secret = normalize_base32_strict(secret.as_deref().ok_or(PayloadError::Unsupported)?)
        .ok_or(PayloadError::Unsupported)?;

    let path_label = percent_decode_str(uri.path().trim_start_matches('/'))
        .decode_utf8()
        .map_err(|_| PayloadError::Unsupported)?
        .into_owned();
    let (path_issuer, account) = split_path_label(&path_label, issuer.as_deref());

    let issuer = clean_label(issuer.or(path_issuer));
    let account = clean_label(account);
    let account = account.filter(|account| {
        issuer
            .as_deref()
            .map(|issuer| !issuer.eq_ignore_ascii_case(account))
            .unwrap_or(true)
    });
    let label = build_label(issuer.as_deref(), account.as_deref());

    let default_setup = algorithm == "SHA1" && digits == 6 && period == 30;
    let value = if default_setup {
        validate_storable(secret.as_str())?;
        secret.to_string()
    } else {
        let uri = build_canonical_uri(
            issuer.as_deref(),
            account.as_deref(),
            secret.as_str(),
            &algorithm,
            digits,
            period,
        )
        .ok_or(PayloadError::Unsupported)?;
        validate_storable(&uri)?;
        uri
    };

    Ok(ScannedOtp { value, label })
}

fn split_path_label(path_label: &str, issuer: Option<&str>) -> (Option<String>, Option<String>) {
    let trimmed = path_label.trim();
    if trimmed.is_empty() {
        return (None, None);
    }

    let (left, right) = match trimmed.split_once(':') {
        Some((left, right)) => (left.trim(), Some(right.trim())),
        None => (trimmed, None),
    };
    let account = right.unwrap_or(left);
    let account = if account.is_empty() {
        None
    } else {
        Some(account.to_string())
    };

    let path_issuer = match issuer {
        Some(issuer) => {
            if left.eq_ignore_ascii_case(issuer) {
                None
            } else if right.is_some() {
                Some(left.to_string())
            } else {
                None
            }
        }
        None if right.is_some() => Some(left.to_string()),
        None => None,
    };

    (path_issuer, account)
}

fn clean_label(value: Option<String>) -> Option<String> {
    let value = value?;
    let mut cleaned = String::with_capacity(value.len());
    let mut pending_space = false;
    for character in value.chars() {
        if matches!(
            get_general_category(character),
            GeneralCategory::Control | GeneralCategory::Format
        ) {
            continue;
        }
        if character.is_whitespace() {
            pending_space = !cleaned.is_empty();
            continue;
        }
        if pending_space {
            cleaned.push(' ');
            pending_space = false;
        }
        cleaned.push(character);
    }
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return None;
    }

    let count = cleaned.chars().count();
    if count > MAX_LABEL_PART_CHARS {
        let mut truncated = cleaned
            .chars()
            .take(MAX_LABEL_PART_CHARS.saturating_sub(1))
            .collect::<String>();
        truncated.push('…');
        Some(truncated)
    } else {
        Some(cleaned.to_string())
    }
}

fn build_label(issuer: Option<&str>, account: Option<&str>) -> Option<String> {
    match (issuer, account) {
        (Some(issuer), Some(account)) => Some(format!("{issuer} · {account}")),
        (Some(issuer), None) => Some(issuer.to_string()),
        (None, Some(account)) => Some(account.to_string()),
        (None, None) => None,
    }
}

fn normalize_base32_strict(value: &str) -> Option<Zeroizing<String>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut normalized = Zeroizing::new(String::with_capacity(trimmed.len()));
    let mut padding = false;
    for character in trimmed.chars() {
        if character.is_whitespace() || character == '-' || character == '_' {
            if padding {
                return None;
            }
            continue;
        }
        if character == '=' {
            padding = true;
            continue;
        }
        if padding {
            return None;
        }
        let upper = character.to_ascii_uppercase();
        if !(upper.is_ascii_uppercase() || ('2'..='7').contains(&upper)) {
            return None;
        }
        normalized.push(upper);
    }

    if normalized.is_empty() {
        return None;
    }
    let decoded = Zeroizing::new(
        data_encoding::BASE32_NOPAD
            .decode(normalized.as_bytes())
            .ok()?,
    );
    if decoded.is_empty() || decoded.len() > MAX_SECRET_BYTES {
        return None;
    }
    Some(normalized)
}

fn build_canonical_uri(
    issuer: Option<&str>,
    account: Option<&str>,
    secret: &str,
    algorithm: &str,
    digits: u8,
    period: u64,
) -> Option<String> {
    let mut uri = Url::parse("otpauth://totp/").ok()?;
    let path_label = match (issuer, account) {
        (Some(issuer), Some(account)) => format!("{issuer}:{account}"),
        (Some(issuer), None) => issuer.to_string(),
        (None, Some(account)) => account.to_string(),
        (None, None) => "Passvault".to_string(),
    };
    uri.set_path(&format!("/{path_label}"));

    let mut query = uri.query_pairs_mut();
    query.append_pair("secret", secret);
    if let Some(issuer) = issuer {
        query.append_pair("issuer", issuer);
    }
    if algorithm != "SHA1" {
        query.append_pair("algorithm", algorithm);
    }
    if digits != 6 {
        query.append_pair("digits", &digits.to_string());
    }
    if period != 30 {
        query.append_pair("period", &period.to_string());
    }
    drop(query);

    Some(uri.into())
}

fn validate_storable(value: &str) -> Result<(), PayloadError> {
    crate::parse_totp(value).map_err(|_| PayloadError::Unsupported)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use qrcode::{Color, QrCode};

    const TEST_SECRET: &str = "JBSWY3DPEHPK3PXP";

    fn qr_image(
        payload: &str,
        module: u32,
        inverted: bool,
        origin: (u32, u32),
        canvas: (u32, u32),
    ) -> (u32, u32, Vec<u8>) {
        let code = QrCode::new(payload.as_bytes()).unwrap();
        let colors = code.to_colors();
        let modules = code.width() as u32;

        let background = if inverted { 0_u8 } else { 255_u8 };
        let dark = if inverted { 255_u8 } else { 0_u8 };
        let mut rgba = vec![0_u8; (canvas.0 * canvas.1 * 4) as usize];
        for offset in (0..rgba.len()).step_by(4) {
            rgba[offset] = background;
            rgba[offset + 1] = background;
            rgba[offset + 2] = background;
            rgba[offset + 3] = 255;
        }

        for my in 0..modules {
            for mx in 0..modules {
                if colors[(my * modules + mx) as usize] != Color::Dark {
                    continue;
                }
                for dy in 0..module {
                    for dx in 0..module {
                        let x = origin.0 + mx * module + dx;
                        let y = origin.1 + my * module + dy;
                        if x >= canvas.0 || y >= canvas.1 {
                            continue;
                        }
                        let offset = ((y * canvas.0 + x) * 4) as usize;
                        rgba[offset] = dark;
                        rgba[offset + 1] = dark;
                        rgba[offset + 2] = dark;
                    }
                }
            }
        }

        (canvas.0, canvas.1, rgba)
    }

    fn decode_all(
        width: u32,
        height: u32,
        rgba: &[u8],
        rect: (i64, i64, i64, i64),
    ) -> Result<ScannedOtp, String> {
        let frame = MonitorFrame {
            width,
            height,
            origin_x: 0,
            origin_y: 0,
            scale_factor: 1.0,
            pixels: Zeroizing::new(rgba.to_vec()),
        };
        let crop = crop_luma(&frame, rect.0, rect.1, rect.2, rect.3)?;
        decode_and_parse(&crop)
    }

    #[test]
    fn captured_frames_are_forced_opaque() {
        let mut pixels = vec![10, 20, 30, 0, 40, 50, 60, 1];
        force_opaque(&mut pixels);
        assert_eq!(pixels, vec![10, 20, 30, 255, 40, 50, 60, 255]);
    }

    #[test]
    fn decodes_default_totp_qr() {
        let payload =
            format!("otpauth://totp/Example:alice@example.com?secret={TEST_SECRET}&issuer=Example");
        let (width, height, rgba) = qr_image(&payload, 7, false, (30, 30), (420, 320));
        let otp = decode_all(
            width,
            height,
            &rgba,
            (0, 0, i64::from(width), i64::from(height)),
        )
        .expect("QR code should decode");

        assert_eq!(otp.value, TEST_SECRET);
        assert_eq!(otp.label.as_deref(), Some("Example · alice@example.com"));
    }

    #[test]
    fn decodes_custom_totp_as_canonical_uri() {
        let payload = format!(
            "otpauth://totp/Example:bob?secret={TEST_SECRET}&issuer=Example&algorithm=SHA256&digits=8&period=60"
        );
        let (width, height, rgba) = qr_image(&payload, 7, false, (30, 30), (420, 320));
        let otp = decode_all(
            width,
            height,
            &rgba,
            (0, 0, i64::from(width), i64::from(height)),
        )
        .expect("QR code should decode");

        assert!(otp.value.starts_with("otpauth://totp/"));
        let parsed = crate::parse_totp(&otp.value).unwrap();
        assert_eq!(parsed.algorithm, totp_rs::Algorithm::SHA256);
        assert_eq!(parsed.digits, 8);
        assert_eq!(parsed.step, 60);
    }

    #[test]
    fn decodes_inverted_totp_qr() {
        let payload = format!("otpauth://totp/Example?secret={TEST_SECRET}&issuer=Example");
        let (width, height, rgba) = qr_image(&payload, 7, true, (30, 30), (420, 320));
        let otp = decode_all(
            width,
            height,
            &rgba,
            (0, 0, i64::from(width), i64::from(height)),
        )
        .expect("inverted QR code should decode");

        assert_eq!(otp.value, TEST_SECRET);
    }

    #[test]
    fn decodes_qr_inside_an_offset_crop() {
        let payload = format!("otpauth://totp/Example?secret={TEST_SECRET}&issuer=Example");
        let (width, height, rgba) = qr_image(&payload, 7, false, (120, 80), (520, 360));
        let otp = decode_all(width, height, &rgba, (100, 60, 250, 250))
            .expect("offset QR code should decode");

        assert_eq!(otp.value, TEST_SECRET);
    }

    #[test]
    fn rejects_invalid_crop_rectangles() {
        let frame = MonitorFrame {
            width: 100,
            height: 100,
            origin_x: 0,
            origin_y: 0,
            scale_factor: 1.0,
            pixels: Zeroizing::new(vec![255; 100 * 100 * 4]),
        };

        assert!(crop_luma(&frame, 0, 0, 0, 10).is_err());
        assert!(crop_luma(&frame, -5, 0, 10, 10).is_err());
        assert!(crop_luma(&frame, 90, 90, 20, 20).is_err());
        assert!(crop_luma(&frame, 0, 0, 5000, 5000).is_err());
        assert!(crop_luma(&frame, 0, 0, i64::MAX, i64::MAX).is_err());
        assert!(crop_luma(&frame, 0, 0, -10, 10).is_err());
        assert!(crop_luma(&frame, 0, 0, 10, 10).is_ok());
    }

    #[test]
    fn parses_plain_base32_secret() {
        let otp = parse_scanned_otp("jbswy3dp ehpk3pxp").unwrap();
        assert_eq!(otp.value, TEST_SECRET);
        assert!(otp.label.is_none());

        let padded = parse_scanned_otp("JBSWY3DPEHPK3PXP====").unwrap();
        assert_eq!(padded.value, TEST_SECRET);
    }

    #[test]
    fn rejects_invalid_base32_secret() {
        assert!(matches!(
            parse_scanned_otp("JBSWY3DPEHPK3PX0"),
            Err(PayloadError::Unsupported)
        ));
        assert!(matches!(
            parse_scanned_otp("JBSWY3DPEHPK3PX8"),
            Err(PayloadError::Unsupported)
        ));
    }

    #[test]
    fn rejects_missing_secret_and_hotp_and_migration() {
        assert!(matches!(
            parse_scanned_otp("otpauth://totp/Example?issuer=Example"),
            Err(PayloadError::Unsupported)
        ));
        assert!(matches!(
            parse_scanned_otp(&format!(
                "otpauth://hotp/Example?secret={TEST_SECRET}&counter=1"
            )),
            Err(PayloadError::Hotp)
        ));
        assert!(matches!(
            parse_scanned_otp("otpauth-migration://offline?data=AAAA"),
            Err(PayloadError::Migration)
        ));
    }

    #[test]
    fn rejects_overlong_and_control_character_payloads() {
        // rqrr decodes into a String and turns non-UTF-8 payloads into a
        // decode error, so that case cannot reach this parser and isn't
        // separately testable here.
        let overlong = "A".repeat(MAX_PAYLOAD_BYTES + 1);
        assert!(matches!(
            parse_scanned_otp(&overlong),
            Err(PayloadError::Unsupported)
        ));
        assert!(matches!(
            parse_scanned_otp("JBSWY3DP\nEHPK3PXP"),
            Err(PayloadError::Unsupported)
        ));
        assert!(matches!(
            parse_scanned_otp("https://example.com"),
            Err(PayloadError::Unsupported)
        ));
    }

    #[test]
    fn sanitizes_bidi_and_control_characters_in_labels() {
        let payload = format!(
            "otpauth://totp/Evil%E2%80%AEExample:alice?secret={TEST_SECRET}&issuer=Evil%E2%80%AEIssuer%01"
        );
        let otp = parse_scanned_otp(&payload).unwrap();
        let label = otp.label.clone().unwrap();

        assert!(!label.contains('\u{202e}'));
        assert!(!label.contains('\u{0001}'));
        assert_eq!(label, "EvilIssuer · alice");
    }

    #[test]
    fn truncates_overlong_label_parts() {
        let long_issuer = "i".repeat(100);
        let payload =
            format!("otpauth://totp/Example:alice?secret={TEST_SECRET}&issuer={long_issuer}");
        let otp = parse_scanned_otp(&payload).unwrap();
        let label = otp.label.clone().unwrap();
        let issuer = label.split(" · ").next().unwrap();

        assert_eq!(issuer.chars().count(), MAX_LABEL_PART_CHARS);
        assert!(issuer.ends_with('…'));
    }

    #[test]
    fn default_setup_is_stored_as_a_bare_secret() {
        let payload = format!(
            "otpauth://totp/Example:alice?secret={TEST_SECRET}&issuer=Example&algorithm=SHA1&digits=6&period=30"
        );
        let otp = parse_scanned_otp(&payload).unwrap();
        assert_eq!(otp.value, TEST_SECRET);
        assert!(crate::parse_totp(&otp.value).is_ok());
    }

    #[test]
    fn custom_setup_is_stored_as_a_canonical_uri() {
        let payload = format!(
            "otpauth://totp/Example:alice?secret={TEST_SECRET}&issuer=Example&algorithm=SHA512&digits=7&period=45"
        );
        let otp = parse_scanned_otp(&payload).unwrap();

        assert!(otp.value.starts_with("otpauth://totp/"));
        let parsed = crate::parse_totp(&otp.value).unwrap();
        assert_eq!(parsed.algorithm, totp_rs::Algorithm::SHA512);
        assert_eq!(parsed.digits, 7);
        assert_eq!(parsed.step, 45);
    }
}
