//! Persistent data model and the operations that keep it consistent.
use crate::layout::{shelf_pack, Metrics};
use crate::look::*;
use crate::rules::{classify, Category};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

pub const CONFIG_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }

    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.x + o.w && o.x < self.x + self.w && self.y < o.y + o.h && o.y < self.y + self.h
    }

    /// Move (and shrink if necessary) so the rectangle lies inside `area`.
    pub fn clamp_into(&self, area: Rect) -> Rect {
        let w = self.w.min(area.w);
        let h = self.h.min(area.h);
        Rect { x: self.x.clamp(area.x, area.x + area.w - w), y: self.y.clamp(area.y, area.y + area.h - h), w, h }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sort {
    Manual,
    Name,
    Kind,
}

/// How a tabbed group lays out its tab strip.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TabStyle {
    /// Tabs share the title bar equally.
    #[default]
    Fill,
    /// Each tab is as wide as its name, packed from the left.
    Fit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Theme {
    Dark,
    Light,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tint {
    Slate,
    Blue,
    Teal,
    Green,
    Amber,
    Rose,
    Violet,
}

impl Tint {
    pub const ALL: [Tint; 7] = [Tint::Slate, Tint::Blue, Tint::Teal, Tint::Green, Tint::Amber, Tint::Rose, Tint::Violet];

    pub fn name(self) -> &'static str {
        match self {
            Tint::Slate => "Slate",
            Tint::Blue => "Blue",
            Tint::Teal => "Teal",
            Tint::Green => "Green",
            Tint::Amber => "Amber",
            Tint::Rose => "Rose",
            Tint::Violet => "Violet",
        }
    }

    pub fn rgb(self) -> [u8; 3] {
        match self {
            Tint::Slate => [0x8A, 0x94, 0xA6],
            Tint::Blue => [0x5B, 0x8D, 0xEF],
            Tint::Teal => [0x2F, 0xB5, 0xA8],
            Tint::Green => [0x5F, 0xB8, 0x6A],
            Tint::Amber => [0xE0, 0xA4, 0x3A],
            Tint::Rose => [0xE0, 0x6C, 0x8B],
            Tint::Violet => [0x9A, 0x7B, 0xE8],
        }
    }
}

/// Everything about how a group looks. The first three fields are the original ones; the rest
/// were added later and all default to the original look, so older layouts load unchanged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Style {
    /// Colour family: tints the surface (unless `color` is set) and is the default accent.
    pub tint: Tint,
    /// Background opacity, percent.
    pub opacity: u8,
    /// Icon size in pixels at 96 DPI.
    pub icon: u32,
    pub material: Material,
    /// Background colour instead of the tinted theme surface.
    pub color: Option<Rgb>,
    /// Active tab, selection and insertion marker; `None` derives it from `tint`.
    pub accent: Option<Rgb>,
    /// Blur strength for wallpaper materials, percent.
    pub blur: u8,
    /// Grain over the background, percent.
    pub noise: u8,
    pub gradient: Gradient,
    pub border: Border,
    pub shadow: Shadow,
    pub title: TitleBar,
    pub text: Typography,
    pub icons: IconLook,
    /// Corner radius at 96 DPI.
    pub radius: u8,
    /// Space between the edge and the icons, at 96 DPI.
    pub padding: u8,
    /// Horizontal room around each icon, at 96 DPI.
    pub spacing: u8,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            tint: Tint::Slate,
            opacity: 62,
            icon: 48,
            material: Material::Transparent,
            color: None,
            accent: None,
            blur: 60,
            noise: 20,
            gradient: Gradient::default(),
            border: Border::default(),
            shadow: Shadow::default(),
            title: TitleBar::default(),
            text: Typography::default(),
            icons: IconLook::default(),
            radius: 10,
            padding: 8,
            spacing: 28,
        }
    }
}

pub const ICON_MIN: u32 = 16;
pub const ICON_MAX: u32 = 128;

