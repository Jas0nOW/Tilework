//! Thin Windows layer: inspecting windows, monitors and virtual desktops, moving windows.

use crate::tree::Rect;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU8, Ordering};
use windows::Win32::System::Console::{SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT};
use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};
use windows::Win32::UI::WindowsAndMessaging::*;

/// Identifies one tiling surface: a virtual desktop on one monitor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SurfaceKey {
    pub desktop: u128,
    pub monitor: isize,
}

pub struct Platform {
    vdm: Option<IVirtualDesktopManager>,
    com_initialized: bool,
}

pub fn hwnd_of(h: isize) -> HWND {
    HWND(h as *mut c_void)
}

pub fn id_of(h: HWND) -> isize {
    h.0 as isize
}

impl Platform {
    pub fn new() -> Self {
        unsafe {
            let com_initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
            let vdm = CoCreateInstance::<_, IVirtualDesktopManager>(&VirtualDesktopManager, None, CLSCTX_ALL).ok();
            Self { vdm, com_initialized }
        }
    }

    pub fn class_name(&self, h: isize) -> String {
        let mut buf = [0u16; 256];
        let n = unsafe { GetClassNameW(hwnd_of(h), &mut buf) } as usize;
        String::from_utf16_lossy(&buf[..n.min(256)])
    }

    pub fn title(&self, h: isize) -> String {
        let mut buf = [0u16; 512];
        let n = unsafe { GetWindowTextW(hwnd_of(h), &mut buf) } as usize;
        String::from_utf16_lossy(&buf[..n.min(512)])
    }

