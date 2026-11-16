//! User settings, stored as a plain `key=value` file in `%LOCALAPPDATA%\Tilework\config.ini`.
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
    pub verbose_log: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self { focus_follows_mouse: true, focus_delay_ms: 300, min_zone_width: 260, min_zone_height: 160, verbose_log: false }
    }
}

impl Config {
    /// Returns the config path when Windows supplied a usable local app-data directory.
    pub fn path() -> Option<PathBuf> {
        config_path(std::env::var_os("LOCALAPPDATA"))
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
                "verbose_log" => if let Ok(b) = v.parse() { c.verbose_log = b; },
                _ => {}
            }
        }
        c
    }

    pub fn to_text(&self) -> String {
        format!(
            "# Tilework settings (changes made here apply after restarting Tilework)\n\
             # Hover a window for focus_delay_ms to give it the keyboard focus.\n\
             focus_follows_mouse={}\n\
             focus_delay_ms={}\n\
             # Smallest zone a window can be squeezed into (pixels).\n\
             min_zone_width={}\n\
             min_zone_height={}\n\
             # Include window titles in the log.\n\
             verbose_log={}\n",
            self.focus_follows_mouse, self.focus_delay_ms, self.min_zone_width, self.min_zone_height, self.verbose_log
        )
    }

    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            crate::app::log("LOCALAPPDATA is unavailable; using default settings");
            return Self::default();
        };
        load_from_path(&path)
    }

    pub fn update_keys(keys: &[(&str, &str)]) -> std::io::Result<()> {
        let path = Self::path().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "LOCALAPPDATA is unavailable"))?;
        update_keys_at(&path, keys)
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

fn config_path(base: Option<std::ffi::OsString>) -> Option<PathBuf> {
    let base = PathBuf::from(base?);
    // A malformed environment variable must not redirect writes into the working directory.
    base.is_absolute().then(|| base.join("Tilework").join("config.ini"))
}

/// Settings location of the former project name, next to the current one.
fn legacy_config_path(path: &Path) -> Option<PathBuf> {
    Some(path.parent()?.parent()?.join("CosmicTile").join("config.ini"))
}

fn load_from_path(path: &Path) -> Config {
    let _guard = match guard_parent(path) {
        Ok(guard) => guard,
        Err(e) => { crate::app::log(&format!("unsafe config directory: {e}")); return Config::default(); }
    };
    if let Err(e) = safe_path(path) {
        crate::app::log(&format!("unsafe config path: {e}"));
        return Config::default();
    }
    match read_file(path) {
        Ok(text) => Config::parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // One-time migration from the former name (CosmicTile): keep the user's settings.
            if let Some(text) = legacy_config_path(path).and_then(|old| std::fs::read_to_string(old).ok()) {
                let config = Config::parse(&text);
                if let Err(write_error) = ensure_exists_at(path, &config.to_text()) {
                    crate::app::log(&format!("could not migrate legacy config: {write_error}"));
                }
                return config;
            }
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
    let _guard = guard_parent(path)?;
    safe_path(path)?;
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
    safe_path(path)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    no_follow(&mut options);
    let mut file = match options.open(path) {
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
    let _guard = guard_parent(path)?;
    use std::io::Write;
    static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    safe_path(path)?;
    std::fs::create_dir_all(parent)?;
    safe_path(path)?;
    let name = path.file_name().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "config path has no filename"))?;
    let (temp_path, mut file) = loop {
        let n = NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut temp_name = name.to_os_string();
        temp_name.push(format!(".{}.{}.tmp", std::process::id(), n));
        let candidate = parent.join(temp_name);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        no_follow(&mut options);
        match options.open(&candidate) {
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
        safe_path(path)?;
        replace_file(&temp_path, path)
    })();
    if result.is_err() { let _ = std::fs::remove_file(&temp_path); }
    result
}

/// Reject existing reparse points in the path before accessing application data.
pub fn safe_path(path: &Path) -> std::io::Result<()> {
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) if is_reparse(&metadata) => return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "reparse point in application data path")),
            Ok(_) => {},
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Keeps every directory in the path open without delete sharing.
/// This prevents a checked directory from being replaced while files are accessed.
pub struct PathGuard {
    _directories: Vec<std::fs::File>,
}

pub fn guard_parent(path: &Path) -> std::io::Result<PathGuard> {
    let parent = path.parent().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent"))?;
    if !parent.is_absolute() { return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "path must be absolute")); }
    let mut directories = Vec::new();
    for directory in parent.ancestors().collect::<Vec<_>>().into_iter().rev() {
        let mut options = std::fs::OpenOptions::new();
        directory_options(&mut options);
        let file = match options.open(directory) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::create_dir(directory) {
                    Ok(()) => {},
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {},
                    Err(e) => return Err(e),
                }
                options.open(directory)?
            }
            Err(e) => return Err(e),
        };
        if is_reparse(&file.metadata()?) {
            return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "reparse point in application data path"));
        }
        directories.push(file);
    }
    Ok(PathGuard { _directories: directories })
}

#[cfg(windows)]
fn directory_options(options: &mut std::fs::OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt;
    options.read(true).access_mode(1).share_mode(3).custom_flags(0x0220_0000);
}

#[cfg(not(windows))]
fn directory_options(options: &mut std::fs::OpenOptions) { options.read(true); }

