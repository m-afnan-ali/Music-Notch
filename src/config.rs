//! Settings persistence (`%APPDATA%\MusicNotch\settings.ini`) and autostart.

use std::{fs, path::PathBuf};

#[derive(Clone)]
pub struct Config {
    /// 0 = left, 1 = center, 2 = right
    pub anchor: i32,
    /// Horizontal offset in physical pixels (from center, or from the edge).
    pub x_off: i32,
    /// Distance from the top of the screen in physical pixels.
    pub y_off: i32,
    /// Monitor device name, empty = primary.
    pub monitor: String,
    pub scale: i32,
    pub opacity: i32,
    pub expand_on_hover: bool,
    pub hide_when_idle: bool,
    pub album_tint: bool,
    pub animate_bars: bool,
    pub locked: bool,
    /// Collapsed notch shows: 0 = song name, 1 = visualizer.
    pub idle_view: i32,
    /// Expanded timeline: animated waveform (true) or classic bar.
    pub wave_timeline: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            anchor: 1,
            x_off: 0,
            y_off: 0,
            monitor: String::new(),
            scale: 100,
            opacity: 100,
            expand_on_hover: true,
            hide_when_idle: false,
            album_tint: true,
            animate_bars: true,
            locked: false,
            idle_view: 0,
            wave_timeline: true,
        }
    }
}

fn path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("MusicNotch").join("settings.ini"))
}

impl Config {
    pub fn load() -> Self {
        let mut c = Self::default();
        let Some(text) = path().and_then(|p| fs::read_to_string(p).ok()) else { return c };
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            let n = v.parse::<i32>().ok();
            let b = v == "1" || v.eq_ignore_ascii_case("true");
            match (k, n) {
                ("anchor", Some(n)) => c.anchor = n.clamp(0, 2),
                ("x_off", Some(n)) => c.x_off = n,
                ("y_off", Some(n)) => c.y_off = n.max(0),
                ("scale", Some(n)) => c.scale = n.clamp(70, 160),
                ("opacity", Some(n)) => c.opacity = n.clamp(30, 100),
                ("monitor", _) => c.monitor = v.to_string(),
                ("expand_on_hover", _) => c.expand_on_hover = b,
                ("hide_when_idle", _) => c.hide_when_idle = b,
                ("album_tint", _) => c.album_tint = b,
                ("animate_bars", _) => c.animate_bars = b,
                ("locked", _) => c.locked = b,
                ("idle_view", Some(n)) => c.idle_view = n.clamp(0, 1),
                ("wave_timeline", _) => c.wave_timeline = b,
                _ => {}
            }
        }
        c
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let b = |v: bool| if v { 1 } else { 0 };
        let text = format!(
            "anchor={}\nx_off={}\ny_off={}\nmonitor={}\nscale={}\nopacity={}\nexpand_on_hover={}\nhide_when_idle={}\nalbum_tint={}\nanimate_bars={}\nlocked={}\nidle_view={}\nwave_timeline={}\n",
            self.anchor, self.x_off, self.y_off, self.monitor, self.scale, self.opacity,
            b(self.expand_on_hover), b(self.hide_when_idle), b(self.album_tint),
            b(self.animate_bars), b(self.locked), self.idle_view, b(self.wave_timeline)
        );
        let _ = fs::write(p, text);
    }
}

/// "Start with Windows" via the per-user Run key (no admin needed).
///
/// Windows also keeps an approval flag per startup entry (what the Startup
/// Apps page and Task Manager toggle). Recent Windows builds skip Run entries
/// that have no flag at all, so we write one, exactly as Task Manager does.
pub mod autostart {
    use windows::core::{w, HSTRING, PCWSTR};
    use windows::Win32::System::Registry::*;

    const RUN: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    const APPROVED: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run");
    const NAME: PCWSTR = w!("MusicNotch");

    fn has_run_entry() -> bool {
        unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN, NAME, RRF_RT_REG_SZ, None, None, None).is_ok() }
    }

    /// Whether the exe the startup entry launches still exists.
    fn run_target_exists() -> bool {
        let mut buf = [0u16; 1024];
        let mut size = (buf.len() * 2) as u32;
        let ok = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                RUN,
                NAME,
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr() as *mut _),
                Some(&mut size),
            )
            .is_ok()
        };
        if !ok {
            return false;
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let cmd = String::from_utf16_lossy(&buf[..end]);
        let path = cmd.trim().trim_start_matches('"').split('"').next().unwrap_or("").trim();
        !path.is_empty() && std::path::Path::new(path).exists()
    }

    /// `Some(true)` = approved, `Some(false)` = disabled by the user in
    /// Startup Apps / Task Manager, `None` = no flag written yet.
    fn approval() -> Option<bool> {
        let mut buf = [0u8; 12];
        let mut size = buf.len() as u32;
        let ok = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                APPROVED,
                NAME,
                RRF_RT_REG_BINARY,
                None,
                Some(buf.as_mut_ptr() as *mut _),
                Some(&mut size),
            )
            .is_ok()
        };
        // Low bit set (03, 07...) means disabled; 02/06 mean enabled.
        (ok && size > 0).then(|| buf[0] & 1 == 0)
    }

    fn write_approval() {
        let flag: [u8; 12] = [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        unsafe {
            let _ = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                APPROVED,
                NAME,
                REG_BINARY.0,
                Some(flag.as_ptr() as *const _),
                flag.len() as u32,
            );
        }
    }

    fn write_run_entry() {
        let Ok(exe) = std::env::current_exe() else { return };
        let value = HSTRING::from(format!("\"{}\"", exe.display()));
        let bytes = (value.len() + 1) * 2;
        unsafe {
            let _ =
                RegSetKeyValueW(HKEY_CURRENT_USER, RUN, NAME, REG_SZ.0, Some(value.as_ptr() as *const _), bytes as u32);
        }
    }

    /// On only if the entry exists and the user hasn't disabled it in Windows.
    pub fn enabled() -> bool {
        has_run_entry() && approval() != Some(false)
    }

    pub fn set(on: bool) {
        if on {
            write_run_entry();
            write_approval();
        } else {
            unsafe {
                let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN, NAME);
                let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, APPROVED, NAME);
            }
        }
    }

    /// Called at launch: if startup is on, add the approval flag older versions
    /// forgot, and if the exe it points to is gone (moved or uninstalled),
    /// point it at this one. A working entry - e.g. the installed copy - is
    /// never hijacked by another copy, and a Task Manager "disabled" is kept.
    pub fn repair() {
        if !has_run_entry() || approval() == Some(false) {
            return;
        }
        if !run_target_exists() {
            write_run_entry();
        }
        if approval().is_none() {
            write_approval();
        }
    }
}
