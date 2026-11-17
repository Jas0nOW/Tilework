//! Per-user autostart and read-only release discovery.

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use windows::core::w;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows::Win32::System::Threading::CREATE_NO_WINDOW;

pub const RELEASES_URL: &str = "https://github.com/Jas0nOW/Tilework/releases";
const RELEASE_API: &str = "https://api.github.com/repos/Jas0nOW/Tilework/releases/latest";
const INSTALLER: &str = "Tilework-Setup-x64.exe";
const RUN_KEY: windows::core::PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_NAME: windows::core::PCWSTR = w!("Tilework");
const MAX_RUN_LENGTH: usize = 260;

fn registry_string(key: windows::core::PCWSTR, name: windows::core::PCWSTR) -> Option<Vec<u16>> {
    let mut value = [0u16; MAX_RUN_LENGTH + 1];
    let mut bytes = size_of_val(&value) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            name,
            RRF_RT_REG_SZ,
            None,
            Some(value.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let value = value.get(..bytes as usize / size_of::<u16>())?;
    Some(value.strip_suffix(&[0])?.to_vec())
}

fn installed_exe() -> Option<PathBuf> {
    let dir = registry_string(w!("Software\\Tilework"), w!("InstallPath"))?;
    let dir = PathBuf::from(OsString::from_wide(&dir));
    dir.is_absolute().then(|| dir.join("tilework.exe"))
}

pub fn is_running_installed() -> bool {
    match (std::env::current_exe(), installed_exe()) {
        (Ok(current), Some(installed)) => {
            match (current.canonicalize(), installed.canonicalize()) {
                (Ok(current), Ok(installed)) => current == installed,
                _ => false,
            }
        }
        _ => false,
    }
}

fn autostart_command() -> Result<Vec<u16>, String> {
    let installed = installed_exe().ok_or("Could not locate the per-user installation.")?;
    let mut command = vec![b'"' as u16];
    command.extend(installed.as_os_str().encode_wide());
    command.push(b'"' as u16);
    if command.len() > MAX_RUN_LENGTH {
        return Err("The installation path is too long for Windows autostart.".into());
    }
    Ok(command)
}

pub fn is_autostart_enabled() -> bool {
    let Ok(expected) = autostart_command() else {
        return false;
    };
    registry_string(RUN_KEY, RUN_NAME) == Some(expected)
}

pub fn set_autostart(enabled: bool) -> Result<(), String> {
    let status = if enabled {
        if !is_running_installed() {
            return Err("Run Tilework from its Start Menu shortcut after installing it.".into());
        }
        let mut command = autostart_command()?;
        command.push(0);
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                RUN_NAME,
                REG_SZ.0,
                Some(command.as_ptr().cast()),
                size_of_val(command.as_slice()) as u32,
            )
        }
    } else {
        let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_NAME) };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        status
    };
    status
        .ok()
        .map_err(|e| format!("Could not change Windows autostart: {e}"))
}

#[derive(Debug, PartialEq, Eq)]
pub enum UpdateStatus {
    Available { tag: String, release_url: String },
    UpToDate,
    NoReleaseFound,
    Error(String),
}

fn parse_version(value: &str) -> Option<(u32, u32, u32)> {
    let mut parts = value.split('.');
    let mut next = || {
        let part = parts.next()?;
        if part.is_empty()
            || (part.starts_with('0') && part.len() > 1)
            || !part.bytes().all(|c| c.is_ascii_digit())
        {
            return None;
        }
        part.parse().ok()
    };
    let version = (next()?, next()?, next()?);
    parts.next().is_none().then_some(version)
}

fn interpret_response(response: &[u8], current: (u32, u32, u32)) -> UpdateStatus {
    let Some(separator) = response.iter().rposition(|b| *b == b'\n') else {
        return UpdateStatus::Error("GitHub did not return an HTTP status.".into());
    };
    let (body, status) = (&response[..separator], &response[separator + 1..]);
    match status {
        b"404" => return UpdateStatus::NoReleaseFound,
        b"200" => {}
        _ => return UpdateStatus::Error("GitHub did not return a release (HTTP error).".into()),
    }
    let result = (|| {
        let release: serde_json::Value = serde_json::from_slice(body).ok()?;
        if release["draft"].as_bool()? || release["prerelease"].as_bool()? {
            return Some(UpdateStatus::NoReleaseFound);
        }
        let tag = release["tag_name"].as_str()?;
        let version = parse_version(tag.strip_prefix('v')?)?;
        if version <= current {
            return Some(UpdateStatus::UpToDate);
        }
        let asset_url = format!("{RELEASES_URL}/download/{tag}/{INSTALLER}");
        if !release["assets"].as_array()?.iter().any(|asset| {
            asset["name"].as_str() == Some(INSTALLER)
                && asset["state"].as_str() == Some("uploaded")
                && asset["browser_download_url"].as_str() == Some(asset_url.as_str())
        }) {
            return None;
        }
        Some(UpdateStatus::Available {
            tag: tag.to_owned(),
            release_url: format!("{RELEASES_URL}/tag/{tag}"),
        })
    })();
    result.unwrap_or_else(|| {
        UpdateStatus::Error(
            "GitHub returned an invalid release or its installer is missing.".into(),
        )
    })
}

