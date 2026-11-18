//! Appearance: everything about how a group looks, the parts a `Style` is made of, and the
//! built-in themes. Pure data; the renderer and the settings window both read it.
//!
//! Every type has a default that reproduces the original Tessera look, and every field is
//! optional in JSON, so layouts written before these existed load unchanged.
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A colour, stored in JSON as `"#rrggbb"`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rgb(pub [u8; 3]);

impl Rgb {
    pub const fn hex(v: u32) -> Rgb {
        Rgb([(v >> 16) as u8, (v >> 8) as u8, v as u8])
    }

    pub fn parse(s: &str) -> Option<Rgb> {
        let s = s.trim().trim_start_matches('#');
        if s.len() != 6 {
            return None;
        }
        u32::from_str_radix(s, 16).ok().map(Rgb::hex)
    }
}

impl Serialize for Rgb {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("#{:02x}{:02x}{:02x}", self.0[0], self.0[1], self.0[2]))
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Rgb, D::Error> {
        let s = String::deserialize(d)?;
        Rgb::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("not a #rrggbb colour: {s}")))
    }
}

/// What fills the group's background.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Material {
    /// A tinted colour at the chosen opacity. The original look.
    #[default]
    Transparent,
    /// Fully opaque colour.
    Solid,
    /// Linear or radial gradient through two or three colours.
    Gradient,
    /// The wallpaper behind the group, blurred, tinted and lightly grained.
    Acrylic,
    /// The wallpaper reduced to its colours: heavy blur, strong tint, no grain.
    Mica,
    /// The wallpaper, blurred, under a pale frost layer.
    Frosted,
}

