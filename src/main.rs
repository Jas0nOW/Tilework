#![windows_subsystem = "windows"]
mod app;
mod config;
mod platform;
mod tree;

use platform::Platform;
use std::collections::HashMap;
use tree::Tree;

fn main() {
    // DPI must be established before creating any window or querying geometry.
    unsafe {
        use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
        if let Err(e) = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) {
            app::log(&format!("could not enable per-monitor DPI awareness: {e}"));
        }
    }
    let args: Vec<String> = std::env::args().collect();
    let parsed = parse_filter(&args);
    if parsed.is_err() || args.iter().any(|a| matches!(a.as_str(), "--dump" | "--snap-status" | "--restore-snap")) {
        unsafe { let _ = windows::Win32::System::Console::AttachConsole(windows::Win32::System::Console::ATTACH_PARENT_PROCESS); }
    } else {
        // Detach even if the process was started with an inherited console.
        unsafe { let _ = windows::Win32::System::Console::FreeConsole(); }
    }
    let filter = match parsed {
        Ok(f) => f,
        Err(e) => { eprintln!("{e}"); std::process::exit(2); }
    };
    if args.iter().any(|a| a == "--snap-status") {
        match platform::snap_status() {
            Some(on) => println!("Windows Snap: {}", if on { "enabled" } else { "disabled" }),
            None => { eprintln!("Could not read Windows Snap status"); std::process::exit(1); }
        }
        return;
    }
    if args.iter().any(|a| a == "--dump") {
        dump();
        return;
    }
    // `--filter TEXT` manages only windows whose title contains TEXT (or whose exe equals TEXT);
    // used for safe tests without touching real windows.
    unsafe {
        use windows::Win32::System::Threading::CreateMutexW;
        use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
        let mutex = match CreateMutexW(None, false, windows::core::w!("Local\\TileworkSingleInstance")) {
            Ok(m) => m,
            Err(e) => { app::log(&format!("could not create single-instance mutex: {e}")); return; }
        };
        let running = GetLastError() == ERROR_ALREADY_EXISTS;
        let restore = args.iter().any(|a| a == "--restore-snap");
        let force = args.iter().any(|a| a == "--force");
        if restore_refused(restore, running, force) {
            eprintln!("Tilework is running; close it first or use --force to restore Snap.");
            let _ = CloseHandle(mutex);
            std::process::exit(3);
        }
        if restore {
            if !platform::restore_snap_explicit() {
                eprintln!("Could not restore Windows Snap");
                let _ = CloseHandle(mutex);
                std::process::exit(1);
            }
        } else if !running {
            app::run(filter);
        }
        let _ = CloseHandle(mutex);
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

fn parse_filter(args: &[String]) -> Result<Option<String>, &'static str> {
    let mut filter = None;
    let mut mode = false;
    let mut restore = false;
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
            "--dump" | "--snap-status" | "--restore-snap" => {
                if mode { return Err("only one diagnostic mode may be supplied"); }
                mode = true;
                restore = args[i] == "--restore-snap";
            }
            "--force" => { if force { return Err("--force may only be supplied once"); } force = true; }
            _ => return Err("unknown argument"),
        }
        i += 1;
    }
    if force && !restore { return Err("--force requires --restore-snap"); }
    Ok(filter)
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
}
