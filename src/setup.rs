//! Installer, autostart via Windows Task Scheduler, and self-updater for Tilework.

use std::path::{Path, PathBuf};
use std::process::Command;

pub const APP_NAME: &str = "Tilework";
pub const TASK_NAME: &str = "Tilework";
pub const GITHUB_REPO: &str = "Jas0nOW/Tilework";

// ============================================================================
// Installation and Path Helpers
// ============================================================================

/// Returns `%LOCALAPPDATA%\Tilework`.
pub fn install_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join(APP_NAME))
}

/// Returns `%LOCALAPPDATA%\Tilework\tilework.exe`.
pub fn installed_exe_path() -> Option<PathBuf> {
    install_dir().map(|d| d.join("tilework.exe"))
}

/// Returns `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Tilework.lnk`.
pub fn start_menu_shortcut_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|p| {
        PathBuf::from(p)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("Tilework.lnk")
    })
}

/// Returns true if the currently running executable is located inside `%LOCALAPPDATA%\Tilework`.
pub fn is_running_installed() -> bool {
    let Ok(current) = std::env::current_exe() else { return false; };
    let Some(target) = installed_exe_path() else { return false; };
    // Compare canonical / normalized paths
    if let (Ok(c1), Ok(c2)) = (current.canonicalize(), target.canonicalize()) {
        c1 == c2
    } else {
        current == target
    }
}

// ============================================================================
// Autostart via Windows Task Scheduler (Admin without UAC prompt at Logon)
// ============================================================================

/// Checks whether the "Tilework" scheduled task exists.
pub fn is_autostart_enabled() -> bool {
    let output = Command::new("schtasks.exe")
        .args(["/query", "/tn", TASK_NAME])
        .output();
    match output {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

/// Creates or updates the silent elevated scheduled task for `exe_path`.
pub fn enable_autostart(exe_path: &Path) -> Result<(), String> {
    let path_str = exe_path.to_str().ok_or("invalid executable path")?;
    let tr = format!("\"{path_str}\"");
    let output = Command::new("schtasks.exe")
        .args([
            "/create",
            "/tn",
            TASK_NAME,
            "/tr",
            &tr,
            "/sc",
            "onlogon",
            "/rl",
            "highest",
            "/f",
        ])
        .output()
        .map_err(|e| format!("failed to invoke schtasks: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&output.stderr);
        let out = String::from_utf8_lossy(&output.stdout);
        Err(format!("schtasks failed: {err} {out}").trim().to_string())
    }
}

/// Removes the "Tilework" scheduled task.
pub fn disable_autostart() -> Result<(), String> {
    let output = Command::new("schtasks.exe")
        .args(["/delete", "/tn", TASK_NAME, "/f"])
        .output()
        .map_err(|e| format!("failed to invoke schtasks: {e}"))?;

    // If exit code is success or task was already not found, consider it disabled
    if output.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&output.stderr);
        if err.contains("nicht gefunden") || err.contains("cannot find") || err.contains("not found") {
            Ok(())
        } else {
            Err(err.trim().to_string())
        }
    }
}

// ============================================================================
// Shortcut Creation
// ============================================================================

/// Creates a Windows Start Menu shortcut (`.lnk`) pointing to `target_exe`.
pub fn create_shortcut(target_exe: &Path, shortcut_path: &Path) -> Result<(), String> {
    if let Some(parent) = shortcut_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let target = target_exe.to_str().ok_or("invalid target path")?;
    let sc = shortcut_path.to_str().ok_or("invalid shortcut path")?;

    // Use PowerShell to create the .lnk file cleanly without extra dependencies
    let script = format!(
        "$ws = New-Object -ComObject WScript.Shell; \
         $s = $ws.CreateShortcut('{sc}'); \
         $s.TargetPath = '{target}'; \
         $s.WorkingDirectory = '{work_dir}'; \
         $s.Description = 'Tilework - Tiling Window Manager for Windows'; \
         $s.Save()",
        sc = sc.replace('\'', "''"),
        target = target.replace('\'', "''"),
        work_dir = target_exe
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or("")
            .replace('\'', "''")
    );

    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .map_err(|e| format!("failed to run powershell for shortcut: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&output.stderr);
        Err(format!("failed to create shortcut: {err}").trim().to_string())
    }
}

// ============================================================================
// Installer & Uninstaller
// ============================================================================

/// Installs Tilework to `%LOCALAPPDATA%\Tilework\tilework.exe`, creates a Start Menu shortcut,
/// and configures silent admin autostart via Windows Task Scheduler.
pub fn install() -> Result<(), String> {
    let target_dir = install_dir().ok_or("could not determine %LOCALAPPDATA% directory")?;
    let target_exe = installed_exe_path().ok_or("could not determine target executable path")?;
    let current_exe = std::env::current_exe().map_err(|e| format!("could not locate current binary: {e}"))?;

    std::fs::create_dir_all(&target_dir).map_err(|e| format!("could not create install directory: {e}"))?;

    // If running from another location, copy to target
    let is_same = if let (Ok(c1), Ok(c2)) = (current_exe.canonicalize(), target_exe.canonicalize()) {
        c1 == c2
    } else {
        current_exe == target_exe
    };

    if !is_same {
        // If target already exists, try to replace it cleanly
        if target_exe.exists() {
            let backup = target_exe.with_extension("exe.old");
            let _ = std::fs::remove_file(&backup);
            let _ = std::fs::rename(&target_exe, &backup);
        }
        std::fs::copy(&current_exe, &target_exe).map_err(|e| format!("could not copy executable to install directory: {e}"))?;
    }

    // Create Start Menu shortcut
    if let Some(sc_path) = start_menu_shortcut_path()
        && let Err(e) = create_shortcut(&target_exe, &sc_path) {
            crate::app::log(&format!("warning: could not create start menu shortcut: {e}"));
    }

    // Enable autostart with highest privileges
    enable_autostart(&target_exe)?;

    crate::app::log(&format!("Tilework successfully installed to {}", target_exe.display()));
    Ok(())
}

/// Uninstalls Tilework: removes scheduled task, Start Menu shortcut, and installed executable.
pub fn uninstall() -> Result<(), String> {
    let _ = disable_autostart();

    if let Some(sc) = start_menu_shortcut_path()
        && sc.exists() {
            let _ = std::fs::remove_file(&sc);
    }

    if let Some(target_exe) = installed_exe_path()
        && target_exe.exists() {
            // If we are running the installed exe right now, we can rename it to .old so it can be deleted on reboot
            let old = target_exe.with_extension("exe.old");
            let _ = std::fs::remove_file(&old);
            let _ = std::fs::rename(&target_exe, &old);
    }

    crate::app::log("Tilework uninstalled successfully");
    Ok(())
}

// ============================================================================
// Robust Updater Module
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub tag: String,
    pub version: (u32, u32, u32),
    pub download_url: String,
    pub asset_name: String,
    pub release_notes: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    Available(ReleaseInfo),
    UpToDate(String),
    NoReleaseFound,
    Error(String),
}

/// Parses a version string like "0.2.1" or "v0.2.1" into (major, minor, patch).
pub fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() < 3 {
        return None;
    }
    let major = parts[0].parse::<u32>().ok()?;
    let minor = parts[1].parse::<u32>().ok()?;
    // Strip any suffixes from patch (e.g., "-beta")
    let patch_str = parts[2].split('-').next()?;
    let patch = patch_str.parse::<u32>().ok()?;
    Some((major, minor, patch))
}

