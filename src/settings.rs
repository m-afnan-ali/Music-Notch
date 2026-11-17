//! The settings window. Custom drawn with Direct2D so it matches the notch,
//! created on demand and fully destroyed (render target and all) when closed.

// Drawing helpers take a render target, brush, fonts and geometry; bundling
// them into structs would only add noise.
#![allow(clippy::too_many_arguments)]

use std::time::Instant;

use windows::{
    core::*,
    Win32::Foundation::*,
    Win32::Graphics::{
        Direct2D::{Common::*, *},
        DirectWrite::*,
        Dwm::*,
        Gdi::InvalidateRect,
    },
    Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow},
    Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT},
    Win32::UI::WindowsAndMessaging::*,
};

use crate::{
    anim::{lerp, Spring},
    config::Config,
    gfx::*,
};

pub const CLASS: PCWSTR = w!("MusicNotchSettings");
pub const W: f32 = 740.0;
pub const H: f32 = 590.0;
const T_ANIM: usize = 1;

const BG: u32 = 0x141416;
const CARD: u32 = 0x1c1c20;
const LINE: u32 = 0x2a2a30;
const TEXT: u32 = 0xececf0;
const SUB: u32 = 0x8a8a96;
const ACCENT: u32 = 0x4c8dff;
const AUTHOR_CREDIT: &str = "by USRNME";
pub const AUTHOR_URL: &str = "https://github.com/m-afnan-ali/Music-Notch";

#[derive(Clone, Copy, PartialEq)]
pub enum Sl {
    X,
    Y,
    Scale,
    Opacity,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Hit {
    Seg(i32),
    View(i32),
    Timeline(i32),
    Slider(Sl),
    Toggle(usize),
    Snap,
    MonPrev,
    MonNext,
    Quit,
    Credit,
}

pub enum Action {
    None,
    Slider(Sl, i32, bool),
    Anchor(i32),
    View(i32),
    Timeline(i32),
    Toggle(usize),
    Snap,
    Monitor(i32),
    Quit,
    OpenAuthor,
}

const TOGGLES: [(&str, &str); 7] = [
    ("Expand on hover", "Off: click the notch to play or pause"),
    ("Hide when nothing is playing", "Fade out completely when idle"),
    ("Tint with album colors", "Equalizer and progress take the cover's color"),
    ("Animated equalizer", "Turn off for the lowest possible CPU use"),
    ("Lock position", "With hover off, swipe left/right to skip"),
    ("Scroll to change volume", "Scroll over the notch to set the app's volume"),
    ("Start with Windows", "Launch quietly when you sign in"),
];

struct Fonts {
    title: IDWriteTextFormat,
    body: IDWriteTextFormat,
    small: IDWriteTextFormat,
    label: IDWriteTextFormat,
}

pub struct SettingsWin {
    pub hwnd: HWND,
    rt: Option<ID2D1HwndRenderTarget>,
    brush: Option<ID2D1SolidColorBrush>,
    fonts: Option<Fonts>,
    k: f32,
    hits: Vec<(Hit, D2D_RECT_F)>,
    tracks: [D2D_RECT_F; 4],
    hover: Option<Hit>,
    pressed: Option<Hit>,
    active: Option<Sl>,
    seg: Spring,
    seg_view: Spring,
    seg_tl: Spring,
    tog: [Spring; 7],
    timer: bool,
    last: Instant,
    tracking: bool,
    pub startup: bool,
    pub mons: Vec<Mon>,
}

pub fn toggle_value(cfg: &Config, startup: bool, i: usize) -> bool {
    match i {
        0 => cfg.expand_on_hover,
        1 => cfg.hide_when_idle,
        2 => cfg.album_tint,
        3 => cfg.animate_bars,
        4 => cfg.locked,
        5 => cfg.scroll_volume,
        _ => startup,
    }
}

impl SettingsWin {
    fn blank(hwnd: HWND, cfg: &Config, k: f32) -> Self {
        let startup = crate::config::autostart::enabled();
        let mut tog = [Spring::new(0.0, 260.0); 7];
        for (i, t) in tog.iter_mut().enumerate() {
            let v = if toggle_value(cfg, startup, i) { 1.0 } else { 0.0 };
            t.reset(v, v);
        }
        Self {
            hwnd,
            rt: None,
            brush: None,
            fonts: None,
            k,
            hits: Vec::new(),
            tracks: [D2D_RECT_F::default(); 4],
            hover: None,
            pressed: None,
            active: None,
            seg: Spring::new(cfg.anchor as f32, 260.0),
            seg_view: Spring::new(cfg.idle_view as f32, 260.0),
            seg_tl: Spring::new(if cfg.wave_timeline { 0.0 } else { 1.0 }, 260.0),
            tog,
            timer: false,
            last: Instant::now(),
            tracking: false,
            startup,
            mons: monitors(),
        }
    }

