//! Music Notch - a tiny, notch-style "now playing" widget for Windows.

#![windows_subsystem = "windows"]

mod anim;
mod audio;
mod config;
mod fullscreen;
mod gfx;
mod media;
mod notch;
mod settings;
mod volume;

use std::cell::RefCell;

use windows::{
    core::*,
    Win32::Foundation::*,
    Win32::Graphics::Gdi::{BeginPaint, EndPaint, ScreenToClient, PAINTSTRUCT},
    Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED},
    Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW},
    Win32::System::ProcessStatus::K32EmptyWorkingSet,
    Win32::System::Threading::{CreateMutexW, GetCurrentProcess},
    Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2},
    Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture},
    Win32::UI::Shell::*,
    Win32::UI::WindowsAndMessaging::*,
};

use config::{autostart, Config};
use gfx::Gfx;
use media::Media;
use notch::{Hud, Notch};
use settings::{Action, SettingsWin, Sl};
use volume::Volume;

const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_APP_MEDIA: u32 = WM_APP + 1;
const WM_APP_TRAY: u32 = WM_APP + 2;
const WM_APP_FOREGROUND: u32 = WM_APP + 3;
const T_TOPMOST: usize = 2;
const T_SNAPSHOT: usize = 4;
/// Quick re-checks after the active window changes (games go fullscreen a moment later).
const T_FSCHECK: usize = 5;