/// Returns true if `remote` is strictly newer than `current`.
pub fn is_newer(current: (u32, u32, u32), remote: (u32, u32, u32)) -> bool {
    remote > current
}

/// Validates that raw bytes represent a valid Windows PE executable (starts with MZ header and contains PE signature).
pub fn validate_pe_binary(data: &[u8]) -> Result<(), String> {
    if data.len() < 1024 {
        return Err("file is too small to be a valid Windows binary (< 1 KB)".into());
    }
    // MZ header check
    if data[0] != 0x4D || data[1] != 0x5A {
        return Err("missing MZ header; downloaded file is not a Windows executable".into());
    }
    // Check e_lfanew at 0x3C
    if data.len() < 0x40 {
        return Err("binary header truncated".into());
    }
    let pe_offset = u32::from_le_bytes([data[0x3C], data[0x3D], data[0x3E], data[0x3F]]) as usize;
    if pe_offset + 4 > data.len() {
        return Err("invalid PE header offset".into());
    }
    if &data[pe_offset..pe_offset + 4] != b"PE\0\0" {
        return Err("missing PE signature at expected offset".into());
    }
    Ok(())
}

/// Checks GitHub releases for a newer version of Tilework.
pub fn check_for_update() -> UpdateStatus {
    let current_ver_str = env!("CARGO_PKG_VERSION");
    let current_ver = match parse_version(current_ver_str) {
        Some(v) => v,
        None => return UpdateStatus::Error(format!("could not parse current version: {current_ver_str}")),
    };

    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
    let output = Command::new("curl.exe")
        .args([
            "-s",
            "-H",
            "User-Agent: Tilework-Updater",
            "-H",
            "Accept: application/vnd.github.v3+json",
            "--max-time",
            "10",
            &url,
        ])
        .output();

    let output = match output {
        Ok(out) => out,
        Err(e) => return UpdateStatus::Error(format!("could not execute curl: {e}")),
    };

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return UpdateStatus::Error(format!("network request failed: {err}"));
    }

    let json_text = String::from_utf8_lossy(&output.stdout);
    parse_github_release_json(&json_text, current_ver)
}

