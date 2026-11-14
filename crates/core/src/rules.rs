//! Classifies desktop items so they can be filed into groups automatically.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Category {
    System,
    Apps,
    Folders,
    Documents,
    Images,
    Media,
    Archives,
    Code,
    Other,
}

impl Category {
    pub const ALL: [Category; 9] = [
        Category::Apps,
        Category::Folders,
        Category::Documents,
        Category::Images,
        Category::Media,
        Category::Archives,
        Category::Code,
        Category::System,
        Category::Other,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Category::System => "System",
            Category::Apps => "Apps",
            Category::Folders => "Folders",
            Category::Documents => "Documents",
            Category::Images => "Images",
            Category::Media => "Media",
            Category::Archives => "Archives",
            Category::Code => "Code",
            Category::Other => "Other",
        }
    }
}

/// `key` is the shell parsing name: a full path, or `::{CLSID}` for virtual items.
pub fn classify(key: &str, is_folder: bool) -> Category {
    if key.starts_with("::") {
        return Category::System;
    }
    if is_folder {
        return Category::Folders;
    }
    let file = key.rsplit(['\\', '/']).next().unwrap_or(key);
    let ext = match file.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext.to_ascii_lowercase(),
        _ => return Category::Other,
    };
    match ext.as_str() {
        "lnk" | "exe" | "url" | "appref-ms" | "bat" | "cmd" | "msi" | "com" | "website" => Category::Apps,
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "md" | "rtf" | "csv" | "odt" | "ods"
        | "odp" | "epub" | "one" => Category::Documents,
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg" | "ico" | "heic" | "tif" | "tiff" | "psd" | "raw" => {
            Category::Images
        }
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" | "mp4" | "mkv" | "mov" | "avi" | "webm" | "wmv" => {
            Category::Media
        }
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "iso" | "cab" => Category::Archives,
        "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "json" | "html" | "css" | "c" | "cpp" | "h" | "hpp" | "cs"
        | "java" | "go" | "sh" | "ps1" | "yaml" | "yml" | "toml" | "xml" | "sql" | "dart" | "kt" | "swift" => {
            Category::Code
        }
        _ => Category::Other,
    }
}

/// True for text that is a single web address, the only kind of dropped text we turn into a shortcut.
pub fn is_web_url(text: &str) -> bool {
    let t = text.trim();
    let lower = t.get(..8).unwrap_or(t).to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://")) && t.len() < 2048 && !t.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// File name for the Internet shortcut created when a link is dropped on a group.
/// Uses the page title if the browser supplied one, otherwise the site's host name.
pub fn shortcut_name(title: &str, url: &str) -> String {
    let clean = |s: &str| -> String {
        let s: String = s.chars().filter(|c| !c.is_control() && !r#"<>:"/\|?*"#.contains(*c)).collect();
        s.trim_matches(|c: char| c == '.' || c.is_whitespace()).chars().take(80).collect::<String>().trim_end().to_string()
    };
    let title = title.trim();
    let cut = title.len().saturating_sub(4);
    let title = if title.is_char_boundary(cut) && title[cut..].eq_ignore_ascii_case(".url") { &title[..cut] } else { title };
    let mut name = clean(title);
    if name.is_empty() {
        let host = url.split("://").nth(1).unwrap_or(url).split(['/', '?', '#', ':']).next().unwrap_or("");
        name = clean(host.strip_prefix("www.").unwrap_or(host));
    }
    if name.is_empty() {
        name = "Link".to_string();
    }
    name + ".url"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_urls_and_shortcut_names() {
        assert!(is_web_url("https://example.com/a?b=1"));
        assert!(is_web_url("  HTTP://Example.com  "));
        assert!(!is_web_url("just some text"));
        assert!(!is_web_url("https://a.com and more"));
        assert!(!is_web_url("file:///C:/Windows"));
        assert_eq!(shortcut_name("Rust: Docs / Home?.url", "https://x"), "Rust Docs  Home.url");
        assert_eq!(shortcut_name("", "https://www.example.com/path"), "example.com.url");
        assert_eq!(shortcut_name("  ...  ", "https://sub.site.org:8080/x"), "sub.site.org.url");
        assert_eq!(shortcut_name("", ""), "Link.url");
        assert!(shortcut_name(&"x".repeat(300), "").len() <= 84);
    }

    #[test]
    fn classifies_by_kind_and_extension() {
        assert_eq!(classify("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}", true), Category::System);
        assert_eq!(classify(r"C:\Users\a\Desktop\Projects", true), Category::Folders);
        assert_eq!(classify(r"C:\Users\a\Desktop\Steam.lnk", false), Category::Apps);
        assert_eq!(classify(r"C:\Users\a\Desktop\Report.PDF", false), Category::Documents);
        assert_eq!(classify(r"C:\Users\a\Desktop\pic.jpeg", false), Category::Images);
        assert_eq!(classify(r"C:\Users\a\Desktop\noext", false), Category::Other);
        assert_eq!(classify(r"C:\Users\a\Desktop\.gitignore", false), Category::Other);
        assert_eq!(classify(r"C:\Users\a.b\Desktop\noext", false), Category::Other);
    }
}
