//! Live output level for the visualizer.
//!
//! Uses the endpoint peak meter that Windows already maintains for the volume
//! mixer (`IAudioMeterInformation`). Nothing is recorded or captured: reading it
//! is a single cheap call, and we only call it while something is animating.

use std::time::Instant;

use windows::{
    core::PWSTR,
    Win32::Media::Audio::Endpoints::IAudioMeterInformation,
    Win32::Media::Audio::{eMultimedia, eRender, IMMDeviceEnumerator, MMDeviceEnumerator},
    Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL},
};

pub struct Meter {
    enumerator: Option<IMMDeviceEnumerator>,
    meter: Option<IAudioMeterInformation>,
    device: String,
    checked: Option<Instant>,
    // auto-gain envelope, so quiet and loud songs both fill the bars
    lo: f32,
    hi: f32,
    smooth: f32,
}

impl Meter {
    pub fn new() -> Self {
        Self { enumerator: None, meter: None, device: String::new(), checked: None, lo: 0.2, hi: 0.8, smooth: 0.0 }
    }

    /// Follows the default output device (headphones plugged in, etc.).
    fn refresh_device(&mut self) {
        if self.checked.is_some_and(|t| t.elapsed().as_secs() < 3) {
            return;
        }
        self.checked = Some(Instant::now());
        unsafe {
            if self.enumerator.is_none() {
                self.enumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok();
            }
            let Some(e) = &self.enumerator else { return };
            let Ok(dev) = e.GetDefaultAudioEndpoint(eRender, eMultimedia) else {
                self.meter = None;
                return;
            };
            let id = dev
                .GetId()
                .map(|p: PWSTR| {
                    let s = p.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(p.0 as *const _));
                    s
                })
                .unwrap_or_default();
            if id != self.device || self.meter.is_none() {
                self.meter = dev.Activate::<IAudioMeterInformation>(CLSCTX_ALL, None).ok();
                self.device = id;
            }
        }
    }

    /// Returns the current level in 0..1 (auto-gained), or `None` if no meter.
    pub fn level(&mut self) -> Option<f32> {
        self.refresh_device();
        let peak = unsafe { self.meter.as_ref()?.GetPeakValue().ok()? };
        // dB scale feels far more musical than linear amplitude.
        let db = if peak > 1e-5 { 20.0 * peak.log10() } else { -90.0 };
        let raw = ((db + 42.0) / 42.0).clamp(0.0, 1.0);
        // Envelope followers: snap outward, relax inward slowly.
        if raw > self.hi {
            self.hi = raw
        } else {
            self.hi += (raw - self.hi) * 0.004
        }
        if raw < self.lo {
            self.lo = raw
        } else {
            self.lo += (raw - self.lo) * 0.002
        }
        let span = (self.hi - self.lo).max(0.18);
        let v = ((raw - self.lo) / span).clamp(0.0, 1.0);
        // Fast attack, softer release.
        let k = if v > self.smooth { 0.65 } else { 0.22 };
        self.smooth += (v - self.smooth) * k;
        Some(if raw <= 0.0 { 0.0 } else { self.smooth })
    }
}
