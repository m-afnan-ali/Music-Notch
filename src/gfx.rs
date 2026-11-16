//! Shared Direct2D / DirectWrite / WIC plumbing plus small drawing helpers.

use std::mem::size_of;

use windows::{
    core::*,
    Win32::Foundation::*,
    Win32::Graphics::{
        Direct2D::{Common::*, *},
        DirectWrite::*,
        Dxgi::Common::*,
        Gdi::*,
        Imaging::*,
    },
    Win32::System::Com::*,
    Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
};
use windows_numerics::{Matrix3x2, Vector2};

use crate::config::Config;

pub struct Gfx {
    pub d2d: ID2D1Factory,
    pub dw: IDWriteFactory,
    pub wic: IWICImagingFactory,
    family: HSTRING,
}

impl Gfx {
    pub fn new() -> Result<Self> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let family = pick_family(&dw);
            Ok(Self { d2d, dw, wic, family })
        }
    }

    pub fn format(&self, size: f32, bold: bool, trim: bool) -> Result<IDWriteTextFormat> {
        unsafe {
            let weight = if bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
            let f = self.dw.CreateTextFormat(
                &self.family,
                None,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("en-us"),
            )?;
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            if trim {
                let sign = self.dw.CreateEllipsisTrimmingSign(&f)?;
                let t = DWRITE_TRIMMING {
                    granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                    delimiter: 0,
                    delimiterCount: 0,
                };
                f.SetTrimming(&t, &sign)?;
            }
            Ok(f)
        }
    }

    /// Builds a text layout and returns it with its (width, height).
    pub fn layout(&self, text: &str, fmt: &IDWriteTextFormat, max_w: f32, max_h: f32) -> Option<Text> {
        unsafe {
            let w: Vec<u16> = text.encode_utf16().collect();
            let l = self.dw.CreateTextLayout(&w, fmt, max_w, max_h).ok()?;
            let mut m = DWRITE_TEXT_METRICS::default();
            l.GetMetrics(&mut m).ok()?;
            Some(Text { l, w: m.widthIncludingTrailingWhitespace, h: m.height })
        }
    }

    /// Decodes an image, center-crops it to a square and scales it to `px`.
    /// Returns premultiplied BGRA pixels.
    pub fn decode_square(&self, bytes: &[u8], px: u32) -> Option<(Vec<u8>, u32)> {
        unsafe {
            let stream = self.wic.CreateStream().ok()?;
            stream.InitializeFromMemory(bytes).ok()?;
            let dec =
                self.wic.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).ok()?;
            let frame = dec.GetFrame(0).ok()?;
            let (mut w, mut h) = (0u32, 0u32);
            frame.GetSize(&mut w, &mut h).ok()?;
            let side = w.min(h);
            if side == 0 {
                return None;
            }
            let clip = self.wic.CreateBitmapClipper().ok()?;
            let rc = WICRect {
                X: ((w - side) / 2) as i32,
                Y: ((h - side) / 2) as i32,
                Width: side as i32,
                Height: side as i32,
            };
            clip.Initialize(&frame, &rc).ok()?;
            let px = px.clamp(16, side.max(16));
            let sc = self.wic.CreateBitmapScaler().ok()?;
            sc.Initialize(&clip, px, px, WICBitmapInterpolationModeHighQualityCubic).ok()?;
            let conv = self.wic.CreateFormatConverter().ok()?;
            conv.Initialize(
                &sc,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeMedianCut,
            )
            .ok()?;
            let mut buf = vec![0u8; (px * px * 4) as usize];
            conv.CopyPixels(std::ptr::null(), px * 4, &mut buf).ok()?;
            Some((buf, px))
        }
    }

    /// Writes premultiplied BGRA pixels to a PNG file (used for screenshots).
    pub fn save_png(&self, w: u32, h: u32, stride: u32, pixels: &[u8], path: &str) -> Result<()> {
        unsafe {
            let bmp = self.wic.CreateBitmapFromMemory(w, h, &GUID_WICPixelFormat32bppPBGRA, stride, pixels)?;
            self.save_source(&bmp.cast()?, w, h, path)
        }
    }

    pub fn save_source(&self, src: &IWICBitmapSource, w: u32, h: u32, path: &str) -> Result<()> {
        unsafe {
            let stream = self.wic.CreateStream()?;
            stream.InitializeFromFilename(&HSTRING::from(path), GENERIC_WRITE.0)?;
            let enc = self.wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
            enc.Initialize(&stream, WICBitmapEncoderNoCache)?;
            let mut frame = None;
            enc.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
            let frame = frame.ok_or_else(|| Error::from(E_FAIL))?;
            frame.Initialize(None)?;
            frame.SetSize(w, h)?;
            let mut fmt = GUID_WICPixelFormat32bppBGRA;
            frame.SetPixelFormat(&mut fmt)?;
            frame.WriteSource(src, std::ptr::null())?;
            frame.Commit()?;
            enc.Commit()
        }
    }
}

