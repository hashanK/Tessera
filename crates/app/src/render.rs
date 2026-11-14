//! Draws groups with Direct2D/DirectWrite into a 32-bpp DIB and hands the result to the
//! compositor with UpdateLayeredWindow. Nothing is drawn unless something changed.
//!
//! The look of a group comes entirely from its `Style` (see `tessera_core::look`). Expensive
//! pieces are computed once and cached: the grain texture, each shadow shape, each recoloured
//! icon. The wallpaper behind wallpaper materials is blurred per paint, from a small capture.
use crate::backdrop::{self, Backdrop};
use crate::shell::IconData;
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use tessera_core::layout::{self, Metrics};
use tessera_core::look::*;
use tessera_core::{Rect, Style, TabStyle, Theme, Tint};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::{Matrix3x2, Vector2};

pub struct ItemView<'a> {
    pub key: &'a str,
    pub name: &'a str,
}

pub struct FenceView<'a> {
    /// Size of the group itself; the window is larger by `m.margin` on every side.
    pub w: i32,
    pub h: i32,
    pub m: Metrics,
    pub title: &'a str,
    pub style: &'a Style,
    pub theme: Theme,
    pub collapsed: bool,
    pub items: Vec<ItemView<'a>>,
    pub scroll: i32,
    pub hover: Option<usize>,
    /// Selected items, by position.
    pub selected: Vec<usize>,
    pub insert: Option<usize>,
    /// Pointer is over the group: show secondary chrome.
    pub hot: bool,
    /// A portal that has been navigated into a subfolder: show a back chevron before the title.
    pub back: bool,
    /// Tab names when the group has more than one tab; empty otherwise.
    pub tabs: Vec<&'a str>,
    pub active_tab: usize,
    /// Tab under the pointer (or under a drag).
    pub tab_hover: Option<usize>,
    pub tab_style: TabStyle,
    /// Screen position of the group's top-left corner, to find the wallpaper behind it.
    pub origin: (i32, i32),
    /// Items mid-animation: (hover, selection, burst) progress. Others are drawn at rest.
    pub anim: HashMap<usize, (f32, f32, f32)>,
}

/// What a paint produced that the window needs for input handling.
#[derive(Default)]
pub struct Painted {
    /// Items whose icon is missing or the wrong size.
    pub missing: Vec<usize>,
    /// Horizontal extent (x, width) of each tab in the title bar, in group pixels.
    pub tabs: Vec<(i32, i32)>,
}

#[derive(Clone)]
struct Fonts {
    title: IDWriteTextFormat,
    label: IDWriteTextFormat,
    count: IDWriteTextFormat,
    back: IDWriteTextFormat,
    tab: IDWriteTextFormat,
}

struct Icon {
    size: i32,
    w: u32,
    h: u32,
    /// Original premultiplied pixels, kept to make recoloured variants.
    raw: Vec<u8>,
    bitmap: ID2D1Bitmap,
    /// The icon as recoloured for one group look: (look key, bitmap).
    variant: Option<(u64, ID2D1Bitmap)>,
}

/// The colours a paint uses, resolved from the style and theme.
struct Palette {
    ink: [u8; 3],
    title: [u8; 3],
    secondary: [u8; 3],
    accent: [u8; 3],
    surface: [u8; 3],
    /// The background reads as light: draw dark text and light shadows.
    light: bool,
}

pub struct Renderer {
    rt: ID2D1DCRenderTarget,
    factory: ID2D1Factory,
    brush: ID2D1SolidColorBrush,
    dwrite: IDWriteFactory,
    fonts: HashMap<String, Fonts>,
    icons: HashMap<String, Icon>,
    /// Keys with an icon request in flight.
    pub pending: HashSet<String>,
    /// The wallpaper, for wallpaper materials; `None` until captured (or if capture failed).
    pub backdrop: Option<Backdrop>,
    noise: Option<ID2D1Bitmap>,
    dash: Option<ID2D1StrokeStyle>,
    shadows: HashMap<String, ID2D1Bitmap>,
}

const ACCENT: [u8; 3] = [0x5B, 0x8D, 0xEF];
const DARK_INK: [u8; 3] = [0x16, 0x18, 0x1D];
const PIXEL_FORMAT: D2D1_PIXEL_FORMAT = D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED };

fn color(rgb: [u8; 3], a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: rgb[0] as f32 / 255.0, g: rgb[1] as f32 / 255.0, b: rgb[2] as f32 / 255.0, a }
}

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    [0, 1, 2].map(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8)
}

/// Relative luminance, 0..1.
fn luminance(c: [u8; 3]) -> f32 {
    (0.2126 * c[0] as f32 + 0.7152 * c[1] as f32 + 0.0722 * c[2] as f32) / 255.0
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}

fn rounded(x: f32, y: f32, w: f32, h: f32, r: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: rect(x, y, w, h), radiusX: r, radiusY: r }
}

fn matrix(sx: f32, sy: f32, dx: f32, dy: f32) -> Matrix3x2 {
    Matrix3x2 { M11: sx, M12: 0.0, M21: 0.0, M22: sy, M31: dx, M32: dy }
}

fn weight(w: u16) -> DWRITE_FONT_WEIGHT {
    DWRITE_FONT_WEIGHT(w.clamp(100, 900) as i32)
}

fn alignment(a: Align) -> DWRITE_TEXT_ALIGNMENT {
    match a {
        Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
        Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
        Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
    }
}

