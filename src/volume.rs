//! Scroll-to-change-volume.
//!
//! Adjusts the volume of the app that is playing - its own slider in the
//! Windows Volume Mixer - so scrolling over the notch turns Spotify (or the
//! browser) up or down without touching anything else. If that app can't be
//! identified, it falls back to the system volume.

use std::time::{Duration, Instant};

use windows::{
    core::{Interface, GUID, HSTRING, PWSTR},
    Win32::Foundation::{CloseHandle, PROPERTYKEY},
    Win32::Media::Audio::Endpoints::IAudioEndpointVolume,
    Win32::Media::Audio::{
        eMultimedia, eRender, AudioSessionStateActive, IAudioSessionControl2, IAudioSessionManager2,
        IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator,
    },
    Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL},
    Win32::System::Threading::{
        GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    },
    Win32::UI::Shell::{FOLDERID_AppsFolder, IShellItem2, SHCreateItemInKnownFolder, KF_FLAG_DEFAULT},
};

/// `System.Link.TargetParsingPath`: where a Start menu entry points.
const PKEY_LINK_TARGET: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0xb9b4b3fc_2b51_4a42_b5d8_324146afcf25), pid: 2 };

pub struct Volume {
    enumerator: Option<IMMDeviceEnumerator>,
    /// Sessions found for the last app, reused for a burst of scroll steps.
    cache: Option<(String, Instant, Vec<ISimpleAudioVolume>)>,
}

impl Volume {
    pub fn new() -> Self {
        Self { enumerator: None, cache: None }
    }

    /// Changes the playing app's volume by `delta` (e.g. 0.04 = 4%).
    /// Returns the new level (0..1) and whether it was the app (`true`) or the
    /// system volume (`false`). A `delta` of 0 only reads the level.
    /// Turning the volume up also unmutes; turning it down never does.
    pub fn nudge(&mut self, app: &str, delta: f32) -> Option<(f32, bool)> {
        let sessions = self.sessions_for(app);
        unsafe {
            if let Some(first) = sessions.first() {
                let level = (first.GetMasterVolume().ok()? + delta).clamp(0.0, 1.0);
                if delta != 0.0 {
                    for s in &sessions {
                        let _ = s.SetMasterVolume(level, std::ptr::null());
                        if delta > 0.0 {
                            let _ = s.SetMute(false, std::ptr::null());
                        }
                    }
                }
                return Some((level, true));
            }
            let ep: IAudioEndpointVolume = self
                .enumerator()?
                .GetDefaultAudioEndpoint(eRender, eMultimedia)
                .ok()?
                .Activate(CLSCTX_ALL, None)
                .ok()?;
            let level = (ep.GetMasterVolumeLevelScalar().ok()? + delta).clamp(0.0, 1.0);
            if delta != 0.0 {
                let _ = ep.SetMasterVolumeLevelScalar(level, std::ptr::null());
                if delta > 0.0 {
                    let _ = ep.SetMute(false, std::ptr::null());
                }
            }
            Some((level, false))
        }
    }

    fn enumerator(&mut self) -> Option<&IMMDeviceEnumerator> {
        if self.enumerator.is_none() {
            self.enumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok() };
        }
        self.enumerator.as_ref()
    }

    fn sessions_for(&mut self, app: &str) -> Vec<ISimpleAudioVolume> {
        if let Some((cached_app, at, sessions)) = &self.cache {
            if cached_app == app && at.elapsed() < Duration::from_secs(3) {
                return sessions.clone();
            }
        }
        let found = self.find_sessions(app).unwrap_or_default();
        self.cache = Some((app.to_string(), Instant::now(), found.clone()));
        found
    }

    fn find_sessions(&mut self, app: &str) -> Option<Vec<ISimpleAudioVolume>> {
        let names = app_names(app);
        let own_pid = unsafe { GetCurrentProcessId() };
        let mgr: IAudioSessionManager2 = unsafe {
            self.enumerator()?.GetDefaultAudioEndpoint(eRender, eMultimedia).ok()?.Activate(CLSCTX_ALL, None).ok()?
        };
        let list = unsafe { mgr.GetSessionEnumerator().ok()? };
        let count = unsafe { list.GetCount().ok()? };

        let mut matched = Vec::new();
        let mut active: Vec<(u32, ISimpleAudioVolume)> = Vec::new();
        for i in 0..count {
            let Ok(ctl) = (unsafe { list.GetSession(i) }) else { continue };
            let Ok(ctl2) = ctl.cast::<IAudioSessionControl2>() else { continue };
            let pid = unsafe { ctl2.GetProcessId().unwrap_or(0) };
            if pid == 0 || pid == own_pid {
                continue; // system sounds / ourselves
            }
            let Ok(vol) = ctl.cast::<ISimpleAudioVolume>() else { continue };
            let exe = process_stem(pid).unwrap_or_default();
            if !exe.is_empty()
                && names.iter().any(|n| n == &exe || (n.len() >= 4 && n.contains(&exe) && exe.len() >= 4))
            {
                matched.push(vol);
            } else if unsafe { ctl.GetState() }.is_ok_and(|s| s == AudioSessionStateActive) {
                active.push((pid, vol));
            }
        }
        if !matched.is_empty() {
            return Some(matched);
        }
        // Couldn't name it, but exactly one app is making sound: that's the one.
        let first_pid = active.first().map(|(p, _)| *p);
        if first_pid.is_some() && active.iter().all(|(p, _)| Some(*p) == first_pid) {
            return Some(active.into_iter().map(|(_, v)| v).collect());
        }
        None
    }
}

/// Lower-case names an app might run under: its id itself (e.g. "spotify.exe"
/// → "spotify", "Chrome" → "chrome", a Store app's package name), plus the exe
/// its Start menu entry points to (needed for ids like Firefox's hash).
fn app_names(app: &str) -> Vec<String> {
    let lower = app.to_lowercase();
    let mut names = vec![lower.trim_end_matches(".exe").to_string()];
    // Store apps: "Publisher.AppName_hash!AppId"
    for part in lower.split(['!', '_', '.']) {
        if part.len() >= 4 && part != "exe" {
            names.push(part.to_string());
        }
    }
    if let Some(stem) = start_menu_target(app) {
        names.push(stem);
    }
    names.sort();
    names.dedup();
    names
}

fn start_menu_target(app: &str) -> Option<String> {
    unsafe {
        let item: IShellItem2 =
            SHCreateItemInKnownFolder(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, &HSTRING::from(app)).ok()?;
        let p: PWSTR = item.GetString(&PKEY_LINK_TARGET).ok()?;
        let path = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const _));
        stem_of(&path)
    }
}

fn process_stem(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        if !ok {
            return None;
        }
        stem_of(&String::from_utf16_lossy(&buf[..len as usize]))
    }
}

fn stem_of(path: &str) -> Option<String> {
    let name = path.rsplit(['\\', '/']).next()?.to_lowercase();
    let stem = name.trim_end_matches(".exe");
    (!stem.is_empty()).then(|| stem.to_string())
}
