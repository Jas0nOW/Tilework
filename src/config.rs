//! User settings, stored as a plain `key=value` file in `%LOCALAPPDATA%\CosmicTile\config.ini`.
//! Unknown keys and broken lines are ignored so an old or hand-edited file never stops the app.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Hovering a tiled window for `focus_delay_ms` gives it the keyboard focus.
    pub focus_follows_mouse: bool,
    pub focus_delay_ms: u64,
    /// Smallest zone a window may be squeezed into (keeps part of the title bar grabbable).
    pub min_zone_width: i32,
    pub min_zone_height: i32,
}

impl Default for Config {
    fn default() -> Self {
        Self { focus_follows_mouse: true, focus_delay_ms: 300, min_zone_width: 260, min_zone_height: 160 }
    }
}

impl Config {
    /// Returns the config path when Windows supplied a usable local app-data directory.
    pub fn path() -> Option<PathBuf> {
        std::env::var_os("LOCALAPPDATA")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .map(|base| base.join("CosmicTile").join("config.ini"))
    }

    pub fn parse(text: &str) -> Self {
        let mut c = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.starts_with(';') { continue; }
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "focus_follows_mouse" => if let Ok(b) = v.parse() { c.focus_follows_mouse = b; },
                "focus_delay_ms" => if let Ok(n) = v.parse::<u64>() { c.focus_delay_ms = n.clamp(50, 5000); },
                "min_zone_width" => if let Ok(n) = v.parse::<i32>() { c.min_zone_width = n.clamp(80, 1000); },
                "min_zone_height" => if let Ok(n) = v.parse::<i32>() { c.min_zone_height = n.clamp(60, 1000); },
                _ => {}
            }
        }
        c
    }

    pub fn to_text(&self) -> String {
        format!(
            "# CosmicTile settings (changes made here apply after restarting CosmicTile)\n\
             # Hover a window for focus_delay_ms to give it the keyboard focus.\n\
             focus_follows_mouse={}\n\
             focus_delay_ms={}\n\
             # Smallest zone a window can be squeezed into (pixels).\n\
             min_zone_width={}\n\
             min_zone_height={}\n",
            self.focus_follows_mouse, self.focus_delay_ms, self.min_zone_width, self.min_zone_height
        )
    }

    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            crate::app::log("LOCALAPPDATA is unavailable; using default settings");
            return Self::default();
        };
        load_from_path(&path)
    }

    pub fn save(&self) {
        let Some(path) = Self::path() else {
            crate::app::log("LOCALAPPDATA is unavailable; settings were not saved");
            return;
        };
        if let Err(e) = save_to_path(&path, &self.to_text()) {
            crate::app::log(&format!("could not save config: {e}"));
        }
    }

    /// Creates the defaults only when the config is absent; never replaces an existing file.
    pub fn ensure_exists(&self) {
        let Some(path) = Self::path() else {
            crate::app::log("LOCALAPPDATA is unavailable; config file was not created");
            return;
        };
        if let Err(e) = ensure_exists_at(&path, &self.to_text()) {
            crate::app::log(&format!("could not create config: {e}"));
        }
    }
}

fn load_from_path(path: &Path) -> Config {
    match std::fs::read_to_string(path) {
        Ok(text) => Config::parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let config = Config::default();
            if let Err(write_error) = ensure_exists_at(path, &config.to_text()) {
                crate::app::log(&format!("could not create default config: {write_error}"));
            }
            config
        }
        Err(e) => {
            crate::app::log(&format!("could not read config: {e}"));
            Config::default()
        }
    }
}

fn ensure_exists_at(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
    let mut file = match std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(e) => return Err(e),
    };
    use std::io::Write;
    file.write_all(text.as_bytes())?;
    file.flush()?;
    file.sync_all()
}

fn save_to_path(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let name = path.file_name().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "config path has no filename"))?;
    let (temp_path, mut file) = loop {
        let n = NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut temp_name = name.to_os_string();
        temp_name.push(format!(".{}.{}.tmp", std::process::id(), n));
        let candidate = parent.join(temp_name);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(file) => break (candidate, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let result = (|| {
        file.write_all(text.as_bytes())?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        replace_file(&temp_path, path)
    })();
    if result.is_err() { let _ = std::fs::remove_file(&temp_path); }
    result
}

#[cfg(windows)]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH};
    use windows::core::PCWSTR;
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe { MoveFileExW(PCWSTR(from.as_ptr()), PCWSTR(to.as_ptr()), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) }
        .map_err(std::io::Error::other)
}

#[cfg(not(windows))]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> { std::fs::rename(from, to) }

#[cfg(test)]
mod tests {
    use super::*;

    fn test_file(label: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(format!("config-test-{}-{label}.ini", std::process::id()))
    }

    #[test]
    fn defaults_follow_the_mouse_after_300_ms() {
        let c = Config::default();
        assert!(c.focus_follows_mouse);
        assert_eq!(c.focus_delay_ms, 300);
    }

    #[test]
    fn a_saved_config_reads_back_the_same() {
        let c = Config { focus_follows_mouse: false, focus_delay_ms: 800, min_zone_width: 300, min_zone_height: 200 };
        assert_eq!(Config::parse(&c.to_text()), c);
    }

    #[test]
    fn broken_lines_and_extreme_values_are_tolerated() {
        let c = Config::parse("garbage\nfocus_delay_ms=abc\nfocus_delay_ms=999999\nmin_zone_width=1\n# comment\nunknown=1\n");
        assert_eq!(c.focus_delay_ms, 5000);
        assert_eq!(c.min_zone_width, 80);
        assert!(c.focus_follows_mouse);
    }

    #[test]
    fn save_atomically_replaces_an_existing_config() {
        let path = test_file("atomic-overwrite");
        let config = Config { focus_follows_mouse: false, focus_delay_ms: 900, min_zone_width: 320, min_zone_height: 220 };
        std::fs::write(&path, "old contents").unwrap();
        save_to_path(&path, &config.to_text()).unwrap();
        assert_eq!(Config::parse(&std::fs::read_to_string(&path).unwrap()), config);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn ensure_exists_preserves_existing_edits() {
        let path = test_file("ensure-preserve");
        let edited = "focus_delay_ms=777\nhand-edited=true\n";
        std::fs::write(&path, edited).unwrap();
        ensure_exists_at(&path, &Config::default().to_text()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn missing_load_creates_defaults_and_returns_them() {
        let path = test_file("missing-load");
        let _ = std::fs::remove_file(&path);
        assert_eq!(load_from_path(&path), Config::default());
        assert_eq!(Config::parse(&std::fs::read_to_string(&path).unwrap()), Config::default());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn read_error_returns_defaults_without_modifying_target() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join(format!("config-test-{}-read-error", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("existing-directory.ini");
        std::fs::write(&path, "sentinel").unwrap();
        assert_eq!(load_from_path(&dir), Config::default());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "sentinel");
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
}