impl Style {
    /// Bring every value into its supported range. Applied to anything that arrives from
    /// outside (commands, imported themes), so the renderer can trust a `Style`.
    pub fn sanitize(&mut self) {
        self.opacity = self.opacity.clamp(5, 100);
        self.icon = self.icon.clamp(ICON_MIN, ICON_MAX);
        self.blur = self.blur.min(100);
        self.noise = self.noise.min(100);
        self.gradient.stops.truncate(3);
        while self.gradient.stops.len() < 2 {
            self.gradient.stops.push(self.gradient.stops.first().copied().unwrap_or(Rgb::hex(0x202430)));
        }
        self.gradient.angle %= 360;
        let b = &mut self.border;
        b.width = if b.width.is_finite() { b.width.clamp(0.0, 4.0) } else { 1.0 };
        b.opacity = b.opacity.min(100);
        let s = &mut self.shadow;
        s.intensity = s.intensity.min(100);
        s.blur = s.blur.min(40);
        s.offset = s.offset.min(24);
        s.spread = s.spread.clamp(-12, 12);
        let t = &mut self.title;
        t.height = t.height.clamp(22, 48);
        t.band = t.band.min(100);
        t.pill_opacity = t.pill_opacity.min(100);
        t.size = if t.size.is_finite() { t.size.clamp(9.0, 20.0) } else { 12.5 };
        t.weight = t.weight.clamp(100, 900);
        let x = &mut self.text;
        x.family = x.family.chars().take(64).collect();
        x.size = if x.size.is_finite() { x.size.clamp(8.0, 18.0) } else { 12.0 };
        x.weight = x.weight.clamp(100, 900);
        x.line_height = if x.line_height.is_finite() { x.line_height.clamp(0.8, 1.6) } else { 1.0 };
        x.letter_spacing = if x.letter_spacing.is_finite() { x.letter_spacing.clamp(-1.0, 4.0) } else { 0.0 };
        x.lines = x.lines.clamp(1, 3);
        let i = &mut self.icons;
        i.opacity = i.opacity.clamp(20, 100);
        i.saturation = i.saturation.min(100);
        i.tint_amount = i.tint_amount.min(MAX_ICON_TINT);
        i.container_opacity = i.container_opacity.min(60);
        self.radius = self.radius.min(24);
        self.padding = self.padding.clamp(2, 24);
        self.spacing = self.spacing.clamp(8, 64);
    }

    /// Take a background colour (and optionally an accent), keeping everything else. A gradient
    /// runs from the colour to a deeper shade of it.
    pub fn recolor(&mut self, color: Rgb, accent: Option<Rgb>) {
        self.color = Some(color);
        if let Some(a) = accent {
            self.accent = Some(a);
        }
        let deeper = Rgb(color.0.map(|v| (v as f32 * 0.55) as u8));
        self.gradient.stops = vec![color, deeper];
    }

    /// The same look with this style's icon size kept: themes change how a group looks,
    /// not how big its icons are.
    pub fn themed(&self, theme: &Style) -> Style {
        Style { icon: self.icon, ..theme.clone() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Fence {
    pub id: u32,
    pub title: String,
    /// Expanded rectangle in virtual-screen pixels.
    pub rect: Rect,
    #[serde(default)]
    pub collapsed: bool,
    #[serde(default)]
    pub style: Style,
    pub sort: Sort,
    /// New items of this category are filed here automatically.
    #[serde(default)]
    pub category: Option<Category>,
    /// Catches every item no other group claims. Exactly one group has this set.
    #[serde(default)]
    pub inbox: bool,
    /// Item keys in display order.
    #[serde(default)]
    pub items: Vec<String>,
    /// A folder portal: this group shows the contents of this folder instead of desktop items.
    /// Its `items` are a live listing owned by the organiser, never part of desktop bookkeeping.
    #[serde(default)]
    pub portal: Option<String>,
    /// Other groups shown as tabs in this group's window, after this group's own tab.
    /// The window (rectangle, look, roll-up) belongs to this group; each tab keeps its own
    /// icons, sort order, rule and portal folder.
    #[serde(default)]
    pub tabs: Vec<u32>,
    /// The tab on screen; None means this group's own tab.
    #[serde(default)]
    pub active_tab: Option<u32>,
    #[serde(default)]
    pub tab_style: TabStyle,
}

impl Fence {
    pub fn is_portal(&self) -> bool {
        self.portal.is_some()
    }
}

/// A complete, independent desktop arrangement that is not currently on screen.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Profile {
    pub id: u32,
    pub name: String,
    pub fences: Vec<Fence>,
    #[serde(default)]
    pub placements: BTreeMap<String, BTreeMap<u32, Rect>>,
}

fn first_profile_id() -> u32 {
    1
}

fn second_profile_id() -> u32 {
    2
}

fn first_profile_name() -> String {
    "Default".to_string()
}

/// What the shell told us about one desktop item.
#[derive(Clone, Debug)]
pub struct Present<'a> {
    pub key: &'a str,
    pub name: &'a str,
    pub is_folder: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub version: u32,
    pub theme: Theme,
    pub next_id: u32,
    /// Style given to newly created groups.
    #[serde(default)]
    pub default_style: Style,
    pub fences: Vec<Fence>,
    /// Group rectangles remembered per display configuration.
    #[serde(default)]
    pub placements: BTreeMap<String, BTreeMap<u32, Rect>>,
    /// The profile on screen: `fences` and `placements` above are its contents.
    #[serde(default = "first_profile_id")]
    pub profile_id: u32,
    #[serde(default = "first_profile_name")]
    pub profile_name: String,
    #[serde(default = "second_profile_id")]
    pub next_profile_id: u32,
    /// Every other profile.
    #[serde(default)]
    pub profiles: Vec<Profile>,
    /// Themes the user saved. The built-in ones live in code (`look::builtin_themes`).
    #[serde(default)]
    pub custom_themes: Vec<Preset>,
    /// Skip wallpaper blur, grain and shadows, for slow machines or remote sessions.
    #[serde(default)]
    pub low_effects: bool,
    /// Recolour every group from the wallpaper's most prominent colour whenever the wallpaper changes.
    #[serde(default)]
    pub wallpaper_auto: bool,
    /// With `wallpaper_auto`, also take a matching accent.
    #[serde(default)]
    pub wallpaper_accent: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            version: CONFIG_VERSION,
            theme: Theme::Dark,
            next_id: 1,
            default_style: Style::default(),
            fences: Vec::new(),
            placements: BTreeMap::new(),
            profile_id: 1,
            profile_name: first_profile_name(),
            next_profile_id: 2,
            profiles: Vec::new(),
            custom_themes: Vec::new(),
            low_effects: false,
            wallpaper_auto: false,
            wallpaper_accent: false,
        }
    }
}

