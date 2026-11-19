//! Automatic update checks and the verified one-click installer download.
//!
//! Trust model: the installer is fetched only from the fixed official release URL of a strictly
//! validated tag and must match the SHA-256 listed in the release's `SHA256SUMS.txt`. Both files
//! come from the same GitHub release, so this detects corruption and a tampered download path,
//! not a compromised release. The binaries are unsigned; the checksum is the only integrity check.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::setup::{RELEASES_URL, INSTALLER, SUMS, parse_version};

/// Seconds between automatic checks and between repeat notifications for one version.
pub const DAY_SECS: u64 = 86_400;
/// Delay after startup before the first automatic check.
pub const FIRST_CHECK_DELAY_MS: u32 = 45_000;
/// How often the running app re-evaluates whether a check is due.
pub const RECHECK_TICK_MS: u32 = 3_600_000;
const MIN_INSTALLER_BYTES: u64 = 256 * 1024;
const MAX_INSTALLER_BYTES: u64 = 32 * 1024 * 1024;
const MAX_SUMS_BYTES: u64 = 16 * 1024;
/// Command line given to the installer. `RelaunchTilework` is read by `Tilework.iss`.
pub const INSTALLER_ARGS: [&str; 5] = ["/SILENT", "/SP-", "/NOCANCEL", "/NORESTART", "/RelaunchTilework=1"];

// ---------------------------------------------------------------------------------------------
// Pure helpers (unit tested)
// ---------------------------------------------------------------------------------------------

/// Accepts exactly `vMAJOR.MINOR.PATCH` (no leading zeros, no suffixes).
pub fn is_valid_tag(tag: &str) -> bool {
    tag.strip_prefix('v').and_then(parse_version).is_some()
}

fn download_url(tag: &str, file: &str) -> Option<String> {
    is_valid_tag(tag).then(|| format!("{RELEASES_URL}/download/{tag}/{file}"))
}

pub fn installer_url(tag: &str) -> Option<String> { download_url(tag, INSTALLER) }
pub fn sums_url(tag: &str) -> Option<String> { download_url(tag, SUMS) }

/// Finds the SHA-256 of `file` in a `sha256sum`-style listing (`<64 hex>  <name>`).
/// Strict: malformed lines, duplicate or conflicting entries and a missing entry are errors.
pub fn parse_sums(text: &str, file: &str) -> Result<String, String> {
    let mut found: Option<String> = None;
    for line in text.trim_start_matches('\u{feff}').lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() { continue; }
        let (hash, name) = line.split_at_checked(64).ok_or("malformed checksum line")?;
        let name = name.strip_prefix("  ").or_else(|| name.strip_prefix(" *")).ok_or("malformed checksum line")?;
        if !hash.bytes().all(|b| b.is_ascii_hexdigit()) || name.is_empty() {
            return Err("malformed checksum line".into());
        }
        if name == file {
            let hash = hash.to_ascii_lowercase();
            if found.as_ref().is_some_and(|previous| *previous != hash) {
                return Err("conflicting checksum entries".into());
            }
            found = Some(hash);
        }
    }
    found.ok_or_else(|| format!("no checksum for {file}"))
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn hash_matches(actual: &[u8; 32], expected_hex: &str) -> bool {
    to_hex(actual) == expected_hex.to_ascii_lowercase()
}

/// Persisted between runs in `update-state.ini` next to `config.ini`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpdateState {
    pub last_check: Option<u64>,
    pub notified_tag: Option<String>,
    pub notified_at: Option<u64>,
}

impl UpdateState {
    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            match k.trim() {
                "last_check" => s.last_check = v.parse().ok(),
                "notified_tag" => s.notified_tag = is_valid_tag(v).then(|| v.to_owned()),
                "notified_at" => s.notified_at = v.parse().ok(),
                _ => {}
            }
        }
        s
    }

    pub fn to_text(&self) -> String {
        let mut out = String::from("# Tilework update bookkeeping (safe to delete)\n");
        if let Some(t) = self.last_check { out.push_str(&format!("last_check={t}\n")); }
        if let Some(t) = &self.notified_tag { out.push_str(&format!("notified_tag={t}\n")); }
        if let Some(t) = self.notified_at { out.push_str(&format!("notified_at={t}\n")); }
        out
    }
}

/// A check is due when none ran yet, 24 h passed, or the clock moved backwards.
pub fn should_check_now(last_check: Option<u64>, now: u64) -> bool {
    match last_check {
        Some(last) => now < last || now - last >= DAY_SECS,
        None => true,
    }
}

/// The same version is announced at most once per 24 h (a new version is announced at once).
pub fn should_notify(state: &UpdateState, tag: &str, now: u64) -> bool {
    match (&state.notified_tag, state.notified_at) {
        (Some(known), Some(at)) if known == tag => now < at || now - at >= DAY_SECS,
        _ => true,
    }
}

