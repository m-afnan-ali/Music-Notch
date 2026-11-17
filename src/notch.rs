//! The notch window: a per-pixel-alpha layered window rendered with Direct2D.
//!
//! Performance notes
//! * Rendering is software Direct2D into a small DIB, pushed with
//!   `UpdateLayeredWindow`. No GPU device, no swap chain, tiny footprint.
//! * The animation timer only runs while something is actually moving. When the
//!   notch is still, the process sleeps with zero CPU and trims its working set.
//! * Transparent pixels are click-through, so the notch never blocks the apps
//!   underneath it.

use std::{ptr::null_mut, time::Instant};

use windows::{
    core::*,
    Win32::Foundation::*,
    Win32::Graphics::{
        Direct2D::{Common::*, *},
        DirectWrite::*,
        Gdi::*,
    },
    Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT},
    Win32::UI::WindowsAndMessaging::*,
};

use crate::{
    anim::{clamp01, ease, lerp, Spring},
    audio::Meter,
    config::Config,
    gfx::*,
    media::{Cmd, Track},
};

pub const T_ANIM: usize = 1;
pub const T_TRIM: usize = 3;

// Logical (96-dpi, 100%) geometry.
const IDLE: (f32, f32) = (130.0, 30.0);
const COLLAPSED: (f32, f32) = (310.0, 38.0);
const EXPANDED: (f32, f32) = (420.0, 150.0);
const EAR: f32 = 12.0;
const WIN_W: f32 = EXPANDED.0 + EAR * 2.0 + 8.0;
const WIN_H: f32 = EXPANDED.1 + 16.0;
const COL_TEXT_W: f32 = 227.0;
const EXP_TITLE_W: f32 = 200.0;
const EXP_ARTIST_W: f32 = 200.0;
/// Live level history (newest first) that feeds the visualizer.
const HIST: usize = 64;
/// How long the song name shows before the visualizer takes over.
const NAME_SECS: f32 = 4.0;
const MARQUEE_GAP: f32 = 40.0;
/// Horizontal drag distance (logical px) that counts as a skip.
const SWIPE_SKIP: f32 = 48.0;

struct Fonts {
    col_b: IDWriteTextFormat,
    col: IDWriteTextFormat,
    exp_title: IDWriteTextFormat,
    exp_artist: IDWriteTextFormat,
    small: IDWriteTextFormat,
    glyph: IDWriteTextFormat,
}

#[derive(Default)]
struct Layouts {
    title: Option<Text>,
    artist: Option<Text>,
    prev_title: Option<Text>,
    exp_title: Option<Text>,
    exp_artist: Option<Text>,
}

impl Layouts {
    fn total(&self) -> f32 {
        self.title.as_ref().map_or(0.0, |t| t.w) + self.artist.as_ref().map_or(0.0, |t| t.w)
    }
}

/// A short overlay shown over the notch's content after an action.
#[derive(Clone, Copy)]
pub enum Hud {
    /// Volume level (0..1) after scrolling.
    Volume(f32),
    /// Play state after a click: true = now playing.
    Play(bool),
}

struct Drag {
    cursor: POINT,
    /// Screen position of the collapsed pill's top-left corner at drag start.
    pill: (f32, f32),
}

pub struct Notch {
    pub hwnd: HWND,
    // surface
    memdc: HDC,
    dib: HBITMAP,
    old_bmp: HGDIOBJ,
    bits: *mut u8,
    dib_w: i32,
    dib_h: i32,
    size: (i32, i32),
    pos: (i32, i32),
    /// Part of the surface currently shown as the window (x0, y0, x1, y1), physical px.
    shown: (i32, i32, i32, i32),
    eq_rect: D2D_RECT_F,
    k: f32,
    /// Which screen edges the pill touches, and whether it grows upward
    /// (when it sits in the lower half of the screen).
    e_top: bool,
    e_bottom: bool,
    e_left: bool,
    e_right: bool,
    grow_up: bool,
    rt: Option<ID2D1DCRenderTarget>,
    brush: Option<ID2D1SolidColorBrush>,
    fonts: Option<Fonts>,
    lay: Layouts,
    // media
    pub track: Track,
    cover_px: Option<(Vec<u8>, u32)>,
    cover_thumb_id: u64,
    cover: Option<ID2D1Bitmap>,
    prev_cover: Option<ID2D1Bitmap>,
    palette: [[f32; 3]; 3],
    palette_target: [[f32; 3]; 3],
    // audio + visualizer
    meter: Meter,
    has_meter: bool,
    level: f32,
    hist: [f32; HIST],
    viz: Spring,
    name_until: Option<Instant>,
    phase: f32,
    viz_rect: D2D_RECT_F,
    partial_viz: bool,
    /// Scrolling-title frame: only the text strip (and mini bars) are repainted.
    partial_text: bool,
    text_rect: D2D_RECT_F,
    text_band: D2D_RECT_F,
    // swipe-to-skip (locked + no hover-expand)
    swipe: Spring,
    // overlay (volume level / play-pause feedback)
    hud: Spring,
    hud_kind: Hud,
    hud_until: Option<Instant>,
    swiping: bool,
    swipe_dx: f32,
    // drawing origin of the bound sub-rectangle, and the current content shift
    org: (f32, f32),
    cur_dx: f32,
    // animation
    w: Spring,
    h: Spring,
    expand: Spring,
    presence: Spring,
    vis: Spring,
    change: Spring,
    cover_fade: Spring,
    play: Spring,
    eq_phase: [f32; 4],
    eq: [f32; 4],
    marquee: f32,
    marquee_pause: f32,
    timer_ms: u32,
    last: Instant,
    // input
    hover: bool,
    hover_pending: Option<(bool, Instant)>,
    pub force_hover: Option<bool>,
    mouse: Option<(f32, f32)>,
    tracking: bool,
    pressed: Option<Cmd>,
    drag: Option<Drag>,
    pub dragging: bool,
    buttons: Option<[D2D_RECT_F; 3]>,
    pub hidden: bool,
    anim_bars: bool,
}

impl Notch {
    pub fn new(hwnd: HWND) -> Self {
        Self {
            hwnd,
            memdc: unsafe { CreateCompatibleDC(None) },
            dib: HBITMAP::default(),
            old_bmp: HGDIOBJ::default(),
            bits: null_mut(),
            dib_w: 0,
            dib_h: 0,
            size: (0, 0),
            pos: (0, 0),
            shown: (0, 0, 0, 0),
            eq_rect: D2D_RECT_F::default(),
            k: 1.0,
            e_top: true,
            e_bottom: false,
            e_left: false,
            e_right: false,
            grow_up: false,
            rt: None,
            brush: None,
            fonts: None,
            lay: Layouts::default(),
            track: Track::default(),
            cover_px: None,
            cover_thumb_id: 0,
            cover: None,
            prev_cover: None,
            palette: [DEFAULT_ACCENT; 3],
            palette_target: [DEFAULT_ACCENT; 3],
            meter: Meter::new(),
            has_meter: false,
            level: 0.0,
            hist: [0.0; HIST],
            viz: Spring::new(0.0, 70.0),
            name_until: None,
            phase: 0.0,
            viz_rect: D2D_RECT_F::default(),
            partial_viz: false,
            partial_text: false,
            text_rect: D2D_RECT_F::default(),
            text_band: D2D_RECT_F::default(),
            swipe: Spring::new(0.0, 240.0),
            hud: Spring::new(0.0, 200.0),
            hud_kind: Hud::Volume(0.0),
            hud_until: None,
            swiping: false,
            swipe_dx: 0.0,
            org: (0.0, 0.0),
            cur_dx: 0.0,
            w: Spring::new(IDLE.0, 170.0),
            h: Spring::new(IDLE.1, 170.0),
            expand: Spring::new(0.0, 160.0),
            presence: Spring::new(0.0, 120.0),
            vis: Spring::new(1.0, 120.0),
            change: Spring::new(1.0, 90.0),
            cover_fade: Spring::new(1.0, 90.0),
            play: Spring::new(0.0, 140.0),
            eq_phase: [0.3, 1.7, 2.9, 4.4],
            eq: [0.2; 4],
            marquee: 0.0,
            marquee_pause: 1.5,
            timer_ms: 0,
            last: Instant::now(),
            hover: false,
            hover_pending: None,
            force_hover: None,
            mouse: None,
            tracking: false,
            pressed: None,
            drag: None,
            dragging: false,
            buttons: None,
            hidden: false,
            anim_bars: true,
        }
    }

    // ------------------------------------------------------------- placement

