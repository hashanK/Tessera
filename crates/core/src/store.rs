//! Config persistence: versioned JSON, written atomically, with the previous file kept as a backup.
use crate::model::{Config, CONFIG_VERSION};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Where layout and logs live: beside the executable in portable mode (a `tessera.portable`
/// marker file next to it), otherwise `%APPDATA%\Tessera`.
pub fn data_dir() -> PathBuf {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)).unwrap_or_default();
    if exe_dir.join("tessera.portable").exists() {
        return exe_dir.join("data");
    }
    std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or(exe_dir).join("Tessera")
}

fn sibling(path: &Path, ext: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(ext);
    PathBuf::from(s)
}

fn read(path: &Path) -> Option<Config> {
    let cfg: Config = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    (cfg.version <= CONFIG_VERSION).then_some(cfg)
}

/// Load the config, falling back to the backup if the main file is missing or unreadable.
pub fn load(path: &Path) -> Option<Config> {
    read(path).or_else(|| read(&sibling(path, ".bak")))
}

/// Write to a temp file, keep the current file as `.bak`, then rename into place.
pub fn save(path: &Path, cfg: &Config) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = sibling(path, ".tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(cfg).map_err(io::Error::other)?)?;
    if read(path).is_some() {
        let _ = fs::copy(path, sibling(path, ".bak"));
    }
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rect;

    #[test]
    fn round_trip_and_backup_fallback() {
        let dir = std::env::temp_dir().join(format!("tessera-store-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("layout.json");
        assert!(load(&path).is_none());

        let mut cfg = Config::default();
        cfg.add_fence("One", Rect { x: 1, y: 2, w: 3, h: 4 });
        save(&path, &cfg).unwrap();
        cfg.add_fence("Two", Rect::default());
        save(&path, &cfg).unwrap();
        assert_eq!(load(&path).unwrap().fences.len(), 2);

        // A truncated main file must not lose the layout: the backup holds the previous save.
        fs::write(&path, b"{ \"version\": 1, \"fen").unwrap();
        let recovered = load(&path).unwrap();
        assert_eq!(recovered.fences.len(), 1);
        assert_eq!(recovered.fences[0].title, "One");

        // Saving over a corrupt main file must not replace the good backup with the corrupt bytes.
        save(&path, &cfg).unwrap();
        assert_eq!(load(&path).unwrap().fences.len(), 2);
        fs::write(&path, b"garbage").unwrap();
        assert_eq!(load(&path).unwrap().fences[0].title, "One");
        let _ = fs::remove_dir_all(&dir);
    }
}