fn pick_family(dw: &IDWriteFactory) -> HSTRING {
    unsafe {
        let mut coll = None;
        if dw.GetSystemFontCollection(&mut coll, false).is_ok() {
            if let Some(c) = coll {
                for name in [w!("Segoe UI Variable Text"), w!("Segoe UI")] {
                    let (mut idx, mut exists) = (0u32, BOOL(0));
                    if c.FindFamilyName(name, &mut idx, &mut exists).is_ok() && exists.as_bool() {
                        return HSTRING::from_wide(name.as_wide());
                    }
                }
            }
        }
        HSTRING::from("Segoe UI")
    }
}

pub struct Text {
    pub l: IDWriteTextLayout,
    pub w: f32,
    pub h: f32,
}

// ------------------------------------------------------------------ geometry helpers

pub fn v2(x: f32, y: f32) -> Vector2 {
    Vector2 { X: x, Y: y }
}

pub fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}

pub fn rr(r: D2D_RECT_F, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }
}

pub fn contains(r: &D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x <= r.right && y >= r.top && y <= r.bottom
}

/// `0xRRGGBB` + alpha → D2D color.
pub fn hex(c: u32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((c >> 16) & 255) as f32 / 255.0,
        g: ((c >> 8) & 255) as f32 / 255.0,
        b: (c & 255) as f32 / 255.0,
        a,
    }
}

pub fn rgb(c: [f32; 3], a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c[0], g: c[1], b: c[2], a }
}

pub fn scale(k: f32) -> Matrix3x2 {
    Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: 0.0, M32: 0.0 }
}

pub fn identity() -> Matrix3x2 {
    scale(1.0)
}

pub fn rt_props(software: bool) -> D2D1_RENDER_TARGET_PROPERTIES {
    D2D1_RENDER_TARGET_PROPERTIES {
        r#type: if software { D2D1_RENDER_TARGET_TYPE_SOFTWARE } else { D2D1_RENDER_TARGET_TYPE_DEFAULT },
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.0,
        dpiY: 96.0,
        usage: D2D1_RENDER_TARGET_USAGE_NONE,
        minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
    }
}

pub fn bitmap_from_pixels(rt: &ID2D1RenderTarget, px: &[u8], n: u32) -> Option<ID2D1Bitmap> {
    unsafe {
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        rt.CreateBitmap(D2D_SIZE_U { width: n, height: n }, Some(px.as_ptr() as *const _), n * 4, &props).ok()
    }
}

/// Draws a bitmap clipped to a rounded rectangle (via a bitmap brush - no layers).
pub fn draw_bitmap_rounded(rt: &ID2D1RenderTarget, bmp: &ID2D1Bitmap, r: D2D_RECT_F, radius: f32, opacity: f32) {
    if opacity <= 0.002 {
        return;
    }
    unsafe {
        let size = bmp.GetPixelSize();
        let Ok(brush) = rt.CreateBitmapBrush(bmp, None, None) else { return };
        let sx = (r.right - r.left) / size.width.max(1) as f32;
        let sy = (r.bottom - r.top) / size.height.max(1) as f32;
        brush.SetTransform(&Matrix3x2 { M11: sx, M12: 0.0, M21: 0.0, M22: sy, M31: r.left, M32: r.top });
        brush.SetOpacity(opacity);
        brush.SetInterpolationMode(D2D1_BITMAP_INTERPOLATION_MODE_LINEAR);
        rt.FillRoundedRectangle(&rr(r, radius), &brush);
    }
}

pub fn triangle(f: &ID2D1Factory, a: Vector2, b: Vector2, c: Vector2) -> Option<ID2D1PathGeometry> {
    unsafe {
        let g = f.CreatePathGeometry().ok()?;
        let s = g.Open().ok()?;
        s.BeginFigure(a, D2D1_FIGURE_BEGIN_FILLED);
        s.AddLine(b);
        s.AddLine(c);
        s.EndFigure(D2D1_FIGURE_END_CLOSED);
        s.Close().ok()?;
        Some(g)
    }
}