    pub fn reposition(&mut self, cfg: &Config) {
        let mon = pick_monitor(cfg);
        let k = mon.dpi as f32 / 96.0 * cfg.scale as f32 / 100.0;
        if (k - self.k).abs() > 0.01 {
            self.k = k;
            self.cover = None; // re-decode at the new resolution
            self.prev_cover = None;
            self.cover_thumb_id = 0;
        }
        let w = (WIN_W * k).ceil() as i32;
        let h = (WIN_H * k).ceil() as i32;
        let ear = (EAR * k) as i32;
        let r = mon.rect;
        let mon_h = r.bottom - r.top;
        let coll_h = (COLLAPSED.1 * k).round() as i32;
        let y_off = cfg.y_off.clamp(0, (mon_h - coll_h).max(0));
        let x = match cfg.anchor {
            0 => r.left + cfg.x_off - ear,
            2 => r.right - cfg.x_off - w + ear,
            _ => r.left + (r.right - r.left) / 2 + cfg.x_off - w / 2,
        };
        self.e_top = y_off <= 0;
        self.e_bottom = y_off >= mon_h - coll_h;
        self.e_left = cfg.anchor == 0 && cfg.x_off <= 0;
        self.e_right = cfg.anchor == 2 && cfg.x_off <= 0;
        // In the lower half of the screen, expand upward so it never leaves the screen.
        self.grow_up = !self.e_top && (y_off + coll_h / 2) > mon_h / 2;
        let top = if self.grow_up {
            r.top + y_off - ((self.grow_line() - COLLAPSED.1) * k).round() as i32
        } else {
            r.top + y_off
        };
        self.size = (w, h);
        self.pos = (x, top);
    }

    /// Surface y (logical) that the pill's bottom edge sits on when growing upward.
    fn grow_line(&self) -> f32 {
        WIN_H - if self.e_bottom { 0.0 } else { 2.0 }
    }

    fn floating(&self) -> bool {
        !(self.e_top || self.e_bottom || self.e_left || self.e_right)
    }

    fn pill_rect(&self, cfg: &Config) -> D2D_RECT_F {
        let (pw, ph) = (self.w.value, self.h.value);
        let x = match cfg.anchor {
            0 => EAR,
            2 => WIN_W - EAR - pw,
            _ => (WIN_W - pw) / 2.0,
        };
        let y = if self.grow_up {
            self.grow_line() - ph
        } else if self.e_top {
            0.0
        } else {
            2.0
        };
        rect(x, y, pw, ph)
    }

    // ------------------------------------------------------------- state

    pub fn update_targets(&mut self, cfg: &Config) {
        let has = self.track.active;
        let hover = self.force_hover.unwrap_or(self.hover);
        let exp = has && hover && cfg.expand_on_hover && !self.dragging;
        self.presence.target = if has { 1.0 } else { 0.0 };
        self.expand.target = if exp { 1.0 } else { 0.0 };
        let (tw, th) = if exp {
            EXPANDED
        } else if has {
            COLLAPSED
        } else {
            IDLE
        };
        self.w.target = tw;
        self.h.target = th;
        self.vis.target = if !has && cfg.hide_when_idle { 0.0 } else { 1.0 };
        self.play.target = if has && self.track.playing { 1.0 } else { 0.0 };
        let name_time = self.name_until.is_some_and(|t| Instant::now() < t);
        let viz = has && self.track.playing && cfg.idle_view == 1 && !name_time;
        self.viz.target = if viz { 1.0 } else { 0.0 };
        self.kick();
    }

    fn accent(&self) -> [f32; 3] {
        self.palette[0]
    }

    pub fn retint(&mut self, cfg: &Config) {
        self.palette_target = match (&self.cover_px, cfg.album_tint) {
            (Some((px, n)), true) => palette(px, *n),
            _ => [DEFAULT_ACCENT; 3],
        };
        self.kick();
    }

    /// Jump every animation to its end state (used for screenshots).
    pub fn settle(&mut self) {
        for s in [
            &mut self.w,
            &mut self.h,
            &mut self.expand,
            &mut self.presence,
            &mut self.vis,
            &mut self.change,
            &mut self.play,
            &mut self.viz,
            &mut self.swipe,
            &mut self.cover_fade,
            &mut self.hud,
        ] {
            s.snap();
        }
        self.palette = self.palette_target;
    }

    pub fn on_media(&mut self, gfx: &Gfx, cfg: &Config, t: Track) {
        self.ensure_fonts(gfx);
        if t.active {
            let new_song = t.key != self.track.key || self.lay.title.is_none();
            if new_song {
                self.prev_cover = self.cover.take();
                self.lay.prev_title = self.lay.title.take();
                self.cover_px = None;
                self.cover_thumb_id = 0;
                self.change.reset(0.0, 1.0);
                self.cover_fade.reset(0.0, 1.0);
                self.marquee = 0.0;
                self.marquee_pause = 1.8;
                // The name always gets its moment when a new song starts.
                self.name_until = Some(Instant::now() + std::time::Duration::from_secs_f32(NAME_SECS));
                if let Some(f) = &self.fonts {
                    let sep = if t.artist.is_empty() { String::new() } else { format!("  ·  {}", t.artist) };
                    self.lay.title = gfx.layout(&t.title, &f.col_b, 4000.0, 40.0);
                    self.lay.artist = gfx.layout(&sep, &f.col, 4000.0, 40.0);
                    self.lay.exp_title = gfx.layout(&t.title, &f.exp_title, EXP_TITLE_W, 30.0);
                    self.lay.exp_artist = gfx.layout(&t.artist, &f.exp_artist, EXP_ARTIST_W, 24.0);
                }
            }
            self.track = t;
            self.decode_cover(gfx, cfg);
        } else {
            // Keep the old text around so it can fade out gracefully.
            self.track.active = false;
            self.track.playing = false;
        }
        self.update_targets(cfg);
    }

    fn decode_cover(&mut self, gfx: &Gfx, cfg: &Config) {
        if self.track.thumb_id == 0 || self.track.thumb_id == self.cover_thumb_id {
            return;
        }
        let Some(bytes) = self.track.thumb.clone() else { return };
        let px = ((64.0 * self.k).ceil() as u32).clamp(48, 256);
        self.cover_thumb_id = self.track.thumb_id;
        // Art that arrives after the title (or gets corrected) still crossfades in.
        if self.cover.is_some() || self.cover_px.is_some() {
            self.prev_cover = self.cover.take();
            self.cover_fade.reset(0.0, 1.0);
        }
        self.cover = None;
        self.cover_px = gfx.decode_square(&bytes, px);
        self.retint(cfg);
    }

    fn ensure_fonts(&mut self, gfx: &Gfx) {
        if self.fonts.is_some() {
            return;
        }
        let f = || -> Result<Fonts> {
            Ok(Fonts {
                col_b: gfx.format(13.0, true, false)?,
                col: gfx.format(13.0, false, false)?,
                exp_title: gfx.format(15.0, true, true)?,
                exp_artist: gfx.format(12.5, false, true)?,
                small: gfx.format(10.5, false, false)?,
                glyph: gfx.format(20.0, false, false)?,
            })
        };
        self.fonts = f().ok();
    }

    // ------------------------------------------------------------- timing

    pub fn kick(&mut self) {
        if self.timer_ms != 16 {
            if self.timer_ms == 0 {
                self.last = Instant::now();
            }
            unsafe {
                SetTimer(Some(self.hwnd), T_ANIM, 16, None);
            }
            self.timer_ms = 16;
        }
    }