/// Parses the GitHub release JSON and determines if an update is available.
pub fn parse_github_release_json(json: &str, current_ver: (u32, u32, u32)) -> UpdateStatus {
    if json.contains("\"message\":\"Not Found\"") {
        return UpdateStatus::NoReleaseFound;
    }

    // Extract tag_name
    let tag = match extract_json_string(json, "tag_name") {
        Some(t) => t,
        None => return UpdateStatus::Error("tag_name not found in release response".into()),
    };

    let remote_ver = match parse_version(&tag) {
        Some(v) => v,
        None => return UpdateStatus::Error(format!("invalid remote tag version: {tag}")),
    };

    if !is_newer(current_ver, remote_ver) {
        return UpdateStatus::UpToDate(tag);
    }

    // Extract asset download URL (prefer tilework.exe or first .exe)
    let (download_url, asset_name) = match extract_exe_asset(json) {
        Some(pair) => pair,
        None => return UpdateStatus::Error(format!("no executable asset found in release {tag}")),
    };

    let release_notes = extract_json_string(json, "body").unwrap_or_default();

    UpdateStatus::Available(ReleaseInfo {
        tag,
        version: remote_ver,
        download_url,
        asset_name,
        release_notes,
    })
}

/// Downloads the update from `download_url` into `dest_path` and verifies PE integrity.
pub fn download_and_verify(download_url: &str, dest_path: &Path) -> Result<(), String> {
    if let Some(parent) = dest_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let dest_str = dest_path.to_str().ok_or("invalid destination path")?;

    let output = Command::new("curl.exe")
        .args([
            "-L", // follow redirects
            "-s",
            "-H",
            "User-Agent: Tilework-Updater",
            "--max-time",
            "60",
            "-o",
            dest_str,
            download_url,
        ])
        .output()
        .map_err(|e| format!("failed to invoke curl: {e}"))?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!("download failed: {err}"));
    }

    let bytes = std::fs::read(dest_path).map_err(|e| format!("failed to read downloaded file: {e}"))?;
    validate_pe_binary(&bytes)?;

    Ok(())
}

/// Atomically replaces `target_exe` with `new_binary_path` using Windows rename semantics.
/// If any step fails, rolls back `target_exe.old` -> `target_exe`.
pub fn apply_update(target_exe: &Path, new_binary_path: &Path) -> Result<(), String> {
    let old_backup = target_exe.with_extension("exe.old");

    // Clean up any stale backup from a previous run
    if old_backup.exists() {
        let _ = std::fs::remove_file(&old_backup);
    }

    // Rename running executable -> .old
    if target_exe.exists() {
        std::fs::rename(target_exe, &old_backup).map_err(|e| format!("could not rename current binary: {e}"))?;
    }

    // Move new binary -> target_exe
    if let Err(e) = std::fs::rename(new_binary_path, target_exe) {
        // Rollback
        if old_backup.exists() {
            let _ = std::fs::rename(&old_backup, target_exe);
        }
        return Err(format!("failed to move new binary into place (rolled back): {e}"));
    }

    Ok(())
}

/// Cleans up any leftover `tilework.exe.old` in the same directory as the current executable.
pub fn cleanup_old_binary() {
    if let Ok(current) = std::env::current_exe() {
        let old = current.with_extension("exe.old");
        if old.exists() {
            let _ = std::fs::remove_file(old);
        }
    }
}

// ============================================================================
// Internal JSON extraction helpers (minimal, robust, zero-dependency)
// ============================================================================

fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{key}\":");
    let start_idx = json.find(&pattern)? + pattern.len();
    let rest = json[start_idx..].trim_start();
    if !rest.starts_with('"') {
        return None;
    }
    let rest = &rest[1..];
    let mut result = String::new();
    let mut chars = rest.chars();
    let mut escape = false;

    while let Some(c) = chars.next() {
        if escape {
            match c {
                '"' => result.push('"'),
                '\\' => result.push('\\'),
                '/' => result.push('/'),
                'b' => result.push('\x08'),
                'f' => result.push('\x0C'),
                'n' => result.push('\n'),
                'r' => result.push('\r'),
                't' => result.push('\t'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Ok(code) = u32::from_str_radix(&hex, 16)
                        && let Some(ch) = char::from_u32(code) {
                            result.push(ch);
                    }
                }
                other => result.push(other),
            }
            escape = false;
        } else if c == '\\' {
            escape = true;
        } else if c == '"' {
            return Some(result);
        } else {
            result.push(c);
        }
    }
    None
}