fn clean_name(name: &str, max: usize) -> Option<String> {
    let name: String = name.trim().chars().filter(|c| !c.is_control()).take(max).collect();
    (!name.is_empty()).then_some(name)
}

impl Config {
    pub fn fence(&self, id: u32) -> Option<&Fence> {
        self.fences.iter().find(|f| f.id == id)
    }

    pub fn fence_mut(&mut self, id: u32) -> Option<&mut Fence> {
        self.fences.iter_mut().find(|f| f.id == id)
    }

    pub fn add_fence(&mut self, title: &str, rect: Rect) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.fences.push(Fence {
            id,
            title: title.to_string(),
            rect,
            collapsed: false,
            style: self.default_style.clone(),
            sort: Sort::Manual,
            category: None,
            inbox: false,
            items: Vec::new(),
            portal: None,
            tabs: Vec::new(),
            active_tab: None,
            tab_style: TabStyle::Fill,
        });
        id
    }

    // ------------------------------------------------------------ tabs

    /// The group whose window shows `id`: the group that has it as a tab, or `id` itself.
    pub fn tab_host(&self, id: u32) -> u32 {
        self.fences.iter().find(|f| f.tabs.contains(&id)).map(|f| f.id).unwrap_or(id)
    }

    /// Groups that have a window of their own (everything that is not someone else's tab).
    pub fn hosts(&self) -> Vec<u32> {
        let members: HashSet<u32> = self.fences.iter().flat_map(|f| f.tabs.iter().copied()).collect();
        self.fences.iter().map(|f| f.id).filter(|id| !members.contains(id)).collect()
    }

    /// The tabs of the window `id` belongs to, in order; a group without tabs is a single tab.
    pub fn tab_order(&self, id: u32) -> Vec<u32> {
        let host = self.tab_host(id);
        std::iter::once(host).chain(self.fence(host).map(|f| f.tabs.clone()).unwrap_or_default()).collect()
    }

    /// The tab currently on screen in the window `id` belongs to.
    pub fn shown_tab(&self, id: u32) -> u32 {
        let host = self.tab_host(id);
        self.fence(host).and_then(|f| f.active_tab.filter(|t| f.tabs.contains(t))).unwrap_or(host)
    }

    /// Bring a tab to the front of its window. Returns whether anything changed.
    pub fn select_tab(&mut self, tab: u32) -> bool {
        let host = self.tab_host(tab);
        let want = (tab != host).then_some(tab);
        match self.fence_mut(host) {
            Some(f) if f.active_tab != want => {
                f.active_tab = want;
                true
            }
            _ => false,
        }
    }

    /// Add an empty tab to the window `id` belongs to and bring it to the front.
    pub fn add_tab(&mut self, id: u32, title: Option<&str>) -> Option<u32> {
        let host = self.tab_host(id);
        let (rect, style) = self.fence(host).map(|f| (f.rect, f.style.clone()))?;
        let title = title.and_then(|t| clean_name(t, 64)).unwrap_or_else(|| format!("Tab {}", self.tab_order(host).len() + 1));
        let tab = self.add_fence(&title, rect);
        self.fence_mut(tab).unwrap().style = style;
        let h = self.fence_mut(host).unwrap();
        h.tabs.push(tab);
        h.active_tab = Some(tab);
        Some(tab)
    }

    /// Put the tabs of `host`'s window in a new order. The first one becomes the host and
    /// takes over the window: its rectangle, look and remembered positions.
    pub fn reorder_tabs(&mut self, host: u32, order: Vec<u32>) -> bool {
        let mut current = self.tab_order(host);
        let mut wanted = order.clone();
        current.sort_unstable();
        wanted.sort_unstable();
        if order.is_empty() || current != wanted || self.tab_host(host) != host {
            return false;
        }
        let shown = self.shown_tab(host);
        let new_host = order[0];
        if new_host != host {
            let old = self.fence(host).unwrap().clone();
            let n = self.fence_mut(new_host).unwrap();
            (n.rect, n.collapsed, n.style, n.tab_style) = (old.rect, old.collapsed, old.style, old.tab_style);
            let o = self.fence_mut(host).unwrap();
            o.tabs.clear();
            o.active_tab = None;
            for rects in self.placements.values_mut() {
                if let Some(r) = rects.remove(&host) {
                    rects.insert(new_host, r);
                }
            }
        }
        let n = self.fence_mut(new_host).unwrap();
        n.tabs = order[1..].to_vec();
        n.active_tab = (shown != new_host).then_some(shown);
        true
    }

    /// Move a tab left (negative) or right within its window.
    pub fn move_tab(&mut self, id: u32, delta: i32) -> bool {
        let mut order = self.tab_order(id);
        let Some(from) = order.iter().position(|t| *t == id) else { return false };
        let to = (from as i32 + delta).clamp(0, order.len() as i32 - 1) as usize;
        if from == to {
            return false;
        }
        let tab = order.remove(from);
        order.insert(to, tab);
        let host = self.tab_host(id);
        self.reorder_tabs(host, order)
    }

    /// Take a tab out of its window and make it a group of its own, next to the window.
    pub fn detach_tab(&mut self, id: u32, area: Rect) -> bool {
        let mut order = self.tab_order(id);
        if order.len() < 2 || self.fence(id).is_none() {
            return false;
        }
        // The detached tab must not be the host, so move it to the end first.
        order.retain(|t| *t != id);
        order.push(id);
        let host = order[0];
        let old_host = self.tab_host(id);
        self.reorder_tabs(old_host, order);
        let (rect, style) = self.fence(host).map(|f| (f.rect, f.style.clone())).unwrap();
        let h = self.fence_mut(host).unwrap();
        h.tabs.retain(|t| *t != id);
        if h.active_tab == Some(id) {
            h.active_tab = None;
        }
        // Beside the window if there is free room (right, below, left), otherwise slightly offset.
        let others: Vec<Rect> = self.hosts().into_iter().filter(|h| *h != id).filter_map(|h| self.fence(h).map(|f| f.rect)).collect();
        let gap = 12;
        let free = |r: &Rect| r.x >= area.x && r.y >= area.y && r.x + r.w <= area.x + area.w && r.y + r.h <= area.y + area.h && !others.iter().any(|o| o.intersects(r));
        let spot = [Rect { x: rect.x + rect.w + gap, ..rect }, Rect { y: rect.y + rect.h + gap, ..rect }, Rect { x: rect.x - rect.w - gap, ..rect }]
            .into_iter()
            .find(free)
            .unwrap_or(Rect { x: rect.x + 28, y: rect.y + 28, ..rect });
        let f = self.fence_mut(id).unwrap();
        f.rect = spot.clamp_into(area);
        f.style = style;
        f.collapsed = false;
        true
    }

    /// Put every tab of `id`'s window into `into`'s window, after its existing tabs.
    pub fn merge_tabs(&mut self, id: u32, into: u32) -> bool {
        let (from, to) = (self.tab_host(id), self.tab_host(into));
        if from == to || self.fence(from).is_none() || self.fence(to).is_none() {
            return false;
        }
        let moving = self.tab_order(from);
        let f = self.fence_mut(from).unwrap();
        f.tabs.clear();
        f.active_tab = None;
        let t = self.fence_mut(to).unwrap();
        t.tabs.extend(moving);
        t.active_tab = Some(from);
        for rects in self.placements.values_mut() {
            rects.remove(&from);
        }
        true
    }

    /// Add a folder portal showing `path`. The organiser fills in its listing.
    pub fn add_portal(&mut self, path: &str, area: Rect) -> u32 {
        let path = path.trim_end_matches(['\\', '/']);
        let title = path.rsplit(['\\', '/']).next().filter(|s| !s.is_empty()).unwrap_or(path);
        let (w, h) = Metrics::new(&self.default_style, 96).size_for(4, 2);
        let rect = Rect { x: area.x + (area.w - w) / 2, y: area.y + (area.h - h) / 2, w, h }.clamp_into(area);
        let id = self.add_fence(title, rect);
        let f = self.fence_mut(id).unwrap();
        f.portal = Some(path.to_string());
        f.sort = Sort::Name;
        id
    }

    // ------------------------------------------------------------ profiles

    /// (id, name, is the one on screen), in creation order.
    pub fn profile_list(&self) -> Vec<(u32, String, bool)> {
        let mut list: Vec<_> = self.profiles.iter().map(|p| (p.id, p.name.clone(), false)).collect();
        list.push((self.profile_id, self.profile_name.clone(), true));
        list.sort_by_key(|p| p.0);
        list
    }

    /// Put the on-screen layout away and bring profile `id` on screen.
    pub fn profile_switch(&mut self, id: u32) -> bool {
        let Some(pos) = self.profiles.iter().position(|p| p.id == id) else { return false };
        let next = self.profiles.remove(pos);
        self.profiles.push(Profile {
            id: self.profile_id,
            name: std::mem::replace(&mut self.profile_name, next.name),
            fences: std::mem::replace(&mut self.fences, next.fences),
            placements: std::mem::replace(&mut self.placements, next.placements),
        });
        self.profile_id = next.id;
        true
    }

    /// Create a profile that starts as a copy of the one on screen, and switch to it.
    /// The original stays exactly as it was, which also makes this the way to snapshot a layout.
    pub fn profile_new(&mut self, name: Option<&str>) -> u32 {
        let id = self.next_profile_id;
        self.next_profile_id += 1;
        let name = name.and_then(|n| clean_name(n, 40)).unwrap_or_else(|| format!("Profile {}", self.profiles.len() + 2));
        // Group ids are unique across all profiles, so the copy gets fresh ones.
        let mut remap = BTreeMap::new();
        let mut copies = self.fences.clone();
        for f in &mut copies {
            remap.insert(f.id, self.next_id);
            f.id = self.next_id;
            self.next_id += 1;
        }
        for f in &mut copies {
            f.tabs = f.tabs.iter().filter_map(|t| remap.get(t).copied()).collect();
            f.active_tab = f.active_tab.and_then(|t| remap.get(&t).copied());
        }
        let placements = self.placements.iter().map(|(display, rects)| (display.clone(), rects.iter().filter_map(|(old, r)| Some((*remap.get(old)?, *r))).collect())).collect();
        self.profiles.push(Profile { id, name, fences: copies, placements });
        self.profile_switch(id);
        id
    }

    pub fn profile_rename(&mut self, id: u32, name: &str) -> bool {
        let Some(name) = clean_name(name, 40) else { return false };
        if id == self.profile_id {
            self.profile_name = name;
        } else if let Some(p) = self.profiles.iter_mut().find(|p| p.id == id) {
            p.name = name;
        } else {
            return false;
        }
        true
    }

    /// Delete a profile. The last one cannot be deleted. Returns whether the on-screen layout changed.
    pub fn profile_delete(&mut self, id: u32) -> Option<bool> {
        if id == self.profile_id {
            let fallback = self.profiles.iter().map(|p| p.id).min()?;
            self.profile_switch(fallback);
            self.profiles.retain(|p| p.id != id);
            return Some(true);
        }
        let before = self.profiles.len();
        self.profiles.retain(|p| p.id != id);
        (self.profiles.len() != before).then_some(false)
    }

    fn inbox_id(&mut self, area: Rect) -> u32 {
        if let Some(f) = self.fences.iter().find(|f| f.inbox) {
            return f.id;
        }
        let m = Metrics::new(&Style::default(), 96);
        let (w, h) = m.size_for(4, 2);
        let id = self.add_fence("Desktop", Rect { x: area.x + 16, y: area.y + 16, w, h }.clamp_into(area));
        let f = self.fence_mut(id).unwrap();
        f.inbox = true;
        f.sort = Sort::Name;
        id
    }

    /// Build the initial groups from what is on the desktop.
    /// Categories with fewer than two items stay in the catch-all "Desktop" group.
    pub fn first_run(&mut self, present: &[Present], area: Rect, dpi: u32) {
        let mut by_cat: BTreeMap<Category, usize> = BTreeMap::new();
        for p in present {
            *by_cat.entry(classify(p.key, p.is_folder)).or_default() += 1;
        }
        for cat in Category::ALL {
            if cat != Category::Other && by_cat.get(&cat).copied().unwrap_or(0) >= 2 {
                let id = self.add_fence(cat.title(), Rect::default());
                let f = self.fence_mut(id).unwrap();
                f.category = Some(cat);
                f.sort = Sort::Name;
            }
        }
        self.inbox_id(area);
        self.reconcile(present, area);

        let m = Metrics::new(&Style::default(), dpi);
        let max_cols = ((area.w - 32) / m.cell_w).clamp(2, 8);
        let sizes: Vec<(i32, i32)> = self
            .fences
            .iter()
            .map(|f| {
                let n = f.items.len().max(1) as i32;
                let cols = (((n as f32) * 1.6).sqrt().ceil() as i32).clamp(2, max_cols);
                m.size_for(cols, (n + cols - 1) / cols)
            })
            .collect();
        let rects = shelf_pack(&sizes, area, m.px(16.0), m.px(12.0));
        for (f, r) in self.fences.iter_mut().zip(rects) {
            f.rect = r.clamp_into(area);
        }
    }

    /// Bring group membership in line with the desktop: drop vanished items, file new ones.
    /// Returns the ids of groups whose contents changed.
    pub fn reconcile(&mut self, present: &[Present], area: Rect) -> Vec<u32> {
        let info: HashMap<&str, &Present> = present.iter().map(|p| (p.key, p)).collect();
        let mut changed = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for f in self.fences.iter_mut().filter(|f| !f.is_portal()) {
            let before = f.items.len();
            f.items.retain(|k| info.contains_key(k.as_str()) && seen.insert(k.clone()));
            if f.items.len() != before {
                changed.push(f.id);
            }
        }
        let new: Vec<&Present> = present.iter().filter(|p| !seen.contains(p.key)).collect();
        if !new.is_empty() {
            let inbox = self.inbox_id(area);
            for p in new {
                let cat = classify(p.key, p.is_folder);
                let target = self.fences.iter().find(|f| f.category == Some(cat) && !f.is_portal()).map(|f| f.id).unwrap_or(inbox);
                self.fence_mut(target).unwrap().items.push(p.key.to_string());
                changed.push(target);
            }
        }
        changed.sort_unstable();
        changed.dedup();
        for id in &changed {
            self.sort_fence(*id, &info);
        }
        changed
    }

    /// Apply the group's sort mode to its stored order. Manual order is left alone.
    pub fn sort_fence(&mut self, id: u32, info: &HashMap<&str, &Present>) {
        let Some(f) = self.fence_mut(id) else { return };
        let name = |k: &String| info.get(k.as_str()).map(|p| p.name.to_lowercase()).unwrap_or_default();
        let is_folder = |k: &String| info.get(k.as_str()).map(|p| p.is_folder).unwrap_or(false);
        match f.sort {
            // A portal has no manual order; like Explorer, it lists folders first, then by name.
            Sort::Manual | Sort::Name if f.portal.is_some() => f.items.sort_by_cached_key(|k| (!is_folder(k), name(k))),
            Sort::Manual => {}
            Sort::Name => f.items.sort_by_cached_key(name),
            Sort::Kind => f.items.sort_by_cached_key(|k| {
                let folder = info.get(k.as_str()).map(|p| p.is_folder).unwrap_or(false);
                (classify(k, folder), name(k))
            }),
        }
    }

    /// The shell reported a rename; keep the item in its group and position.
    pub fn rename_key(&mut self, old: &str, new: &str) -> bool {
        for f in &mut self.fences {
            if let Some(slot) = f.items.iter_mut().find(|k| k.eq_ignore_ascii_case(old)) {
                *slot = new.to_string();
                return true;
            }
        }
        false
    }

    /// Move an item to `index` of group `to`. Reordering makes the target group manually sorted.
    pub fn move_item(&mut self, key: &str, to: u32, index: usize) -> bool {
        // Portals list a folder; desktop items cannot be regrouped into or out of them.
        if self.fence(to).is_none_or(|f| f.is_portal()) {
            return false;
        }
        let mut index = index;
        let mut found = false;
        for f in self.fences.iter_mut().filter(|f| !f.is_portal()) {
            if let Some(pos) = f.items.iter().position(|k| k == key) {
                f.items.remove(pos);
                if f.id == to && pos < index {
                    index -= 1;
                }
                found = true;
                break;
            }
        }
        if !found {
            return false;
        }
        let f = self.fence_mut(to).unwrap();
        f.items.insert(index.min(f.items.len()), key.to_string());
        f.sort = Sort::Manual;
        true
    }

    /// Delete a group; its items fall back to the catch-all group. The catch-all itself cannot be deleted.
    pub fn remove_fence(&mut self, id: u32, area: Rect) -> bool {
        if self.fence(id).is_none_or(|f| f.inbox) {
            return false;
        }
        // A window's host hands the window to its next tab before it goes.
        let order = self.tab_order(id);
        if order.len() > 1 && order[0] == id {
            let mut rotated = order;
            rotated.rotate_left(1);
            self.reorder_tabs(id, rotated);
        }
        let host = self.tab_host(id);
        if let Some(h) = self.fence_mut(host).filter(|_| host != id) {
            h.tabs.retain(|t| *t != id);
            if h.active_tab == Some(id) {
                h.active_tab = None;
            }
        }
        let pos = self.fences.iter().position(|f| f.id == id).unwrap();
        let removed = self.fences.remove(pos);
        if !removed.is_portal() {
            let inbox = self.inbox_id(area);
            self.fence_mut(inbox).unwrap().items.extend(removed.items);
        }
        for p in self.placements.values_mut() {
            p.remove(&id);
        }
        true
    }

    /// Remember the current rectangles under a display-configuration key.
    pub fn store_placement(&mut self, display: &str) {
        let map = self.fences.iter().map(|f| (f.id, f.rect)).collect();
        self.placements.insert(display.to_string(), map);
    }

    /// Restore rectangles saved for `display`; groups without one are clamped into `area`.
    pub fn apply_placement(&mut self, display: &str, area: Rect) {
        let saved = self.placements.get(display).cloned().unwrap_or_default();
        for f in &mut self.fences {
            f.rect = saved.get(&f.id).copied().unwrap_or(f.rect).clamp_into(area);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect { x: 0, y: 0, w: 1920, h: 1040 };

    fn p<'a>(key: &'a str, folder: bool) -> Present<'a> {
        Present { key, name: key.rsplit('\\').next().unwrap(), is_folder: folder }
    }

    fn sample() -> Vec<Present<'static>> {
        vec![
            p(r"C:\D\Steam.lnk", false),
            p(r"C:\D\Audacity.lnk", false),
            p(r"C:\D\Movies", true),
            p(r"C:\D\Work", true),
            p(r"C:\D\notes.txt", false),
            p("::{RECYCLE}", true),
        ]
    }

    #[test]
    fn first_run_groups_by_kind_and_keeps_singletons_in_desktop() {
        let mut c = Config::default();
        c.first_run(&sample(), AREA, 96);
        let titles: Vec<_> = c.fences.iter().map(|f| f.title.as_str()).collect();
        assert_eq!(titles, ["Apps", "Folders", "Desktop"]);
        assert_eq!(c.fences[0].items, [r"C:\D\Audacity.lnk", r"C:\D\Steam.lnk"], "sorted by name");
        assert_eq!(c.fences[2].items.len(), 2, "lone document and lone system item");
        let total: usize = c.fences.iter().map(|f| f.items.len()).sum();
        assert_eq!(total, 6);
        for f in &c.fences {
            assert!(f.rect.w > 0 && f.rect.x + f.rect.w <= AREA.w && f.rect.y + f.rect.h <= AREA.h);
        }
    }

    #[test]
    fn reconcile_files_new_items_and_drops_removed_ones() {
        let mut c = Config::default();
        let mut items = sample();
        c.first_run(&items, AREA, 96);
        items.remove(0);
        items.push(p(r"C:\D\Zed.exe", false));
        items.push(p(r"C:\D\photo.png", false));
        let changed = c.reconcile(&items, AREA);
        assert_eq!(changed.len(), 2);
        assert_eq!(c.fences[0].items, [r"C:\D\Audacity.lnk", r"C:\D\Zed.exe"]);
        assert!(c.fences[2].items.contains(&r"C:\D\photo.png".to_string()));
        assert!(c.reconcile(&items, AREA).is_empty(), "idempotent");
    }

    #[test]
    fn move_item_reorders_and_switches_to_manual() {
        let mut c = Config::default();
        c.first_run(&sample(), AREA, 96);
        let (apps, folders) = (c.fences[0].id, c.fences[1].id);
        assert!(c.move_item(r"C:\D\Steam.lnk", folders, 0));
        assert_eq!(c.fence(folders).unwrap().items[0], r"C:\D\Steam.lnk");
        assert_eq!(c.fence(folders).unwrap().sort, Sort::Manual);
        assert_eq!(c.fence(apps).unwrap().items.len(), 1);
        // Reorder inside one group: moving the first item to the end.
        assert!(c.move_item(r"C:\D\Steam.lnk", folders, 3));
        assert_eq!(c.fence(folders).unwrap().items.last().unwrap(), r"C:\D\Steam.lnk");
        assert!(!c.move_item("missing", folders, 0));
    }

    #[test]
    fn rename_keeps_group_and_remove_fence_returns_items_to_desktop() {
        let mut c = Config::default();
        c.first_run(&sample(), AREA, 96);
        assert!(c.rename_key(r"c:\d\steam.lnk", r"C:\D\Steam2.lnk"));
        assert!(c.fences[0].items.contains(&r"C:\D\Steam2.lnk".to_string()));
        let (apps, desktop) = (c.fences[0].id, c.fences[2].id);
        assert!(!c.remove_fence(desktop, AREA), "catch-all cannot be removed");
        assert!(c.remove_fence(apps, AREA));
        assert_eq!(c.fence(desktop).unwrap().items.len(), 4);
    }

    #[test]
    fn profiles_are_independent_layouts() {
        let mut c = Config::default();
        c.first_run(&sample(), AREA, 96);
        c.store_placement("big");
        let (apps, folders) = (c.fences[0].id, c.fences[1].id);
        assert_eq!(c.profile_list(), [(1, "Default".to_string(), true)]);

        let gaming = c.profile_new(Some("  Gaming "));
        assert_eq!(c.profile_list(), [(1, "Default".to_string(), false), (gaming, "Gaming".to_string(), true)]);
        assert_eq!(c.fences.len(), 3, "starts as a copy");
        let copy_of_apps = c.fences[0].id;
        assert!(copy_of_apps != apps && c.placements["big"].contains_key(&copy_of_apps), "copies get fresh ids and keep their positions");

        // Rearranging the new profile leaves the original untouched.
        assert!(c.move_item(r"C:\D\Steam.lnk", c.fences[1].id, 0));
        c.fences[0].title = "Games".into();
        assert!(c.profile_switch(1));
        assert_eq!(c.fence(apps).unwrap().title, "Apps");
        assert_eq!(c.fence(apps).unwrap().items.len(), 2);
        assert!(c.fence(folders).is_some() && c.fence(copy_of_apps).is_none());
        assert!(!c.profile_switch(1), "already on screen");
        assert!(!c.profile_switch(99));

        assert!(c.profile_rename(gaming, "Play") && !c.profile_rename(gaming, "   "));
        assert_eq!(c.profile_delete(gaming), Some(false));
        assert_eq!(c.profile_delete(1), None, "the last profile stays");
        let other = c.profile_new(None);
        assert_eq!(c.profile_delete(other), Some(true), "deleting the on-screen profile falls back to another");
        assert_eq!((c.profile_id, c.profiles.len()), (1, 0));
    }

    #[test]
    fn portals_stay_out_of_desktop_bookkeeping() {
        let mut c = Config::default();
        c.first_run(&sample(), AREA, 96);
        let portal = c.add_portal(r"C:\Users\a\Downloads\", AREA);
        let f = c.fence_mut(portal).unwrap();
        assert_eq!((f.title.as_str(), f.portal.as_deref()), ("Downloads", Some(r"C:\Users\a\Downloads")));
        f.items = vec![r"C:\Users\a\Downloads\b.zip".into(), r"C:\Users\a\Downloads\Sub".into(), r"C:\Users\a\Downloads\a.txt".into()];

        // Reconciling the desktop neither empties the portal nor files desktop items into it.
        assert!(c.reconcile(&sample(), AREA).is_empty());
        assert_eq!(c.fence(portal).unwrap().items.len(), 3);
        assert!(!c.move_item(r"C:\D\Steam.lnk", portal, 0), "desktop items cannot be regrouped into a portal");
        assert!(!c.move_item(r"C:\Users\a\Downloads\a.txt", c.fences[0].id, 0), "nor portal items out of one");

        // Folders first, then by name.
        let listing = [p(r"C:\Users\a\Downloads\b.zip", false), p(r"C:\Users\a\Downloads\Sub", true), p(r"C:\Users\a\Downloads\a.txt", false)];
        c.sort_fence(portal, &listing.iter().map(|p| (p.key, p)).collect());
        assert_eq!(c.fence(portal).unwrap().items, [r"C:\Users\a\Downloads\Sub", r"C:\Users\a\Downloads\a.txt", r"C:\Users\a\Downloads\b.zip"]);

        let desktop_items = c.fences.iter().find(|f| f.inbox).unwrap().items.len();
        assert!(c.remove_fence(portal, AREA));
        assert_eq!(c.fences.iter().find(|f| f.inbox).unwrap().items.len(), desktop_items, "a deleted portal's listing is not dumped on the desktop");
    }

    #[test]
    fn tabs_share_a_window_but_keep_their_own_items() {
        let mut c = Config::default();
        c.first_run(&sample(), AREA, 96);
        let (apps, folders, desktop) = (c.fences[0].id, c.fences[1].id, c.fences[2].id);
        c.store_placement("big");
        assert_eq!(c.hosts(), [apps, folders, desktop]);

        // Combine Folders into Apps: one window, two tabs, the newcomer in front.
        assert!(c.merge_tabs(folders, apps));
        assert_eq!(c.hosts(), [apps, desktop]);
        assert_eq!(c.tab_order(folders), [apps, folders]);
        assert_eq!((c.tab_host(folders), c.shown_tab(apps)), (apps, folders));
        assert!(!c.placements["big"].contains_key(&folders), "a tab has no window of its own");
        assert_eq!(c.fence(folders).unwrap().items.len(), 2, "items stay with their tab");

        // New items are still filed by each tab's own rule.
        let mut more = sample();
        more.push(p(r"C:\D\Games", true));
        c.reconcile(&more, AREA);
        assert_eq!(c.fence(folders).unwrap().items.len(), 3);

        let tab = c.add_tab(apps, Some("  Work ")).unwrap();
        assert_eq!((c.tab_order(apps), c.shown_tab(apps)), (vec![apps, folders, tab], tab));
        assert_eq!(c.fence(tab).unwrap().title, "Work");
        assert!(c.select_tab(apps) && !c.select_tab(apps));
        assert_eq!(c.shown_tab(tab), apps);

        // Moving the host's tab hands the window to the new first tab.
        let rect = c.fence(apps).unwrap().rect;
        assert!(c.move_tab(apps, 1));
        assert_eq!(c.tab_order(apps), [folders, apps, tab]);
        assert_eq!((c.fence(folders).unwrap().rect, c.hosts()), (rect, vec![folders, desktop]));
        assert!(c.placements["big"].contains_key(&folders));
        assert_eq!(c.shown_tab(folders), apps, "the tab on screen stays on screen");

        assert!(c.detach_tab(apps, AREA));
        assert_eq!(c.tab_order(folders), [folders, tab]);
        assert!(c.hosts().contains(&apps) && c.fence(apps).unwrap().rect.x > rect.x);

        // Deleting the host keeps the window: its next tab takes over. Items go to the catch-all.
        assert!(c.remove_fence(folders, AREA));
        assert_eq!(c.tab_order(tab), [tab]);
        assert_eq!(c.fence(tab).unwrap().rect, rect);
        assert!(c.fence(desktop).unwrap().items.len() >= 3);
        assert!(!c.merge_tabs(tab, tab));
    }

    #[test]
    fn placements_are_per_display_configuration() {
        let mut c = Config::default();
        c.first_run(&sample(), AREA, 96);
        c.store_placement("big");
        let original = c.fences[1].rect;
        c.fences[1].rect.x = 1700;
        c.store_placement("big");
        let small = Rect { x: 0, y: 0, w: 1280, h: 680 };
        c.apply_placement("small", small);
        assert!(c.fences[1].rect.x + c.fences[1].rect.w <= 1280, "clamped onto the smaller display");
        c.store_placement("small");
        c.apply_placement("big", AREA);
        assert_eq!(c.fences[1].rect.x, 1700, "restored exactly when the big display returns");
        assert_ne!(original.x, 1700);
    }
}