    pub fn tick(&mut self, gfx: &Gfx, cfg: &Config) {
        let now = Instant::now();
        let raw = (now - self.last).as_secs_f32();
        self.last = now;
        let dt = raw.min(0.05);

        if let Some((want, since)) = self.hover_pending {
            let delay = if want { 0.12 } else { 0.35 };
            if (now - since).as_secs_f32() >= delay {
                self.hover_pending = None;
                if self.hover != want {
                    self.hover = want;
                    self.update_targets(cfg);
                }
            }
        }

        let mut slow = false;
        if let Some(t) = self.hud_until {
            if now >= t {
                self.hud_until = None;
                self.hud.target = 0.0;
            } else {
                slow = true; // wake up to hide it
            }
        }
        if let Some(t) = self.name_until {
            if now >= t {
                self.name_until = None;
                self.update_targets(cfg);
            } else if cfg.idle_view == 1 {
                slow = true; // wake up in time to switch to the visualizer
            }
        }

        let mut fast = self.hover_pending.is_some();
        for s in [
            &mut self.w,
            &mut self.h,
            &mut self.expand,
            &mut self.presence,
            &mut self.vis,
            &mut self.change,
            &mut self.play,
            &mut self.viz,
            &mut self.swipe,
            &mut self.cover_fade,
            &mut self.hud,
        ] {
            fast |= s.step(dt);
        }

        let ka = 1.0 - (-dt * 4.0).exp();
        for c in 0..3 {
            for i in 0..3 {
                let d = self.palette_target[c][i] - self.palette[c][i];
                if d.abs() > 0.002 {
                    self.palette[c][i] += d * ka;
                    fast = true;
                } else {
                    self.palette[c][i] = self.palette_target[c][i];
                }
            }
        }

        let playing = self.track.active && self.track.playing;
        let eq_anim = cfg.animate_bars && (playing || self.play.value > 0.01) && self.vis.value > 0.01;
        if eq_anim {
            self.phase += dt;
            // Live level from the system output, or a soft procedural pulse.
            let lvl = if playing { self.meter.level() } else { Some(0.0) };
            self.has_meter = lvl.is_some();
            let p = self.phase;
            self.level = lvl.unwrap_or(0.35 + 0.6 * ((p * 2.3).sin() * (p * 0.71 + 1.0).sin()).abs());
            self.hist.copy_within(0..HIST - 1, 1);
            self.hist[0] = self.level;
            for i in 0..4 {
                self.eq_phase[i] += dt * (5.5 + i as f32 * 1.7);
                let q = self.eq_phase[i];
                let synth = 0.35 + 0.65 * (q.sin() * (q * 0.37 + i as f32).sin()).abs();
                let target = if self.has_meter { 0.2 + 0.8 * self.hist[i * 3 + 1] } else { synth };
                self.eq[i] = lerp(self.eq[i], target, 1.0 - (-dt * 18.0).exp());
            }
        }

        let a_col = clamp01(1.0 - self.expand.value * 2.2) * self.presence.value * (1.0 - clamp01(self.viz.value));
        let total = self.lay.total();
        let mut marquee_fast = false;
        if self.track.active && total > COL_TEXT_W && a_col > 0.01 {
            if self.marquee_pause > 0.0 {
                self.marquee_pause -= raw;
                slow = true;
            } else {
                self.marquee += dt * 28.0;
                if self.marquee >= total + MARQUEE_GAP {
                    self.marquee = 0.0;
                    self.marquee_pause = 2.5;
                }
                marquee_fast = true;
            }
        } else if total <= COL_TEXT_W {
            self.marquee = 0.0;
        }
        if self.expand.value > 0.5 && playing && self.track.dur > 0.0 {
            slow = true; // progress bar
        }

        // When only the bars or the scrolling title move, repaint just those pixels.
        let partial = !fast && (eq_anim || marquee_fast) && self.expand.value < 0.01 && self.hud.value < 0.001;
        self.partial_viz = self.viz.value > 0.5;
        self.partial_text = !self.partial_viz && marquee_fast;
        self.render_frame(gfx, cfg, partial);

        let want = if fast || marquee_fast {
            16
        } else if eq_anim {
            33
        } else if slow {
            250
        } else {
            0
        };
        if want != self.timer_ms {
            unsafe {
                if want == 0 {
                    let _ = KillTimer(Some(self.hwnd), T_ANIM);
                    SetTimer(Some(self.hwnd), T_TRIM, 5000, None);
                } else {
                    SetTimer(Some(self.hwnd), T_ANIM, want, None);
                }
            }
            self.timer_ms = want;
        }
    }

    pub fn is_idle(&self) -> bool {
        self.timer_ms == 0
    }

    // ------------------------------------------------------------- rendering