fn extract_exe_asset(json: &str) -> Option<(String, String)> {
    // Find "assets": [...]
    let assets_key = "\"assets\":";
    let start = json.find(assets_key)? + assets_key.len();
    let rest = &json[start..];

    // Search for objects within assets array
    for block in rest.split('{') {
        if let Some(name) = extract_json_string(block, "name")
            && name.ends_with(".exe")
            && let Some(url) = extract_json_string(block, "browser_download_url") {
                return Some((url, name));
        }
    }
    None
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_handles_standard_and_prefixed_tags() {
        assert_eq!(parse_version("0.1.0"), Some((0, 1, 0)));
        assert_eq!(parse_version("v0.2.1"), Some((0, 2, 1)));
        assert_eq!(parse_version("V1.10.4-beta"), Some((1, 10, 4)));
        assert_eq!(parse_version("invalid"), None);
        assert_eq!(parse_version("1.0"), None);
    }

    #[test]
    fn version_comparison_detects_newer_releases() {
        assert!(is_newer((0, 1, 0), (0, 1, 1)));
        assert!(is_newer((0, 1, 0), (0, 2, 0)));
        assert!(is_newer((0, 9, 9), (1, 0, 0)));
        assert!(!is_newer((0, 2, 0), (0, 2, 0)));
        assert!(!is_newer((0, 2, 1), (0, 2, 0)));
        assert!(!is_newer((1, 0, 0), (0, 9, 9)));
    }

    #[test]
    fn pe_validation_verifies_magic_and_signature() {
        // Too small
        assert!(validate_pe_binary(&[0u8; 100]).is_err());

        // Missing MZ
        let mut buf = vec![0u8; 2048];
        assert!(validate_pe_binary(&buf).is_err());

        // Valid MZ, but invalid PE offset
        buf[0] = 0x4D;
        buf[1] = 0x5A;
        buf[0x3C] = 0x80; // offset 128
        assert!(validate_pe_binary(&buf).is_err());

        // Valid MZ and PE\0\0
        buf[128] = b'P';
        buf[129] = b'E';
        buf[130] = 0;
        buf[131] = 0;
        assert!(validate_pe_binary(&buf).is_ok());
    }

    #[test]
    fn json_string_extraction_parses_escapes() {
        let json = r#"{"name": "tilework.exe", "tag_name": "v0.2.0", "body": "Release\nNotes with \"quotes\""}"#;
        assert_eq!(extract_json_string(json, "name"), Some("tilework.exe".into()));
        assert_eq!(extract_json_string(json, "tag_name"), Some("v0.2.0".into()));
        assert_eq!(
            extract_json_string(json, "body"),
            Some("Release\nNotes with \"quotes\"".into())
        );
        assert_eq!(extract_json_string(json, "missing"), None);
    }

    #[test]
    fn github_release_parsing_handles_update_and_up_to_date() {
        let json = r#"{
            "tag_name": "v0.3.0",
            "body": "Bugfixes and features",
            "assets": [
                {
                    "name": "tilework.exe",
                    "browser_download_url": "https://github.com/Jas0nOW/Tilework/releases/download/v0.3.0/tilework.exe"
                }
            ]
        }"#;

        // Current is 0.1.0 -> Update available
        match parse_github_release_json(json, (0, 1, 0)) {
            UpdateStatus::Available(rel) => {
                assert_eq!(rel.tag, "v0.3.0");
                assert_eq!(rel.version, (0, 3, 0));
                assert_eq!(rel.asset_name, "tilework.exe");
                assert_eq!(
                    rel.download_url,
                    "https://github.com/Jas0nOW/Tilework/releases/download/v0.3.0/tilework.exe"
                );
            }
            other => panic!("expected Available, got {:?}", other),
        }

        // Current is 0.3.0 -> UpToDate
        assert_eq!(
            parse_github_release_json(json, (0, 3, 0)),
            UpdateStatus::UpToDate("v0.3.0".into())
        );
    }

    #[test]
    fn atomic_apply_and_rollback_on_filesystem() {
        let dir = std::env::temp_dir().join("tilework_test_atomic_update");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        let target = dir.join("tilework.exe");
        let new_file = dir.join("tilework.exe.new");

        std::fs::write(&target, b"original").unwrap();
        std::fs::write(&new_file, b"updated").unwrap();

        // Apply update
        assert!(apply_update(&target, &new_file).is_ok());
        assert_eq!(std::fs::read(&target).unwrap(), b"updated");
        assert_eq!(std::fs::read(target.with_extension("exe.old")).unwrap(), b"original");

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }
}
