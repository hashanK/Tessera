//! Commands another process (the settings window, a script) can send to the running organiser.
//! They are JSON objects tagged by `cmd`, e.g. `{"cmd":"fence","id":2,"opacity":80}`.
use crate::look::Preset;
use crate::model::{Config, Rect, Sort, Style, TabStyle, Theme, Tint};
use crate::rules::Category;
use serde::{Deserialize, Serialize};

/// `dwData` of the WM_COPYDATA message that carries a command ("TESS").
pub const COPYDATA_MAGIC: usize = 0x5445_5353;
pub const MAIN_WINDOW_CLASS: &str = "TesseraMain";
pub const ICON_SIZES: [u32; 3] = [32, 48, 64];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    Theme { value: Theme },
    /// Change any subset of one group's properties.
    Fence {
        id: u32,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        tint: Option<Tint>,
        #[serde(default)]
        opacity: Option<u8>,
        #[serde(default)]
        icon: Option<u32>,
        #[serde(default)]
        sort: Option<Sort>,
        #[serde(default)]
        collapsed: Option<bool>,
        #[serde(default)]
        tab_style: Option<TabStyle>,
    },
    /// Which kind of new item this group collects; `null` clears the rule.
    Rule { id: u32, category: Option<Category> },
    /// Style given to groups created from now on.
    Defaults {
        #[serde(default)]
        tint: Option<Tint>,
        #[serde(default)]
        opacity: Option<u8>,
        #[serde(default)]
        icon: Option<u32>,
    },
    /// Give every existing group the default style.
    ApplyAll,
    NewGroup,
    Delete { id: u32 },
    Rebuild,
    /// Float the groups above every window (`true`) or return them to the desktop.
    Peek { value: bool },
    /// Hide or show every group.
    Hidden { value: bool },
    ProfileSwitch { id: u32 },
    /// New profile, starting as a copy of the one on screen; the organiser switches to it.
    ProfileNew {
        #[serde(default)]
        name: Option<String>,
    },
    ProfileRename { id: u32, name: String },
    ProfileDelete { id: u32 },
    /// Add an empty tab to the window group `id` belongs to.
    TabAdd {
        id: u32,
        #[serde(default)]
        title: Option<String>,
    },
    /// Bring tab `id` to the front of its window.
    TabSelect { id: u32 },
    /// Put every tab of `id`'s window into `into`'s window.
    TabMerge { id: u32, into: u32 },
    /// Make tab `id` a group of its own.
    TabDetach { id: u32 },
    /// Move tab `id` left (negative) or right.
    TabMove { id: u32, delta: i32 },
    /// New folder portal. Without a path the organiser asks the user to pick a folder.
    NewPortal {
        #[serde(default)]
        path: Option<String>,
    },
    /// Replace a group's whole appearance (the appearance editor sends this, live while
    /// previewing on the desktop). Applies to the window the group is shown in.
    Look { id: u32, style: Box<Style> },
    /// Appearance for groups created from now on.
    LookDefaults { style: Box<Style> },
    /// Give every group this appearance, keeping each group's icon size.
    LookAll { style: Box<Style> },
    /// Save a custom theme, replacing one with the same name.
    ThemeSave { name: String, style: Box<Style> },
    ThemeRename { from: String, to: String },
    ThemeDelete { name: String },
    /// Turn wallpaper blur, grain and shadows off (`true`) or back on.
    Effects { low: bool },
    Quit,
}

/// What the organiser has to do on screen after a command changed the model.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    None,
    /// Re-place and repaint these groups.
    Restyle(Vec<u32>),
    /// The group's sort mode changed; re-sort it, then repaint.
    Resort(u32),
    RepaintAll,
    Created(u32),
    Deleted(u32),
    Rebuild,
    Peek(bool),
    Hidden(bool),
    /// A different profile is now the on-screen layout: rebuild every group window.
    Reload,
    /// Nothing visible changed, but the model did and must be saved.
    SaveOnly,
    /// Ask the user for a folder, then add a portal for it.
    PickPortal,
    /// Which groups have windows changed (tabs combined, split or reordered): sync the windows.
    Windows,
    Quit,
}

fn patch_style(style: &mut Style, tint: Option<Tint>, opacity: Option<u8>, icon: Option<u32>) -> bool {
    let before = style.clone();
    if let Some(t) = tint {
        // A colour family picked from the quick swatches replaces any custom colours, or the
        // choice would have no visible effect.
        style.tint = t;
        style.color = None;
        style.accent = None;
    }
    if let Some(o) = opacity {
        style.opacity = o.clamp(20, 100);
    }
    if let Some(i) = icon.filter(|i| ICON_SIZES.contains(i)) {
        style.icon = i;
    }
    *style != before
}