struct App {
    cfg: Config,
    gfx: Gfx,
    notch: Notch,
    settings: Option<SettingsWin>,
    media: Media,
    host: HWND,
    hinst: HINSTANCE,
    icon: HICON,
    taskbar_created: u32,
    snapshot: Option<String>,
    volume: Volume,
    fs_checks: u32,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` with the app state. Returns `None` if the state is already borrowed
/// (i.e. Windows re-entered our window procedure from inside another handler).
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|c| match c.try_borrow_mut() {
        Ok(mut g) => g.as_mut().map(f),
        Err(_) => None,
    })
}

fn xy(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xffff) as i16 as i32, ((lp.0 >> 16) & 0xffff) as i16 as i32)
}

fn main() {
    let snapshot = {
        let args: Vec<String> = std::env::args().collect();
        args.iter().position(|a| a == "--snapshot").and_then(|i| args.get(i + 1).cloned())
    };

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // Single instance.
        let _mutex = CreateMutexW(None, true, w!("Local\\MusicNotch.SingleInstance"));
        if GetLastError() == ERROR_ALREADY_EXISTS
            && snapshot.is_none()
            && std::env::var_os("MUSIC_NOTCH_VOLPROBE").is_none()
        {
            return;
        }
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        enable_dark_menus();

        let hinst: HINSTANCE = GetModuleHandleW(None).map(|h| h.into()).unwrap_or_default();
        let icon = load_icon(hinst, 32);
        let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();

        let register = |name: PCWSTR, proc_: WNDPROC| {
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: proc_,
                hInstance: hinst,
                hIcon: icon,
                hIconSm: load_icon(hinst, 16),
                hCursor: cursor,
                lpszClassName: name,
                ..Default::default()
            };
            RegisterClassExW(&wc);
        };
        register(w!("MusicNotchHost"), Some(host_proc));
        register(w!("MusicNotchWindow"), Some(notch_proc));
        register(settings::CLASS, Some(settings_proc));

        let Ok(host) = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("MusicNotchHost"),
            w!("Music Notch"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst),
            None,
        ) else {
            return;
        };
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("MusicNotchWindow"),
            w!("Music Notch"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(hinst),
            None,
        ) else {
            return;
        };

        let Ok(gfx) = Gfx::new() else { return };
        let cfg = Config::load();
        if snapshot.is_none() {
            autostart::repair();
        }
        let media = media::start(hwnd, WM_APP_MEDIA);
        let mut notch = Notch::new(hwnd);
        notch.reposition(&cfg);
        notch.update_targets(&cfg);
        notch.render(&gfx, &cfg);
        notch.sync_shown();
        SetTimer(Some(hwnd), T_TOPMOST, 1000, None);
        fullscreen::watch_foreground(hwnd, WM_APP_FOREGROUND);

        let taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated"));
        if snapshot.is_some() {
            SetTimer(Some(hwnd), T_SNAPSHOT, 2500, None);
        }

        APP.with(|c| {
            *c.borrow_mut() = Some(App {
                cfg,
                gfx,
                notch,
                settings: None,
                media,
                host,
                hinst,
                icon,
                taskbar_created,
                snapshot,
                volume: Volume::new(),
                fs_checks: 0,
            })
        });
        with_app(|a| {
            if a.snapshot.is_none() {
                tray(a, NIM_ADD)
            }
        });
        if std::env::args().any(|a| a == "--settings") {
            with_app(open_settings);
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        with_app(|a| {
            if a.snapshot.is_none() {
                tray(a, NIM_DELETE)
            }
        });
        APP.with(|c| c.borrow_mut().take());
    }
}

unsafe fn load_icon(hinst: HINSTANCE, size: i32) -> HICON {
    // Resource id 1 (MAKEINTRESOURCE). LR_SHARED: Windows owns the handle, so
    // reloading the icon (e.g. after Explorer restarts) never leaks.
    let id = PCWSTR(std::ptr::without_provenance(1));
    LoadImageW(Some(hinst), id, IMAGE_ICON, size, size, LR_DEFAULTCOLOR | LR_SHARED)
        .map(|h| HICON(h.0))
        .unwrap_or_default()
}

/// Opt in to dark context menus (undocumented uxtheme ordinals, safe no-op if missing).
unsafe fn enable_dark_menus() {
    let Ok(ux) = LoadLibraryW(w!("uxtheme.dll")) else { return };
    if let Some(f) = GetProcAddress(ux, PCSTR(135 as *const u8)) {
        let set_mode: extern "system" fn(i32) -> i32 = std::mem::transmute(f);
        set_mode(1); // AllowDark
    }
    if let Some(f) = GetProcAddress(ux, PCSTR(136 as *const u8)) {
        let flush: extern "system" fn() = std::mem::transmute(f);
        flush();
    }
}

unsafe fn tray(a: &App, op: NOTIFY_ICON_MESSAGE) {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: a.host,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_APP_TRAY,
        hIcon: load_icon(a.hinst, GetSystemMetrics(SM_CXSMICON)),
        ..Default::default()
    };
    if nid.hIcon.is_invalid() {
        nid.hIcon = a.icon;
    }
    for (d, s) in nid.szTip.iter_mut().zip("Music Notch".encode_utf16()) {
        *d = s;
    }
    let _ = Shell_NotifyIconW(op, &nid);
}

fn open_settings(a: &mut App) {
    if let Some(s) = &a.settings {
        s.focus();
        return;
    }
    a.settings = SettingsWin::open(&a.cfg, a.hinst);
}

/// Applies a change coming from the settings window.
fn apply(a: &mut App, act: Action) {
    match act {
        Action::None => return,
        Action::Slider(sl, v, save) => {
            match sl {
                Sl::X => a.cfg.x_off = v,
                Sl::Y => a.cfg.y_off = v,
                Sl::Scale => a.cfg.scale = v,
                Sl::Opacity => a.cfg.opacity = v,
            }
            if save {
                a.cfg.save();
            }
        }
        Action::Anchor(i) => {
            if a.cfg.anchor != i {
                a.cfg.anchor = i;
                a.cfg.x_off = 0; // Left/Right sit flush in the corner
                a.cfg.save();
            }
        }
        Action::View(i) => {
            a.cfg.idle_view = i;
            a.cfg.save();
        }
        Action::Timeline(i) => {
            a.cfg.wave_timeline = i == 0;
            a.cfg.save();
        }
        Action::Toggle(i) => {
            match i {
                0 => a.cfg.expand_on_hover ^= true,
                1 => a.cfg.hide_when_idle ^= true,
                2 => a.cfg.album_tint ^= true,
                3 => a.cfg.animate_bars ^= true,
                4 => a.cfg.locked ^= true,
                5 => a.cfg.scroll_volume ^= true,
                6 => a.cfg.hide_fullscreen ^= true,
                _ => {
                    autostart::set(!autostart::enabled());
                    if let Some(s) = a.settings.as_mut() {
                        s.startup = autostart::enabled();
                    }
                }
            }
            a.cfg.save();
            a.notch.retint(&a.cfg);
        }
        Action::Snap => {
            a.cfg.anchor = 1;
            a.cfg.x_off = 0;
            a.cfg.y_off = 0;
            a.cfg.save();
        }
        Action::Monitor(d) => {
            let mons = gfx::monitors();
            if !mons.is_empty() {
                let cur = gfx::pick_monitor(&a.cfg);
                let i = mons.iter().position(|m| m.name == cur.name).unwrap_or(0) as i32;
                let n = (i + d).rem_euclid(mons.len() as i32) as usize;
                a.cfg.monitor = mons[n].name.clone();
                a.cfg.x_off = 0;
                a.cfg.y_off = a.cfg.y_off.min(mons[n].height() - 60);
                a.cfg.save();
            }
            if let Some(s) = a.settings.as_mut() {
                s.mons = mons;
            }
        }
        Action::Quit => {
            unsafe { PostQuitMessage(0) };
            return;
        }
        Action::OpenAuthor => {
            unsafe {
                let url = HSTRING::from(settings::AUTHOR_URL);
                ShellExecuteW(None, w!("open"), &url, None, None, SW_SHOWNORMAL);
            }
            return;
        }
    }
    refresh_notch(a);
}

/// Hides the notch while a fullscreen app covers its monitor.
fn check_fullscreen(a: &mut App) {
    let on = a.cfg.hide_fullscreen && fullscreen::active_on(&gfx::pick_monitor(&a.cfg).rect);
    a.notch.set_suppressed(on, &a.gfx, &a.cfg);
}

fn refresh_notch(a: &mut App) {
    a.notch.reposition(&a.cfg);
    a.notch.update_targets(&a.cfg);
    a.notch.render(&a.gfx, &a.cfg);
    if let Some(s) = &a.settings {
        s.invalidate();
    }
}

unsafe fn show_menu(host: HWND) {
    let Some((locked, hidden)) = with_app(|a| (a.cfg.locked, a.notch.hidden)) else { return };
    let Ok(m) = CreatePopupMenu() else { return };
    let _ = AppendMenuW(m, MF_STRING, 1, w!("Settings…"));
    let _ = AppendMenuW(m, MF_STRING, 2, w!("Snap to top center"));
    let _ = AppendMenuW(m, MF_STRING | if locked { MF_CHECKED } else { MF_UNCHECKED }, 3, w!("Lock position"));
    let _ = AppendMenuW(m, MF_STRING, 4, if hidden { w!("Show notch") } else { w!("Hide notch") });
    let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
    let _ = AppendMenuW(m, MF_STRING, 9, w!("Quit"));
    let mut p = POINT::default();
    let _ = GetCursorPos(&mut p);
    let _ = SetForegroundWindow(host);
    // Run the menu outside of any app borrow so animations keep ticking.
    let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON, p.x, p.y, None, host, None).0;
    let _ = PostMessageW(Some(host), WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(m);
    with_app(|a| match cmd {
        1 => open_settings(a),
        2 => apply(a, Action::Snap),
        3 => {
            a.cfg.locked ^= true;
            a.cfg.save();
            if let Some(s) = &a.settings {
                s.invalidate();
            }
        }
        4 => {
            a.notch.hidden ^= true;
            refresh_notch(a);
            a.notch.sync_shown();
        }
        9 => PostQuitMessage(0),
        _ => {}
    });
}

unsafe extern "system" fn host_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_APP_TRAY {
        match (lp.0 & 0xffff) as u32 {
            WM_LBUTTONUP => {
                with_app(open_settings);
            }
            WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
            _ => {}
        }
        return LRESULT(0);
    }
    // Explorer restarted - put the tray icon back.
    if msg != 0 && with_app(|a| a.taskbar_created == msg) == Some(true) {
        with_app(|a| tray(a, NIM_ADD));
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

unsafe extern "system" fn notch_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_SETCURSOR => {
            let over = with_app(|a| a.notch.over_button()).unwrap_or(false);
            let c = LoadCursorW(None, if over { IDC_HAND } else { IDC_ARROW }).ok();
            SetCursor(c);
            return LRESULT(1);
        }
        WM_TIMER => {
            with_app(|a| match wp.0 {
                notch::T_ANIM => a.notch.tick(&a.gfx, &a.cfg),
                notch::T_TRIM => {
                    let _ = KillTimer(Some(hwnd), notch::T_TRIM);
                    if a.notch.is_idle() && a.settings.is_none() {
                        // Hand unused pages back to Windows while we sit still.
                        let _ = K32EmptyWorkingSet(GetCurrentProcess());
                    }
                }
                T_TOPMOST => {
                    // Never push above a fullscreen game: check first.
                    check_fullscreen(a);
                    if a.notch.is_shown() {
                        let _ = SetWindowPos(
                            hwnd,
                            Some(HWND_TOPMOST),
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                        );
                    }
                }
                T_SNAPSHOT => snapshot(a),
                T_FSCHECK => {
                    check_fullscreen(a);
                    a.fs_checks = a.fs_checks.saturating_sub(1);
                    if a.fs_checks == 0 {
                        let _ = KillTimer(Some(hwnd), T_FSCHECK);
                    }
                }
                _ => {}
            });
            return LRESULT(0);
        }
        WM_APP_FOREGROUND => {
            with_app(|a| {
                check_fullscreen(a);
                // Re-check every 250 ms for 3 s while the new window settles.
                a.fs_checks = 12;
                SetTimer(Some(hwnd), T_FSCHECK, 250, None);
            });
            return LRESULT(0);
        }
        WM_APP_MEDIA => {
            let done = with_app(|a| {
                let t = a.media.shared.lock().map(|t| t.clone()).unwrap_or_default();
                // Dev check: which app's volume would scrolling control? (read-only)
                if t.active && std::env::var_os("MUSIC_NOTCH_VOLPROBE").is_some() {
                    let r = a.volume.nudge(&t.app, 0.0);
                    let line = format!(
                        "app={} result={:?}
",
                        t.app, r
                    );
                    let _ = std::fs::write(std::env::temp_dir().join("music-notch-volprobe.txt"), line);
                    PostQuitMessage(0);
                }
                a.notch.on_media(&a.gfx, &a.cfg, t);
            });
            if done.is_none() {
                let _ = PostMessageW(Some(hwnd), msg, wp, lp); // busy - try again shortly
            }
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            let (x, y) = xy(lp);
            with_app(|a| {
                if a.notch.drag_move(&mut a.cfg) {
                    refresh_notch(a);
                } else {
                    a.notch.mouse_move(x, y, &a.cfg);
                }
            });
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            with_app(|a| a.notch.mouse_leave());
            return LRESULT(0);
        }
        WM_LBUTTONDOWN => {
            let (x, y) = xy(lp);
            SetCapture(hwnd);
            with_app(|a| a.notch.lbutton_down(x, y, &a.cfg));
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            let (x, y) = xy(lp);
            let _ = ReleaseCapture();
            with_app(|a| {
                let (cmd, dragged) = a.notch.lbutton_up(x, y, &a.cfg);
                if let Some(c) = cmd {
                    a.media.command(c);
                }
                if dragged {
                    a.cfg.save();
                }
                a.notch.kick();
            });
            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            // Scroll over the notch: change the playing app's volume.
            let delta = ((wp.0 >> 16) & 0xffff) as i16 as i32;
            let (sx, sy) = xy(lp); // screen coordinates for wheel messages
            let handled = with_app(|a| {
                if !a.cfg.scroll_volume || !a.notch.track.active || !a.notch.hit_screen(&a.cfg, sx, sy) {
                    return false;
                }
                let step = delta as f32 / WHEEL_DELTA as f32 * 0.04; // 4% per notch
                if let Some((level, _)) = a.volume.nudge(&a.notch.track.app, step) {
                    a.notch.show_hud(Hud::Volume(level));
                }
                true
            });
            if handled == Some(true) {
                return LRESULT(0);
            }
        }
        WM_RBUTTONUP => {
            if let Some(host) = with_app(|a| a.host) {
                let _ = PostMessageW(Some(host), WM_APP_TRAY, WPARAM(0), LPARAM(WM_RBUTTONUP as isize));
            }
            return LRESULT(0);
        }
        WM_DPICHANGED | WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
            if with_app(refresh_notch).is_none() {
                let _ = PostMessageW(Some(hwnd), WM_DISPLAYCHANGE, WPARAM(0), LPARAM(0));
            }
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

unsafe extern "system" fn settings_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(hwnd, &mut ps);
            with_app(|a| {
                if let Some(s) = a.settings.as_mut() {
                    s.paint(&a.gfx, &a.cfg);
                }
            });
            let _ = EndPaint(hwnd, &ps);
            return LRESULT(0);
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_SIZE => {
            with_app(|a| a.settings.as_mut().map(|s| s.resize()));
            return LRESULT(0);
        }
        WM_TIMER => {
            with_app(|a| a.settings.as_mut().map(|s| s.tick()));
            return LRESULT(0);
        }
        WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP => {
            let (x, y) = xy(lp);
            if msg == WM_LBUTTONDOWN {
                SetCapture(hwnd);
            } else if msg == WM_LBUTTONUP {
                let _ = ReleaseCapture();
            }
            with_app(|a| {
                let Some(s) = a.settings.as_mut() else { return };
                let act = match msg {
                    WM_MOUSEMOVE => s.on_move(x, y, &a.cfg),
                    WM_LBUTTONDOWN => s.on_down(x, y, &a.cfg),
                    _ => s.on_up(x, y, &a.cfg),
                };
                apply(a, act);
            });
            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xffff) as i16 as i32;
            let (x, y) = xy(lp);
            let mut p = POINT { x, y };
            let _ = ScreenToClient(hwnd, &mut p);
            with_app(|a| {
                let Some(s) = a.settings.as_mut() else { return };
                let _ = s.on_move(p.x, p.y, &a.cfg);
                let act = s.on_wheel(delta, &a.cfg);
                apply(a, act);
            });
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            with_app(|a| a.settings.as_mut().map(|s| s.on_leave()));
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            let r = *(lp.0 as *const RECT);
            with_app(|a| a.settings.as_mut().map(|s| s.dpi_changed(&r)));
            return LRESULT(0);
        }
        WM_DESTROY => {
            with_app(|a| {
                a.settings = None; // frees the render target, fonts and brushes
                SetTimer(Some(a.notch.hwnd), notch::T_TRIM, 3000, None);
            });
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

/// `--snapshot <dir>`: renders the notch (collapsed + expanded) and the
/// settings window to PNGs, then exits. Used for README screenshots.
fn snapshot(a: &mut App) {
    unsafe {
        let _ = KillTimer(Some(a.notch.hwnd), T_SNAPSHOT);
    }
    let Some(dir) = a.snapshot.clone() else { return };
    let _ = std::fs::create_dir_all(&dir);
    let demo = std::env::args().any(|x| x == "--demo");
    // Demo screenshots use default settings rather than the user's own (never saved;
    // the app quits right after).
    if demo {
        a.cfg = Config::default();
        a.notch.reposition(&a.cfg);
        a.notch.demo(&a.gfx, &a.cfg);
    }
    let saved_view = a.cfg.idle_view;
    for (hover, view, name) in
        [(false, 0, "notch-collapsed"), (true, 0, "notch-expanded"), (false, 1, "notch-visualizer")]
    {
        a.cfg.idle_view = view; // not saved - screenshot only
        a.notch.skip_name_intro();
        a.notch.force_hover = Some(hover);
        a.notch.update_targets(&a.cfg);
        a.notch.settle();
        a.notch.render(&a.gfx, &a.cfg);
        if !hover {
            a.notch.eq_frames(&a.gfx, &a.cfg, 6);
        }
        a.notch.save_png(&a.gfx, &format!("{dir}\\{name}.png"));
    }
    // Volume overlay (shown while scrolling over the notch).
    a.cfg.idle_view = 0;
    a.notch.force_hover = Some(false);
    a.notch.update_targets(&a.cfg);
    a.notch.show_hud(Hud::Volume(0.64));
    a.notch.settle();
    a.notch.render(&a.gfx, &a.cfg);
    a.notch.save_png(&a.gfx, &format!("{dir}\\notch-volume.png"));
    // Feedback after clicking a non-expanding notch to pause.
    a.notch.show_hud(Hud::Play(false));
    a.notch.settle();
    a.notch.render(&a.gfx, &a.cfg);
    a.notch.save_png(&a.gfx, &format!("{dir}\\notch-paused.png"));
    a.notch.clear_hud();
    a.cfg.idle_view = saved_view;
    // Demo screenshots show default options rather than the user's own settings.
    let shown_cfg = if demo { Config::default() } else { a.cfg.clone() };
    let mut s = SettingsWin::headless(&shown_cfg);
    if demo {
        s.set_startup(false);
    }
    let _ = s.save_png(&a.gfx, &shown_cfg, &format!("{dir}\\settings.png"));
    unsafe { PostQuitMessage(0) };
}
