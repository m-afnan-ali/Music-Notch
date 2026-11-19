//! Detects fullscreen games and videos, so the notch can step aside.
//!
//! An always-on-top window that keeps re-asserting itself above an exclusive
//! or borderless fullscreen game knocks the game out of its fullscreen mode
//! over and over, which can hang it. While something is fullscreen on the
//! notch's monitor, the notch hides completely and stops re-asserting itself.

use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::{
    Foundation::{HWND, LPARAM, RECT, WPARAM},
    UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK},
    UI::Shell::{SHQueryUserNotificationState, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN},
    UI::WindowsAndMessaging::*,
};

/// Window that gets `msg` posted whenever the foreground window changes.
static TARGET: AtomicIsize = AtomicIsize::new(0);
static MSG: AtomicIsize = AtomicIsize::new(0);

unsafe extern "system" fn on_foreground(
    _hook: HWINEVENTHOOK,
    _event: u32,
    _hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    let target = TARGET.load(Ordering::Relaxed);
    if target != 0 {
        let _ = unsafe {
            PostMessageW(Some(HWND(target as *mut _)), MSG.load(Ordering::Relaxed) as u32, WPARAM(0), LPARAM(0))
        };
    }
}

/// Asks Windows to tell `hwnd` (via `msg`) whenever the active window changes.
pub fn watch_foreground(hwnd: HWND, msg: u32) {
    TARGET.store(hwnd.0 as isize, Ordering::Relaxed);
    MSG.store(msg as isize, Ordering::Relaxed);
    unsafe {
        let _ = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(on_foreground),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
    }
}

/// True when a fullscreen app (game, video, presentation) covers `monitor`.
pub fn active_on(monitor: &RECT) -> bool {
    unsafe {
        // Exclusive-mode Direct3D games and presentation mode: always step aside.
        if let Ok(state) = SHQueryUserNotificationState() {
            if state == QUNS_RUNNING_D3D_FULL_SCREEN || state == QUNS_PRESENTATION_MODE {
                return true;
            }
        }
        // Borderless fullscreen: the active window covers our whole monitor.
        let fg = GetForegroundWindow();
        if fg.is_invalid() || IsZoomed(fg).as_bool() || is_shell(fg) {
            return false; // maximized apps and the desktop don't count
        }
        let mut r = RECT::default();
        if GetWindowRect(fg, &mut r).is_err() {
            return false;
        }
        r.left <= monitor.left && r.top <= monitor.top && r.right >= monitor.right && r.bottom >= monitor.bottom
    }
}

fn is_shell(hwnd: HWND) -> bool {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) } as usize;
    let class = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
    matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
}