impl Material {
    /// Shows the (blurred) wallpaper and so needs the backdrop capture.
    pub fn uses_backdrop(self) -> bool {
        matches!(self, Material::Acrylic | Material::Mica | Material::Frosted)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Gradient {
    /// Two or three colours, first to last.
    pub stops: Vec<Rgb>,
    /// Direction of a linear gradient in degrees; 0 runs left to right, 90 top to bottom.
    pub angle: u16,
    /// Radial from the centre instead of linear.
    pub radial: bool,
}

impl Default for Gradient {
    fn default() -> Self {
        Gradient { stops: vec![Rgb::hex(0x2B3A67), Rgb::hex(0x1A1F2E)], angle: 135, radial: false }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Line {
    #[default]
    Solid,
    Dashed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Border {
    /// Stroke width at 96 DPI; 0 draws no border.
    pub width: f32,
    /// `None` uses the text colour of the theme.
    pub color: Option<Rgb>,
    /// Percent.
    pub opacity: u8,
    pub line: Line,
    /// A soft light edge along the top, as light catching glass.
    pub highlight: bool,
    /// A second, fainter line just inside the border.
    pub inner: bool,
    /// A faint darkening just inside the edges.
    pub inner_shadow: bool,
}

impl Default for Border {
    fn default() -> Self {
        Border { width: 1.0, color: None, opacity: 10, line: Line::Solid, highlight: false, inner: false, inner_shadow: false }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShadowKind {
    #[default]
    None,
    /// Even, close, low: the group sits on the desktop.
    Ambient,
    /// Wider and offset down.
    Soft,
    /// Large and well below: the group hovers.
    Floating,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Shadow {
    pub kind: ShadowKind,
    /// Darkness, percent.
    pub intensity: u8,
    /// Blur radius at 96 DPI.
    pub blur: u8,
    /// Downward offset at 96 DPI.
    pub offset: u8,
    /// Grows (positive) or shrinks the shadow's shape before blurring.
    pub spread: i8,
}

impl Default for Shadow {
    fn default() -> Self {
        Shadow::preset(ShadowKind::None)
    }
}

impl Shadow {
    /// Values a shadow kind starts with; the sliders adjust from there.
    pub fn preset(kind: ShadowKind) -> Shadow {
        let (intensity, blur, offset, spread) = match kind {
            ShadowKind::None => (0, 0, 0, 0),
            ShadowKind::Ambient => (30, 8, 1, 0),
            ShadowKind::Soft => (35, 16, 4, 0),
            ShadowKind::Floating => (40, 26, 10, -4),
        };
        Shadow { kind, intensity, blur, offset, spread }
    }

    pub fn visible(&self) -> bool {
        self.kind != ShadowKind::None && self.intensity > 0
    }

    /// How far the shadow can reach beyond the group, at 96 DPI. The group window is grown
    /// by this much on every side so the shadow has room to draw.
    pub fn margin(&self) -> u32 {
        if self.visible() {
            (self.blur as i32 + self.offset as i32 + self.spread.max(0) as i32 + 2).max(0) as u32
        } else {
            0
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Align {
    Left,
    #[default]
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TitleBar {
    /// Height at 96 DPI.
    pub height: u8,
    /// Opacity of a band behind the title, percent; 0 for none.
    pub band: u8,
    /// A hairline under the title bar.
    pub separator: bool,
    pub align: Align,
    pub size: f32,
    pub weight: u16,
    /// `None` uses the text colour of the theme.
    pub color: Option<Rgb>,
}

impl Default for TitleBar {
    fn default() -> Self {
        TitleBar { height: 30, band: 0, separator: false, align: Align::Left, size: 12.5, weight: 600, color: None }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextShadow {
    /// On dark themes and over wallpaper materials, where it helps.
    #[default]
    Auto,
    On,
    Off,
}

/// Icon labels (and the font family used for the title too).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Typography {
    /// Empty for the system UI font.
    pub family: String,
    pub size: f32,
    pub weight: u16,
    /// `None` uses the text colour of the theme.
    pub color: Option<Rgb>,
    /// Item counts and tabs that aren't in front. `None` derives it from `color`.
    pub secondary: Option<Rgb>,
    /// Multiple of the font's natural line height.
    pub line_height: f32,
    /// Extra space between letters, in pixels at 96 DPI.
    pub letter_spacing: f32,
    pub align: Align,
    /// Lines a label may wrap to before it is cut with an ellipsis.
    pub lines: u8,
    pub shadow: TextShadow,
}

impl Default for Typography {
    fn default() -> Self {
        Typography { family: String::new(), size: 12.0, weight: 400, color: None, secondary: None, line_height: 1.0, letter_spacing: 0.0, align: Align::Center, lines: 2, shadow: TextShadow::Auto }
    }
}

pub const DEFAULT_FONT: &str = "Segoe UI Variable Text";

impl Typography {
    pub fn family(&self) -> &str {
        if self.family.trim().is_empty() {
            DEFAULT_FONT
        } else {
            self.family.trim()
        }
    }

    /// Height of one label line at 96 DPI.
    pub fn line_px(&self) -> f32 {
        self.size * 1.33 * self.line_height
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Container {
    #[default]
    None,
    Rounded,
    Circle,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Hover {
    /// A faint tile behind the item. The original look.
    #[default]
    Tile,
    /// The icon rises a little.
    Lift,
    /// A soft glow in the accent colour behind the icon.
    Glow,
    None,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Selection {
    #[default]
    Tile,
    Outline,
    Underline,
}

/// How the hover effect comes and goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum HoverMotion {
    /// Instant.
    None,
    /// The hover effect fades in and out.
    #[default]
    Fade,
    /// Fades, and the icon grows a little.
    Grow,
    /// Fades, and the icon rises gently.
    Float,
    /// Fades, and the icon springs up in size with a small overshoot.
    Pop,
}

/// How an icon reacts when it is selected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SelectMotion {
    /// Instant.
    None,
    /// The selection fades in.
    #[default]
    Fade,
    /// Fades in, and a ring ripples out once.
    Pulse,
    /// Fades in, and the icon bounces once.
    Pop,
}

impl HoverMotion {
    /// Seconds to go fully in; going out takes a little longer.
    pub fn seconds(self) -> f32 {
        match self {
            HoverMotion::None => 0.0,
            HoverMotion::Fade => 0.14,
            HoverMotion::Grow | HoverMotion::Float => 0.18,
            HoverMotion::Pop => 0.26,
        }
    }
}

impl SelectMotion {
    pub fn seconds(self) -> f32 {
        match self {
            SelectMotion::None => 0.0,
            _ => 0.16,
        }
    }

    /// Length of the one-off ripple or bounce, if any.
    pub fn burst_seconds(self) -> f32 {
        match self {
            SelectMotion::Pulse => 0.45,
            SelectMotion::Pop => 0.36,
            _ => 0.0,
        }
    }
}

/// Smooth start and end.
pub fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Rises past 1 and settles back: for springy motion.
pub fn ease_back(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0) - 1.0;
    let c = 1.9;
    1.0 + (c + 1.0) * t * t * t + c * t * t
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LabelPos {
    #[default]
    Below,
    Right,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct IconLook {
    /// Percent.
    pub opacity: u8,
    /// Percent; 100 is the icon's own colours, 0 is greyscale.
    pub saturation: u8,
    /// Wash the icon towards this colour by `tint_amount` percent. Capped so brands stay
    /// recognisable.
    pub tint: Option<Rgb>,
    pub tint_amount: u8,
    pub container: Container,
    /// Opacity of the container, percent.
    pub container_opacity: u8,
    pub hover: Hover,
    pub selection: Selection,
    pub label: LabelPos,
    pub hover_motion: HoverMotion,
    pub select_motion: SelectMotion,
}

pub const MAX_ICON_TINT: u8 = 60;

impl Default for IconLook {
    fn default() -> Self {
        IconLook { opacity: 100, saturation: 100, tint: None, tint_amount: 0, container: Container::None, container_opacity: 10, hover: Hover::Tile, selection: Selection::Tile, label: LabelPos::Below, hover_motion: HoverMotion::Fade, select_motion: SelectMotion::Fade }
    }
}

impl IconLook {
    /// The icon's pixels are changed (and need their own cached copy).
    pub fn recolours(&self) -> bool {
        self.saturation < 100 || (self.tint.is_some() && self.tint_amount > 0)
    }
}

/// A named appearance the user can apply to groups.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub style: crate::model::Style,
}

/// A picture's main colours, largest share first: k-means (8 clusters, a deterministic start)
/// over the given pixels, with look-alike clusters merged. Returns at most 6 (colour, share).
/// The settings window runs the same steps in JavaScript for its palette.
pub fn palette(pixels: &[[u8; 3]]) -> Vec<(Rgb, f32)> {
    if pixels.is_empty() {
        return Vec::new();
    }
    let px: Vec<[f32; 3]> = pixels.iter().map(|p| p.map(|v| v as f32)).collect();
    // Perceptual-ish distance: green matters most, blue least.
    let dist = |a: &[f32; 3], b: &[f32; 3]| 2.0 * (a[0] - b[0]).powi(2) + 4.0 * (a[1] - b[1]).powi(2) + 3.0 * (a[2] - b[2]).powi(2);
    let n = px.len() as f32;
    let mean = [0, 1, 2].map(|c| px.iter().map(|p| p[c]).sum::<f32>() / n);
    let mut centres = vec![mean];
    // Then repeatedly the pixel farthest from every centre so far.
    while centres.len() < 8 {
        let far = px.iter().step_by(7).max_by(|a, b| {
            let da = centres.iter().map(|c| dist(a, c)).fold(f32::MAX, f32::min);
            let db = centres.iter().map(|c| dist(b, c)).fold(f32::MAX, f32::min);
            da.total_cmp(&db)
        });
        centres.push(*far.unwrap_or(&mean));
    }
    let mut counts = vec![0usize; centres.len()];
    for _ in 0..12 {
        let mut sums = vec![[0f32; 3]; centres.len()];
        counts.iter_mut().for_each(|c| *c = 0);
        for p in &px {
            let k = (0..centres.len()).min_by(|&i, &j| dist(p, &centres[i]).total_cmp(&dist(p, &centres[j]))).unwrap_or(0);
            for c in 0..3 {
                sums[k][c] += p[c];
            }
            counts[k] += 1;
        }
        for (j, c) in centres.iter_mut().enumerate() {
            if counts[j] > 0 {
                *c = sums[j].map(|s| s / counts[j] as f32);
            }
        }
    }
    let mut clusters: Vec<([f32; 3], usize)> = centres.into_iter().zip(counts).filter(|(_, n)| *n > 0).collect();
    let mut i = 0;
    while i < clusters.len() {
        let mut j = clusters.len() - 1;
        while j > i {
            if dist(&clusters[i].0, &clusters[j].0) < 9.0 * 24.0 * 24.0 {
                let (b, nb) = clusters.remove(j);
                let (a, na) = clusters[i];
                let total = (na + nb) as f32;
                clusters[i] = ([0, 1, 2].map(|c| (a[c] * na as f32 + b[c] * nb as f32) / total), na + nb);
            }
            j -= 1;
        }
        i += 1;
    }
    clusters.sort_by(|a, b| b.1.cmp(&a.1));
    clusters.into_iter().take(6).map(|(c, k)| (Rgb(c.map(|v| v.round() as u8)), k as f32 / n)).collect()
}

/// An accent to go with background `bg`: the most colourful other palette colour, lightened until
/// it stands out on a group.
pub fn accent_for(palette: &[(Rgb, f32)], bg: Rgb) -> Rgb {
    let lum = |c: [f32; 3]| (0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]) / 255.0;
    let sat = |c: [f32; 3]| {
        let (mx, mn) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
        if mx == 0.0 { 0.0 } else { (mx - mn) / mx }
    };
    let f = |c: Rgb| c.0.map(|v| v as f32);
    let b = f(bg);
    let others: Vec<[f32; 3]> = palette.iter().map(|(c, _)| f(*c)).filter(|c| (lum(*c) - lum(b)).abs() > 0.04 || sat(*c) > sat(b) + 0.15).collect();
    let mut pick = others.into_iter().fold(b, |best, c| if sat(c) * (0.4 + lum(c)) > sat(best) * (0.4 + lum(best)) { c } else { best });
    for _ in 0..8 {
        if lum(pick) >= 0.45 {
            break;
        }
        pick = pick.map(|v| v + (255.0 - v) * 0.25);
    }
    Rgb(pick.map(|v| v.round() as u8))
}

/// The built-in themes. Each is a starting point; users adjust and save their own.
pub fn builtin_themes() -> Vec<Preset> {
    use crate::model::{Style, Tint};
    let base = Style::default();
    let theme = |name: &str, f: &dyn Fn(&mut Style)| {
        let mut style = base.clone();
        f(&mut style);
        Preset { name: name.into(), style }
    };
    vec![
        theme("Tessera", &|_| {}),
        theme("Midnight Glass", &|s| {
            s.material = Material::Acrylic;
            s.color = Some(Rgb::hex(0x10131C));
            s.opacity = 58;
            s.blur = 70;
            s.noise = 25;
            s.border = Border { width: 1.0, color: Some(Rgb::hex(0xFFFFFF)), opacity: 12, highlight: true, ..Border::default() };
            s.shadow = Shadow::preset(ShadowKind::Soft);
            s.radius = 12;
        }),
        theme("Frosted Light", &|s| {
            s.material = Material::Frosted;
            s.color = Some(Rgb::hex(0xF4F6FA));
            s.opacity = 45;
            s.blur = 60;
            s.noise = 15;
            s.border = Border { width: 1.0, color: Some(Rgb::hex(0xFFFFFF)), opacity: 45, highlight: true, ..Border::default() };
            s.shadow = Shadow::preset(ShadowKind::Soft);
            s.text.color = Some(Rgb::hex(0x16181D));
            s.title.color = Some(Rgb::hex(0x16181D));
            s.text.shadow = TextShadow::Off;
            s.radius = 14;
        }),
        theme("Soft Glass", &|s| {
            s.material = Material::Frosted;
            s.color = Some(Rgb::hex(0xDDE3EE));
            s.opacity = 28;
            s.blur = 85;
            s.noise = 8;
            s.border = Border { width: 1.0, color: Some(Rgb::hex(0xFFFFFF)), opacity: 30, highlight: true, inner: true, ..Border::default() };
            s.shadow = Shadow::preset(ShadowKind::Floating);
            s.title.align = Align::Center;
            s.radius = 18;
            s.icons.container = Container::Rounded;
            s.icons.container_opacity = 14;
            s.icons.hover_motion = HoverMotion::Float;
            s.icons.hover = Hover::Lift;
        }),
        theme("Fluent", &|s| {
            s.material = Material::Mica;
            s.color = Some(Rgb::hex(0x202020));
            s.opacity = 70;
            s.border = Border { width: 1.0, color: Some(Rgb::hex(0xFFFFFF)), opacity: 8, ..Border::default() };
            s.shadow = Shadow::preset(ShadowKind::Ambient);
            s.title.band = 6;
            s.title.separator = true;
            s.radius = 8;
            s.accent = Some(Rgb::hex(0x4CC2FF));
            s.icons.selection = Selection::Outline;
            s.icons.select_motion = SelectMotion::Pulse;
        }),
        theme("Minimal", &|s| {
            s.opacity = 20;
            s.border = Border { width: 0.0, ..Border::default() };
            s.title.size = 12.0;
            s.title.weight = 500;
            s.text.shadow = TextShadow::On;
            s.icons.hover = Hover::Lift;
            s.icons.hover_motion = HoverMotion::Grow;
        }),
        theme("Deep Ocean", &|s| {
            s.material = Material::Gradient;
            s.gradient = Gradient { stops: vec![Rgb::hex(0x0B3D5C), Rgb::hex(0x06213A), Rgb::hex(0x041427)], angle: 120, radial: false };
            s.opacity = 82;
            s.tint = Tint::Teal;
            s.accent = Some(Rgb::hex(0x38C6D9));
            s.border = Border { width: 1.0, color: Some(Rgb::hex(0x38C6D9)), opacity: 22, ..Border::default() };
            s.shadow = Shadow::preset(ShadowKind::Soft);
            s.radius = 12;
        }),
        theme("Warm Sunset", &|s| {
            s.material = Material::Gradient;
            s.gradient = Gradient { stops: vec![Rgb::hex(0x5A2A4A), Rgb::hex(0x8A3B3B), Rgb::hex(0xB8683A)], angle: 160, radial: false };
            s.opacity = 78;
            s.tint = Tint::Amber;
            s.accent = Some(Rgb::hex(0xFFB35C));
            s.border = Border { width: 1.0, color: Some(Rgb::hex(0xFFD1A1)), opacity: 20, highlight: true, ..Border::default() };
            s.shadow = Shadow::preset(ShadowKind::Soft);
            s.radius = 14;
        }),
        theme("Monochrome", &|s| {
            s.material = Material::Solid;
            s.color = Some(Rgb::hex(0x161616));
            s.opacity = 100;
            s.accent = Some(Rgb::hex(0xE6E6E6));
            s.border = Border { width: 1.0, color: Some(Rgb::hex(0xFFFFFF)), opacity: 14, ..Border::default() };
            s.radius = 6;
            s.icons.saturation = 0;
            s.icons.selection = Selection::Underline;
            s.icons.select_motion = SelectMotion::Pop;
            s.title.align = Align::Center;
            s.title.weight = 700;
            s.title.size = 11.5;
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_ranks_colours_by_share() {
        // 70 % dark blue, 20 % orange, 10 % near-white.
        let mut px = vec![[20, 30, 60]; 700];
        px.extend(vec![[230, 120, 40]; 200]);
        px.extend(vec![[240, 240, 235]; 100]);
        let p = palette(&px);
        assert_eq!(p.len(), 3, "look-alike clusters merge: {p:?}");
        assert_eq!(p[0].0, Rgb([20, 30, 60]));
        assert!((p[0].1 - 0.7).abs() < 0.01 && (p[1].1 - 0.2).abs() < 0.01);
        // The accent for the blue background is the colourful orange, not the white.
        assert_eq!(accent_for(&p, p[0].0), Rgb([230, 120, 40]));
        assert!(palette(&[]).is_empty());
    }

    #[test]
    fn a_dark_accent_is_lightened() {
        let p = vec![(Rgb([10, 10, 12]), 0.8), (Rgb([60, 10, 10]), 0.2)];
        let a = accent_for(&p, Rgb([10, 10, 12]));
        let lum = (0.2126 * a.0[0] as f32 + 0.7152 * a.0[1] as f32 + 0.0722 * a.0[2] as f32) / 255.0;
        assert!(lum >= 0.45, "{a:?}");
    }
}
