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
    // Clean up any old binary from a previous update
    setup::cleanup_old_binary();

    // DPI must be established before creating any window or querying geometry.
    unsafe {
        use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
        if let Err(e) = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) {
            app::log(&format!("could not enable per-monitor DPI awareness: {e}"));
        }
    }
    let args: Vec<String> = std::env::args().collect();
    let action = match parse_cli_action(&args) {
        Ok(a) => a,
        Err(e) => {
            unsafe { let _ = windows::Win32::System::Console::AttachConsole(windows::Win32::System::Console::ATTACH_PARENT_PROCESS); }
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    match action {
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
        CliAction::Install => {
            unsafe { let _ = windows::Win32::System::Console::AttachConsole(windows::Win32::System::Console::ATTACH_PARENT_PROCESS); }
            match setup::install() {
                Ok(()) => {
                    println!("Tilework installed successfully.");
                    println!("Binary: %LOCALAPPDATA%\\Tilework\\tilework.exe");
                    println!("Start Menu shortcut created.");
                    println!("Silent admin autostart registered via Windows Task Scheduler.");
                }
                Err(e) => {
                    eprintln!("Installation failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        CliAction::Uninstall => {
            unsafe { let _ = windows::Win32::System::Console::AttachConsole(windows::Win32::System::Console::ATTACH_PARENT_PROCESS); }
            match setup::uninstall() {
                Ok(()) => println!("Tilework uninstalled successfully."),
                Err(e) => {
                    eprintln!("Uninstall failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        CliAction::AutostartOn => {
            unsafe { let _ = windows::Win32::System::Console::AttachConsole(windows::Win32::System::Console::ATTACH_PARENT_PROCESS); }
            let exe = match std::env::current_exe() {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("Could not locate executable: {e}");
                    std::process::exit(1);
                }
            };
            match setup::enable_autostart(&exe) {
                Ok(()) => println!("Autostart enabled with highest privileges."),
                Err(e) => {
                    eprintln!("Failed to enable autostart: {e}");
                    std::process::exit(1);
                }
            }
        }
        CliAction::AutostartOff => {
            unsafe { let _ = windows::Win32::System::Console::AttachConsole(windows::Win32::System::Console::ATTACH_PARENT_PROCESS); }
            match setup::disable_autostart() {
                Ok(()) => println!("Autostart disabled."),
                Err(e) => {
                    eprintln!("Failed to disable autostart: {e}");
                    std::process::exit(1);
                }
            }
        }
        CliAction::AutostartStatus => {
            unsafe { let _ = windows::Win32::System::Console::AttachConsole(windows::Win32::System::Console::ATTACH_PARENT_PROCESS); }
            let enabled = setup::is_autostart_enabled();
            println!("Tilework autostart: {}", if enabled { "enabled" } else { "disabled" });
        }
        CliAction::CheckUpdate => {
            unsafe { let _ = windows::Win32::System::Console::AttachConsole(windows::Win32::System::Console::ATTACH_PARENT_PROCESS); }
            println!("Checking GitHub releases for Tilework...");
            match setup::check_for_update() {
                setup::UpdateStatus::Available(rel) => {
                    println!("Update available: {} (current: v{})", rel.tag, env!("CARGO_PKG_VERSION"));
                    println!("Download URL: {}", rel.download_url);
                    if !rel.release_notes.is_empty() {
                        println!("\nRelease Notes:\n{}", rel.release_notes);
                    }
                }
                setup::UpdateStatus::UpToDate(tag) => {
                    println!("Tilework is up to date (version {tag}).");
                }
                setup::UpdateStatus::NoReleaseFound => {
                    println!("No releases found on GitHub repository yet.");
                }
                setup::UpdateStatus::Error(e) => {
                    eprintln!("Update check failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        CliAction::Update => {
            unsafe { let _ = windows::Win32::System::Console::AttachConsole(windows::Win32::System::Console::ATTACH_PARENT_PROCESS); }
            println!("Checking for updates...");
            match setup::check_for_update() {
                setup::UpdateStatus::Available(rel) => {
                    println!("Found update: {}", rel.tag);
                    let temp_path = std::env::temp_dir().join("tilework_update.exe");
                    println!("Downloading {}...", rel.download_url);
                    if let Err(e) = setup::download_and_verify(&rel.download_url, &temp_path) {
                        eprintln!("Download/verification failed: {e}");
                        std::process::exit(1);
                    }
                    let current_exe = match std::env::current_exe() {
                        Ok(e) => e,
                        Err(e) => {
                            eprintln!("Could not locate current binary: {e}");
                            std::process::exit(1);
                        }
                    };
                    println!("Applying update...");
                    if let Err(e) = setup::apply_update(&current_exe, &temp_path) {
                        eprintln!("Update failed: {e}");
                        std::process::exit(1);
                    }
                    println!("Successfully updated to {}! Restarting...", rel.tag);
                    let _ = std::process::Command::new(&current_exe).spawn();
                    std::process::exit(0);
                }
                setup::UpdateStatus::UpToDate(tag) => {
                    println!("Already up to date ({tag}).");
                }
                setup::UpdateStatus::NoReleaseFound => {
                    println!("No releases found on GitHub repository.");
                }
                setup::UpdateStatus::Error(e) => {
                    eprintln!("Update failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        CliAction::RestoreSnap(force) => {
            unsafe { let _ = windows::Win32::System::Console::AttachConsole(windows::Win32::System::Console::ATTACH_PARENT_PROCESS); }
            unsafe {
                use windows::Win32::System::Threading::CreateMutexW;
                use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
                let mutex = match CreateMutexW(None, false, windows::core::w!("Local\\TileworkSingleInstance")) {
                    Ok(m) => m,
                    Err(e) => { app::log(&format!("could not create single-instance mutex: {e}")); return; }
                };
                let running = GetLastError() == ERROR_ALREADY_EXISTS;
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
        CliAction::Run(filter) => {
            // Detach console for normal GUI execution
            unsafe { let _ = windows::Win32::System::Console::FreeConsole(); }
            unsafe {
                use windows::Win32::System::Threading::CreateMutexW;
                use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
                let mutex = match CreateMutexW(None, false, windows::core::w!("Local\\TileworkSingleInstance")) {
                    Ok(m) => m,
                    Err(e) => { app::log(&format!("could not create single-instance mutex: {e}")); return; }
                };
                let running = GetLastError() == ERROR_ALREADY_EXISTS;
                if !running {
                    app::run(filter);
                }
                let _ = CloseHandle(mutex);
            }
        }
    }
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
    Run(Option<String>),
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
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
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
            _ => return Err("unknown argument"),
        }
        i += 1;
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
    } else {
        Ok(CliAction::Run(filter))
    }
}

#[cfg(test)]
fn parse_filter(args: &[String]) -> Result<Option<String>, &'static str> {
    match parse_cli_action(args)? {
        CliAction::Run(f) => Ok(f),
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