fn read_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    no_follow(&mut options);
    let mut file = options.open(path)?;
    if is_reparse(&file.metadata()?) {
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "reparse point in application data path"));
    }
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok(text)
}

#[cfg(windows)]
fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    reparse_attribute(metadata.file_attributes())
}

#[cfg(any(windows, test))]
fn reparse_attribute(attributes: u32) -> bool { attributes & 0x400 != 0 }

#[cfg(not(windows))]
fn is_reparse(metadata: &std::fs::Metadata) -> bool { metadata.file_type().is_symlink() }

#[cfg(windows)]
fn no_follow(options: &mut std::fs::OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt;
    options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
}

#[cfg(not(windows))]
fn no_follow(_options: &mut std::fs::OpenOptions) {}

fn update_keys_at(path: &Path, keys: &[(&str, &str)]) -> std::io::Result<()> {
    let _guard = guard_parent(path)?;
    safe_path(path)?;
    let text = match read_file(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default().to_text(),
        Err(e) => return Err(e),
    };
    let mut found = vec![false; keys.len()];
    let mut result = String::new();
    for line in text.split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        let key = content.split_once('=').map(|(key, _)| key.trim());
        if let Some(index) = keys.iter().position(|(name, _)| Some(*name) == key) {
            result.push_str(keys[index].0);
            result.push('=');
            result.push_str(keys[index].1);
            result.push('\n');
            found[index] = true;
        } else {
            result.push_str(line);
        }
    }
    for (index, (key, value)) in keys.iter().enumerate() {
        if !found[index] {
            if !result.is_empty() && !result.ends_with('\n') { result.push('\n'); }
            result.push_str(&format!("{key}={value}\n"));
        }
    }
    save_to_path(path, &result)
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

    #[test]
    fn unavailable_or_relative_app_data_never_produces_a_write_path() {
        assert_eq!(config_path(None), None);
        assert_eq!(config_path(Some("".into())), None);
        assert_eq!(config_path(Some("relative-app-data".into())), None);
        let base = std::env::temp_dir();
        assert_eq!(config_path(Some(base.clone().into_os_string())), Some(base.join("Tilework").join("config.ini")));
    }

    fn test_file(label: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("tilework-config-{}-{nonce}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(format!("config-test-{}-{label}.ini", std::process::id()))
    }

    #[test]
    fn defaults_follow_the_mouse_after_300_ms() {
        let c = Config::default();
        assert!(c.focus_follows_mouse);
        assert!(!c.verbose_log);
        assert_eq!(c.focus_delay_ms, 300);
    }

    #[test]
    fn a_saved_config_reads_back_the_same() {
        let c = Config { focus_follows_mouse: false, focus_delay_ms: 800, min_zone_width: 300, min_zone_height: 200, verbose_log: true };
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
        let config = Config { focus_follows_mouse: false, focus_delay_ms: 900, min_zone_width: 320, min_zone_height: 220, verbose_log: false };
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
    fn settings_of_the_former_name_are_migrated_once() {
        let root = test_file("legacy-root");
        let old = root.join("CosmicTile").join("config.ini");
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, "focus_delay_ms=900
min_zone_width=333
").unwrap();
        let path = root.join("Tilework").join("config.ini");
        let c = load_from_path(&path);
        assert_eq!((c.focus_delay_ms, c.min_zone_width), (900, 333));
        assert_eq!(Config::parse(&std::fs::read_to_string(&path).unwrap()), c);
        std::fs::write(&old, "focus_delay_ms=100
").unwrap();
        assert_eq!(load_from_path(&path).focus_delay_ms, 900, "the migrated file wins afterwards");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn read_error_returns_defaults_without_modifying_target() {
        let dir = test_file("read-error");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("existing-directory.ini");
        std::fs::write(&path, "sentinel").unwrap();
        assert_eq!(load_from_path(&dir), Config::default());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "sentinel");
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn tray_updates_keep_manual_edits_and_unknown_lines() {
        let path = test_file("tray");
        std::fs::write(&path, "# custom\nmin_zone_width=413\ncustom=keep\nfocus_follows_mouse=false\n").unwrap();
        update_keys_at(&path, &[("focus_follows_mouse", "true"), ("focus_delay_ms", "800")]).unwrap();
        let updated = std::fs::read_to_string(&path).unwrap();
        assert!(updated.contains("# custom\nmin_zone_width=413\ncustom=keep\n"));
        let config = Config::parse(&updated);
        assert_eq!(config.min_zone_width, 413);
        assert!(config.focus_follows_mouse);
        assert_eq!(config.focus_delay_ms, 800);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn reparse_attribute_detection_rejects_the_flag() {
        assert!(reparse_attribute(0x400));
        assert!(!reparse_attribute(0x20));
    }

    #[cfg(windows)]
    #[test]
    fn directory_guard_prevents_replacement_until_released() {
        let path = test_file("guard");
        let directory = path.parent().unwrap();
        let moved = directory.with_extension("moved");
        let guard = guard_parent(&path).unwrap();
        assert!(std::fs::rename(directory, &moved).is_err());
        drop(guard);
        std::fs::rename(directory, &moved).unwrap();
        std::fs::remove_dir(moved).unwrap();
    }
}
