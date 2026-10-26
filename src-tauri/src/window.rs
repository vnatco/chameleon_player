//! Window shaping for the cover-first design.
//!
//! The player window is transparent and undecorated: the cover plus a glow
//! margin. Windows can't do per-pixel click-through for a webview, so a small
//! thread polls the cursor (~60 Hz) and turns click-through on whenever the
//! pointer is outside the interactive rectangles the UI reports (the cover,
//! or the whole library frame). It also tells the UI when the pointer enters
//! or leaves, because a click-through window gets no mouse events at all.
//!
//! Moves and resizes go through one `SetWindowPos` call so position and size
//! change in the same frame (two separate calls visibly jump).

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, Default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

#[derive(Default)]
struct Hit {
    /// Interactive areas in logical px relative to the window's top-left.
    rects: Vec<Rect>,
    ignoring: bool,
    inside: bool,
}

#[derive(Clone, Default)]
pub struct HitTest(Arc<Mutex<Hit>>);

impl HitTest {
    pub fn set_rects(&self, rects: Vec<Rect>) {
        self.0.lock().rects = rects;
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PointerInside {
    pub inside: bool,
}

/// Start the click-through poller for `window`.
pub fn spawn_hit_test(app: &AppHandle, window: WebviewWindow) -> HitTest {
    let hit = HitTest::default();
    let shared = hit.0.clone();
    let app = app.clone();
    let spawned = std::thread::Builder::new().name("hit-test".into()).spawn(move || loop {
        std::thread::sleep(Duration::from_millis(16));
        let Some(local) = cursor_in_window(&window) else { continue };
        let mut h = shared.lock();
        let over = h.rects.iter().any(|r| r.contains(local.0, local.1));
        // Keep input while a button is held, so dragging a slider or the
        // window past the cover's edge doesn't drop the drag.
        let inside = over || (!h.ignoring && mouse_button_down());
        let want_ignore = !inside;
        if want_ignore != h.ignoring {
            match window.set_ignore_cursor_events(want_ignore) {
                Ok(()) => h.ignoring = want_ignore,
                Err(e) => log::warn!("click-through toggle failed: {e}"),
            }
        }
        if inside != h.inside {
            h.inside = inside;
            let _ = app.emit("pointer-inside", PointerInside { inside });
        }
    });
    if let Err(e) = spawned {
        log::error!("can't start hit-test thread ({e}); the glow margin will block clicks");
    }
    hit
}

/// Cursor position in logical px relative to the window's top-left, or None
/// if the window is minimized or hidden.
#[cfg(windows)]
fn cursor_in_window(window: &WebviewWindow) -> Option<(f64, f64)> {
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetWindowRect, IsIconic, IsWindowVisible};
    let hwnd = HWND(window.hwnd().ok()?.0 as _);
    unsafe {
        if IsIconic(hwnd).as_bool() || !IsWindowVisible(hwnd).as_bool() {
            return None;
        }
        let mut p = POINT::default();
        GetCursorPos(&mut p).ok()?;
        let mut r = RECT::default();
        GetWindowRect(hwnd, &mut r).ok()?;
        let scale = window.scale_factor().ok()?;
        Some(((p.x - r.left) as f64 / scale, (p.y - r.top) as f64 / scale))
    }
}

#[cfg(windows)]
fn mouse_button_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0 }
}

#[cfg(not(windows))]
fn cursor_in_window(_: &WebviewWindow) -> Option<(f64, f64)> {
    None
}

#[cfg(not(windows))]
fn mouse_button_down() -> bool {
    false
}

// ---- Commands --------------------------------------------------------------

type Res<T> = Result<T, String>;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Frame {
    /// Outer rect in logical screen px.
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub scale: f64,
    /// Work area (screen minus taskbar) of the monitor the window is on.
    pub work: Rect,
    pub maximized: bool,
    /// Minimized windows are parked at -32000, -32000 at 160 x 28; that rect
    /// must never be saved as the window's place.
    pub minimized: bool,
}

#[tauri::command]
pub fn window_hit(hit: tauri::State<'_, HitTest>, rects: Vec<Rect>) {
    hit.set_rects(rects);
}

#[tauri::command]
pub fn window_frame(window: WebviewWindow) -> Res<Frame> {
    frame(&window)
}

fn frame(window: &WebviewWindow) -> Res<Frame> {
    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    let pos = window.outer_position().map_err(|e| e.to_string())?;
    let size = window.outer_size().map_err(|e| e.to_string())?;
    let work = match window.current_monitor().map_err(|e| e.to_string())? {
        Some(m) => {
            let a = m.work_area();
            Rect { x: a.position.x as f64 / scale, y: a.position.y as f64 / scale, w: a.size.width as f64 / scale, h: a.size.height as f64 / scale }
        }
        None => Rect { x: 0.0, y: 0.0, w: 1920.0, h: 1080.0 },
    };
    Ok(Frame {
        x: pos.x as f64 / scale,
        y: pos.y as f64 / scale,
        w: size.width as f64 / scale,
        h: size.height as f64 / scale,
        scale,
        work,
        maximized: window.is_maximized().unwrap_or(false),
        minimized: window.is_minimized().unwrap_or(false),
    })
}

/// Bumped by every move, so a newer move or glide cancels a running glide.
static MOVE_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Move and resize in one step (logical screen px).
#[tauri::command]
pub fn window_set_frame(window: WebviewWindow, x: f64, y: f64, w: f64, h: f64) -> Res<Frame> {
    MOVE_GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER};
        let hwnd = HWND(window.hwnd().map_err(|e| e.to_string())?.0 as _);
        let r = |v: f64| (v * scale).round() as i32;
        unsafe { SetWindowPos(hwnd, None, r(x), r(y), r(w), r(h), SWP_NOZORDER | SWP_NOACTIVATE) }.map_err(|e| format!("Can't move window: {e}"))?;
    }
    #[cfg(not(windows))]
    {
        window.set_position(tauri::LogicalPosition::new(x, y)).map_err(|e| e.to_string())?;
        window.set_size(tauri::LogicalSize::new(w, h)).map_err(|e| e.to_string())?;
    }
    frame(&window)
}