    pub fn exe_name(&self, h: isize) -> String {
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd_of(h), Some(&mut pid));
            let Ok(proc) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
            let mut buf = [0u16; 512];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(proc, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
            let _ = CloseHandle(proc);
            if !ok {
                return String::new();
            }
            let full = String::from_utf16_lossy(&buf[..len as usize]);
            full.rsplit('\\').next().unwrap_or("").to_string()
        }
    }

    fn cloaked(&self, h: isize) -> u32 {
        let mut v = 0u32;
        unsafe {
            let _ = DwmGetWindowAttribute(hwnd_of(h), DWMWA_CLOAKED, &mut v as *mut u32 as *mut c_void, 4);
        }
        v
    }

    /// True for normal application windows that should be tiled.
    pub fn is_manageable(&self, h: isize, current_desktop: u128) -> bool {
        unsafe {
            let hw = hwnd_of(h);
            if !IsWindow(Some(hw)).as_bool() || !IsWindowVisible(hw).as_bool() && self.cloaked(h) != 2 {
                return false;
            }
            if IsIconic(hw).as_bool() {
                return false;
            }
            // 2 = cloaked by the shell (window lives on another virtual desktop) -> still ours.
            let cloak = self.cloaked(h);
            if cloak != 0 && cloak != 2 {
                return false;
            }
            // Cloaked but on the desktop that is shown: a suspended/ghost frame, not a real window.
            if cloak == 2 {
                let d = self.raw_desktop_id(h);
                if d == 0 || d == current_desktop {
                    return false;
                }
            }
            let style = GetWindowLongW(hw, GWL_STYLE) as u32;
            let ex = GetWindowLongW(hw, GWL_EXSTYLE) as u32;
            // Normal windows have a title bar, a resize border or the usual min/max/system buttons
            // (frameless Java/Electron windows only have the buttons).
            let looks_like_app = style & WS_CAPTION.0 == WS_CAPTION.0
                || style & (WS_THICKFRAME.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_MAXIMIZEBOX.0) != 0;
            if !looks_like_app {
                return false;
            }
            if style & WS_CHILD.0 != 0 || ex & WS_EX_TOOLWINDOW.0 != 0 && ex & WS_EX_APPWINDOW.0 == 0 {
                return false;
            }
            if ex & WS_EX_NOACTIVATE.0 != 0 {
                return false;
            }
            if GetWindow(hw, GW_OWNER).map(|o| !o.is_invalid()).unwrap_or(false) {
                return false;
            }
            if self.title(h).is_empty() {
                return false;
            }
            let class = self.class_name(h);
            if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "CosmicTileOverlay" | "CosmicTileMain") {
                return false;
            }
            let mut r = RECT::default();
            !(GetWindowRect(hw, &mut r).is_err() || r.right - r.left < 80 || r.bottom - r.top < 40)
        }
    }

    pub fn all_top_level(&self) -> Vec<isize> {
        unsafe extern "system" fn cb(h: HWND, lp: LPARAM) -> BOOL {
            let v = unsafe { &mut *(lp.0 as *mut Vec<isize>) };
            v.push(h.0 as isize);
            BOOL(1)
        }
        let mut out: Vec<isize> = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(cb), LPARAM(&mut out as *mut Vec<isize> as isize));
        }
        out
    }

    fn raw_desktop_id(&self, h: isize) -> u128 {
        let Some(vdm) = &self.vdm else { return 0 };
        match unsafe { vdm.GetWindowDesktopId(hwnd_of(h)) } {
            Ok(g) => g.to_u128(),
            Err(_) => 0,
        }
    }

    /// Desktop a window lives on. Windows that do not report one (some UWP frames) belong to the
    /// desktop that is currently shown.
    pub fn desktop_id(&self, h: isize) -> u128 {
        match self.raw_desktop_id(h) {
            0 => self.current_desktop(),
            id => id,
        }
    }

    /// Use confirmed visible application windows, rejecting conflicting IDs rather than trusting
    /// the first one (a pinned window may report its original desktop).
    pub fn current_desktop(&self) -> u128 {
        let Some(vdm) = &self.vdm else { return 0 };
        let mut ids = Vec::new();
        for h in self.all_top_level() {
            unsafe {
                let hw = hwnd_of(h);
                let mut cloak = 0u32;
                if self.is_manageable(h, 0) && IsWindowVisible(hw).as_bool() && !IsIconic(hw).as_bool()
                    && DwmGetWindowAttribute(hw, DWMWA_CLOAKED, &mut cloak as *mut u32 as *mut c_void, 4).is_ok() && cloak == 0
                    && vdm.IsWindowOnCurrentVirtualDesktop(hw).map(|v| v.as_bool()).unwrap_or(false) {
                    let id = self.raw_desktop_id(h);
                    if id != 0 {
                        ids.push(id);
                    }
                }
            }
        }
        desktop_consensus(ids)
    }

    /// Monitor handle and the usable work area (without the taskbar) for a window.
    pub fn monitor_of(&self, h: isize) -> (isize, Rect) {
        unsafe {
            let m = MonitorFromWindow(hwnd_of(h), MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(m, &mut mi);
            let w = mi.rcWork;
            (m.0 as isize, Rect::new(w.left, w.top, w.right - w.left, w.bottom - w.top))
        }
    }

    pub fn surface_of(&self, h: isize) -> (SurfaceKey, Rect) {
        let (monitor, area) = self.monitor_of(h);
        (SurfaceKey { desktop: self.desktop_id(h), monitor }, area)
    }

    /// Visible rectangle of a window (without the invisible resize borders Windows 10/11 adds).
    pub fn visible_rect(&self, h: isize) -> Option<Rect> {
        unsafe {
            let mut r = RECT::default();
            DwmGetWindowAttribute(hwnd_of(h), DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut RECT as *mut c_void, std::mem::size_of::<RECT>() as u32).ok()?;
            Some(Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top))
        }
    }

    /// True for a window that covers its whole monitor including the taskbar area (F11 in a
    /// browser, a video or a game in full screen). Such windows must be left alone.
    pub fn is_fullscreen(&self, h: isize) -> bool {
        let Some(v) = self.visible_rect(h) else { return false };
        unsafe {
            let m = MonitorFromWindow(hwnd_of(h), MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if !GetMonitorInfoW(m, &mut mi).as_bool() {
                return false;
            }
            let r = mi.rcMonitor;
            v.x <= r.left && v.y <= r.top && v.right() >= r.right && v.bottom() >= r.bottom
        }
    }

    pub fn is_maximized(&self, h: isize) -> bool {
        unsafe { IsZoomed(hwnd_of(h)).as_bool() }
    }

    pub fn is_minimized(&self, h: isize) -> bool {
        unsafe { IsIconic(hwnd_of(h)).as_bool() }
    }

    pub fn is_hung(&self, h: isize) -> bool {
        unsafe { IsHungAppWindow(hwnd_of(h)).as_bool() }
    }

    /// Queues a move so that the window's *visible* edges match `target`.
    pub fn place(&self, h: isize, target: Rect) -> bool {
        unsafe {
            let hw = hwnd_of(h);
            match placement_step(self.is_hung(h), self.is_minimized(h), self.is_maximized(h)) {
                PlacementStep::Wait => return false,
                PlacementStep::Restore => {
                    // Restore without activation, then let a later poll measure normal borders.
                    // The queued restore has not placed the window at `target` yet.
                    let _ = ShowWindowAsync(hw, SW_SHOWNOACTIVATE);
                    return false;
                }
                PlacementStep::Position => {}
            }
            let mut outer = RECT::default();
            if GetWindowRect(hw, &mut outer).is_err() {
                return false;
            }
            let vis = self.visible_rect(h).unwrap_or(Rect::new(outer.left, outer.top, outer.right - outer.left, outer.bottom - outer.top));
            let (il, it) = (vis.x - outer.left, vis.y - outer.top);
            let (ir, ib) = (outer.right - vis.right(), outer.bottom - vis.bottom());
            SetWindowPos(
                hw,
                None,
                target.x - il,
                target.y - it,
                target.w + il + ir,
                target.h + it + ib,
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS,
            ).is_ok()
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum PlacementStep { Wait, Restore, Position }

fn placement_step(hung: bool, minimized: bool, maximized: bool) -> PlacementStep {
    if hung || minimized { PlacementStep::Wait }
    else if maximized { PlacementStep::Restore }
    else { PlacementStep::Position }
}

fn desktop_consensus(ids: impl IntoIterator<Item = u128>) -> u128 {
    let mut current = 0;
    for id in ids.into_iter().filter(|&id| id != 0) {
        if current != 0 && current != id { return 0; }
        current = id;
    }
    current
}

const SPI_GETWINARRANGING: u32 = 0x0082;
const SPI_SETWINARRANGING: u32 = 0x0083;

// 0 = not prepared, 1 = originally off, 2 = originally on.
static SNAP_ORIGINAL: AtomicU8 = AtomicU8::new(0);

fn read_snap() -> Option<bool> {
    let mut v: i32 = 0;
    unsafe {
        SystemParametersInfoW(SYSTEM_PARAMETERS_INFO_ACTION(SPI_GETWINARRANGING), 0, Some(&mut v as *mut i32 as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)).ok()?;
    }
    Some(v != 0)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SnapWrite { Applied, SetFailed, ReadFailed, Mismatch }

fn write_snap(on: bool) -> SnapWrite {
    if unsafe { SystemParametersInfoW(SYSTEM_PARAMETERS_INFO_ACTION(SPI_SETWINARRANGING), on as u32, Some(on as usize as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)).is_err() } {
        return SnapWrite::SetFailed;
    }
    match read_snap() {
        Some(value) if value == on => SnapWrite::Applied,
        Some(_) => SnapWrite::Mismatch,
        None => SnapWrite::ReadFailed,
    }
}

fn write_snap_logged(on: bool) -> bool {
    match write_snap(on) {
        SnapWrite::Applied => true,
        SnapWrite::SetFailed => { crate::app::log("could not set Windows Snap"); false }
        SnapWrite::ReadFailed => { crate::app::log("could not verify Windows Snap after setting it"); false }
        SnapWrite::Mismatch => { crate::app::log("Windows Snap did not take the requested value"); false }
    }
}

fn marker_path() -> Option<std::path::PathBuf> {
    Some(crate::config::Config::path()?.with_file_name("snap-off.flag"))
}

fn parse_marker(bytes: &[u8]) -> Option<bool> {
    match bytes { b"1" => Some(true), b"0" => Some(false), _ => None }
}

/// Whether Windows' own window snapping (drag to an edge) is currently active.
pub fn snap_status() -> Option<bool> { read_snap() }
pub fn snap_enabled() -> bool { snap_status().unwrap_or(true) }

/// Direct session-only setting; callers managing ownership should use `managed_snap`.
pub fn set_snap(on: bool) -> bool { write_snap_logged(on) }

/// Recover a previous interrupted session and install process-level recovery handlers.
/// Call only after obtaining the single-instance mutex.
pub fn prepare_snap_recovery() -> bool {
    let ready = prepare_snap_recovery_inner();
    if !ready { crate::app::log("could not prepare Windows Snap recovery"); }
    ready
}

fn prepare_snap_recovery_inner() -> bool {
    let Some(path) = marker_path() else { return false };
    if !restore_marker(&path, None, write_snap_logged) { return false; }
    let Some(original) = read_snap() else { return false };
    SNAP_ORIGINAL.store(if original { 2 } else { 1 }, Ordering::SeqCst);
    if unsafe { SetConsoleCtrlHandler(Some(snap_ctrl_handler), true) }.is_err() {
        SNAP_ORIGINAL.store(0, Ordering::SeqCst);
        return false;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_owned_snap();
        previous(info);
    }));
    true
}

unsafe extern "system" fn snap_ctrl_handler(event: u32) -> BOOL {
    if matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT) {
        restore_owned_snap();
    }
    BOOL(0)
}

/// Set Snap while preserving the original value in a recovery marker before disabling it.
pub fn managed_snap(on: bool) -> bool {
    let success = managed_snap_inner(on);
    if !success { crate::app::log("could not update managed Windows Snap state"); }
    success
}

fn managed_snap_inner(on: bool) -> bool {
    let original = match SNAP_ORIGINAL.load(Ordering::SeqCst) { 1 => false, 2 => true, _ => return false };
    if on {
        // A session which started with Snap off must not enable it implicitly.
        return restore_owned_snap();
    }
    let Some(path) = marker_path() else { return false };
    let Some(dir) = path.parent() else { return false };
    if std::fs::create_dir_all(dir).is_err() { return false; }
    let marker = if original { b"1" } else { b"0" };
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            use std::io::Write;
            if file.write_all(marker).is_err() || file.sync_all().is_err() { return false; }
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if std::fs::read(&path).ok().as_deref() != Some(marker.as_slice()) { return false; }
        }
        Err(_) => return false,
    }
    write_snap_logged(false)
}

