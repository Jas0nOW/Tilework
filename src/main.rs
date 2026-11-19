#![windows_subsystem = "windows"]
mod app;
mod config;
mod platform;
pub mod setup;
mod tree;

use platform::Platform;
use std::collections::HashMap;
use tree::Tree;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let action = match parse_cli_action(&args) {
        Ok(a) => a,
        Err(e) => {
            unsafe { let _ = windows::Win32::System::Console::AttachConsole(windows::Win32::System::Console::ATTACH_PARENT_PROCESS); }
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    if action == CliAction::Version {
        attach_console();
        println!("Tilework {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    // DPI must be established before creating any window or querying geometry.
    unsafe {
        use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
        if let Err(e) = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) {
            app::log(&format!("could not enable per-monitor DPI awareness: {e}"));
        }
    }

    match action {
        CliAction::Version => unreachable!(),
        CliAction::SnapStatus => {
            unsafe { let _ = windows::Win32::System::Console::AttachConsole(windows::Win32::System::Console::ATTACH_PARENT_PROCESS); }
            match platform::snap_status() {
                Some(on) => println!("Windows Snap: {}", if on { "enabled" } else { "disabled" }),
                None => { eprintln!("Could not read Windows Snap status"); std::process::exit(1); }
            }
        }
        CliAction::Dump => {
            unsafe { let _ = windows::Win32::System::Console::AttachConsole(windows::Win32::System::Console::ATTACH_PARENT_PROCESS); }
            dump();
        }
        CliAction::Install | CliAction::Uninstall | CliAction::Update => {
            attach_console();
            let message = match action {
                CliAction::Install => "Download and run Tilework-Setup-x64.exe from the official releases.",
                CliAction::Uninstall => "Exit Tilework and uninstall it in Windows Settings > Apps.",
                _ => "Use --check-update and install the new version with the official setup.",
            };
            eprintln!("{message}\n{}", setup::RELEASES_URL);
            std::process::exit(2);
        }
        CliAction::AutostartOn | CliAction::AutostartOff => {
            attach_console();
            let enabled = action == CliAction::AutostartOn;
            if let Err(e) = setup::set_autostart(enabled) {
                eprintln!("{e}");
                std::process::exit(1);
            }
            println!("Tilework autostart entry {}", if enabled { "registered" } else { "removed" });
        }
        CliAction::AutostartStatus => {
            attach_console();
            let enabled = setup::is_autostart_enabled();
            println!("Tilework autostart entry: {}", if enabled { "registered" } else { "absent" });
        }
        CliAction::CheckUpdate => {
            attach_console();
            match setup::check_for_update() {
                setup::UpdateStatus::Available { tag, release_url } => {
                    println!("Update available: {tag} (current: v{})\n{release_url}", env!("CARGO_PKG_VERSION"));
                }
                setup::UpdateStatus::UpToDate => println!("Tilework v{} is up to date.", env!("CARGO_PKG_VERSION")),
                setup::UpdateStatus::NoReleaseFound => println!("No public stable release found."),
                setup::UpdateStatus::Error(e) => {
                    eprintln!("Update check failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        CliAction::RestoreSnap(force) => {
            unsafe { let _ = windows::Win32::System::Console::AttachConsole(windows::Win32::System::Console::ATTACH_PARENT_PROCESS); }
            unsafe {
                use windows::Win32::Foundation::CloseHandle;
                let Some((mutex, running)) = claim_instance() else {
                    eprintln!("Could not check whether Tilework is running; Snap was not changed.");
                    std::process::exit(1);
                };
                if restore_refused(true, running, force) {
                    eprintln!("Tilework is running; close it first or use --force to restore Snap.");
                    let _ = CloseHandle(mutex);
                    std::process::exit(3);
                }
                if !platform::restore_snap_explicit() {
                    eprintln!("Could not restore Windows Snap");
                    let _ = CloseHandle(mutex);
                    std::process::exit(1);
                }
                let _ = CloseHandle(mutex);
            }
        }
        CliAction::Run(filter) => run_manager(filter, false),
        CliAction::Replace(filter) => run_manager(filter, true),
    }
}

fn attach_console() {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    unsafe { let _ = AttachConsole(ATTACH_PARENT_PROCESS); }
}

/// Read-only: prints which windows would be tiled and where, without touching any window.
fn dump() {
    let cfg = config::Config::load();
    tree::set_min_zone(cfg.min_zone_width, cfg.min_zone_height);
    let p = Platform::new();
    let mut trees: HashMap<platform::SurfaceKey, (Tree, tree::Rect)> = HashMap::new();
    let mut ignored = Vec::new();
    let cur = p.current_desktop();
    for h in p.all_top_level().into_iter().rev() {
        if cur != 0 && p.is_manageable(h, cur) && !p.is_fullscreen(h) {
            let (key, area) = p.surface_of(h);
            let e = trees.entry(key).or_insert_with(|| (Tree::new(), area));
            e.0.add(h, area, None);
        } else if unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(platform::hwnd_of(h)).as_bool() } && !p.title(h).is_empty() {
            ignored.push(h);
        }
    }
    for (key, (t, area)) in &trees {
        println!("surface desktop={:032x} monitor={:x} area={}x{} windows={}", key.desktop, key.monitor, area.w, area.h, t.leaves().len());
        for (h, r) in t.layout(*area) {
            println!("  {:>8x}  {:>4}x{:<4} at {:>5},{:<5} {:<22} {}", h, r.w, r.h, r.x, r.y, p.exe_name(h), p.title(h).chars().take(40).collect::<String>());
        }
    }
    println!("--- visible but not managed:");
    for h in ignored {
        println!("  {:>8x} {:<18} {:<22} {}", h, p.class_name(h), p.exe_name(h), p.title(h).chars().take(35).collect::<String>());
    }
}

#[derive(Debug, PartialEq, Eq)]
enum CliAction {
    Version,
    Run(Option<String>),
    /// Internal: like `Run`, but waits for a previous instance to exit (used by "Restart as administrator").
    Replace(Option<String>),
    Dump,
    SnapStatus,
    RestoreSnap(bool),
    Install,
    Uninstall,
    AutostartOn,
    AutostartOff,
    AutostartStatus,
    CheckUpdate,
    Update,
}

fn parse_cli_action(args: &[String]) -> Result<CliAction, &'static str> {
    let mut filter = None;
    let mut action: Option<CliAction> = None;
    let mut force = false;
    let mut replace = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--version" => {
                if action.is_some() { return Err("multiple command flags cannot be combined"); }
                action = Some(CliAction::Version);
            }
            "--filter" => {
                if filter.is_some() { return Err("--filter may only be supplied once"); }
                match args.get(i + 1) {
                    Some(value) if !value.trim().is_empty() && !value.starts_with("--") => filter = Some(value.clone()),
                    _ => return Err("--filter requires a non-empty value"),
                }
                i += 1;
            }
            "--dump" => {
                if action.is_some() { return Err("multiple command flags cannot be combined"); }
                action = Some(CliAction::Dump);
            }
            "--snap-status" => {
                if action.is_some() { return Err("multiple command flags cannot be combined"); }
                action = Some(CliAction::SnapStatus);
            }
            "--restore-snap" => {
                if action.is_some() { return Err("multiple command flags cannot be combined"); }
                action = Some(CliAction::RestoreSnap(false));
            }
            "--install" => {
                if action.is_some() { return Err("multiple command flags cannot be combined"); }
                action = Some(CliAction::Install);
            }
            "--uninstall" => {
                if action.is_some() { return Err("multiple command flags cannot be combined"); }
                action = Some(CliAction::Uninstall);
            }
            "--autostart-on" => {
                if action.is_some() { return Err("multiple command flags cannot be combined"); }
                action = Some(CliAction::AutostartOn);
            }
            "--autostart-off" => {
                if action.is_some() { return Err("multiple command flags cannot be combined"); }
                action = Some(CliAction::AutostartOff);
            }
            "--autostart-status" => {
                if action.is_some() { return Err("multiple command flags cannot be combined"); }
                action = Some(CliAction::AutostartStatus);
            }
            "--check-update" => {
                if action.is_some() { return Err("multiple command flags cannot be combined"); }
                action = Some(CliAction::CheckUpdate);
            }
            "--update" => {
                if action.is_some() { return Err("multiple command flags cannot be combined"); }
                action = Some(CliAction::Update);
            }
            "--force" => {
                if force { return Err("--force may only be supplied once"); }
                force = true;
            }
            "--replace" => {
                if replace { return Err("--replace may only be supplied once"); }
                replace = true;
            }
            _ => return Err("unknown argument"),
        }
        i += 1;
    }

    if replace && action.is_some() {
        return Err("--replace cannot be combined with commands");
    }

    if force {
        match action {
            Some(CliAction::RestoreSnap(_)) => action = Some(CliAction::RestoreSnap(true)),
            _ => return Err("--force requires --restore-snap"),
        }
    }

    if let Some(act) = action {
        if filter.is_some() {
            return Err("--filter cannot be combined with commands");
        }
        Ok(act)
    } else if replace {
        Ok(CliAction::Replace(filter))
    } else {
        Ok(CliAction::Run(filter))
    }
}