/// Three vivid colors from the cover, for the multicolor waveform.
pub fn palette(px: &[u8], n: u32) -> [[f32; 3]; 3] {
    let mut buckets = [[0f32; 5]; 12];
    let step = (n / 24).max(1) as usize;
    let n = n as usize;
    for y in (0..n).step_by(step) {
        for x in (0..n).step_by(step) {
            let i = (y * n + x) * 4;
            let (b, g, r) = (px[i] as f32 / 255.0, px[i + 1] as f32 / 255.0, px[i + 2] as f32 / 255.0);
            let (h, s, v) = rgb_hsv(r, g, b);
            if v < 0.2 {
                continue;
            }
            let bk = &mut buckets[((h * 12.0) as usize).min(11)];
            bk[0] += s * v;
            bk[1] += r;
            bk[2] += g;
            bk[3] += b;
            bk[4] += 1.0;
        }
    }
    let mut order: Vec<&[f32; 5]> = buckets.iter().filter(|b| b[4] > 0.0 && b[0] > 0.3).collect();
    order.sort_by(|a, b| b[0].total_cmp(&a[0]));
    let vivid = |b: &[f32; 5]| {
        let (h, s, v) = rgb_hsv(b[1] / b[4], b[2] / b[4], b[3] / b[4]);
        (h, (s * 1.15 + 0.12).min(1.0), v.max(0.88))
    };
    let Some(first) = order.first() else { return [DEFAULT_ACCENT; 3] };
    let (h0, s0, v0) = vivid(first);
    let mut out = [hsv_rgb(h0, s0, v0); 3];
    for (i, slot) in out.iter_mut().enumerate().skip(1) {
        *slot = match order.get(i) {
            Some(b) => {
                let (h, s, v) = vivid(b);
                hsv_rgb(h, s, v)
            }
            // Not enough distinct colors: derive neighbours of the main hue.
            None => {
                let shift = if i == 1 { 0.07 } else { -0.07 };
                hsv_rgb((h0 + shift).rem_euclid(1.0), s0, v0)
            }
        };
    }
    out
}

pub const DEFAULT_ACCENT: [f32; 3] = [0.92, 0.92, 0.94];

fn rgb_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        ((g - b) / d).rem_euclid(6.0) / 6.0
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    (h, if max == 0.0 { 0.0 } else { d / max }, max)
}

fn hsv_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - f * s), v * (1.0 - (1.0 - f) * s));
    match i as i32 % 6 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

// ------------------------------------------------------------------ monitors

#[derive(Clone)]
pub struct Mon {
    pub name: String,
    pub rect: RECT,
    pub work: RECT,
    pub primary: bool,
    pub dpi: u32,
}

impl Mon {
    pub fn width(&self) -> i32 {
        self.rect.right - self.rect.left
    }
    pub fn height(&self) -> i32 {
        self.rect.bottom - self.rect.top
    }
}

pub fn mon_info(h: HMONITOR) -> Option<Mon> {
    unsafe {
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(h, &mut mi as *mut _ as *mut MONITORINFO).as_bool() {
            return None;
        }
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        let end = mi.szDevice.iter().position(|&c| c == 0).unwrap_or(mi.szDevice.len());
        Some(Mon {
            name: String::from_utf16_lossy(&mi.szDevice[..end]),
            rect: mi.monitorInfo.rcMonitor,
            work: mi.monitorInfo.rcWork,
            primary: mi.monitorInfo.dwFlags & 1 != 0,
            dpi: dx,
        })
    }
}

pub fn monitors() -> Vec<Mon> {
    unsafe extern "system" fn cb(h: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> BOOL {
        let v = unsafe { &mut *(lp.0 as *mut Vec<Mon>) };
        if let Some(m) = mon_info(h) {
            v.push(m);
        }
        BOOL(1)
    }
    let mut v: Vec<Mon> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut v as *mut _ as isize));
    }
    // Stable order: left to right.
    v.sort_by_key(|m| (m.rect.left, m.rect.top));
    v
}

pub fn pick_monitor(cfg: &Config) -> Mon {
    let all = monitors();
    all.iter()
        .find(|m| m.name == cfg.monitor)
        .or_else(|| all.iter().find(|m| m.primary))
        .or(all.first())
        .cloned()
        .unwrap_or(Mon {
            name: String::new(),
            rect: RECT { left: 0, top: 0, right: 1920, bottom: 1080 },
            work: RECT { left: 0, top: 0, right: 1920, bottom: 1040 },
            primary: true,
            dpi: 96,
        })
}