// ---------------------------------------------------------------------------------------------
// SHA-256 through Windows CNG (no extra crate)
// ---------------------------------------------------------------------------------------------

pub fn sha256_file(path: &Path) -> Result<[u8; 32], String> {
    use std::io::Read;
    use windows::Win32::Security::Cryptography::*;
    let mut file = std::fs::File::open(path).map_err(|e| format!("could not open the download: {e}"))?;
    let mut algorithm = BCRYPT_ALG_HANDLE::default();
    let mut hash = BCRYPT_HASH_HANDLE::default();
    let mut out = [0u8; 32];
    unsafe {
        BCryptOpenAlgorithmProvider(&mut algorithm, BCRYPT_SHA256_ALGORITHM, None, BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0))
            .ok().map_err(|e| format!("SHA-256 is unavailable: {e}"))?;
        let result = (|| {
            BCryptCreateHash(algorithm, &mut hash, None, None, 0).ok().map_err(|e| format!("SHA-256 failed: {e}"))?;
            let inner = (|| {
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    let n = file.read(&mut buf).map_err(|e| format!("could not read the download: {e}"))?;
                    if n == 0 { break; }
                    BCryptHashData(hash, &buf[..n], 0).ok().map_err(|e| format!("SHA-256 failed: {e}"))?;
                }
                BCryptFinishHash(hash, &mut out, 0).ok().map_err(|e| format!("SHA-256 failed: {e}"))
            })();
            let _ = BCryptDestroyHash(hash);
            inner
        })();
        let _ = BCryptCloseAlgorithmProvider(algorithm, 0);
        result?;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// State persistence
// ---------------------------------------------------------------------------------------------

pub fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn data_dir() -> Option<PathBuf> {
    crate::config::Config::path()?.parent().map(Path::to_path_buf)
}

pub fn load_state() -> UpdateState {
    let Some(path) = data_dir().map(|d| d.join("update-state.ini")) else { return UpdateState::default() };
    crate::config::read_state(&path).map(|t| UpdateState::parse(&t)).unwrap_or_default()
}

pub fn save_state(state: &UpdateState) {
    let Some(path) = data_dir().map(|d| d.join("update-state.ini")) else { return };
    if let Err(e) = crate::config::write_state(&path, &state.to_text()) {
        crate::app::log(&format!("could not save update state: {e}"));
    }
}

// ---------------------------------------------------------------------------------------------
// Shared runtime state between the UI thread and background workers
// ---------------------------------------------------------------------------------------------

/// Newest known stable release (valid tag) that is newer than this build.
static AVAILABLE: Mutex<Option<String>> = Mutex::new(None);
static NOTE: Mutex<Option<(String, String)>> = Mutex::new(None);
static INSTALLER_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