fn effect_if(changed: bool) -> Effect {
    if changed {
        Effect::Windows
    } else {
        Effect::None
    }
}

impl Config {
    /// Apply a command to the model. Invalid values are ignored rather than rejected wholesale.
    pub fn apply(&mut self, cmd: Command, area: Rect) -> Effect {
        match cmd {
            Command::Theme { value } => {
                if self.theme == value {
                    return Effect::None;
                }
                self.theme = value;
                Effect::RepaintAll
            }
            Command::Fence { id, title, tint, opacity, icon, sort, collapsed, tab_style } => {
                let Some(f) = self.fence_mut(id) else { return Effect::None };
                let mut changed = patch_style(&mut f.style, tint, opacity, icon);
                if let Some(t) = title.map(|t| t.trim().chars().take(64).collect::<String>()).filter(|t| !t.is_empty()) {
                    changed |= f.title != t;
                    f.title = t;
                }
                if let Some(c) = collapsed {
                    changed |= f.collapsed != c;
                    f.collapsed = c;
                }
                if let Some(t) = tab_style {
                    changed |= f.tab_style != t;
                    f.tab_style = t;
                }
                match sort.filter(|s| *s != f.sort) {
                    Some(s) => {
                        f.sort = s;
                        Effect::Resort(id)
                    }
                    None if changed => Effect::Restyle(vec![id]),
                    None => Effect::None,
                }
            }
            Command::Rule { id, category } => {
                if self.fence(id).is_none_or(|f| f.is_portal()) {
                    return Effect::None;
                }
                // A kind is collected by at most one group, so filing stays unambiguous.
                for f in &mut self.fences {
                    if category.is_some() && f.category == category {
                        f.category = None;
                    }
                }
                self.fence_mut(id).unwrap().category = category;
                Effect::SaveOnly
            }
            Command::Defaults { tint, opacity, icon } => {
                patch_style(&mut self.default_style, tint, opacity, icon);
                Effect::SaveOnly
            }
            Command::ApplyAll => {
                let style = self.default_style.clone();
                self.fences.iter_mut().for_each(|f| f.style = style.clone());
                Effect::Restyle(self.fences.iter().map(|f| f.id).collect())
            }
            Command::NewGroup => {
                let (w, h) = crate::layout::Metrics::new(&self.default_style, 96).size_for(3, 2);
                let rect = Rect { x: area.x + (area.w - w) / 2, y: area.y + (area.h - h) / 2, w, h }.clamp_into(area);
                Effect::Created(self.add_fence("New group", rect))
            }
            Command::Delete { id } => {
                if self.remove_fence(id, area) {
                    Effect::Deleted(id)
                } else {
                    Effect::None
                }
            }
            Command::Rebuild => Effect::Rebuild,
            Command::ProfileSwitch { id } => {
                if self.profile_switch(id) {
                    Effect::Reload
                } else {
                    Effect::None
                }
            }
            Command::ProfileNew { name } => {
                self.profile_new(name.as_deref());
                Effect::Reload
            }
            Command::ProfileRename { id, name } => {
                if self.profile_rename(id, &name) {
                    Effect::SaveOnly
                } else {
                    Effect::None
                }
            }
            Command::ProfileDelete { id } => match self.profile_delete(id) {
                Some(true) => Effect::Reload,
                Some(false) => Effect::SaveOnly,
                None => Effect::None,
            },
            Command::TabAdd { id, title } => match self.add_tab(id, title.as_deref()) {
                Some(_) => Effect::Windows,
                None => Effect::None,
            },
            Command::TabSelect { id } => {
                if self.select_tab(id) {
                    Effect::Restyle(vec![self.tab_host(id)])
                } else {
                    Effect::None
                }
            }
            Command::TabMerge { id, into } => effect_if(self.merge_tabs(id, into)),
            Command::TabDetach { id } => effect_if(self.detach_tab(id, area)),
            Command::TabMove { id, delta } => effect_if(self.move_tab(id, delta)),
            Command::NewPortal { path: None } => Effect::PickPortal,
            Command::NewPortal { path: Some(path) } => {
                if path.trim().is_empty() {
                    Effect::None
                } else {
                    Effect::Created(self.add_portal(&path, area))
                }
            }
            Command::Look { id, mut style } => {
                let host = self.tab_host(id);
                let Some(f) = self.fence_mut(host) else { return Effect::None };
                style.sanitize();
                if f.style == *style {
                    return Effect::None;
                }
                f.style = *style;
                Effect::Restyle(vec![host])
            }
            Command::LookDefaults { mut style } => {
                style.sanitize();
                self.default_style = *style;
                Effect::SaveOnly
            }
            Command::LookAll { mut style } => {
                style.sanitize();
                let hosts = self.hosts();
                for f in self.fences.iter_mut().filter(|f| hosts.contains(&f.id)) {
                    f.style = f.style.themed(&style);
                }
                Effect::Restyle(hosts)
            }
            Command::ThemeSave { name, mut style } => {
                let Some(name) = theme_name(&name) else { return Effect::None };
                style.sanitize();
                let count = self.custom_themes.len();
                match self.custom_themes.iter_mut().find(|t| t.name.eq_ignore_ascii_case(&name)) {
                    Some(t) => t.style = *style,
                    None if count < MAX_THEMES => self.custom_themes.push(Preset { name, style: *style }),
                    None => return Effect::None,
                }
                Effect::SaveOnly
            }
            Command::ThemeRename { from, to } => {
                let Some(to) = theme_name(&to) else { return Effect::None };
                let taken = self.custom_themes.iter().any(|t| t.name.eq_ignore_ascii_case(&to) && !t.name.eq_ignore_ascii_case(&from));
                match self.custom_themes.iter_mut().find(|t| t.name == from) {
                    Some(t) if !taken => {
                        t.name = to;
                        Effect::SaveOnly
                    }
                    _ => Effect::None,
                }
            }
            Command::ThemeDelete { name } => {
                let before = self.custom_themes.len();
                self.custom_themes.retain(|t| t.name != name);
                if self.custom_themes.len() != before {
                    Effect::SaveOnly
                } else {
                    Effect::None
                }
            }
            Command::Effects { low } => {
                if self.low_effects == low {
                    return Effect::None;
                }
                self.low_effects = low;
                Effect::Restyle(self.hosts())
            }
            Command::Peek { value } => Effect::Peek(value),
            Command::Hidden { value } => Effect::Hidden(value),
            Command::Quit => Effect::Quit,
        }
    }
}