pub fn check_for_update() -> UpdateStatus {
    let Some(current) = parse_version(env!("CARGO_PKG_VERSION")) else {
        return UpdateStatus::Error("This build has an invalid version.".into());
    };
    let mut system = [0u16; 260];
    let len = unsafe { GetSystemDirectoryW(Some(&mut system)) } as usize;
    if len == 0 || len >= system.len() {
        return UpdateStatus::Error("Could not locate the Windows system directory.".into());
    }
    let curl = PathBuf::from(OsString::from_wide(&system[..len])).join("curl.exe");
    match Command::new(curl)
        .creation_flags(CREATE_NO_WINDOW.0)
        .args([
            "-q",
            "--silent",
            "--proto",
            "=https",
            "--max-time",
            "10",
            "--max-filesize",
            "1048576",
            "--write-out",
            "\n%{http_code}",
            "--header",
            "Accept: application/vnd.github+json",
            "--user-agent",
            "Tilework-Updater",
            RELEASE_API,
        ])
        .output()
    {
        Ok(response) if response.status.success() => interpret_response(&response.stdout, current),
        Ok(_) => UpdateStatus::Error(
            "Could not reach GitHub or the response exceeded the size limit.".into(),
        ),
        Err(e) => UpdateStatus::Error(format!("Could not run Windows curl: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(tag: &str, url: &str, draft: bool, prerelease: bool) -> Vec<u8> {
        let release = serde_json::json!({
            "draft": draft, "prerelease": prerelease, "tag_name": tag,
            "assets": [{ "name": INSTALLER, "state": "uploaded", "browser_download_url": url }],
        });
        format!("{release}\n200").into_bytes()
    }

    #[test]
    fn only_recognizes_new_stable_versions_with_the_exact_official_installer() {
        let tag = "v0.4.0";
        let url = format!("{RELEASES_URL}/download/{tag}/{INSTALLER}");
        let valid = response(tag, &url, false, false);
        assert_eq!(
            interpret_response(&valid, (0, 3, 0)),
            UpdateStatus::Available {
                tag: tag.into(),
                release_url: format!("{RELEASES_URL}/tag/{tag}")
            },
        );
        assert_eq!(
            interpret_response(&valid, (0, 4, 0)),
            UpdateStatus::UpToDate
        );
        assert_eq!(
            interpret_response(&valid, (0, 5, 0)),
            UpdateStatus::UpToDate
        );
        for url in [
            "https://example.invalid/Tilework-Setup-x64.exe",
            "https://github.com/Jas0nOW/Tilework/releases/download/v0.2.0/tilework.exe",
        ] {
            assert!(matches!(
                interpret_response(&response(tag, url, false, false), (0, 3, 0)),
                UpdateStatus::Error(_)
            ));
        }
        for tag in ["v0.4.0-rc.1", "v0.4.0.1", "v0.04.0", "v0.+4.0"] {
            assert!(matches!(
                interpret_response(&response(tag, &url, false, false), (0, 3, 0)),
                UpdateStatus::Error(_)
            ));
        }
        for (draft, prerelease) in [(true, false), (false, true)] {
            assert_eq!(
                interpret_response(&response(tag, &url, draft, prerelease), (0, 3, 0)),
                UpdateStatus::NoReleaseFound
            );
        }
    }

    #[test]
    fn distinguishes_missing_http_errors_and_invalid_responses() {
        assert_eq!(
            interpret_response(b"{}\n404", (0, 3, 0)),
            UpdateStatus::NoReleaseFound
        );
        for invalid in [b"{}\n403".as_slice(), b"{}\n200", b"{\n200", b"{}"] {
            assert!(matches!(
                interpret_response(invalid, (0, 3, 0)),
                UpdateStatus::Error(_)
            ));
        }
    }
}