fn lock<T>(m: &'static Mutex<T>) -> std::sync::MutexGuard<'static, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn available_tag() -> Option<String> { lock(&AVAILABLE).clone() }
pub fn set_available(tag: Option<String>) { *lock(&AVAILABLE) = tag; }
pub fn set_note(title: &str, text: &str) { *lock(&NOTE) = Some((title.to_owned(), text.to_owned())); }
pub fn take_note() -> Option<(String, String)> { lock(&NOTE).take() }
pub fn set_installer(path: PathBuf) { *lock(&INSTALLER_PATH) = Some(path); }

/// Starts the verified installer once Tilework has released its mutex and is about to exit.
/// The installer is per-user, so it runs as the current (non-elevated) user.
pub fn launch_pending_installer() {
    use std::os::windows::process::CommandExt;
    let Some(path) = lock(&INSTALLER_PATH).take() else { return };
    crate::app::log("starting the verified installer");
    let result = std::process::Command::new(&path)
        .args(INSTALLER_ARGS)
        .current_dir(path.parent().unwrap_or(Path::new(".")))
        .creation_flags(windows::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP.0)
        .spawn();
    if let Err(e) = result {
        crate::app::log(&format!("could not start the installer: {e}"));
        crate::app::message_box(
            "Tilework Update",
            &format!("Could not start the installer ({e}).\nTilework has closed; start it again, or install the update from {RELEASES_URL}."),
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Download and verification
// ---------------------------------------------------------------------------------------------

/// Removes downloaded installers once Tilework runs again, i.e. after the installer finished or an
/// attempt was abandoned. Best effort: a file still locked by a finishing installer is kept until
/// the next start or attempt.
pub fn remove_leftover_downloads() {
    let Some(root) = data_dir().map(|d| d.join("updates")) else { return };
    if crate::config::safe_path(&root).is_err() || !root.is_dir() { return; }
    match std::fs::remove_dir_all(&root) {
        Ok(()) => crate::app::log("removed downloaded update files"),
        Err(e) => crate::app::log(&format!("could not remove downloaded update files yet: {e}")),
    }
}

fn fresh_attempt_dir() -> Result<PathBuf, String> {
    let root = data_dir().ok_or("LOCALAPPDATA is unavailable")?.join("updates");
    let err = |e: std::io::Error| format!("could not prepare the update folder: {e}");
    crate::config::safe_path(&root).map_err(err)?;
    std::fs::create_dir_all(&root).map_err(err)?;
    crate::config::safe_path(&root).map_err(err)?;
    // Old attempts are never reused: remove them all, then start a fresh directory.
    for entry in std::fs::read_dir(&root).map_err(err)?.flatten() {
        let path = entry.path();
        let _ = if entry.file_type().is_ok_and(|t| t.is_dir()) { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
    }
    let dir = root.join(format!("{}-{}", std::process::id(), unix_now()));
    std::fs::create_dir(&dir).map_err(err)?;
    Ok(dir)
}

fn curl_download(url: &str, out: &Path, max_time: &str, max_size: u64) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let output = std::process::Command::new(crate::setup::curl_path()?)
        .creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0)
        .args(["-q", "--silent", "--show-error", "--fail", "--location", "--proto", "=https", "--proto-redir", "=https",
               // A freshly published release can briefly answer 5xx from GitHub's CDN; curl retries
               // only transient failures (timeouts, 408, 429, 5xx).
               "--retry", "3", "--retry-delay", "3",
               "--max-redirs", "5", "--max-time", max_time, "--max-filesize", &max_size.to_string(),
               "--user-agent", "Tilework-Updater", "--output"])
        .arg(out)
        .arg(url)
        .output()
        .map_err(|e| format!("could not run Windows curl: {e}"))?;
    if output.status.success() { return Ok(()); }
    let detail = String::from_utf8_lossy(&output.stderr);
    crate::app::log(&format!("download failed: {}", detail.lines().next().unwrap_or("unknown error")));
    Err("the download from GitHub failed".into())
}

/// Downloads the installer of `tag` into a fresh directory and returns its path only if the
/// SHA-256 matches the release's `SHA256SUMS.txt`. A failed attempt leaves nothing behind.
pub fn download_and_verify(tag: &str) -> Result<PathBuf, String> {
    let (installer_url, sums_url) = installer_url(tag).zip(sums_url(tag)).ok_or("invalid release tag")?;
    let dir = fresh_attempt_dir()?;
    let result = (|| {
        let sums_path = dir.join(SUMS);
        curl_download(&sums_url, &sums_path, "20", MAX_SUMS_BYTES)?;
        let sums = std::fs::read(&sums_path).map_err(|e| format!("could not read the checksum list: {e}"))?;
        if sums.len() as u64 > MAX_SUMS_BYTES { return Err("the checksum list is too large".to_string()); }
        let expected = parse_sums(&String::from_utf8_lossy(&sums), INSTALLER)?;
        let path = dir.join(INSTALLER);
        curl_download(&installer_url, &path, "300", MAX_INSTALLER_BYTES)?;
        let size = std::fs::metadata(&path).map_err(|e| format!("could not read the download: {e}"))?.len();
        if !(MIN_INSTALLER_BYTES..=MAX_INSTALLER_BYTES).contains(&size) {
            return Err("the installer has an implausible size".into());
        }
        if !hash_matches(&sha256_file(&path)?, &expected) {
            return Err("the installer does not match its published SHA-256 checksum".into());
        }
        Ok(path)
    })();
    if result.is_err() { let _ = std::fs::remove_dir_all(&dir); }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH_A: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const HASH_B: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

    #[test]
    fn tags_must_be_plain_stable_versions() {
        for ok in ["v0.3.0", "v1.20.300"] { assert!(is_valid_tag(ok), "{ok}"); }
        for bad in ["", "v", "0.3.0", "v0.3", "v0.3.0.1", "v0.3.0-rc1", "v01.2.3", "v1.2.x", "v1.2.3/../x", "V1.2.3", "v1.2.3 ", "v+1.2.3", "v99999999999.0.0"] {
            assert!(!is_valid_tag(bad), "{bad}");
        }
    }

    #[test]
    fn urls_are_only_built_for_valid_tags() {
        assert_eq!(installer_url("v0.4.0").unwrap(), "https://github.com/Jas0nOW/Tilework/releases/download/v0.4.0/Tilework-Setup-x64.exe");
        assert_eq!(sums_url("v0.4.0").unwrap(), "https://github.com/Jas0nOW/Tilework/releases/download/v0.4.0/SHA256SUMS.txt");
        assert!(installer_url("v0.4.0/../../evil").is_none());
        assert!(sums_url("latest").is_none());
    }

    #[test]
    fn sums_are_parsed_strictly() {
        let text = format!("{HASH_A}  Tilework-Portable-x64.exe\r\n{}  Tilework-Setup-x64.exe\r\n", HASH_B.to_uppercase());
        assert_eq!(parse_sums(&text, INSTALLER).unwrap(), HASH_B);
        assert_eq!(parse_sums(&format!("{HASH_A} *{INSTALLER}\n"), INSTALLER).unwrap(), HASH_A);
        assert_eq!(parse_sums(&format!("{HASH_A}  {INSTALLER}\n{HASH_A}  {INSTALLER}\n"), INSTALLER).unwrap(), HASH_A);
        // Missing, conflicting and malformed input is rejected.
        assert!(parse_sums(&format!("{HASH_A}  other.exe\n"), INSTALLER).is_err());
        assert!(parse_sums(&format!("{HASH_A}  {INSTALLER}\n{HASH_B}  {INSTALLER}\n"), INSTALLER).is_err());
        assert!(parse_sums(&format!("{}  {INSTALLER}\n", &HASH_A[..63]), INSTALLER).is_err());
        assert!(parse_sums(&format!("{}g  {INSTALLER}\n", &HASH_A[..63]), INSTALLER).is_err());
        assert!(parse_sums(&format!("{HASH_A} {INSTALLER}\n"), INSTALLER).is_err());
        assert!(parse_sums(&format!("{HASH_A}  {INSTALLER}.evil\n"), INSTALLER).is_err());
        assert!(parse_sums("", INSTALLER).is_err());
        assert!(parse_sums("not a checksum file\n", INSTALLER).is_err());
    }

    #[test]
    fn hashes_compare_case_insensitively_and_exactly() {
        let mut h = [0u8; 32];
        h[0] = 0xab; h[31] = 0x01;
        let hex = to_hex(&h);
        assert!(hash_matches(&h, &hex));
        assert!(hash_matches(&h, &hex.to_uppercase()));
        h[5] = 1;
        assert!(!hash_matches(&h, &hex));
        assert!(!hash_matches(&[0; 32], ""));
    }

    #[test]
    fn check_is_due_once_per_day() {
        assert!(should_check_now(None, 1000));
        assert!(!should_check_now(Some(1000), 1000 + DAY_SECS - 1));
        assert!(should_check_now(Some(1000), 1000 + DAY_SECS));
        assert!(should_check_now(Some(5000), 1000), "clock moved backwards");
    }

    #[test]
    fn a_version_is_announced_once_per_day() {
        let mut s = UpdateState::default();
        assert!(should_notify(&s, "v0.4.0", 100));
        s.notified_tag = Some("v0.4.0".into());
        s.notified_at = Some(100);
        assert!(!should_notify(&s, "v0.4.0", 100 + DAY_SECS - 1));
        assert!(should_notify(&s, "v0.4.0", 100 + DAY_SECS));
        assert!(should_notify(&s, "v0.5.0", 101), "a newer version is announced immediately");
    }

    #[test]
    fn state_round_trips_and_ignores_garbage() {
        let s = UpdateState { last_check: Some(12), notified_tag: Some("v0.4.0".into()), notified_at: Some(34) };
        assert_eq!(UpdateState::parse(&s.to_text()), s);
        let g = UpdateState::parse("last_check=x\nnotified_tag=../../evil\nnotified_at=7\nnoise\n");
        assert_eq!(g, UpdateState { last_check: None, notified_tag: None, notified_at: Some(7) });
    }

    /// Network test against the real v0.3.0 release: `cargo test live_ -- --ignored`.
    #[test]
    #[ignore]
    fn live_download_of_a_published_release_verifies() {
        let path = download_and_verify("v0.3.0").unwrap();
        assert!(path.ends_with(INSTALLER));
        assert!(download_and_verify("v0.0.1").is_err(), "a missing release must fail cleanly");
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    #[ignore]
    fn live_release_check_reaches_github() {
        let status = crate::setup::check_for_update();
        assert!(!matches!(status, crate::setup::UpdateStatus::Error(_)), "{status:?}");
    }

    #[test]
    fn sha256_matches_known_vectors() {
        let dir = std::env::temp_dir().join(format!("tilework-sha-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let hash = |name: &str, data: &[u8]| {
            let p = dir.join(name);
            std::fs::write(&p, data).unwrap();
            to_hex(&sha256_file(&p).unwrap())
        };
        assert_eq!(hash("empty", b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hash("abc", b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        // Larger than the 64 KiB read buffer: one million 'a' (FIPS 180-2 vector).
        assert_eq!(hash("million", &vec![b'a'; 1_000_000]), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