/// Coverage (0..1) of a rounded rectangle at a point, anti-aliased over one pixel.
fn coverage(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32, r: f32) -> f32 {
    let (hw, hh) = (w / 2.0, h / 2.0);
    let r = r.min(hw).min(hh).max(0.0);
    let qx = (px - (x + hw)).abs() - (hw - r);
    let qy = (py - (y + hh)).abs() - (hh - r);
    let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r;
    (0.5 - d).clamp(0.0, 1.0)
}

/// A memory DC with a top-down 32-bpp DIB selected into it.
struct Surface {
    dc: HDC,
    dib: HBITMAP,
    old: HGDIOBJ,
    w: i32,
    h: i32,
}

impl Surface {
    unsafe fn new(w: i32, h: i32) -> Result<Surface> {
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        ReleaseDC(None, screen);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, ..Default::default() },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let dib = match CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(dib) => dib,
            Err(e) => {
                let _ = DeleteDC(dc);
                return Err(e);
            }
        };
        let old = SelectObject(dc, dib.into());
        Ok(Surface { dc, dib, old, w, h })
    }

    /// Push the pixels to a layered window.
    unsafe fn present(&self, hwnd: HWND) {
        let size = SIZE { cx: self.w, cy: self.h };
        let blend = BLENDFUNCTION { BlendOp: 0, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: 1 };
        let _ = UpdateLayeredWindow(hwnd, None, None, Some(&size), Some(self.dc), Some(&POINT::default()), COLORREF(0), Some(&blend), ULW_ALPHA);
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.dib.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// Recolour premultiplied BGRA: saturation 0..1, then wash towards `tint` by `amount`.
fn recolour(px: &[u8], saturation: f32, tint: Option<([u8; 3], f32)>) -> Vec<u8> {
    let mut out = px.to_vec();
    for p in out.chunks_exact_mut(4) {
        let a = p[3] as f32;
        let (b, g, r) = (p[0] as f32, p[1] as f32, p[2] as f32);
        let grey = 0.0722 * b + 0.7152 * g + 0.2126 * r;
        let mut c = [b, g, r].map(|v| grey + (v - grey) * saturation);
        if let Some((t, amount)) = tint {
            // Tint in premultiplied space: the target colour scaled by this pixel's alpha.
            let target = [t[2], t[1], t[0]].map(|v| v as f32 * a / 255.0);
            for i in 0..3 {
                c[i] += (target[i] - c[i]) * amount;
            }
        }
        for i in 0..3 {
            p[i] = c[i].round().clamp(0.0, a) as u8;
        }
    }
    out
}

impl Renderer {
    pub fn new() -> Result<Renderer> {
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            // Software target: surfaces are small and static, and this keeps a GPU device (and its memory) out of an idle process.
            let props = D2D1_RENDER_TARGET_PROPERTIES { r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE, pixelFormat: PIXEL_FORMAT, dpiX: 96.0, dpiY: 96.0, ..Default::default() };
            let rt = factory.CreateDCRenderTarget(&props)?;
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let brush = rt.CreateSolidColorBrush(&color([0, 0, 0], 1.0), None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            Ok(Renderer {
                rt,
                factory,
                brush,
                dwrite,
                fonts: HashMap::new(),
                icons: HashMap::new(),
                pending: HashSet::new(),
                backdrop: None,
                noise: None,
                dash: None,
                shadows: HashMap::new(),
            })
        }
    }

    unsafe fn fonts(&mut self, style: &Style, m: &Metrics) -> Result<Fonts> {
        let (t, x, scale) = (&style.title, &style.text, m.scale);
        let key = format!("{}|{scale}|{}|{}|{:?}|{}|{}|{:?}|{}|{}", x.family(), t.size, t.weight, t.align, x.size, x.weight, x.align, x.line_height, m.label_right);
        if let Some(f) = self.fonts.get(&key) {
            return Ok(f.clone());
        }
        if self.fonts.len() > 24 {
            self.fonts.clear();
        }
        let family = HSTRING::from(x.family());
        let make = |w: DWRITE_FONT_WEIGHT, size: f32, wrap: bool, align: DWRITE_TEXT_ALIGNMENT| -> Result<IDWriteTextFormat> {
            // An unknown family falls back to the system font inside DirectWrite.
            let f = self.dwrite.CreateTextFormat(&family, None, w, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size * scale, w!("en-us"))?;
            f.SetTextAlignment(align)?;
            f.SetWordWrapping(if wrap { DWRITE_WORD_WRAPPING_WRAP } else { DWRITE_WORD_WRAPPING_NO_WRAP })?;
            let sign = self.dwrite.CreateEllipsisTrimmingSign(&f)?;
            f.SetTrimming(&DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 }, &sign)?;
            Ok(f)
        };
        let title = make(weight(t.weight), t.size, false, alignment(t.align))?;
        title.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        let count = make(DWRITE_FONT_WEIGHT_NORMAL, (t.size - 1.5).max(9.0), false, DWRITE_TEXT_ALIGNMENT_TRAILING)?;
        count.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        let back = make(DWRITE_FONT_WEIGHT_NORMAL, 20.0, false, DWRITE_TEXT_ALIGNMENT_LEADING)?;
        back.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        let tab = make(weight(t.weight), t.size, false, DWRITE_TEXT_ALIGNMENT_CENTER)?;
        tab.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        let label_align = if m.label_right && x.align == Align::Center { Align::Left } else { x.align };
        let label = make(weight(x.weight), x.size, true, alignment(label_align))?;
        let line = x.line_px() * scale;
        label.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, line, line * 0.8)?;
        if m.label_right {
            label.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        }
        let fonts = Fonts { title, label, count, back, tab };
        self.fonts.insert(key, fonts.clone());
        Ok(fonts)
    }

    pub fn set_icon(&mut self, data: IconData) {
        self.pending.remove(&data.key);
        if let Some(bitmap) = unsafe { self.bitmap(data.w, data.h, &data.bgra) } {
            self.icons.insert(data.key, Icon { size: data.size, w: data.w, h: data.h, raw: data.bgra, bitmap, variant: None });
        }
    }

    pub fn forget_icon(&mut self, key: &str) {
        self.icons.remove(key);
        self.pending.remove(key);
    }

    unsafe fn bitmap(&self, w: u32, h: u32, bgra: &[u8]) -> Option<ID2D1Bitmap> {
        let props = D2D1_BITMAP_PROPERTIES { pixelFormat: PIXEL_FORMAT, dpiX: 96.0, dpiY: 96.0 };
        self.rt.CreateBitmap(D2D_SIZE_U { width: w, height: h }, Some(bgra.as_ptr() as *const c_void), w * 4, &props).ok()
    }

    /// The icon to draw for `key` under this look: the original, or a cached recoloured copy.
    unsafe fn icon_for(&mut self, key: &str, look: &IconLook) -> Option<(i32, ID2D1Bitmap)> {
        let recolours = look.recolours();
        let tint = look.tint.map(|t| t.0).filter(|_| look.tint_amount > 0);
        let look_key = (look.saturation as u64) << 32 | (look.tint_amount as u64) << 24 | tint.map(|t| (t[0] as u64) << 16 | (t[1] as u64) << 8 | t[2] as u64).unwrap_or(0);
        let icon = self.icons.get(key)?;
        if !recolours {
            return Some((icon.size, icon.bitmap.clone()));
        }
        if let Some((k, b)) = &icon.variant {
            if *k == look_key {
                return Some((icon.size, b.clone()));
            }
        }
        let px = recolour(&icon.raw, look.saturation as f32 / 100.0, tint.map(|t| (t, look.tint_amount.min(MAX_ICON_TINT) as f32 / 100.0)));
        let bitmap = self.bitmap(icon.w, icon.h, &px)?;
        let icon = self.icons.get_mut(key)?;
        icon.variant = Some((look_key, bitmap.clone()));
        Some((icon.size, bitmap))
    }

    unsafe fn fill(&self, shape: &D2D1_ROUNDED_RECT, c: D2D1_COLOR_F) {
        self.brush.SetColor(&c);
        self.rt.FillRoundedRectangle(shape, &self.brush);
    }

    unsafe fn stroke(&self, shape: &D2D1_ROUNDED_RECT, c: D2D1_COLOR_F, width: f32) {
        self.brush.SetColor(&c);
        self.rt.DrawRoundedRectangle(shape, &self.brush, width, None);
    }

    unsafe fn text(&self, s: &str, font: &IDWriteTextFormat, area: D2D_RECT_F, c: D2D1_COLOR_F) {
        let wide: Vec<u16> = s.encode_utf16().collect();
        self.brush.SetColor(&c);
        self.rt.DrawText(&wide, font, &area, &self.brush, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
    }

    /// Text with extra letter spacing needs a text layout of its own.
    unsafe fn spaced_text(&self, s: &str, font: &IDWriteTextFormat, area: D2D_RECT_F, c: D2D1_COLOR_F, spacing: f32) {
        if spacing.abs() < 0.05 {
            return self.text(s, font, area, c);
        }
        let wide: Vec<u16> = s.encode_utf16().collect();
        let Ok(layout) = self.dwrite.CreateTextLayout(&wide, font, (area.right - area.left).max(1.0), (area.bottom - area.top).max(1.0)) else { return };
        if let Ok(l1) = layout.cast::<IDWriteTextLayout1>() {
            let _ = l1.SetCharacterSpacing(0.0, spacing, 0.0, DWRITE_TEXT_RANGE { startPosition: 0, length: wide.len() as u32 });
        }
        self.brush.SetColor(&c);
        self.rt.PushAxisAlignedClip(&area, D2D1_ANTIALIAS_MODE_ALIASED);
        self.rt.DrawTextLayout(Vector2 { X: area.left, Y: area.top }, &layout, &self.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
        self.rt.PopAxisAlignedClip();
    }

    unsafe fn begin(&self, surface: &Surface) -> Result<()> {
        self.rt.BindDC(surface.dc, &RECT { left: 0, top: 0, right: surface.w, bottom: surface.h })?;
        self.rt.BeginDraw();
        self.rt.SetTransform(&Matrix3x2::identity());
        self.rt.Clear(Some(&D2D1_COLOR_F::default()));
        Ok(())
    }

    /// Draw one group.
    pub fn paint_fence(&mut self, hwnd: HWND, v: &FenceView) -> Painted {
        match unsafe { self.paint_fence_inner(hwnd, v) } {
            Ok(painted) => painted,
            Err(e) => {
                crate::log(&format!("paint failed: {e}"));
                Painted::default()
            }
        }
    }

    /// Lay the tabs out across the title bar: equal shares, or each as wide as its name.
    unsafe fn tab_layout(&self, v: &FenceView, font: &IDWriteTextFormat, x0: f32, x1: f32) -> Vec<(f32, f32)> {
        let n = v.tabs.len();
        let avail = (x1 - x0).max(1.0);
        if v.tab_style == TabStyle::Fill {
            let w = avail / n as f32;
            return (0..n).map(|i| (x0 + w * i as f32, w)).collect();
        }
        let gap = 2.0 * v.m.scale;
        let mut widths: Vec<f32> = v
            .tabs
            .iter()
            .map(|t| {
                let wide: Vec<u16> = t.encode_utf16().collect();
                let text = self.dwrite.CreateTextLayout(&wide, font, 4096.0, 64.0).and_then(|l| {
                    let mut m = DWRITE_TEXT_METRICS::default();
                    l.GetMetrics(&mut m).map(|_| m.width)
                });
                text.unwrap_or(40.0) + 22.0 * v.m.scale
            })
            .collect();
        let total: f32 = widths.iter().sum::<f32>() + gap * (n as f32 - 1.0);
        if total > avail {
            let k = (avail - gap * (n as f32 - 1.0)).max(1.0) / widths.iter().sum::<f32>();
            widths.iter_mut().for_each(|w| *w *= k);
        }
        let mut x = x0;
        widths
            .into_iter()
            .map(|w| {
                let r = (x, w);
                x += w + gap;
                r
            })
            .collect()
    }

    /// The material that can actually be drawn: wallpaper materials need a wallpaper capture.
    fn material(&self, style: &Style) -> Material {
        if style.material.uses_backdrop() && self.backdrop.is_none() {
            Material::Transparent
        } else {
            style.material
        }
    }

    fn palette(&self, style: &Style, theme: Theme) -> Palette {
        let dark = theme == Theme::Dark;
        let base = if dark { [0x14, 0x16, 0x1A] } else { [0xF4, 0xF5, 0xF7] };
        let surface = style.color.map(|c| c.0).unwrap_or_else(|| mix(base, style.tint.rgb(), if dark { 0.16 } else { 0.12 }));
        let accent = style.accent.map(|c| c.0).unwrap_or(if style.tint == Tint::Slate { ACCENT } else { style.tint.rgb() });
        let op = style.opacity as f32 / 100.0;
        // What the text sits on, roughly: decides whether default text is dark or light.
        let behind = match self.material(style) {
            Material::Solid => Some(surface),
            Material::Gradient => {
                let n = style.gradient.stops.len().max(1) as u32;
                let sum = style.gradient.stops.iter().fold([0u32; 3], |a, c| [a[0] + c.0[0] as u32, a[1] + c.0[1] as u32, a[2] + c.0[2] as u32]);
                Some(sum.map(|v| (v / n) as u8))
            }
            _ if op >= 0.4 => Some(surface),
            _ => None,
        };
        let light = behind.map(|c| luminance(c) > 0.55).unwrap_or(!dark);
        let ink = style.text.color.map(|c| c.0).unwrap_or(if light { DARK_INK } else { [0xFF, 0xFF, 0xFF] });
        Palette {
            ink,
            title: style.title.color.map(|c| c.0).unwrap_or(ink),
            secondary: style.text.secondary.map(|c| c.0).unwrap_or(ink),
            accent,
            surface,
            light: luminance(ink) < 0.5,
        }
    }

    /// A tileable grain texture, made once.
    unsafe fn noise(&mut self) -> Option<ID2D1Bitmap> {
        if self.noise.is_none() {
            const N: usize = 128;
            let mut seed: u32 = 0x9E37_79B9;
            let mut px = vec![0u8; N * N * 4];
            for p in px.chunks_exact_mut(4) {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let n = (seed & 0xFF) as f32 / 255.0;
                // Half the grains darken, half lighten; premultiplied.
                let (v, a) = if n < 0.5 { (0.0, (0.5 - n) * 2.0) } else { (1.0, (n - 0.5) * 2.0) };
                let a = a * 0.16;
                let c = (v * a * 255.0) as u8;
                p.copy_from_slice(&[c, c, c, (a * 255.0) as u8]);
            }
            self.noise = self.bitmap(N as u32, N as u32, &px);
        }
        self.noise.clone()
    }

    unsafe fn dash(&mut self) -> Option<ID2D1StrokeStyle> {
        if self.dash.is_none() {
            let props = D2D1_STROKE_STYLE_PROPERTIES { dashStyle: D2D1_DASH_STYLE_CUSTOM, dashCap: D2D1_CAP_STYLE_ROUND, ..Default::default() };
            self.dash = self.factory.CreateStrokeStyle(&props, Some(&[3.0, 2.5])).ok();
        }
        self.dash.clone()
    }

    /// The soft shadow around a group of this size, as a half-resolution alpha bitmap the size
    /// of the whole window. It is cut away under the group, so translucent groups aren't
    /// darkened by their own shadow. Cached per size and settings.
    unsafe fn shadow(&mut self, win_w: i32, win_h: i32, m: &Metrics, w: f32, h: f32, s: &Shadow) -> Option<ID2D1Bitmap> {
        let key = format!("{win_w}x{win_h}|{}|{}|{:?}|{}", m.margin, m.radius, s, m.scale);
        if let Some(b) = self.shadows.get(&key) {
            return Some(b.clone());
        }
        if self.shadows.len() > 32 {
            self.shadows.clear();
        }
        let (hw, hh) = ((win_w + 1) / 2, (win_h + 1) / 2);
        let k = 0.5; // half resolution
        let sc = m.scale * k;
        let mg = m.margin as f32 * k;
        let spread = s.spread as f32 * sc;
        let (sx, sy, sw, sh) = (mg - spread, mg + s.offset as f32 * sc - spread, w * k + 2.0 * spread, h * k + 2.0 * spread);
        let r = m.radius as f32 * k;
        let mut px = vec![0u8; (hw * hh * 4) as usize];
        for y in 0..hh {
            for x in 0..hw {
                let c = coverage(x as f32 + 0.5, y as f32 + 0.5, sx, sy, sw, sh, r + spread.max(0.0));
                px[((y * hw + x) * 4 + 3) as usize] = (c * 255.0) as u8;
            }
        }
        let blur = ((s.blur as f32 * sc) / 1.7).round() as usize;
        for _ in 0..3 {
            backdrop::box_blur(&mut px, hw as usize, hh as usize, blur);
        }
        let strength = s.intensity as f32 / 100.0 * 0.9;
        for y in 0..hh {
            for x in 0..hw {
                let i = ((y * hw + x) * 4) as usize;
                let under = coverage(x as f32 + 0.5, y as f32 + 0.5, mg, mg, w * k, h * k, r);
                let a = px[i + 3] as f32 * strength * (1.0 - under);
                px[i..i + 4].copy_from_slice(&[0, 0, 0, a as u8]);
            }
        }
        let bitmap = self.bitmap(hw as u32, hh as u32, &px)?;
        self.shadows.insert(key, bitmap.clone());
        Some(bitmap)
    }

    unsafe fn gradient_brush(&self, g: &Gradient, w: f32, h: f32, opacity: f32) -> Option<ID2D1Brush> {
        let n = g.stops.len().max(2);
        let stops: Vec<D2D1_GRADIENT_STOP> = (0..n)
            .map(|i| D2D1_GRADIENT_STOP { position: i as f32 / (n - 1) as f32, color: color(g.stops.get(i).or(g.stops.last()).map(|c| c.0).unwrap_or([0x20, 0x24, 0x30]), 1.0) })
            .collect();
        let collection = self.rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP).ok()?;
        let props = D2D1_BRUSH_PROPERTIES { opacity, transform: Matrix3x2::identity() };
        if g.radial {
            let p = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES { center: Vector2 { X: w / 2.0, Y: h / 2.0 }, gradientOriginOffset: Vector2 { X: 0.0, Y: 0.0 }, radiusX: w * 0.75, radiusY: h * 0.75 };
            return self.rt.CreateRadialGradientBrush(&p, Some(&props), &collection).ok().map(|b| b.into());
        }
        let a = (g.angle as f32).to_radians();
        let (dx, dy) = (a.cos(), a.sin());
        let half = (w * dx.abs() + h * dy.abs()) / 2.0;
        let (cx, cy) = (w / 2.0, h / 2.0);
        let p = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: Vector2 { X: cx - dx * half, Y: cy - dy * half }, endPoint: Vector2 { X: cx + dx * half, Y: cy + dy * half } };
        self.rt.CreateLinearGradientBrush(&p, Some(&props), &collection).ok().map(|b| b.into())
    }

    /// The background: material, grain, inner shadow.
    unsafe fn paint_background(&mut self, v: &FenceView, pal: &Palette, panel: &D2D1_ROUNDED_RECT, w: f32, h: f32) {
        let style = v.style;
        let op = style.opacity as f32 / 100.0;
        let material = self.material(style);
        match material {
            Material::Transparent => self.fill(panel, color(pal.surface, op)),
            Material::Solid => self.fill(panel, color(pal.surface, 1.0)),
            Material::Gradient => match self.gradient_brush(&style.gradient, w, h, op) {
                Some(brush) => self.rt.FillRoundedRectangle(panel, &brush),
                None => self.fill(panel, color(pal.surface, op)),
            },
            Material::Acrylic | Material::Mica | Material::Frosted => {
                // Blur radius in screen pixels: Mica is always very soft.
                let radius = match material {
                    Material::Mica => 90.0,
                    _ => 4.0 + style.blur as f32 * 0.6,
                } * v.m.scale;
                let screen = Rect { x: v.origin.0, y: v.origin.1, w: v.w, h: v.h };
                let piece = self.backdrop.as_ref().and_then(|b| b.piece(screen, radius));
                let drawn = piece.and_then(|p| {
                    let bitmap = self.bitmap(p.w, p.h, &p.bgra)?;
                    let (sx, sy) = (w / (p.src.2 - p.src.0), h / (p.src.3 - p.src.1));
                    let bp = D2D1_BITMAP_BRUSH_PROPERTIES { extendModeX: D2D1_EXTEND_MODE_CLAMP, extendModeY: D2D1_EXTEND_MODE_CLAMP, interpolationMode: D2D1_BITMAP_INTERPOLATION_MODE_LINEAR };
                    let props = D2D1_BRUSH_PROPERTIES { opacity: 1.0, transform: matrix(sx, sy, -p.src.0 * sx, -p.src.1 * sy) };
                    let brush = self.rt.CreateBitmapBrush(&bitmap, Some(&bp), Some(&props)).ok()?;
                    self.rt.FillRoundedRectangle(panel, &brush);
                    Some(())
                });
                if drawn.is_none() {
                    self.fill(panel, color(pal.surface, op.max(0.75)));
                    return;
                }
                match material {
                    // A pale veil first, so the frost reads as frost even with a dark tint.
                    Material::Frosted => {
                        self.fill(panel, color([0xFF, 0xFF, 0xFF], 0.10));
                        self.fill(panel, color(pal.surface, op));
                    }
                    _ => self.fill(panel, color(pal.surface, op)),
                }
            }
        }
        if style.noise > 0 {
            if let Some(noise) = self.noise() {
                let bp = D2D1_BITMAP_BRUSH_PROPERTIES { extendModeX: D2D1_EXTEND_MODE_WRAP, extendModeY: D2D1_EXTEND_MODE_WRAP, interpolationMode: D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR };
                let props = D2D1_BRUSH_PROPERTIES { opacity: style.noise as f32 / 100.0, transform: Matrix3x2::identity() };
                if let Ok(brush) = self.rt.CreateBitmapBrush(&noise, Some(&bp), Some(&props)) {
                    self.rt.FillRoundedRectangle(panel, &brush);
                }
            }
        }
        if style.border.inner_shadow {
            let s = v.m.scale;
            for i in 1..=6 {
                let d = i as f32 * s;
                let r = (panel.radiusX - d).max(0.0);
                let ring = rounded(panel.rect.left + d, panel.rect.top + d, w - 1.0 - 2.0 * d, h - 1.0 - 2.0 * d, r);
                self.stroke(&ring, color([0, 0, 0], 0.07 * (1.0 - i as f32 / 7.0)), s);
            }
        }
    }

    /// Border, inner line and top highlight.
    unsafe fn paint_border(&mut self, style: &Style, pal: &Palette, w: f32, h: f32, radius: f32, scale: f32) {
        let b = &style.border;
        if b.width > 0.0 && b.opacity > 0 {
            let width = (b.width * scale).max(1.0);
            let inset = width / 2.0;
            let shape = rounded(inset, inset, w - width, h - width, (radius - inset).max(0.0));
            self.brush.SetColor(&color(b.color.map(|c| c.0).unwrap_or(pal.ink), b.opacity as f32 / 100.0));
            let dash = if b.line == Line::Dashed { self.dash() } else { None };
            self.rt.DrawRoundedRectangle(&shape, &self.brush, width, dash.as_ref());
        }
        let edge = (b.width * scale).max(if b.width > 0.0 { 1.0 } else { 0.0 });
        if b.inner {
            let d = edge + scale;
            let shape = rounded(d + 0.5, d + 0.5, w - 2.0 * d - 1.0, h - 2.0 * d - 1.0, (radius - d).max(0.0));
            self.stroke(&shape, color([0xFF, 0xFF, 0xFF], if pal.light { 0.35 } else { 0.06 }), 1.0);
        }
        if b.highlight {
            // Light along the top edge, fading out downwards.
            let stops = [
                D2D1_GRADIENT_STOP { position: 0.0, color: color([0xFF, 0xFF, 0xFF], if pal.light { 0.75 } else { 0.32 }) },
                D2D1_GRADIENT_STOP { position: 1.0, color: color([0xFF, 0xFF, 0xFF], 0.0) },
            ];
            if let Ok(collection) = self.rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) {
                let p = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: Vector2 { X: 0.0, Y: 0.0 }, endPoint: Vector2 { X: 0.0, Y: (radius * 2.5).max(24.0 * scale).min(h) } };
                if let Ok(brush) = self.rt.CreateLinearGradientBrush(&p, None, &collection) {
                    let d = edge;
                    let shape = rounded(d + 0.5, d + 0.5, w - 2.0 * d - 1.0, h - 2.0 * d - 1.0, (radius - d).max(0.0));
                    self.rt.DrawRoundedRectangle(&shape, &brush, scale.max(1.0), None);
                }
            }
        }
    }

    unsafe fn paint_fence_inner(&mut self, hwnd: HWND, v: &FenceView) -> Result<Painted> {
        let m = &v.m;
        let style = v.style;
        let s = |px: f32| px * m.scale;
        let fonts = self.fonts(style, m)?;
        let mg = m.margin;
        let surface = Surface::new((v.w + 2 * mg).max(1), (v.h + 2 * mg).max(1))?;
        self.begin(&surface)?;
        let (w, h) = (v.w as f32, v.h as f32);
        let radius = m.radius as f32;
        let title_h = m.title_h as f32;
        let pal = self.palette(style, v.theme);

        if mg > 0 && style.shadow.visible() {
            if let Some(shadow) = self.shadow(surface.w, surface.h, m, w, h, &style.shadow) {
                self.rt.DrawBitmap(&shadow, Some(&rect(0.0, 0.0, surface.w as f32, surface.h as f32)), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
            }
        }
        self.rt.SetTransform(&matrix(1.0, 1.0, mg as f32, mg as f32));

        let panel = rounded(0.5, 0.5, w - 1.0, h - 1.0, radius);
        self.paint_background(v, &pal, &panel, w, h);

        // Title band and separator.
        if style.title.band > 0 {
            self.rt.PushAxisAlignedClip(&rect(0.0, 0.0, w, title_h), D2D1_ANTIALIAS_MODE_ALIASED);
            self.fill(&panel, color(pal.title, style.title.band as f32 / 100.0 * 0.6));
            self.rt.PopAxisAlignedClip();
        }
        if style.title.separator && !v.collapsed {
            self.fill(&rounded(0.0, title_h - 1.0, w, 1.0, 0.0), color(pal.ink, 0.10));
        }
        self.paint_border(style, &pal, w, h, radius, m.scale);

        if v.back {
            self.text("\u{2039}", &fonts.back, rect(s(11.0), -s(2.0), s(16.0), title_h), color(pal.title, if v.hot { 0.95 } else { 0.6 }));
        }
        let mut tab_rects = Vec::new();
        if v.tabs.len() > 1 {
            // Tab strip: the tab on screen is a pill in the accent colour; the others are quiet text.
            let x0 = if v.back { s(30.0) } else { s(5.0) };
            for (i, (x, tw)) in self.tab_layout(v, &fonts.tab, x0, w - s(5.0)).into_iter().enumerate() {
                let pill = rounded(x + s(1.5), s(4.0), (tw - s(3.0)).max(1.0), title_h - s(7.0), s(6.0).min(radius.max(s(3.0))));
                let (active, hover) = (i == v.active_tab, v.tab_hover == Some(i));
                if active {
                    self.fill(&pill, color(pal.accent, if pal.light { 0.32 } else { 0.62 }));
                } else if hover {
                    self.fill(&pill, color(pal.title, 0.09));
                }
                let label = rect(x + s(7.0), 0.0, (tw - s(14.0)).max(1.0), title_h - s(1.0));
                if active || hover {
                    self.text(v.tabs[i], &fonts.tab, label, color(pal.title, if active { 0.98 } else { 0.85 }));
                } else {
                    self.text(v.tabs[i], &fonts.tab, label, color(pal.secondary, 0.58));
                }
                tab_rects.push((x.round() as i32, tw.round() as i32));
            }
        } else {
            // Title; the item count is secondary chrome and only shows while the pointer is over the group.
            let count_w = if v.hot || v.collapsed { s(34.0) } else { 0.0 };
            let title_x = if v.back { s(30.0) } else { s(12.0) };
            // A centred title is centred on the whole bar, not on what is left beside the count.
            let title_w = if style.title.align == Align::Left { w - title_x - s(8.0) - count_w } else { w - 2.0 * title_x.max(count_w + s(12.0)) };
            let title_x = if style.title.align == Align::Left { title_x } else { title_x.max(count_w + s(12.0)) };
            self.spaced_text(v.title, &fonts.title, rect(title_x, 0.0, title_w.max(1.0), title_h), color(pal.title, 0.88), style.text.letter_spacing * m.scale);
            if count_w > 0.0 {
                self.text(&v.items.len().to_string(), &fonts.count, rect(w - s(12.0) - count_w, 0.0, count_w, title_h), color(pal.secondary, 0.45));
            }
        }

        let mut missing = Vec::new();
        if !v.collapsed {
            let g = layout::grid(m, v.w, v.items.len());
            let body = rect(1.0, title_h, w - 2.0, (h - title_h - s(4.0)).max(0.0));
            self.rt.PushAxisAlignedClip(&body, D2D1_ANTIALIAS_MODE_ALIASED);
            let look = &style.icons;
            let shadow_text = match style.text.shadow {
                TextShadow::On => true,
                TextShadow::Off => false,
                TextShadow::Auto => !pal.light,
            };
            for (i, item) in v.items.iter().enumerate() {
                let c = layout::cell_rect(m, &g, i, v.scroll);
                if c.y + c.h < m.title_h || c.y > v.h {
                    continue;
                }
                let (cx, cy, cw, ch) = (c.x as f32, c.y as f32, c.w as f32, c.h as f32);
                // Animation progress: hover and selection 0..1, burst 0..1 (1 = none running).
                let (h_raw, s_raw, burst) = v.anim.get(&i).copied().unwrap_or((
                    if v.hover == Some(i) { 1.0 } else { 0.0 },
                    if v.selected.contains(&i) { 1.0 } else { 0.0 },
                    1.0,
                ));
                let (hv, sv) = (ease(h_raw), ease(s_raw));
                let tile = rounded(cx + s(2.0), cy + s(2.0), cw - s(4.0), ch - s(4.0), s(6.0));
                if sv > 0.0 {
                    match look.selection {
                        Selection::Tile => {
                            self.fill(&tile, color(pal.accent, 0.32 * sv));
                            self.stroke(&tile, color(pal.accent, 0.60 * sv), 1.0);
                        }
                        Selection::Outline => self.stroke(&tile, color(pal.accent, 0.9 * sv), s(1.5)),
                        Selection::Underline => {}
                    }
                }
                if hv > 0.0 && look.hover == Hover::Tile {
                    self.fill(&tile, color(pal.ink, 0.09 * hv * (1.0 - sv)));
                }
                if burst < 1.0 && look.select_motion == SelectMotion::Pulse {
                    // A ring that spreads from the tile and fades.
                    let grow = s(7.0) * ease(burst);
                    let ring = rounded(tile.rect.left - grow, tile.rect.top - grow, tile.rect.right - tile.rect.left + 2.0 * grow, tile.rect.bottom - tile.rect.top + 2.0 * grow, s(6.0) + grow);
                    self.stroke(&ring, color(pal.accent, 0.75 * (1.0 - burst)), s(2.0) * (1.0 - burst * 0.5));
                }
                let icon_px = m.icon as f32;
                // Lift: the hover effect, plus Float motion. Scale: Grow and Pop motion, plus a selection bounce.
                let mut lift = if look.hover == Hover::Lift { s(3.0) * hv } else { 0.0 };
                let mut scale = 1.0;
                match look.hover_motion {
                    HoverMotion::Grow => scale += 0.07 * hv,
                    HoverMotion::Float => lift += s(4.0) * hv,
                    HoverMotion::Pop => scale += 0.09 * if h_raw > 0.0 && h_raw < 1.0 && v.hover == Some(i) { ease_back(h_raw) } else { hv },
                    _ => {}
                }
                if burst < 1.0 && look.select_motion == SelectMotion::Pop {
                    scale += 0.14 * (std::f32::consts::PI * burst).sin() * (1.0 - burst * 0.4);
                }
                let base = if m.label_right {
                    rect(cx + s(8.0), cy + (ch - icon_px) / 2.0, icon_px, icon_px)
                } else {
                    rect(cx + (cw - icon_px) / 2.0, cy + s(8.0), icon_px, icon_px)
                };
                let grown = icon_px * scale;
                let icon = rect(base.left - (grown - icon_px) / 2.0, base.top - (grown - icon_px) / 2.0 - lift, grown, grown);
                if hv > 0.0 && look.hover == Hover::Glow {
                    let stops = [
                        D2D1_GRADIENT_STOP { position: 0.0, color: color(pal.accent, 0.55 * hv) },
                        D2D1_GRADIENT_STOP { position: 1.0, color: color(pal.accent, 0.0) },
                    ];
                    if let Ok(collection) = self.rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) {
                        let center = Vector2 { X: (icon.left + icon.right) / 2.0, Y: (icon.top + icon.bottom) / 2.0 };
                        let p = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES { center, gradientOriginOffset: Vector2::default(), radiusX: icon_px * 0.85, radiusY: icon_px * 0.85 };
                        if let Ok(brush) = self.rt.CreateRadialGradientBrush(&p, None, &collection) {
                            self.rt.FillEllipse(&D2D1_ELLIPSE { point: center, radiusX: icon_px * 0.85, radiusY: icon_px * 0.85 }, &brush);
                        }
                    }
                }
                if look.container != Container::None && look.container_opacity > 0 {
                    let d = s(5.0) * scale;
                    let r = if look.container == Container::Circle { (grown + 2.0 * d) / 2.0 } else { grown * 0.26 };
                    let bg = rounded(icon.left - d, icon.top - d, grown + 2.0 * d, grown + 2.0 * d, r);
                    self.fill(&bg, color(pal.ink, look.container_opacity as f32 / 100.0 * if pal.light { 0.6 } else { 1.0 }));
                }
                match self.icon_for(item.key, look) {
                    Some((size, bitmap)) => {
                        self.rt.DrawBitmap(&bitmap, Some(&icon), look.opacity as f32 / 100.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
                        if size != m.icon {
                            missing.push(i);
                        }
                    }
                    None => {
                        self.fill(&D2D1_ROUNDED_RECT { rect: icon, radiusX: s(8.0), radiusY: s(8.0) }, color(pal.ink, 0.07));
                        missing.push(i);
                    }
                }
                let label = if m.label_right {
                    let x = base.right + s(8.0);
                    rect(x, cy + (ch - m.label_h as f32) / 2.0, (cx + cw - x - s(4.0)).max(1.0), m.label_h as f32)
                } else {
                    rect(cx + s(3.0), base.bottom + s(4.0), cw - s(6.0), m.label_h as f32)
                };
                let spacing = style.text.letter_spacing * m.scale;
                if shadow_text {
                    let shade = if pal.light { color([0xFF, 0xFF, 0xFF], 0.55) } else { color([0, 0, 0], 0.45) };
                    let shadow = D2D_RECT_F { top: label.top + 1.0, bottom: label.bottom + 1.0, ..label };
                    self.spaced_text(item.name, &fonts.label, shadow, shade, spacing);
                }
                self.spaced_text(item.name, &fonts.label, label, color(pal.ink, 0.95), spacing);
                if sv > 0.0 && look.selection == Selection::Underline {
                    let bar_w = (cw * 0.4).max(s(12.0)) * sv;
                    self.fill(&rounded(cx + (cw - bar_w) / 2.0, cy + ch - s(4.0), bar_w, s(2.0), s(1.0)), color(pal.accent, 0.95));
                }
            }
            if let Some(index) = v.insert {
                let count = v.items.len();
                let x = if count == 0 {
                    m.pad as f32
                } else if index < count {
                    layout::cell_rect(m, &g, index, v.scroll).x as f32
                } else {
                    let last = layout::cell_rect(m, &g, count - 1, v.scroll);
                    (last.x + last.w) as f32
                };
                let row = layout::cell_rect(m, &g, index.min(count.saturating_sub(1)), v.scroll);
                self.fill(&rounded(x - s(1.0), row.y as f32 + s(8.0), s(2.0), row.h as f32 - s(16.0), s(1.0)), color(pal.accent, 1.0));
            }
            self.rt.PopAxisAlignedClip();

            let max = layout::max_scroll(m, &g, v.h);
            if max > 0 {
                let track = h - title_h - m.pad as f32;
                let thumb = (track * track / g.content_h as f32).max(s(16.0));
                let y = title_h + (track - thumb) * v.scroll as f32 / max as f32;
                self.fill(&rounded(w - s(5.0), y, s(3.0), thumb, s(1.5)), color(pal.ink, if v.hot { 0.30 } else { 0.14 }));
            }
        }
        self.rt.EndDraw(None, None)?;
        surface.present(hwnd);
        Ok(Painted { missing, tabs: tab_rects })
    }
}