    fn ensure_surface(&mut self) -> bool {
        let (w, h) = self.size;
        if w <= 0 || h <= 0 {
            return false;
        }
        if !self.bits.is_null() && self.dib_size() == (w, h) {
            return true;
        }
        unsafe {
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = null_mut();
            let Ok(dib) = CreateDIBSection(Some(self.memdc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
                return false;
            };
            let prev = SelectObject(self.memdc, dib.into());
            if self.old_bmp.is_invalid() {
                self.old_bmp = prev;
            }
            if !self.dib.is_invalid() {
                let _ = DeleteObject(self.dib.into());
            }
            self.dib = dib;
            self.bits = bits as *mut u8;
            self.dib_w = w;
            self.dib_h = h;
        }
        true
    }

    fn dib_size(&self) -> (i32, i32) {
        (self.dib_w, self.dib_h)
    }

    pub fn render(&mut self, gfx: &Gfx, cfg: &Config) {
        self.render_frame(gfx, cfg, false);
    }

    /// The window only covers the pill (plus its "ears"), not the whole surface,
    /// so every frame touches as few pixels as possible.
    fn visible_bounds(&self, cfg: &Config) -> (i32, i32, i32, i32) {
        let r = self.pill_rect(cfg);
        let k = self.k;
        let (w, h) = self.size;
        let x0 = (((r.left - EAR - 1.0) * k).floor() as i32).clamp(0, w - 1);
        let x1 = (((r.right + EAR + 1.0) * k).ceil() as i32).clamp(x0 + 1, w);
        let y0 = (((r.top - 3.0) * k).floor() as i32).clamp(0, h - 1);
        let y1 = (((r.bottom + 3.0) * k).ceil() as i32).clamp(y0 + 1, h);
        (x0, y0, x1, y1)
    }

    fn render_frame(&mut self, gfx: &Gfx, cfg: &Config, partial: bool) {
        if self.hidden || !self.ensure_surface() {
            return;
        }
        self.ensure_fonts(gfx);
        self.anim_bars = cfg.animate_bars;
        unsafe {
            if self.rt.is_none() {
                self.rt = gfx.d2d.CreateDCRenderTarget(&rt_props(true)).ok();
                self.brush = None;
                self.cover = None;
                self.prev_cover = None;
            }
            let Some(rt) = self.rt.clone() else { return };
            if self.brush.is_none() {
                self.brush = rt.CreateSolidColorBrush(&hex(0, 1.0), None).ok();
            }
            if self.cover.is_none() {
                if let Some((px, n)) = &self.cover_px {
                    self.cover = bitmap_from_pixels(&rt, px, *n);
                }
            }
            let (w, h) = self.size;
            let shown = self.visible_bounds(cfg);
            let partial = partial && shown == self.shown;
            let k = self.k;
            // Bind only the pixels this frame changes. Direct2D copies the bound
            // area back to the bitmap on EndDraw, so a small rect = a cheap frame.
            let region = if partial {
                let e = self.dirty_rect();
                RECT {
                    left: (((e.left - 2.0) * k).floor() as i32).max(0),
                    top: (((e.top - 2.0) * k).floor() as i32).max(0),
                    right: (((e.right + 2.0) * k).ceil() as i32).min(w),
                    bottom: (((e.bottom + 2.0) * k).ceil() as i32).min(h),
                }
            } else {
                RECT { left: shown.0, top: shown.1, right: shown.2, bottom: shown.3 }
            };
            if region.right <= region.left || region.bottom <= region.top {
                return;
            }
            if partial && self.partial_viz {
                let a = clamp01(1.0 - self.expand.value * 2.2) * clamp01(self.presence.value);
                self.raster_viz(&region, a, ease(self.viz.value));
            } else {
                if rt.BindDC(self.memdc, &region).is_err() {
                    return;
                }
                self.org = (region.left as f32, region.top as f32);
                self.cur_dx = 0.0;
                rt.BeginDraw();
                rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
                match (&self.brush, partial) {
                    // The bars sit on solid black, so a bars-only frame is just
                    // "black rectangle + bars" - no shapes, text or images.
                    (Some(b), true) => {
                        rt.Clear(Some(&hex(0, 1.0)));
                        rt.SetTransform(&self.xf());
                        let a = clamp01(1.0 - self.expand.value * 2.2) * clamp01(self.presence.value);
                        if self.partial_viz {
                            self.draw_viz(&rt, gfx, b, self.viz_rect, a, ease(self.viz.value));
                        } else if self.partial_text {
                            let a = a * (1.0 - ease(self.viz.value));
                            self.draw_eq(&rt, b, self.eq_rect, a);
                            self.draw_marquee(&rt, b, self.text_rect, a, ease(self.change.value), 0.0);
                        } else {
                            self.draw_eq(&rt, b, self.eq_rect, a);
                        }
                    }
                    _ => {
                        rt.Clear(Some(&hex(0, 0.0)));
                        rt.SetTransform(&self.xf());
                        self.draw(&rt, gfx, cfg);
                    }
                }
                rt.SetTransform(&identity());
                if let Err(e) = rt.EndDraw(None, None) {
                    if e.code() == D2DERR_RECREATE_TARGET {
                        self.rt = None;
                    }
                }
            }

            let alpha = (clamp01(self.vis.value) * cfg.opacity as f32 / 100.0 * 255.0).round() as u8;
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: alpha,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let (x0, y0, x1, y1) = shown;
            self.shown = shown;
            let _ = UpdateLayeredWindow(
                self.hwnd,
                None,
                Some(&POINT { x: self.pos.0 + x0, y: self.pos.1 + y0 }),
                Some(&SIZE { cx: x1 - x0, cy: y1 - y0 }),
                Some(self.memdc),
                Some(&POINT { x: x0, y: y0 }),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
        }
    }

    /// Logical → bound-surface transform (DPI scale, sub-rect origin, swipe shift).
    fn xf(&self) -> windows_numerics::Matrix3x2 {
        windows_numerics::Matrix3x2 {
            M11: self.k,
            M12: 0.0,
            M21: 0.0,
            M22: self.k,
            M31: self.cur_dx * self.k - self.org.0,
            M32: -self.org.1,
        }
    }

    fn dirty_rect(&self) -> D2D_RECT_F {
        if self.partial_viz {
            self.viz_rect
        } else if self.partial_text {
            self.text_band
        } else {
            self.eq_rect
        }
    }

    /// The pill outline. Sides touching a screen edge are square; where the pill
    /// meets the top or bottom edge it flares into the screen with concave
    /// "ears", like a real notch. Flush into a corner, only the inner side flares.
    fn shape(&self, gfx: &Gfx, r: D2D_RECT_F, t: f32) -> Option<ID2D1Geometry> {
        unsafe {
            let (x, y, w, h) = (r.left, r.top, r.right - r.left, r.bottom - r.top);
            let rad = (h / 2.0).min(lerp(h / 2.0, 30.0, t)).min(w / 2.0);
            let (top, bot, left, right) = (self.e_top, self.e_bottom, self.e_left, self.e_right);
            let corner = |touch: bool| if touch { 0.0 } else { rad };
            let (r_tl, r_tr, r_br, r_bl) =
                (corner(top || left), corner(top || right), corner(bot || right), corner(bot || left));
            let (ear_tl, ear_tr, ear_bl, ear_br) = (top && !left, top && !right, bot && !left, bot && !right);
            let e = EAR * 0.9;

            let g = gfx.d2d.CreatePathGeometry().ok()?;
            let s = g.Open().ok()?;
            let arc = |p: (f32, f32), rr: f32| D2D1_ARC_SEGMENT {
                point: v2(p.0, p.1),
                size: D2D_SIZE_F { width: rr, height: rr },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: D2D1_ARC_SIZE_SMALL,
            };
            let quad = |c: (f32, f32), p: (f32, f32)| D2D1_QUADRATIC_BEZIER_SEGMENT {
                point1: v2(c.0, c.1),
                point2: v2(p.0, p.1),
            };
            // top edge, left to right
            s.BeginFigure(if ear_tl { v2(x - e, y) } else { v2(x + r_tl, y) }, D2D1_FIGURE_BEGIN_FILLED);
            if ear_tr {
                s.AddLine(v2(x + w + e, y));
                s.AddQuadraticBezier(&quad((x + w, y), (x + w, y + e)));
            } else {
                s.AddLine(v2(x + w - r_tr, y));
                if r_tr > 0.0 {
                    s.AddArc(&arc((x + w, y + r_tr), r_tr));
                }
            }
            // right edge, down
            if ear_br {
                s.AddLine(v2(x + w, y + h - e));
                s.AddQuadraticBezier(&quad((x + w, y + h), (x + w + e, y + h)));
            } else {
                s.AddLine(v2(x + w, y + h - r_br));
                if r_br > 0.0 {
                    s.AddArc(&arc((x + w - r_br, y + h), r_br));
                }
            }
            // bottom edge, right to left
            if ear_bl {
                s.AddLine(v2(x - e, y + h));
                s.AddQuadraticBezier(&quad((x, y + h), (x, y + h - e)));
            } else {
                s.AddLine(v2(x + r_bl, y + h));
                if r_bl > 0.0 {
                    s.AddArc(&arc((x, y + h - r_bl), r_bl));
                }
            }
            // left edge, up
            if ear_tl {
                s.AddLine(v2(x, y + e));
                s.AddQuadraticBezier(&quad((x, y), (x - e, y)));
            } else {
                s.AddLine(v2(x, y + r_tl));
                if r_tl > 0.0 {
                    s.AddArc(&arc((x + r_tl, y), r_tl));
                }
            }
            s.EndFigure(D2D1_FIGURE_END_CLOSED);
            s.Close().ok()?;
            g.cast().ok()
        }
    }

    unsafe fn draw(&mut self, rt: &ID2D1RenderTarget, gfx: &Gfx, cfg: &Config) {
        let (Some(b), Some(f)) = (self.brush.clone(), self.fonts.as_ref()) else { return };
        let r = self.pill_rect(cfg);
        let t = clamp01(self.expand.value);
        let pres = clamp01(self.presence.value);

        if let Some(geo) = self.shape(gfx, r, t) {
            b.SetColor(&hex(0x000000, 1.0));
            rt.FillGeometry(&geo, &b, None);
            if self.floating() {
                b.SetColor(&hex(0xffffff, 0.09));
                rt.DrawGeometry(&geo, &b, 1.0 / self.k, None);
            }
        }
        self.buttons = None;
        if pres <= 0.01 && self.lay.title.is_none() {
            return;
        }

        // Swipe-to-skip: content follows the finger, with a hint of what's next.
        let sw = self.swipe.value;
        let swiping = sw.abs() > 0.05;
        if swiping {
            let hint = clamp01(sw.abs() / 24.0);
            let cmd = if sw > 0.0 { Cmd::Next } else { Cmd::Prev };
            let cy = (r.top + r.bottom) / 2.0;
            let hx = if sw > 0.0 { r.left + 10.0 } else { r.right - 30.0 };
            b.SetColor(&hex(0xffffff, pres * hint));
            self.draw_icon(rt, gfx, &b, cmd, rect(hx, cy - 10.0, 20.0, 20.0));
            rt.PushAxisAlignedClip(
                &D2D_RECT_F { left: r.left + 6.0, right: r.right - 6.0, ..r },
                D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            );
            self.cur_dx = sw;
            rt.SetTransform(&self.xf());
        }
        let pres = pres * (1.0 - clamp01(sw.abs() / 28.0) * 0.55);

        let ph = r.bottom - r.top;
        let ch = ease(self.change.value);
        let cf = ease(self.cover_fade.value);

        // --- cover: morphs between the two layouts
        let cs = lerp(24.0, 64.0, t).min(ph - 12.0).max(8.0);
        let cy_col = r.top + (ph - cs) / 2.0;
        let cover_r = rect(r.left + lerp(9.0, 18.0, t), lerp(cy_col, r.top + 18.0, t), cs, cs);
        let radius = lerp(6.0, 13.0, t);
        if let Some(prev) = &self.prev_cover {
            draw_bitmap_rounded(rt, prev, cover_r, radius, pres * (1.0 - cf));
        }
        match &self.cover {
            Some(cover) => {
                let sc = lerp(0.86, 1.0, cf);
                let (cx, cy) = ((cover_r.left + cover_r.right) / 2.0, (cover_r.top + cover_r.bottom) / 2.0);
                let rr_ = rect(cx - cs * sc / 2.0, cy - cs * sc / 2.0, cs * sc, cs * sc);
                draw_bitmap_rounded(rt, cover, rr_, radius * sc, pres * cf);
            }
            None => {
                b.SetColor(&hex(0x2a2a31, pres * cf));
                rt.FillRoundedRectangle(&rr(cover_r, radius), &b);
                if let Some(g) = gfx.layout("♪", &f.glyph, cs, cs) {
                    let s = cs / 34.0;
                    b.SetColor(&hex(0x9a9aa6, pres * cf));
                    let mut m = self.xf();
                    m.M11 *= s;
                    m.M22 *= s;
                    m.M31 += (cover_r.left + (cs - g.w * s) / 2.0) * self.k;
                    m.M32 += (cover_r.top + (cs - g.h * s) / 2.0) * self.k;
                    rt.SetTransform(&m);
                    rt.DrawTextLayout(v2(0.0, 0.0), &g.l, &b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                    rt.SetTransform(&self.xf());
                }
            }
        }

        let right = r.right - lerp(14.0, 18.0, t);
        let tx = cover_r.right + lerp(10.0, 14.0, t);
        let eq_w = 16.0;

        // --- collapsed: song name (+ mini equalizer) or the visualizer
        let hud = ease(self.hud.value);
        let a_col = clamp01(1.0 - t * 2.2) * pres * (1.0 - hud);
        let vz = ease(self.viz.value);
        let cyc = (r.top + r.bottom) / 2.0;
        self.eq_rect = rect(right - eq_w, cyc - 7.0, eq_w, 14.0);
        self.viz_rect = D2D_RECT_F { left: tx, top: r.top + 6.0, right, bottom: r.bottom - 6.0 };
        if a_col > 0.002 {
            if vz < 0.999 {
                self.draw_eq(rt, &b, self.eq_rect, a_col * (1.0 - vz));
                let text_r = D2D_RECT_F { left: tx, top: r.top, right: right - eq_w - 10.0, bottom: r.bottom };
                self.text_rect = text_r;
                // The strip a scrolling-title frame repaints: always solid black pill.
                self.text_band =
                    D2D_RECT_F { left: tx - 1.0, top: cyc - 11.0, right: self.eq_rect.right + 1.0, bottom: cyc + 11.0 };
                self.draw_marquee(rt, &b, text_r, a_col * (1.0 - vz), ch, vz * 10.0);
            }
            if vz > 0.001 {
                self.draw_viz(rt, gfx, &b, self.viz_rect, a_col, vz);
            }
        }

        if hud > 0.002 && t < 0.5 {
            let area = D2D_RECT_F { left: tx, top: r.top, right, bottom: r.bottom };
            self.draw_hud(rt, gfx, &b, area, clamp01(1.0 - t * 2.2) * pres * hud);
        }

        // --- expanded: title, artist, controls, timeline
        let a_exp = clamp01(t * 2.0 - 1.0) * pres;
        if a_exp > 0.002 {
            let slide = (1.0 - a_exp) * 6.0;
            let top = r.top + 18.0 + slide;
            if let Some(l) = &self.lay.exp_title {
                b.SetColor(&hex(0xffffff, a_exp * (1.0 - hud)));
                rt.DrawTextLayout(v2(tx, top + 7.0), &l.l, &b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
            }
            if let Some(l) = &self.lay.exp_artist {
                b.SetColor(&hex(0xffffff, a_exp * 0.58 * (1.0 - hud)));
                rt.DrawTextLayout(v2(tx, top + 31.0), &l.l, &b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
            }

            let bs = 30.0;
            let gap = 4.0;
            let bx = right - (bs * 3.0 + gap * 2.0) + 4.0;
            let by = top + (64.0 - bs) / 2.0;
            let mut btns = [D2D_RECT_F::default(); 3];
            for (i, cmd) in [Cmd::Prev, Cmd::Toggle, Cmd::Next].into_iter().enumerate() {
                let br = rect(bx + i as f32 * (bs + gap), by, bs, bs);
                btns[i] = br;
                if let Some((mx, my)) = self.mouse {
                    if contains(&br, mx, my) {
                        let pressed = self.pressed.map(|p| p as u8) == Some(cmd as u8);
                        b.SetColor(&hex(0xffffff, a_exp * if pressed { 0.28 } else { 0.16 }));
                        rt.FillEllipse(
                            &D2D1_ELLIPSE {
                                point: v2(br.left + bs / 2.0, br.top + bs / 2.0),
                                radiusX: bs / 2.0,
                                radiusY: bs / 2.0,
                            },
                            &b,
                        );
                    }
                }
                b.SetColor(&hex(0xffffff, a_exp));
                self.draw_icon(rt, gfx, &b, cmd, br);
            }
            self.buttons = Some(btns);
            if hud > 0.002 {
                let area = D2D_RECT_F { left: tx, top: top + 4.0, right: bx - 10.0, bottom: top + 54.0 };
                self.draw_hud(rt, gfx, &b, area, a_exp * hud);
            }

            // timeline row
            let cy = r.bottom - 24.0 + slide * 0.5;
            let lx = r.left + 18.0;
            if self.track.dur > 0.0 {
                let pos = self.track.position();
                b.SetColor(&hex(0xffffff, a_exp * 0.55));
                if let Some(l) = gfx.layout(&fmt_time(pos), &f.small, 60.0, 20.0) {
                    rt.DrawTextLayout(v2(lx, cy - l.h / 2.0), &l.l, &b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                }
                if let Some(l) = gfx.layout(&fmt_time(self.track.dur), &f.small, 60.0, 20.0) {
                    rt.DrawTextLayout(v2(right - l.w, cy - l.h / 2.0), &l.l, &b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                }
                let (x0, x1) = (lx + 40.0, right - 40.0);
                let frac = (pos / self.track.dur) as f32;
                if cfg.wave_timeline {
                    self.draw_hills(rt, gfx, &b, x0, x1, cy, frac, a_exp);
                } else {
                    let bh = 4.0;
                    b.SetColor(&hex(0xffffff, a_exp * 0.16));
                    rt.FillRoundedRectangle(&rr(rect(x0, cy - bh / 2.0, x1 - x0, bh), bh / 2.0), &b);
                    let fw = ((x1 - x0) * frac).max(bh);
                    b.SetColor(&rgb(self.accent(), a_exp));
                    rt.FillRoundedRectangle(&rr(rect(x0, cy - bh / 2.0, fw, bh), bh / 2.0), &b);
                }
            } else {
                let s = if self.track.playing { "Playing" } else { "Paused" };
                if let Some(l) = gfx.layout(s, &f.small, 120.0, 20.0) {
                    b.SetColor(&hex(0xffffff, a_exp * 0.45));
                    rt.DrawTextLayout(v2(lx, cy - l.h / 2.0), &l.l, &b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                }
            }
        }
        if swiping {
            self.cur_dx = 0.0;
            rt.SetTransform(&self.xf());
            rt.PopAxisAlignedClip();
        }
    }

    /// Collapsed visualizer: the same soft, layered hills as the timeline,
    /// stretched across the notch and swaying with the live output level.
    unsafe fn draw_viz(
        &self,
        rt: &ID2D1RenderTarget,
        gfx: &Gfx,
        b: &ID2D1SolidColorBrush,
        r: D2D_RECT_F,
        alpha: f32,
        grow: f32,
    ) {
        let base = r.bottom - 2.0;
        // a quiet baseline the hills rise from
        b.SetColor(&rgb(self.palette[0].map(|c| c * 0.45 + 0.08), alpha * grow));
        rt.FillRoundedRectangle(&rr(rect(r.left, base - 1.0, r.right - r.left, 3.0), 1.5), b);
        let amp = grow * (0.35 + 0.95 * self.play.value);
        self.hill_layers(rt, gfx, b, r.left, r.right, base, r.bottom - r.top - 3.0, 4, alpha * grow, amp);
    }

    /// Peak positions/widths/heights for the three hill layers.
    fn bump_set(&self, x0: f32, end: f32, max_h: f32, k: usize, amp_scale: f32) -> [[(f32, f32, f32); 4]; 3] {
        let wdt = end - x0;
        let live = self.anim_bars && self.track.playing;
        const SHAPE: [f32; 4] = [0.5, 1.0, 0.72, 0.85];
        let mut out = [[(0.0f32, 1.0f32, 0.0f32); 4]; 3];
        for (layer, bumps) in out.iter_mut().enumerate() {
            let lf = layer as f32;
            for (j, bump) in bumps.iter_mut().take(k).enumerate() {
                let jf = j as f32;
                let drift =
                    if live { 0.22 * (self.phase * (0.45 + 0.12 * lf) + lf * 1.7 + jf * 2.1).sin() } else { 0.0 };
                // Offset each layer a little so the peaks interleave.
                let c = x0 + wdt * ((jf + 0.5 + drift + (lf - 1.0) * 0.22) / k as f32);
                let sigma = (wdt / k as f32 * (0.3 + 0.06 * lf)).max(7.0);
                // Average a few samples so peaks breathe instead of flicker.
                let lvl = if live {
                    let i = (j * 6 + layer * 3).min(HIST - 4);
                    self.hist[i..i + 4].iter().sum::<f32>() / 4.0
                } else {
                    0.6
                };
                let amp = max_h * (0.3 + 0.7 * lvl) * SHAPE[(j + layer) % k] * [1.0, 0.82, 0.66][layer] * amp_scale;
                *bump = (c, sigma, amp);
            }
        }
        out
    }

    /// Draws the collapsed visualizer straight into the bitmap - no Direct2D.
    /// Each column computes three hill heights and blends them over black with
    /// box-filtered anti-aliasing. Much cheaper than filling curved paths.
    fn raster_viz(&self, region: &RECT, alpha: f32, grow: f32) {
        if self.bits.is_null() {
            return;
        }
        let k = self.k;
        let r = self.viz_rect;
        let base = r.bottom - 2.0;
        let max_h = r.bottom - r.top - 3.0;
        let amp = grow * (0.35 + 0.95 * self.play.value);
        let bumps = self.bump_set(r.left, r.right, max_h, 4, amp);
        let a = alpha * grow;
        let dark = self.palette[0].map(|c| c * 0.45 + 0.08);
        let mut cols = [([0.0f32; 3], 0.0f32); 3];
        for (l, c) in cols.iter_mut().enumerate() {
            *c = (self.palette[l].map(|v| v * 0.5 + 0.5), a * [0.5, 0.56, 0.66][l]);
        }
        let overlap = |p: f32, lo: f32, hi: f32| (hi.min(p + 1.0) - lo.max(p)).clamp(0.0, 1.0);
        let stride = self.size.0 as usize * 4;
        let (bl0, bl1) = ((base - 1.0) * k, (base + 2.0) * k);
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::GdiFlush();
            for px in region.left..region.right {
                let lx = (px as f32 + 0.5) / k;
                let cov_x = overlap(px as f32, r.left * k, r.right * k);
                let mut h = [0.0f32; 3];
                if cov_x > 0.0 {
                    for l in 0..3 {
                        h[l] = hill_height(&bumps[l], 4, lx, r.left, r.right, max_h);
                    }
                }
                for py in region.top..region.bottom {
                    let y = py as f32;
                    let mut c = [0.0f32; 3];
                    let mut blend = |col: [f32; 3], a: f32| {
                        for i in 0..3 {
                            c[i] += (col[i] - c[i]) * a;
                        }
                    };
                    if cov_x > 0.0 {
                        blend(dark, a * cov_x * overlap(y, bl0, bl1));
                        for l in 0..3 {
                            if h[l] > 0.01 {
                                let cov = overlap(y, (base - h[l]) * k, base * k) * cov_x;
                                if cov > 0.0 {
                                    blend(cols[l].0, cols[l].1 * cov);
                                }
                            }
                        }
                    }
                    let i = py as usize * stride + px as usize * 4;
                    let d = self.bits.add(i);
                    *d = (c[2] * 255.0) as u8;
                    *d.add(1) = (c[1] * 255.0) as u8;
                    *d.add(2) = (c[0] * 255.0) as u8;
                    *d.add(3) = 255;
                }
            }
        }
    }

    /// Three translucent layers of smooth peaks, tinted from the album palette.
    /// Peak heights follow the recent audio level; peak positions drift slowly.
    #[allow(clippy::too_many_arguments)]
    unsafe fn hill_layers(
        &self,
        rt: &ID2D1RenderTarget,
        gfx: &Gfx,
        b: &ID2D1SolidColorBrush,
        x0: f32,
        end: f32,
        base: f32,
        max_h: f32,
        k: usize,
        alpha: f32,
        amp_scale: f32,
    ) {
        let wdt = end - x0;
        if wdt <= 18.0 || alpha <= 0.002 {
            return;
        }
        let k = k.clamp(1, 4);
        const N: usize = 48;
        let sets = self.bump_set(x0, end, max_h, k, amp_scale);
        for (layer, bumps) in sets.iter().enumerate() {
            let Ok(g) = gfx.d2d.CreatePathGeometry() else { return };
            let Ok(sink) = g.Open() else { return };
            sink.BeginFigure(v2(x0, base), D2D1_FIGURE_BEGIN_FILLED);
            for n in 0..=N {
                let x = x0 + wdt * n as f32 / N as f32;
                sink.AddLine(v2(x, base - hill_height(bumps, k, x, x0, end, max_h)));
            }
            sink.AddLine(v2(end, base));
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            if sink.Close().is_err() {
                return;
            }
            // Tint each layer toward white so the stack reads as soft light.
            let c = self.palette[layer].map(|v| v * 0.5 + 0.5);
            b.SetColor(&rgb(c, alpha * [0.5, 0.56, 0.66][layer]));
            rt.FillGeometry(&g, b, None);
        }
    }

    /// One UI 9-style timeline: soft, translucent hills in album colors rise
    /// from the played part and sway with the music; a ring knob marks the
    /// playhead and the rest of the track stays a quiet flat line.
    #[allow(clippy::too_many_arguments)]
    unsafe fn draw_hills(
        &self,
        rt: &ID2D1RenderTarget,
        gfx: &Gfx,
        b: &ID2D1SolidColorBrush,
        x0: f32,
        x1: f32,
        cy: f32,
        frac: f32,
        alpha: f32,
    ) {
        let th = 5.0;
        let knob = 8.0;
        let px = x0 + frac.clamp(0.0, 1.0) * (x1 - x0);
        let dark = self.palette[0].map(|c| c * 0.32 + 0.04);

        // rest of the track
        let rest = px + knob + 3.0;
        if x1 > rest {
            b.SetColor(&hex(0xffffff, alpha * 0.32));
            rt.FillRoundedRectangle(&rr(rect(rest, cy - th / 2.0, x1 - rest, th), th / 2.0), b);
        }
        // played track
        if px - x0 > 1.0 {
            b.SetColor(&rgb(dark, alpha));
            rt.FillRoundedRectangle(&rr(rect(x0, cy - th / 2.0, px - x0, th), th / 2.0), b);
        }

        // hills
        let base = cy - th / 2.0 + 1.5;
        self.hill_layers(rt, gfx, b, x0, px - knob * 0.5, base, 26.0, 3, alpha, 1.0);

        // knob
        let e = D2D1_ELLIPSE { point: v2(px, cy), radiusX: knob, radiusY: knob };
        b.SetColor(&rgb(dark, alpha));
        rt.FillEllipse(&e, b);
        b.SetColor(&hex(0xffffff, alpha));
        rt.DrawEllipse(&e, b, 2.5, None);
    }

    /// Volume bar or play/pause feedback, vertically centered in `area`.
    unsafe fn draw_hud(
        &self,
        rt: &ID2D1RenderTarget,
        gfx: &Gfx,
        b: &ID2D1SolidColorBrush,
        area: D2D_RECT_F,
        alpha: f32,
    ) {
        let Some(f) = self.fonts.as_ref() else { return };
        let cy = (area.top + area.bottom) / 2.0;
        match self.hud_kind {
            Hud::Volume(level) => {
                // speaker
                let x = area.left + 2.0;
                b.SetColor(&hex(0xffffff, alpha));
                rt.FillRoundedRectangle(&rr(rect(x, cy - 3.0, 4.0, 6.0), 1.0), b);
                if let Some(g) = triangle(&gfx.d2d, v2(x + 3.0, cy - 3.0), v2(x + 9.0, cy - 7.0), v2(x + 9.0, cy + 7.0))
                {
                    rt.FillGeometry(&g, b, None);
                }
                if let Some(g) = triangle(&gfx.d2d, v2(x + 3.0, cy - 3.0), v2(x + 9.0, cy + 7.0), v2(x + 3.0, cy + 3.0))
                {
                    rt.FillGeometry(&g, b, None);
                }
                // bar
                let pct = format!("{}%", (level * 100.0).round() as i32);
                let label = gfx.layout(&pct, &f.col_b, 80.0, 30.0);
                let lw = label.as_ref().map_or(30.0, |l| l.w);
                let (x0, x1) = (x + 18.0, area.right - lw - 10.0);
                if x1 > x0 {
                    let bh = 5.0;
                    b.SetColor(&hex(0xffffff, alpha * 0.18));
                    rt.FillRoundedRectangle(&rr(rect(x0, cy - bh / 2.0, x1 - x0, bh), bh / 2.0), b);
                    b.SetColor(&rgb(self.accent(), alpha));
                    let fw = ((x1 - x0) * level).max(bh);
                    rt.FillRoundedRectangle(&rr(rect(x0, cy - bh / 2.0, fw, bh), bh / 2.0), b);
                }
                if let Some(l) = label {
                    b.SetColor(&hex(0xffffff, alpha));
                    rt.DrawTextLayout(v2(area.right - l.w, cy - l.h / 2.0), &l.l, b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                }
            }
            Hud::Play(playing) => {
                // Same icon the expanded controls use, with a word next to it.
                let icon = rect(area.left, cy - 11.0, 22.0, 22.0);
                b.SetColor(&hex(0xffffff, alpha));
                self.draw_icon_for(rt, gfx, b, Cmd::Toggle, icon, playing);
                let word = if playing { "Playing" } else { "Paused" };
                if let Some(l) = gfx.layout(word, &f.col_b, 200.0, 30.0) {
                    rt.DrawTextLayout(v2(icon.right + 8.0, cy - l.h / 2.0), &l.l, b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                }
            }
        }
    }

    /// Shows the overlay for a moment.
    pub fn show_hud(&mut self, kind: Hud) {
        self.hud_kind = kind;
        self.hud.target = 1.0;
        let secs = if matches!(kind, Hud::Volume(_)) { 1.2 } else { 0.9 };
        self.hud_until = Some(Instant::now() + std::time::Duration::from_secs_f32(secs));
        self.kick();
    }

    pub fn clear_hud(&mut self) {
        self.hud.reset(0.0, 0.0);
        self.hud_until = None;
    }

    /// Whether a screen point is over the visible pill (for scroll-to-volume).
    pub fn hit_screen(&self, cfg: &Config, sx: i32, sy: i32) -> bool {
        let (lx, ly) = ((sx - self.pos.0) as f32 / self.k, (sy - self.pos.1) as f32 / self.k);
        self.pill_contains(cfg, lx, ly)
    }

    unsafe fn draw_eq(&self, rt: &ID2D1RenderTarget, b: &ID2D1SolidColorBrush, r: D2D_RECT_F, alpha: f32) {
        b.SetColor(&rgb(self.accent(), alpha));
        let n = 4;
        let bw = (r.right - r.left) / (n * 2 - 1) as f32;
        let h_total = r.bottom - r.top;
        let cy = (r.top + r.bottom) / 2.0;
        const STATIC: [f32; 4] = [0.55, 0.95, 0.7, 0.4];
        for (i, &still) in STATIC.iter().enumerate().take(n) {
            let active = if self.anim_bars { self.eq[i] } else { still };
            let lvl = lerp(0.18, active, self.play.value);
            let h = (h_total * lvl).max(bw);
            let x = r.left + i as f32 * bw * 2.0;
            rt.FillRoundedRectangle(&rr(rect(x, cy - h / 2.0, bw, h), bw / 2.0), b);
        }
    }

    unsafe fn draw_marquee(
        &self,
        rt: &ID2D1RenderTarget,
        b: &ID2D1SolidColorBrush,
        tr: D2D_RECT_F,
        alpha: f32,
        ch: f32,
        lift: f32,
    ) {
        let Some(title) = &self.lay.title else { return };
        let artist_w = self.lay.artist.as_ref().map_or(0.0, |a| a.w);
        let total = title.w + artist_w;
        let overflow = total > tr.right - tr.left;
        let offset = if overflow { self.marquee } else { 0.0 };
        let cy = (tr.top + tr.bottom) / 2.0;
        let y = cy - title.h / 2.0 - lift;

        rt.PushAxisAlignedClip(&tr, D2D1_ANTIALIAS_MODE_ALIASED);
        if ch < 1.0 {
            if let Some(p) = &self.lay.prev_title {
                b.SetColor(&hex(0xffffff, alpha * (1.0 - ch)));
                rt.DrawTextLayout(v2(tr.left, y - ch * 8.0), &p.l, b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
            }
        }
        let dy = (1.0 - ch) * 8.0;
        let copies = if overflow { 2 } else { 1 };
        for c in 0..copies {
            let x = tr.left - offset + c as f32 * (total + MARQUEE_GAP);
            b.SetColor(&hex(0xffffff, alpha * ch));
            rt.DrawTextLayout(v2(x, y + dy), &title.l, b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
            if let Some(a) = &self.lay.artist {
                b.SetColor(&hex(0xffffff, alpha * ch * 0.55));
                rt.DrawTextLayout(v2(x + title.w, y + dy), &a.l, b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
            }
        }
        rt.PopAxisAlignedClip();
    }

    unsafe fn draw_icon(&self, rt: &ID2D1RenderTarget, gfx: &Gfx, b: &ID2D1SolidColorBrush, cmd: Cmd, br: D2D_RECT_F) {
        self.draw_icon_for(rt, gfx, b, cmd, br, self.track.playing)
    }

    unsafe fn draw_icon_for(
        &self,
        rt: &ID2D1RenderTarget,
        gfx: &Gfx,
        b: &ID2D1SolidColorBrush,
        cmd: Cmd,
        br: D2D_RECT_F,
        playing: bool,
    ) {
        let (cx, cy) = ((br.left + br.right) / 2.0, (br.top + br.bottom) / 2.0);
        match cmd {
            Cmd::Toggle => {
                let u = 8.5;
                if playing {
                    let bw = u * 0.34;
                    for x in [cx - u * 0.5, cx + u * 0.5 - bw] {
                        rt.FillRoundedRectangle(&rr(rect(x, cy - u * 0.62, bw, u * 1.24), bw / 3.0), b);
                    }
                } else if let Some(g) = triangle(
                    &gfx.d2d,
                    v2(cx - u * 0.42, cy - u * 0.66),
                    v2(cx + u * 0.7, cy),
                    v2(cx - u * 0.42, cy + u * 0.66),
                ) {
                    rt.FillGeometry(&g, b, None);
                }
            }
            _ => {
                let d = if matches!(cmd, Cmd::Next) { 1.0 } else { -1.0 };
                let u = 6.0;
                for k in [0.0f32, 1.0] {
                    let off = (k - 0.5) * u * 0.75 * d;
                    if let Some(g) = triangle(
                        &gfx.d2d,
                        v2(cx + off - u * 0.4 * d, cy - u * 0.5),
                        v2(cx + off + u * 0.4 * d, cy),
                        v2(cx + off - u * 0.4 * d, cy + u * 0.5),
                    ) {
                        rt.FillGeometry(&g, b, None);
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------- input

    fn pill_contains(&self, cfg: &Config, x: f32, y: f32) -> bool {
        let r = self.pill_rect(cfg);
        x >= r.left - 2.0 && x <= r.right + 2.0 && y >= r.top - 2.0 && y <= r.bottom + 4.0
    }

    fn btn_at(&self, x: f32, y: f32) -> Option<Cmd> {
        let b = self.buttons?;
        [Cmd::Prev, Cmd::Toggle, Cmd::Next].into_iter().zip(b).find(|(_, r)| contains(r, x, y)).map(|(c, _)| c)
    }

    fn logical(&self, x: i32, y: i32) -> (f32, f32) {
        ((x + self.shown.0) as f32 / self.k, (y + self.shown.1) as f32 / self.k)
    }

    pub fn over_button(&self) -> bool {
        self.mouse.is_some_and(|(x, y)| self.btn_at(x, y).is_some())
    }

    fn set_hover(&mut self, want: bool) {
        if want == self.hover {
            self.hover_pending = None;
        } else if self.hover_pending.map(|p| p.0) != Some(want) {
            self.hover_pending = Some((want, Instant::now()));
            self.kick();
        }
    }

    pub fn mouse_move(&mut self, x: i32, y: i32, cfg: &Config) {
        let (lx, ly) = self.logical(x, y);
        self.mouse = Some((lx, ly));
        if !self.tracking {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                let _ = TrackMouseEvent(&mut tme);
            }
            self.tracking = true;
        }
        let inside = self.pill_contains(cfg, lx, ly);
        self.set_hover(inside);
        if self.expand.value > 0.3 {
            self.kick(); // button hover highlight
        }
    }

    pub fn mouse_leave(&mut self) {
        self.tracking = false;
        self.mouse = None;
        if !self.dragging {
            self.set_hover(false);
        }
        self.kick();
    }

    /// Screen position of the collapsed pill's top-left corner for a config.
    fn collapsed_origin(&self, cfg: &Config) -> (f32, f32) {
        let mon = pick_monitor(cfg);
        let r = mon.rect;
        let cw = COLLAPSED.0 * self.k;
        let x = match cfg.anchor {
            0 => (r.left + cfg.x_off) as f32,
            2 => (r.right - cfg.x_off) as f32 - cw,
            _ => (r.left + (r.right - r.left) / 2 + cfg.x_off) as f32 - cw / 2.0,
        };
        (x, (r.top + cfg.y_off) as f32)
    }

    pub fn lbutton_down(&mut self, x: i32, y: i32, cfg: &Config) {
        let (lx, ly) = self.logical(x, y);
        self.pressed = self.btn_at(lx, ly);
        let mut cursor = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut cursor);
        }
        self.drag = Some(Drag { cursor, pill: self.collapsed_origin(cfg) });
        self.kick();
    }

    /// Returns the command to run (if a button was clicked) and whether a drag ended.
    pub fn lbutton_up(&mut self, x: i32, y: i32, cfg: &Config) -> (Option<Cmd>, bool) {
        let (lx, ly) = self.logical(x, y);
        let mut cmd = None;
        if let Some(p) = self.pressed.take() {
            let hit = self.btn_at(lx, ly);
            if hit.map(|h| h as u8) == Some(p as u8) {
                cmd = Some(p);
                if matches!(p, Cmd::Toggle) {
                    // Optimistic UI - Windows confirms a moment later.
                    let pos = self.track.position();
                    self.track.pos = pos;
                    self.track.at = Some(Instant::now());
                    self.track.playing = !self.track.playing;
                }
            }
        }
        let swiped = self.swiping;
        if self.swiping {
            self.swiping = false;
            self.swipe.target = 0.0; // spring back
            if self.swipe_dx >= SWIPE_SKIP {
                cmd = Some(Cmd::Next);
            } else if self.swipe_dx <= -SWIPE_SKIP {
                cmd = Some(Cmd::Prev);
            }
            self.swipe_dx = 0.0;
        }
        let dragged = self.dragging;
        // A plain click on a notch that doesn't expand toggles play/pause.
        let clicked = self.drag.is_some() && !dragged && !swiped && cmd.is_none();
        if clicked && !cfg.expand_on_hover && self.track.active && self.pill_contains(cfg, lx, ly) {
            cmd = Some(Cmd::Toggle);
            let pos = self.track.position();
            self.track.pos = pos;
            self.track.at = Some(Instant::now());
            self.track.playing = !self.track.playing;
            self.show_hud(Hud::Play(self.track.playing));
        }
        self.drag = None;
        self.dragging = false;
        self.update_targets(cfg);
        (cmd, dragged)
    }

    /// Moves the notch while dragging. Returns true when the config changed.
    pub fn drag_move(&mut self, cfg: &mut Config) -> bool {
        let Some((d_cursor, d_pill)) = self.drag.as_ref().map(|d| (d.cursor, d.pill)) else { return false };
        let mut cur = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut cur);
        }
        let (dx, dy) = (cur.x - d_cursor.x, cur.y - d_cursor.y);
        // Locked + no hover-expand: a horizontal drag skips tracks instead.
        let swipe_mode = cfg.locked && !cfg.expand_on_hover && self.track.active;
        if swipe_mode && self.pressed.is_none() && (self.swiping || dx.abs() + dy.abs() > 5) {
            self.swiping = true;
            self.swipe_dx = dx as f32 / self.k;
            let v = (self.swipe_dx * 0.5).clamp(-28.0, 28.0);
            self.swipe.reset(v, v);
            self.kick();
            return false;
        }
        if !self.dragging {
            if cfg.locked || self.pressed.is_some() || dx.abs() + dy.abs() <= 5 {
                return false;
            }
            self.dragging = true;
            self.update_targets(cfg);
        }
        let (px, py) = (d_pill.0 + dx as f32, d_pill.1 + dy as f32);
        let hmon = unsafe { MonitorFromPoint(cur, MONITOR_DEFAULTTONEAREST) };
        let Some(mon) = mon_info(hmon) else { return false };
        let r = mon.rect;
        let k = self.k;
        let snap = 22.0 * k;
        let (cw, chh) = (COLLAPSED.0 * k, COLLAPSED.1 * k);
        // Horizontal: snap flush into the left/right edge, or to the center.
        if px - (r.left as f32) < snap {
            cfg.anchor = 0;
            cfg.x_off = 0;
        } else if (r.right as f32) - (px + cw) < snap {
            cfg.anchor = 2;
            cfg.x_off = 0;
        } else {
            let mut x_off = (px + cw / 2.0) as i32 - (r.left + (r.right - r.left) / 2);
            if (x_off.abs() as f32) < snap {
                x_off = 0;
            }
            cfg.anchor = 1;
            cfg.x_off = x_off;
        }
        // Vertical: snap to the top or bottom edge.
        let max_y = (mon.height() as f32 - chh).max(0.0);
        let mut y_off = py - r.top as f32;
        if y_off < snap {
            y_off = 0.0;
        } else if max_y - y_off < snap {
            y_off = max_y;
        }
        cfg.y_off = y_off.clamp(0.0, max_y).round() as i32;
        cfg.monitor = mon.name;
        true
    }

    /// Loads a made-up track with a generated cover (for README screenshots).
    pub fn demo(&mut self, gfx: &Gfx, cfg: &Config) {
        let t = Track {
            active: true,
            title: "Midnight Drive".into(),
            artist: "Neon Coast".into(),
            playing: true,
            pos: 83.0,
            dur: 214.0,
            at: Some(Instant::now()),
            key: 0xdead_beef,
            thumb: None,
            thumb_id: 0,
            app: String::new(),
        };
        self.on_media(gfx, cfg, t);
        let n = 168u32;
        let mut px = vec![0u8; (n * n * 4) as usize];
        for y in 0..n {
            for x in 0..n {
                let (fx, fy) = (x as f32 / n as f32, y as f32 / n as f32);
                let d = ((fx - 0.68).powi(2) + (fy - 0.34).powi(2)).sqrt();
                let sun = (1.0 - d / 0.22).clamp(0.0, 1.0);
                let r = lerp(0.36, 1.0, fy) * 0.85 + sun * 0.4;
                let g = lerp(0.16, 0.45, fy) * 0.8 + sun * 0.35;
                let b = lerp(0.62, 0.38, fy) + sun * 0.1;
                let i = ((y * n + x) * 4) as usize;
                px[i] = (b.min(1.0) * 255.0) as u8;
                px[i + 1] = (g.min(1.0) * 255.0) as u8;
                px[i + 2] = (r.min(1.0) * 255.0) as u8;
                px[i + 3] = 255;
            }
        }
        self.cover_px = Some((px, n));
        self.cover = None;
        self.prev_cover = None;
        self.lay.prev_title = None;
        self.name_until = None;
        for i in 0..HIST {
            let x = i as f32;
            self.hist[i] = (0.55 + 0.4 * (x * 0.5).sin() * (x * 0.13 + 1.0).cos()).clamp(0.05, 1.0) * (1.0 - x / 90.0);
        }
        self.level = 0.8;
        self.retint(cfg);
    }

    pub fn skip_name_intro(&mut self) {
        self.name_until = None;
    }

    /// Runs a few equalizer-only frames through the partial redraw path
    /// (screenshot mode uses this so the optimization is exercised too).
    pub fn eq_frames(&mut self, gfx: &Gfx, cfg: &Config, n: usize) {
        for f in 0..n {
            for i in 0..4 {
                self.eq[i] = 0.3 + 0.7 * ((f + i * 3) % 5) as f32 / 4.0;
            }
            self.hist.rotate_right(1);
            self.partial_viz = self.viz.value > 0.5;
            self.partial_text = false;
            self.render_frame(gfx, cfg, true);
        }
    }

    /// Saves the current frame as a PNG (developer screenshot mode).
    pub fn save_png(&self, gfx: &Gfx, path: &str) {
        let (w, h) = self.size;
        if self.bits.is_null() {
            return;
        }
        let _ = h;
        let (x0, y0, x1, y1) = self.shown;
        let (cw, ch) = (x1 - x0, y1 - y0);
        let start = ((y0 * w + x0) * 4) as usize;
        let len = ((ch - 1) * w * 4 + cw * 4) as usize;
        let px = unsafe { std::slice::from_raw_parts(self.bits.add(start), len) };
        let _ = gfx.save_png(cw as u32, ch as u32, (w * 4) as u32, px, path);
    }
}

impl Drop for Notch {
    fn drop(&mut self) {
        unsafe {
            if !self.old_bmp.is_invalid() {
                SelectObject(self.memdc, self.old_bmp);
            }
            if !self.dib.is_invalid() {
                let _ = DeleteObject(self.dib.into());
            }
            let _ = DeleteDC(self.memdc);
        }
    }
}

/// Height of one hill layer at `x`, tapered into both ends.
fn hill_height(bumps: &[(f32, f32, f32); 4], k: usize, x: f32, x0: f32, end: f32, max_h: f32) -> f32 {
    let taper = ease(((x - x0) / 16.0).min(1.0)) * ease(((end - x) / 12.0).min(1.0));
    let mut h = 0.0;
    for &(c, sg, amp) in bumps.iter().take(k) {
        let d = (x - c) / sg;
        h += amp * (-d * d).exp();
    }
    // Soft limit: loud passages round off toward the top instead of being sliced flat.
    max_h * (h / max_h).tanh() * taper
}

fn fmt_time(sec: f64) -> String {
    let s = sec.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}