const MAX_THEMES: usize = 100;

/// A custom theme's name: trimmed, printable, at most 40 characters, and not the name of a
/// built-in theme (those can't be overwritten).
fn theme_name(name: &str) -> Option<String> {
    let name: String = name.trim().chars().filter(|c| !c.is_control()).take(40).collect();
    let builtin = crate::look::builtin_themes().iter().any(|t| t.name.eq_ignore_ascii_case(&name));
    (!name.is_empty() && !builtin).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::look::{Material, Rgb};

    const AREA: Rect = Rect { x: 0, y: 0, w: 1920, h: 1040 };

    fn parse(json: &str) -> Command {
        serde_json::from_str(json).unwrap()
    }

    fn two_groups() -> (Config, u32, u32) {
        let mut c = Config::default();
        let a = c.add_fence("A", Rect { x: 0, y: 0, w: 200, h: 200 });
        let b = c.add_fence("B", Rect { x: 300, y: 0, w: 200, h: 200 });
        c.fence_mut(b).unwrap().inbox = true;
        (c, a, b)
    }

    #[test]
    fn json_shape_is_stable() {
        assert_eq!(parse(r#"{"cmd":"theme","value":"Light"}"#), Command::Theme { value: Theme::Light });
        assert_eq!(parse(r#"{"cmd":"new_group"}"#), Command::NewGroup);
        assert_eq!(parse(r#"{"cmd":"rule","id":3,"category":null}"#), Command::Rule { id: 3, category: None });
        assert_eq!(
            parse(r#"{"cmd":"fence","id":2,"opacity":80}"#),
            Command::Fence { id: 2, title: None, tint: None, opacity: Some(80), icon: None, sort: None, collapsed: None, tab_style: None }
        );
        assert_eq!(parse(r#"{"cmd":"tab_move","id":4,"delta":-1}"#), Command::TabMove { id: 4, delta: -1 });
        assert!(serde_json::from_str::<Command>(r#"{"cmd":"format_disk"}"#).is_err());
    }

    #[test]
    fn fence_patch_validates_and_reports_the_right_effect() {
        let (mut c, a, _) = two_groups();
        let e = c.apply(parse(&format!(r#"{{"cmd":"fence","id":{a},"title":"  Work  ","tint":"Teal","opacity":5,"icon":40}}"#)), AREA);
        assert_eq!(e, Effect::Restyle(vec![a]));
        let f = c.fence(a).unwrap();
        assert_eq!((f.title.as_str(), f.style.tint, f.style.opacity, f.style.icon), ("Work", Tint::Teal, 20, 48), "opacity clamped, bad icon size ignored");
        assert_eq!(c.apply(parse(&format!(r#"{{"cmd":"fence","id":{a},"title":"   "}}"#)), AREA), Effect::None, "blank title ignored");
        assert_eq!(c.apply(parse(&format!(r#"{{"cmd":"fence","id":{a},"sort":"Name"}}"#)), AREA), Effect::Resort(a));
        assert_eq!(c.apply(parse(r#"{"cmd":"fence","id":999,"opacity":50}"#), AREA), Effect::None);
    }

    #[test]
    fn a_kind_belongs_to_one_group() {
        let (mut c, a, b) = two_groups();
        c.apply(parse(&format!(r#"{{"cmd":"rule","id":{a},"category":"Apps"}}"#)), AREA);
        c.apply(parse(&format!(r#"{{"cmd":"rule","id":{b},"category":"Apps"}}"#)), AREA);
        assert_eq!(c.fence(a).unwrap().category, None);
        assert_eq!(c.fence(b).unwrap().category, Some(Category::Apps));
    }

    #[test]
    fn defaults_apply_to_new_and_optionally_all_groups() {
        let (mut c, a, b) = two_groups();
        c.apply(parse(r#"{"cmd":"defaults","tint":"Rose","opacity":90,"icon":64}"#), AREA);
        assert_eq!(c.fence(a).unwrap().style, Style::default(), "existing groups untouched");
        let Effect::Created(n) = c.apply(Command::NewGroup, AREA) else { panic!() };
        assert_eq!(c.fence(n).unwrap().style, Style { tint: Tint::Rose, opacity: 90, icon: 64, ..Style::default() });
        assert_eq!(c.apply(Command::ApplyAll, AREA), Effect::Restyle(vec![a, b, n]));
        assert_eq!(c.fence(a).unwrap().style.tint, Tint::Rose);
    }

    #[test]
    fn looks_are_sanitised_and_themes_managed() {
        let (mut c, a, b) = two_groups();
        // A look sent as JSON with only some fields: the rest default, wild values are clamped.
        let e = c.apply(parse(&format!(r##"{{"cmd":"look","id":{a},"style":{{"material":"Acrylic","blur":250,"color":"#102030","shadow":{{"kind":"Soft","blur":99}}}}}}"##)), AREA);
        assert_eq!(e, Effect::Restyle(vec![a]));
        let s = &c.fence(a).unwrap().style;
        assert_eq!((s.material, s.blur, s.color, s.shadow.blur, s.icon), (Material::Acrylic, 100, Some(Rgb::hex(0x102030)), 40, 48));
        assert!(serde_json::from_str::<Command>(&format!(r#"{{"cmd":"look","id":{a},"style":{{"color":"red"}}}}"#)).is_err(), "bad colour rejected");

        // Applying to all keeps each group's icon size.
        c.fence_mut(b).unwrap().style.icon = 64;
        let midnight = crate::look::builtin_themes().into_iter().find(|t| t.name == "Midnight Glass").unwrap().style;
        assert_eq!(c.apply(Command::LookAll { style: Box::new(midnight.clone()) }, AREA), Effect::Restyle(vec![a, b]));
        assert_eq!(c.fence(b).unwrap().style, Style { icon: 64, ..midnight.clone() });

        // Custom themes: save, overwrite by name, refuse built-in names, rename, delete.
        let save = |name: &str| Command::ThemeSave { name: name.into(), style: Box::new(midnight.clone()) };
        assert_eq!(c.apply(save("  Mine "), AREA), Effect::SaveOnly);
        assert_eq!(c.apply(save("mine"), AREA), Effect::SaveOnly);
        assert_eq!(c.custom_themes.len(), 1);
        assert_eq!(c.apply(save("Fluent"), AREA), Effect::None, "built-in names are reserved");
        assert_eq!(c.apply(Command::ThemeRename { from: "Mine".into(), to: "Night".into() }, AREA), Effect::SaveOnly);
        assert_eq!(c.apply(Command::ThemeDelete { name: "Night".into() }, AREA), Effect::SaveOnly);
        assert!(c.custom_themes.is_empty());

        // Every built-in theme survives sanitising unchanged: presets are already in range.
        for t in crate::look::builtin_themes() {
            let mut s = t.style.clone();
            s.sanitize();
            assert_eq!(s, t.style, "{} out of range", t.name);
        }
    }

    #[test]
    fn old_layouts_load_with_the_original_look() {
        let json = r#"{"tint":"Teal","opacity":40,"icon":32}"#;
        let s: Style = serde_json::from_str(json).unwrap();
        assert_eq!(s, Style { tint: Tint::Teal, opacity: 40, icon: 32, ..Style::default() });
        let back: Style = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s, "round trip");
    }

    #[test]
    fn delete_respects_the_catch_all() {
        let (mut c, a, b) = two_groups();
        assert_eq!(c.apply(Command::Delete { id: b }, AREA), Effect::None);
        assert_eq!(c.apply(Command::Delete { id: a }, AREA), Effect::Deleted(a));
        assert_eq!(c.fences.len(), 1);
    }
}