/// Slide the window (size unchanged) to `x`, `y` (logical screen px) over
/// `ms`, one step per DWM frame, eased like the UI's `--ease-out`.
///
/// Moving the window together with its content never shows a stale frame,
/// unlike moving it while the content shifts the other way in the same step:
/// the screen briefly shows the moved window with the old content (the cover
/// flicks sideways). The library morph uses this to bring a library that
/// opened past the screen edge back onto the screen.
#[tauri::command]
pub fn window_glide(window: WebviewWindow, x: f64, y: f64, ms: u64) -> Res<()> {
    use std::sync::atomic::Ordering;
    let gen = MOVE_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    let from = window.outer_position().map_err(|e| e.to_string())?;
    let (x0, y0) = (from.x as f64, from.y as f64);
    let (x1, y1) = (x * scale, y * scale);
    #[cfg(windows)]
    let hwnd = window.hwnd().map_err(|e| e.to_string())?.0 as isize;
    std::thread::Builder::new()
        .name("window-glide".into())
        .spawn(move || {
            let start = std::time::Instant::now();
            let total = ms.max(1) as f64;
            loop {
                if MOVE_GEN.load(Ordering::SeqCst) != gen {
                    return;
                }
                let t = (start.elapsed().as_secs_f64() * 1000.0 / total).min(1.0);
                let e = ease_out(t);
                let (px, py) = ((x0 + (x1 - x0) * e).round() as i32, (y0 + (y1 - y0) * e).round() as i32);
                #[cfg(windows)]
                {
                    use windows::Win32::Foundation::HWND;
                    use windows::Win32::UI::WindowsAndMessaging::{SetWindowPos, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER};
                    // Async: never block on the UI thread, which is busy with the morph.
                    if let Err(e) = unsafe { SetWindowPos(HWND(hwnd as _), None, px, py, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS) } {
                        log::warn!("window glide step failed: {e}");
                        return;
                    }
                }
                #[cfg(not(windows))]
                let _ = (px, py);
                if t >= 1.0 {
                    return;
                }
                wait_frame();
            }
        })
        .map_err(|e| format!("Can't move window: {e}"))?;
    Ok(())
}

/// Block until the next DWM composition (one display frame).
fn wait_frame() {
    #[cfg(windows)]
    if unsafe { windows::Win32::Graphics::Dwm::DwmFlush() }.is_ok() {
        return;
    }
    std::thread::sleep(Duration::from_millis(8));
}

/// cubic-bezier(0.2, 0.8, 0.2, 1), the UI's `--ease-out`.
fn ease_out(t: f64) -> f64 {
    let (x1, y1, x2, y2) = (0.2, 0.8, 0.2, 1.0);
    let bez = |a: f64, b: f64, s: f64| 3.0 * a * s * (1.0 - s).powi(2) + 3.0 * b * s * s * (1.0 - s) + s.powi(3);
    // Solve x(s) = t by bisection; x is monotonic for these control points.
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..40 {
        let mid = (lo + hi) / 2.0;
        if bez(x1, x2, mid) < t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    bez(y1, y2, (lo + hi) / 2.0)
}

/// Switch between the square player (transparent, square corners, fixed
/// size) and the library (rounded corners, Mica, resizable).
#[tauri::command]
pub fn window_chrome(window: WebviewWindow, library: bool, mica: bool) -> Res<()> {
    window.set_resizable(library).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DWMWCP_ROUND, DWM_WINDOW_CORNER_PREFERENCE};
        let hwnd = HWND(window.hwnd().map_err(|e| e.to_string())?.0 as _);
        let pref: DWM_WINDOW_CORNER_PREFERENCE = if library { DWMWCP_ROUND } else { DWMWCP_DONOTROUND };
        // Fails harmlessly on Windows 10, which has no rounded corners.
        let _ = unsafe { DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &pref as *const _ as _, std::mem::size_of_val(&pref) as u32) };
    }
    let effects = if library && mica {
        Some(tauri::utils::config::WindowEffectsConfig {
            effects: vec![tauri::window::Effect::Mica],
            ..Default::default()
        })
    } else {
        None
    };
    window.set_effects(effects).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn window_set_on_top(window: WebviewWindow, on: bool) -> Res<()> {
    window.set_always_on_top(on).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn window_toggle_maximize(window: WebviewWindow) -> Res<Frame> {
    if window.is_maximized().unwrap_or(false) {
        window.unmaximize().map_err(|e| e.to_string())?;
    } else {
        window.maximize().map_err(|e| e.to_string())?;
    }
    frame(&window)
}

#[tauri::command]
pub fn window_show(app: AppHandle) -> Res<()> {
    let w = app.get_webview_window("main").ok_or("No main window")?;
    w.show().map_err(|e| e.to_string())?;
    w.set_focus().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::ease_out;

    #[test]
    fn ease_out_matches_the_css_curve() {
        assert!(ease_out(0.0).abs() < 1e-6);
        assert!((ease_out(1.0) - 1.0).abs() < 1e-6);
        // Rises steeply, then settles: well past halfway at a quarter of the time.
        assert!(ease_out(0.25) > 0.6);
        let mut last = 0.0;
        for i in 1..=100 {
            let v = ease_out(i as f64 / 100.0);
            assert!(v >= last, "not monotonic at {i}");
            last = v;
        }
    }
}