    pub fn open(cfg: &Config, hinst: HINSTANCE) -> Option<Self> {
        unsafe {
            let mon = pick_monitor(cfg);
            let k = mon.dpi as f32 / 96.0;
            let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
            let mut rc = RECT { left: 0, top: 0, right: (W * k) as i32, bottom: (H * k) as i32 };
            let _ = AdjustWindowRectExForDpi(&mut rc, style, false, WINDOW_EX_STYLE(0), mon.dpi);
            let (ww, wh) = (rc.right - rc.left, rc.bottom - rc.top);
            let wa = mon.work;
            let x = wa.left + ((wa.right - wa.left) - ww) / 2;
            let y = wa.top + ((wa.bottom - wa.top) - wh) / 2;
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CLASS,
                w!("Music Notch"),
                style,
                x,
                y.max(wa.top),
                ww,
                wh,
                None,
                None,
                Some(hinst),
                None,
            )
            .ok()?;
            let dark = BOOL(1);
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const _, 4);
            let caption = COLORREF(0x161414); // BGR of #141416
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, &caption as *const _ as *const _, 4);
            let s = Self::blank(hwnd, cfg, GetDpiForWindow(hwnd) as f32 / 96.0);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            Some(s)
        }
    }

    /// Offscreen instance used for README screenshots.
    pub fn headless(cfg: &Config) -> Self {
        Self::blank(HWND::default(), cfg, 2.0)
    }

    /// Overrides the "Start with Windows" state (and its switch position).
    pub fn set_startup(&mut self, on: bool) {
        self.startup = on;
        let v = if on { 1.0 } else { 0.0 };
        self.tog[6].reset(v, v);
    }

    pub fn invalidate(&self) {
        if !self.hwnd.is_invalid() {
            unsafe {
                let _ = InvalidateRect(Some(self.hwnd), None, false);
            }
        }
    }

    pub fn focus(&self) {
        unsafe {
            if IsIconic(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(self.hwnd);
        }
    }

    pub fn dpi_changed(&mut self, suggested: &RECT) {
        unsafe {
            self.k = GetDpiForWindow(self.hwnd) as f32 / 96.0;
            let _ = SetWindowPos(
                self.hwnd,
                None,
                suggested.left,
                suggested.top,
                suggested.right - suggested.left,
                suggested.bottom - suggested.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        self.resize();
    }

    pub fn resize(&mut self) {
        unsafe {
            if let Some(rt) = &self.rt {
                let mut rc = RECT::default();
                let _ = GetClientRect(self.hwnd, &mut rc);
                let _ = rt.Resize(&D2D_SIZE_U { width: rc.right as u32, height: rc.bottom as u32 });
            }
        }
        self.invalidate();
    }

    fn ensure_fonts(&mut self, gfx: &Gfx) {
        if self.fonts.is_none() {
            let f = || -> Result<Fonts> {
                Ok(Fonts {
                    title: gfx.format(20.0, true, false)?,
                    body: gfx.format(13.0, false, true)?,
                    small: gfx.format(11.5, false, true)?,
                    label: gfx.format(11.0, true, false)?,
                })
            };
            self.fonts = f().ok();
        }
    }

    pub fn paint(&mut self, gfx: &Gfx, cfg: &Config) {
        unsafe {
            if self.rt.is_none() {
                let mut rc = RECT::default();
                let _ = GetClientRect(self.hwnd, &mut rc);
                let hp = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                    hwnd: self.hwnd,
                    pixelSize: D2D_SIZE_U { width: rc.right as u32, height: rc.bottom as u32 },
                    presentOptions: D2D1_PRESENT_OPTIONS_NONE,
                };
                self.rt = gfx.d2d.CreateHwndRenderTarget(&rt_props(true), &hp).ok();
                self.brush = None;
            }
            let Some(rt) = self.rt.clone() else { return };
            if self.brush.is_none() {
                self.brush = rt.CreateSolidColorBrush(&hex(0, 1.0), None).ok();
            }
            let Some(b) = self.brush.clone() else { return };
            rt.BeginDraw();
            rt.SetTransform(&scale(self.k));
            self.draw(&rt, &b, gfx, cfg);
            if rt.EndDraw(None, None).is_err() {
                self.rt = None;
            }
        }
        if self.animating() && !self.timer {
            unsafe {
                SetTimer(Some(self.hwnd), T_ANIM, 16, None);
            }
            self.timer = true;
            self.last = Instant::now();
        }
    }

    /// Renders the window into a PNG (screenshot mode).
    pub fn save_png(&mut self, gfx: &Gfx, cfg: &Config, path: &str) -> Result<()> {
        unsafe {
            let (w, h) = ((W * self.k) as u32, (H * self.k) as u32);
            let bmp = gfx.wic.CreateBitmap(
                w,
                h,
                &windows::Win32::Graphics::Imaging::GUID_WICPixelFormat32bppPBGRA,
                windows::Win32::Graphics::Imaging::WICBitmapCacheOnLoad,
            )?;
            let rt = gfx.d2d.CreateWicBitmapRenderTarget(&bmp, &rt_props(true))?;
            let b = rt.CreateSolidColorBrush(&hex(0, 1.0), None)?;
            rt.BeginDraw();
            rt.SetTransform(&scale(self.k));
            self.draw(&rt, &b, gfx, cfg);
            rt.EndDraw(None, None)?;
            gfx.save_source(&bmp.cast()?, w, h, path)
        }
    }

    fn animating(&self) -> bool {
        !self.seg.settled()
            || !self.seg_view.settled()
            || !self.seg_tl.settled()
            || self.tog.iter().any(|t| !t.settled())
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f32().min(0.05);
        self.last = now;
        self.seg.step(dt);
        self.seg_view.step(dt);
        self.seg_tl.step(dt);
        for t in &mut self.tog {
            t.step(dt);
        }
        if !self.animating() {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), T_ANIM);
            }
            self.timer = false;
        }
        self.invalidate();
    }

    // ---------------------------------------------------------------- drawing

    unsafe fn text(
        &self,
        rt: &ID2D1RenderTarget,
        gfx: &Gfx,
        b: &ID2D1SolidColorBrush,
        fmt: &IDWriteTextFormat,
        s: &str,
        r: D2D_RECT_F,
        color: u32,
        alpha: f32,
        align: DWRITE_TEXT_ALIGNMENT,
    ) {
        if let Some(t) = gfx.layout(s, fmt, r.right - r.left, r.bottom - r.top) {
            let _ = t.l.SetTextAlignment(align);
            let _ = t.l.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            b.SetColor(&hex(color, alpha));
            rt.DrawTextLayout(v2(r.left, r.top), &t.l, b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
        }
    }

    unsafe fn card(&self, rt: &ID2D1RenderTarget, b: &ID2D1SolidColorBrush, r: D2D_RECT_F) {
        b.SetColor(&hex(CARD, 1.0));
        rt.FillRoundedRectangle(&rr(r, 12.0), b);
        b.SetColor(&hex(LINE, 1.0));
        let inset = D2D_RECT_F { left: r.left + 0.5, top: r.top + 0.5, right: r.right - 0.5, bottom: r.bottom - 0.5 };
        rt.DrawRoundedRectangle(&rr(inset, 12.0), b, 1.0, None);
    }

    unsafe fn button(
        &mut self,
        rt: &ID2D1RenderTarget,
        gfx: &Gfx,
        b: &ID2D1SolidColorBrush,
        hit: Hit,
        r: D2D_RECT_F,
        label: &str,
        color: u32,
    ) {
        let hov = self.hover == Some(hit);
        let down = hov && self.pressed == Some(hit);
        b.SetColor(&hex(
            if down {
                0x34343c
            } else if hov {
                0x2c2c33
            } else {
                0x242429
            },
            1.0,
        ));
        rt.FillRoundedRectangle(&rr(r, 8.0), b);
        b.SetColor(&hex(0x34343b, 1.0));
        let inset = D2D_RECT_F { left: r.left + 0.5, top: r.top + 0.5, right: r.right - 0.5, bottom: r.bottom - 0.5 };
        rt.DrawRoundedRectangle(&rr(inset, 8.0), b, 1.0, None);
        if let Some(f) = &self.fonts {
            self.text(rt, gfx, b, &f.body, label, r, color, 1.0, DWRITE_TEXT_ALIGNMENT_CENTER);
        }
        self.hits.push((hit, r));
    }

    /// A pill-shaped segmented control with a sliding (spring-animated) highlight.
    unsafe fn segmented(
        &mut self,
        rt: &ID2D1RenderTarget,
        gfx: &Gfx,
        b: &ID2D1SolidColorBrush,
        font: &IDWriteTextFormat,
        seg: D2D_RECT_F,
        options: &[&str],
        value: f32,
        hit: fn(i32) -> Hit,
    ) {
        let (w, h) = (seg.right - seg.left, seg.bottom - seg.top);
        b.SetColor(&hex(0x26262c, 1.0));
        rt.FillRoundedRectangle(&rr(seg, 9.0), b);
        let n = options.len() as f32;
        let sw = (w - 6.0) / n;
        b.SetColor(&hex(TEXT, 1.0));
        rt.FillRoundedRectangle(&rr(rect(seg.left + 3.0 + value * sw, seg.top + 3.0, sw, h - 6.0), 7.0), b);
        for (i, name) in options.iter().enumerate() {
            let r = rect(seg.left + 3.0 + i as f32 * sw, seg.top, sw, h);
            let sel = (1.0 - (value - i as f32).abs()).clamp(0.0, 1.0);
            let c = mix(0xc4c4cc, 0x111114, sel);
            let a = if self.hover == Some(hit(i as i32)) && sel < 0.5 { 1.0 } else { 0.92 };
            self.text(rt, gfx, b, font, name, r, c, a, DWRITE_TEXT_ALIGNMENT_CENTER);
            self.hits.push((hit(i as i32), r));
        }
    }

    unsafe fn slider(
        &mut self,
        rt: &ID2D1RenderTarget,
        gfx: &Gfx,
        b: &ID2D1SolidColorBrush,
        cfg: &Config,
        sl: Sl,
        row: D2D_RECT_F,
        label: &str,
    ) {
        let (lo, hi) = self.range(cfg, sl);
        let v = slider_value(cfg, sl);
        let unit = if matches!(sl, Sl::X | Sl::Y) { "px" } else { "%" };
        let Some(f) = &self.fonts else { return };
        let (f_body, f_small) = (f.body.clone(), f.small.clone());
        self.text(
            rt,
            gfx,
            b,
            &f_body,
            label,
            D2D_RECT_F { right: row.left + 96.0, ..row },
            TEXT,
            1.0,
            DWRITE_TEXT_ALIGNMENT_LEADING,
        );
        self.text(
            rt,
            gfx,
            b,
            &f_small,
            &format!("{v}{unit}"),
            D2D_RECT_F { left: row.right - 56.0, ..row },
            SUB,
            1.0,
            DWRITE_TEXT_ALIGNMENT_TRAILING,
        );
        let cy = (row.top + row.bottom) / 2.0;
        let tr = D2D_RECT_F { left: row.left + 104.0, top: cy - 2.0, right: row.right - 66.0, bottom: cy + 2.0 };
        let frac = if hi > lo { (v - lo) as f32 / (hi - lo) as f32 } else { 0.0 }.clamp(0.0, 1.0);
        let kx = lerp(tr.left, tr.right, frac);
        b.SetColor(&hex(0x34343b, 1.0));
        rt.FillRoundedRectangle(&rr(tr, 2.0), b);
        b.SetColor(&hex(ACCENT, 1.0));
        rt.FillRoundedRectangle(&rr(D2D_RECT_F { right: kx, ..tr }, 2.0), b);
        let big = self.hover == Some(Hit::Slider(sl)) || self.active == Some(sl);
        let kr = if big { 8.0 } else { 7.0 };
        b.SetColor(&hex(0xffffff, 1.0));
        rt.FillEllipse(&D2D1_ELLIPSE { point: v2(kx, cy), radiusX: kr, radiusY: kr }, b);
        let idx = sl as usize;
        self.tracks[idx] = tr;
        self.hits.push((
            Hit::Slider(sl),
            D2D_RECT_F { left: tr.left - 10.0, top: row.top, right: tr.right + 10.0, bottom: row.bottom },
        ));
    }

    unsafe fn draw(&mut self, rt: &ID2D1RenderTarget, b: &ID2D1SolidColorBrush, gfx: &Gfx, cfg: &Config) {
        self.ensure_fonts(gfx);
        let Some(f) = &self.fonts else { return };
        let (f_title, f_body, f_small, f_label) = (f.title.clone(), f.body.clone(), f.small.clone(), f.label.clone());
        self.hits.clear();
        rt.Clear(Some(&hex(BG, 1.0)));

        // Header
        self.text(
            rt,
            gfx,
            b,
            &f_title,
            "Music Notch",
            rect(24.0, 18.0, 400.0, 30.0),
            TEXT,
            1.0,
            DWRITE_TEXT_ALIGNMENT_LEADING,
        );
        self.text(
            rt,
            gfx,
            b,
            &f_small,
            "Changes apply instantly and are saved automatically.",
            rect(24.0, 46.0, 500.0, 18.0),
            SUB,
            1.0,
            DWRITE_TEXT_ALIGNMENT_LEADING,
        );
        self.text(
            rt,
            gfx,
            b,
            &f_small,
            concat!("v", env!("CARGO_PKG_VERSION")),
            rect(W - 224.0, 20.0, 200.0, 20.0),
            SUB,
            1.0,
            DWRITE_TEXT_ALIGNMENT_TRAILING,
        );
        // Author credit - clickable, opens the GitHub profile.
        let credit = rect(W - 264.0, 42.0, 240.0, 20.0);
        let hot = self.hover == Some(Hit::Credit);
        self.text(
            rt,
            gfx,
            b,
            &f_small,
            AUTHOR_CREDIT,
            credit,
            if hot { ACCENT } else { SUB },
            1.0,
            DWRITE_TEXT_ALIGNMENT_TRAILING,
        );
        self.hits.push((Hit::Credit, D2D_RECT_F { left: credit.right - 160.0, ..credit }));

        let (lx, rx, cw) = (20.0, 380.0, 340.0);
        let section = |s: &Self, title: &str, x: f32, y: f32| {
            s.text(rt, gfx, b, &f_label, title, rect(x + 4.0, y, 300.0, 16.0), SUB, 1.0, DWRITE_TEXT_ALIGNMENT_LEADING);
        };

        // ---------------- Position
        section(self, "POSITION", lx, 86.0);
        let card = rect(lx, 106.0, cw, 262.0);
        self.card(rt, b, card);
        let (ix, iw) = (card.left + 16.0, cw - 32.0);
        // segmented control
        self.seg.target = cfg.anchor as f32;
        let v = self.seg.value;
        self.segmented(
            rt,
            gfx,
            b,
            &f_body,
            rect(ix, card.top + 16.0, iw, 34.0),
            &["Left", "Center", "Right"],
            v,
            Hit::Seg,
        );
        let x_label = if cfg.anchor == 1 { "Horizontal" } else { "From edge" };
        self.slider(rt, gfx, b, cfg, Sl::X, rect(ix, card.top + 60.0, iw, 32.0), x_label);
        self.slider(rt, gfx, b, cfg, Sl::Y, rect(ix, card.top + 96.0, iw, 32.0), "From top");

        // monitor picker
        let row = rect(ix, card.top + 134.0, iw, 32.0);
        self.text(
            rt,
            gfx,
            b,
            &f_body,
            "Monitor",
            D2D_RECT_F { right: row.left + 96.0, ..row },
            TEXT,
            1.0,
            DWRITE_TEXT_ALIGNMENT_LEADING,
        );
        let cur = self.mons.iter().position(|m| m.name == pick_monitor(cfg).name).unwrap_or(0);
        let label = self
            .mons
            .get(cur)
            .map(|m| {
                let name = if m.primary { "Main".to_string() } else { format!("Display {}", cur + 1) };
                format!("{name}  ·  {}×{}", m.width(), m.height())
            })
            .unwrap_or_else(|| "Display 1".into());
        let many = self.mons.len() > 1;
        let pr = rect(row.left + 100.0, row.top + 2.0, 28.0, 28.0);
        let nr = rect(row.right - 28.0, row.top + 2.0, 28.0, 28.0);
        if many {
            self.button(rt, gfx, b, Hit::MonPrev, pr, "‹", TEXT);
            self.button(rt, gfx, b, Hit::MonNext, nr, "›", TEXT);
        }
        self.text(
            rt,
            gfx,
            b,
            &f_small,
            &label,
            D2D_RECT_F { left: pr.right + 4.0, right: nr.left - 4.0, ..row },
            if many { TEXT } else { SUB },
            1.0,
            DWRITE_TEXT_ALIGNMENT_CENTER,
        );

        self.button(rt, gfx, b, Hit::Snap, rect(ix, card.top + 178.0, iw, 34.0), "Snap back to top center", TEXT);
        self.text(
            rt,
            gfx,
            b,
            &f_small,
            "Tip: you can also just drag the notch around.",
            rect(ix, card.top + 220.0, iw, 28.0),
            SUB,
            1.0,
            DWRITE_TEXT_ALIGNMENT_LEADING,
        );

        // ---------------- Appearance
        section(self, "APPEARANCE", lx, 386.0);
        let card = rect(lx, 406.0, cw, 164.0);
        self.card(rt, b, card);
        self.slider(rt, gfx, b, cfg, Sl::Scale, rect(ix, card.top + 12.0, iw, 32.0), "Size");
        self.slider(rt, gfx, b, cfg, Sl::Opacity, rect(ix, card.top + 48.0, iw, 32.0), "Opacity");
        for (row_y, label) in [(card.top + 86.0, "Collapsed"), (card.top + 122.0, "Timeline")] {
            self.text(
                rt,
                gfx,
                b,
                &f_body,
                label,
                rect(ix, row_y, 96.0, 30.0),
                TEXT,
                1.0,
                DWRITE_TEXT_ALIGNMENT_LEADING,
            );
        }
        self.seg_view.target = cfg.idle_view as f32;
        let v = self.seg_view.value;
        self.segmented(
            rt,
            gfx,
            b,
            &f_small,
            rect(ix + 100.0, card.top + 86.0, iw - 100.0, 30.0),
            &["Song name", "Visualizer"],
            v,
            Hit::View,
        );
        self.seg_tl.target = if cfg.wave_timeline { 0.0 } else { 1.0 };
        let v = self.seg_tl.value;
        self.segmented(
            rt,
            gfx,
            b,
            &f_small,
            rect(ix + 100.0, card.top + 122.0, iw - 100.0, 30.0),
            &["Waveform", "Classic"],
            v,
            Hit::Timeline,
        );

        // ---------------- Behavior
        section(self, "BEHAVIOR", rx, 86.0);
        let row_h = 52.0;
        let card = rect(rx, 106.0, cw, row_h * 7.0 + 8.0);
        self.card(rt, b, card);
        for (i, &(name, desc)) in TOGGLES.iter().enumerate() {
            let row = rect(rx + 16.0, card.top + 4.0 + i as f32 * row_h, cw - 32.0, row_h);
            if i > 0 {
                b.SetColor(&hex(0x26262b, 1.0));
                rt.FillRectangle(&rect(row.left, row.top, row.right - row.left, 1.0), b);
            }
            self.text(
                rt,
                gfx,
                b,
                &f_body,
                name,
                rect(row.left, row.top + 8.0, cw - 100.0, 20.0),
                TEXT,
                1.0,
                DWRITE_TEXT_ALIGNMENT_LEADING,
            );
            self.text(
                rt,
                gfx,
                b,
                &f_small,
                desc,
                rect(row.left, row.top + 27.0, cw - 100.0, 17.0),
                SUB,
                1.0,
                DWRITE_TEXT_ALIGNMENT_LEADING,
            );
            let on = toggle_value(cfg, self.startup, i);
            self.tog[i].target = if on { 1.0 } else { 0.0 };
            let t = self.tog[i].value.clamp(0.0, 1.0);
            let sw = rect(row.right - 40.0, row.top + (row_h - 22.0) / 2.0, 40.0, 22.0);
            let base = if self.hover == Some(Hit::Toggle(i)) { 0x44444d } else { 0x3a3a42 };
            b.SetColor(&hex(mix(base, ACCENT, t), 1.0));
            rt.FillRoundedRectangle(&rr(sw, 11.0), b);
            let kx = lerp(sw.left + 11.0, sw.right - 11.0, self.tog[i].value);
            b.SetColor(&hex(0xffffff, 1.0));
            rt.FillEllipse(&D2D1_ELLIPSE { point: v2(kx, sw.top + 11.0), radiusX: 8.0, radiusY: 8.0 }, b);
            self.hits.push((Hit::Toggle(i), row));
        }

        // ---------------- Footer
        let fy = card.bottom + 18.0;
        let fcard = rect(rx, fy, cw, H - 20.0 - fy);
        self.card(rt, b, fcard);
        let qr = rect(fcard.right - 16.0 - 120.0, fcard.top + (fcard.bottom - fcard.top - 34.0) / 2.0, 120.0, 34.0);
        self.text(
            rt,
            gfx,
            b,
            &f_body,
            "Lives in your system tray",
            rect(fcard.left + 16.0, qr.top - 2.0, 190.0, 20.0),
            TEXT,
            1.0,
            DWRITE_TEXT_ALIGNMENT_LEADING,
        );
        self.text(
            rt,
            gfx,
            b,
            &f_small,
            "Right-click the notch or tray icon",
            rect(fcard.left + 16.0, qr.top + 17.0, 190.0, 18.0),
            SUB,
            1.0,
            DWRITE_TEXT_ALIGNMENT_LEADING,
        );
        self.button(rt, gfx, b, Hit::Quit, qr, "Quit", 0xff7a7a);
    }

    // ---------------------------------------------------------------- input

    fn range(&self, cfg: &Config, sl: Sl) -> (i32, i32) {
        let m = pick_monitor(cfg);
        match sl {
            Sl::X if cfg.anchor == 1 => (-m.width() / 2, m.width() / 2),
            Sl::X => (0, (m.width() - 300).max(0)),
            Sl::Y => {
                let coll_h = (38.0 * m.dpi as f32 / 96.0 * cfg.scale as f32 / 100.0).round() as i32;
                (0, (m.height() - coll_h).max(0))
            }
            Sl::Scale => (70, 160),
            Sl::Opacity => (30, 100),
        }
    }

    fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        self.hits.iter().rev().find(|(_, r)| contains(r, x, y)).map(|(h, _)| *h)
    }

    fn logical(&self, x: i32, y: i32) -> (f32, f32) {
        (x as f32 / self.k, y as f32 / self.k)
    }

    fn value_at(&self, cfg: &Config, sl: Sl, x: f32) -> i32 {
        let tr = self.tracks[sl as usize];
        let (lo, hi) = self.range(cfg, sl);
        let frac = ((x - tr.left) / (tr.right - tr.left).max(1.0)).clamp(0.0, 1.0);
        let mut v = (lo as f32 + frac * (hi - lo) as f32).round() as i32;
        if sl == Sl::X && cfg.anchor == 1 && v.abs() < (hi - lo) / 80 {
            v = 0; // gentle detent at center
        }
        v
    }

    pub fn on_move(&mut self, x: i32, y: i32, cfg: &Config) -> Action {
        let (lx, ly) = self.logical(x, y);
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
        if let Some(sl) = self.active {
            return Action::Slider(sl, self.value_at(cfg, sl, lx), false);
        }
        let h = self.hit(lx, ly);
        if h != self.hover {
            self.hover = h;
            self.invalidate();
        }
        Action::None
    }

    pub fn on_leave(&mut self) {
        self.tracking = false;
        if self.active.is_none() {
            self.hover = None;
            self.invalidate();
        }
    }

    pub fn on_down(&mut self, x: i32, y: i32, cfg: &Config) -> Action {
        let (lx, ly) = self.logical(x, y);
        let h = self.hit(lx, ly);
        self.pressed = h;
        self.invalidate();
        if let Some(Hit::Slider(sl)) = h {
            self.active = Some(sl);
            return Action::Slider(sl, self.value_at(cfg, sl, lx), false);
        }
        Action::None
    }

    pub fn on_up(&mut self, x: i32, y: i32, cfg: &Config) -> Action {
        let (lx, ly) = self.logical(x, y);
        if let Some(sl) = self.active.take() {
            self.pressed = None;
            self.invalidate();
            return Action::Slider(sl, self.value_at(cfg, sl, lx), true);
        }
        let h = self.hit(lx, ly);
        let p = self.pressed.take();
        self.invalidate();
        if h.is_none() || h != p {
            return Action::None;
        }
        match h.unwrap() {
            Hit::Seg(i) => Action::Anchor(i),
            Hit::View(i) => Action::View(i),
            Hit::Timeline(i) => Action::Timeline(i),
            Hit::Toggle(i) => Action::Toggle(i),
            Hit::Snap => Action::Snap,
            Hit::MonPrev => Action::Monitor(-1),
            Hit::MonNext => Action::Monitor(1),
            Hit::Quit => Action::Quit,
            Hit::Credit => Action::OpenAuthor,
            Hit::Slider(_) => Action::None,
        }
    }

    pub fn on_wheel(&mut self, delta: i32, cfg: &Config) -> Action {
        let Some(Hit::Slider(sl)) = self.hover else { return Action::None };
        let step = match sl {
            Sl::X | Sl::Y => 4,
            _ => 1,
        };
        let (lo, hi) = self.range(cfg, sl);
        let v = (slider_value(cfg, sl) + step * delta.signum()).clamp(lo, hi);
        Action::Slider(sl, v, true)
    }
}

fn slider_value(cfg: &Config, sl: Sl) -> i32 {
    match sl {
        Sl::X => cfg.x_off,
        Sl::Y => cfg.y_off,
        Sl::Scale => cfg.scale,
        Sl::Opacity => cfg.opacity,
    }
}

fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |s: u32| {
        let x = ((a >> s) & 255) as f32;
        let y = ((b >> s) & 255) as f32;
        (lerp(x, y, t).round() as u32) << s
    };
    ch(16) | ch(8) | ch(0)
}