/// Runs the tiling manager unless another instance owns the mutex. With `replace` (a restart from the
/// tray) the old instance may still be shutting down, so wait up to 10 s for it to release the mutex.
fn run_manager(filter: Option<String>, replace: bool) {
    // Detach console for normal GUI execution
    unsafe { let _ = windows::Win32::System::Console::FreeConsole(); }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let Some((mutex, running)) = claim_instance() else { return; };
        if !running {
            app::run(filter);
            unsafe { let _ = windows::Win32::Foundation::CloseHandle(mutex); }
            return;
        }
        if !mutex.is_invalid() { unsafe { let _ = windows::Win32::Foundation::CloseHandle(mutex); } }
        if !replace { return; }
        if std::time::Instant::now() >= deadline {
            app::log("previous Tilework instance did not exit in time");
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Opens the single-instance mutex. Returns the handle and whether another instance already owns it.
/// An instance running with different elevation owns a mutex we may not open (access denied), which
/// also means "already running". `None` means the check itself failed (logged).
fn claim_instance() -> Option<(windows::Win32::Foundation::HANDLE, bool)> {
    use windows::Win32::Foundation::{GetLastError, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, HANDLE};
    use windows::Win32::System::Threading::CreateMutexW;
    unsafe {
        match CreateMutexW(None, false, windows::core::w!("Local\\TileworkSingleInstance")) {
            Ok(m) => Some((m, GetLastError() == ERROR_ALREADY_EXISTS)),
            Err(e) if e.code() == ERROR_ACCESS_DENIED.to_hresult() => Some((HANDLE::default(), true)),
            Err(e) => { app::log(&format!("could not create single-instance mutex: {e}")); None }
        }
    }
}

#[cfg(test)]
fn parse_filter(args: &[String]) -> Result<Option<String>, &'static str> {
    match parse_cli_action(args)? {
        CliAction::Run(f) | CliAction::Replace(f) => Ok(f),
        CliAction::Dump | CliAction::SnapStatus | CliAction::RestoreSnap(_) => Ok(None),
        _ => Err("installer/updater flags cannot be evaluated as simple filters"),
    }
}

fn restore_refused(restore: bool, running: bool, force: bool) -> bool { restore && running && !force }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filter_requires_a_nonempty_value() {
        for args in [vec!["app", "--filter"], vec!["app", "--filter", ""], vec!["app", "--filter", "  "], vec!["app", "--filter", "--dump"], vec!["app", "--filter", "test", "--filter"], vec!["app", "--filter", "test", "--filter", "other"]] {
            assert!(parse_filter(&args.into_iter().map(String::from).collect::<Vec<_>>()).is_err());
        }
        assert_eq!(parse_filter(&["app".into(), "--filter".into(), "test".into()]), Ok(Some("test".into())));
        assert!(parse_filter(&["app".into(), "--unknown".into()]).is_err());
        assert!(parse_filter(&["app".into(), "--dump".into(), "--snap-status".into()]).is_err());
    }
    #[test]
    fn restore_requires_force_when_a_manager_is_running() {
        assert!(restore_refused(true, true, false));
        assert!(!restore_refused(true, true, true));
        assert!(!restore_refused(true, false, false));
        assert!(!restore_refused(false, true, false));
        assert!(parse_filter(&["app".into(), "--force".into()]).is_err());
        assert!(parse_filter(&["app".into(), "--force".into(), "--restore-snap".into()]).is_ok());
    }

    #[test]
    fn replace_only_combines_with_run_and_filter() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(parse_cli_action(&a(&["app", "--replace"])), Ok(CliAction::Replace(None)));
        assert_eq!(parse_cli_action(&a(&["app", "--replace", "--filter", "x"])), Ok(CliAction::Replace(Some("x".into()))));
        assert!(parse_cli_action(&a(&["app", "--replace", "--replace"])).is_err());
        assert!(parse_cli_action(&a(&["app", "--replace", "--dump"])).is_err());
        assert!(parse_cli_action(&a(&["app", "--version", "--replace"])).is_err());
        assert!(parse_cli_action(&a(&["app", "--replace", "--force"])).is_err());
    }

    #[test]
    fn cli_action_parses_installer_and_updater_flags() {
        assert_eq!(parse_cli_action(&["app".into(), "--install".into()]), Ok(CliAction::Install));
        assert_eq!(parse_cli_action(&["app".into(), "--uninstall".into()]), Ok(CliAction::Uninstall));
        assert_eq!(parse_cli_action(&["app".into(), "--autostart-on".into()]), Ok(CliAction::AutostartOn));
        assert_eq!(parse_cli_action(&["app".into(), "--autostart-off".into()]), Ok(CliAction::AutostartOff));
        assert_eq!(parse_cli_action(&["app".into(), "--autostart-status".into()]), Ok(CliAction::AutostartStatus));
        assert_eq!(parse_cli_action(&["app".into(), "--check-update".into()]), Ok(CliAction::CheckUpdate));
        assert_eq!(parse_cli_action(&["app".into(), "--update".into()]), Ok(CliAction::Update));
        assert!(parse_cli_action(&["app".into(), "--install".into(), "--update".into()]).is_err());
    }
}
