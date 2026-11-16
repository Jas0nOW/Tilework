#![windows_subsystem = "windows"]
#![allow(dead_code)]
mod app;
mod config;
mod platform;
mod tree;

use platform::Platform;
use std::collections::HashMap;
use tree::Tree;

fn main() {
    // GUI subsystem has no console; attach to the parent's so that --dump can print.
    unsafe {
        let _ = windows::Win32::System::Console::AttachConsole(windows::Win32::System::Console::ATTACH_PARENT_PROCESS);
    }
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--dump") {
        dump();
        return;
    }
    if args.iter().any(|a| a == "--restore-snap") {
        platform::set_snap(true);
        return;
    }
    // `--filter TEXT` manages only windows whose title contains TEXT (or whose exe equals TEXT);
    // used for safe tests without touching real windows.
    let filter = args.iter().position(|a| a == "--filter").and_then(|i| args.get(i + 1)).cloned();
    app::run(filter);
}

/// Read-only: prints which windows would be tiled and where, without touching any window.
fn dump() {
    let p = Platform::new();
    let mut trees: HashMap<platform::SurfaceKey, (Tree, tree::Rect)> = HashMap::new();
    let mut ignored = Vec::new();
    let cur = p.current_desktop();
    for h in p.all_top_level().into_iter().rev() {
        if p.is_manageable(h, cur) {
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