/// Restore the captured state; retain the marker if Windows rejects the change.
pub fn restore_owned_snap() -> bool {
    let original = match SNAP_ORIGINAL.load(Ordering::SeqCst) { 1 => false, 2 => true, _ => return false };
    let Some(path) = marker_path() else { return false };
    restore_marker(&path, Some(original), |on| write_snap(on) == SnapWrite::Applied)
}

/// Recovery is testable without changing Windows settings. An absent marker never writes Snap.
fn restore_marker(path: &std::path::Path, expected: Option<bool>, set: impl FnOnce(bool) -> bool) -> bool {
    let original = match std::fs::read(path) {
        Ok(bytes) => match parse_marker(&bytes) { Some(value) => value, None => return false },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return true,
        Err(_) => return false,
    };
    if expected.is_some_and(|value| value != original) || !set(original) { return false; }
    match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

/// Explicit user recovery enables Snap regardless of the captured session state.
pub fn restore_snap_explicit() -> bool {
    if !write_snap_logged(true) { return false; }
    let Some(path) = marker_path() else { return false };
    match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

impl Drop for Platform {
    fn drop(&mut self) {
        self.vdm.take();
        if self.com_initialized { unsafe { CoUninitialize(); } }
    }
}

#[cfg(test)]
mod snap_tests {
    use super::{desktop_consensus, parse_marker, placement_step, restore_marker, PlacementStep};

    #[test]
    fn maximized_adoption_restores_before_a_later_normal_placement() {
        assert_eq!(placement_step(true, false, true), PlacementStep::Wait);
        assert_eq!(placement_step(false, true, false), PlacementStep::Wait);
        assert_eq!(placement_step(false, false, true), PlacementStep::Restore);
        assert_eq!(placement_step(false, false, false), PlacementStep::Position);
    }

    #[test]
    fn desktop_identity_requires_consistent_nonzero_evidence() {
        assert_eq!(desktop_consensus([]), 0);
        assert_eq!(desktop_consensus([0, 0]), 0);
        assert_eq!(desktop_consensus([0, 42, 42]), 42);
        assert_eq!(desktop_consensus([42, 99]), 0);
        assert_eq!(desktop_consensus([99, 42, 42]), 0);
    }

    #[test]
    fn recovery_only_changes_snap_for_a_valid_marker_and_retains_failed_recovery() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join(format!("snap-test-{}.flag", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert!(restore_marker(&path, None, |_| panic!("no marker must not change Snap")));
        std::fs::write(&path, "1").unwrap();
        assert!(!restore_marker(&path, None, |value| { assert!(value); false }));
        assert!(path.exists());
        assert!(!restore_marker(&path, Some(false), |_| panic!("ownership mismatch must not change Snap")));
        assert!(restore_marker(&path, None, |value| { assert!(value); true }));
        assert!(!path.exists());
        std::fs::write(&path, "0").unwrap();
        assert!(restore_marker(&path, None, |value| { assert!(!value); true }));
        assert!(!path.exists());
        std::fs::write(&path, "truncated marker").unwrap();
        assert!(!restore_marker(&path, None, |_| panic!("corrupt marker must not change Snap")));
        assert!(path.exists());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn marker_only_accepts_the_stored_original_state() {
        assert_eq!(parse_marker(b"1"), Some(true));
        assert_eq!(parse_marker(b"0"), Some(false));
        assert_eq!(parse_marker(b""), None);
        assert_eq!(parse_marker(b"other"), None);
    }
}

