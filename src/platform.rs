//! Thin Windows layer: inspecting windows, monitors and virtual desktops, moving windows.

use crate::tree::Rect;
use std::ffi::c_void;
use std::cell::Cell;
use std::time::{Duration, Instant};
use std::sync::atomic::{AtomicU8, Ordering};
use windows::Win32::System::Console::{SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT};
use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_APARTMENTTHREADED};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};
use windows::Win32::UI::WindowsAndMessaging::*;

/// Identifies one tiling surface: a virtual desktop on one monitor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SurfaceKey {
    pub desktop: u128,
    pub monitor: isize,
}

/// The window operations used by the tiler. Implementations can simulate window behavior.
pub trait Win {
    fn visible_rect(&self, h: isize) -> Option<Rect>;
    fn is_visible(&self, h: isize, current: u128) -> bool;
    fn is_ghost(&self, _h: isize, _current: u128) -> bool { false }
    fn cursor_position(&self) -> Option<(i32, i32)> { None }
    fn cursor_surface(&self, desktop: u128) -> Option<(SurfaceKey, Rect, i32, i32)>;
    fn foreground_window(&self) -> isize;
    fn resize_intent(&self, _h: isize) -> Option<bool> { None }
    fn dpi(&self, _h: isize) -> u32 { 96 }
    fn move_size_active(&self, _h: isize) -> Option<bool> { None }
    fn is_maximized(&self, h: isize) -> bool;
    fn is_minimized(&self, h: isize) -> bool;
    fn is_fullscreen(&self, h: isize) -> bool;
    fn is_hung(&self, h: isize) -> bool;
    fn place(&self, h: isize, target: Rect) -> bool;
    fn desktop_id(&self, h: isize) -> u128;
    fn all_top_level(&self) -> Vec<isize>;
    fn monitor_of(&self, h: isize) -> (isize, Rect);
    fn is_manageable(&self, h: isize, current: u128) -> bool;
    fn title(&self, h: isize) -> String;
    fn exe_name(&self, h: isize) -> String;
    fn class_name(&self, h: isize) -> String;
    fn current_desktop(&self) -> u128;
    fn invalidate_desktop(&self);
    fn surface_of(&self, h: isize) -> (SurfaceKey, Rect) {
        let (monitor, area) = self.monitor_of(h);
        (SurfaceKey { desktop: self.desktop_id(h), monitor }, area)
    }
}

const ELEVATION_CACHE_TTL: Duration = Duration::from_secs(30);

unsafe fn token_elevated(token: HANDLE) -> bool {
    let mut info = TOKEN_ELEVATION::default();
    let mut len = 0u32;
    unsafe {
        GetTokenInformation(token, TokenElevation, Some(&mut info as *mut TOKEN_ELEVATION as *mut c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32, &mut len).is_ok() && info.TokenIsElevated != 0
    }
}

/// True when this Tilework process runs elevated (administrator). Determined once from the own token.
pub fn self_elevated() -> bool {
    static ELEVATED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ELEVATED.get_or_init(|| unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() { return false; }
        let elevated = token_elevated(token);
        let _ = CloseHandle(token);
        elevated
    })
}

pub struct Platform {
    desktop_cache: Cell<Option<(Instant, u128)>>,
    vdm: Option<IVirtualDesktopManager>,
    com_initialized: bool,
    elevated_pids: std::cell::RefCell<std::collections::HashMap<u32, (bool, Instant)>>,
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
            Self { vdm, com_initialized, desktop_cache: Cell::new(None), elevated_pids: Default::default() }
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

    pub fn is_visible(&self, h: isize, current: u128) -> bool {
        let cloak = self.cloaked(h);
        tracked_visibility(unsafe { IsWindowVisible(hwnd_of(h)).as_bool() }, cloak,
            if cloak == 2 { self.raw_desktop_id(h) } else { 0 }, current)
    }

    /// True when the window belongs to an elevated process this Tilework instance cannot move.
    /// A standard-user Tilework is blocked by UIPI from `SetWindowPos` on elevated windows, which
    /// would otherwise be retried forever and leave a hole in the layout. An elevated Tilework can
    /// move them, so nothing is skipped. Results are cached per pid for a short time because pids
    /// are reused.
    fn is_elevated_window(&self, h: isize) -> bool {
        if self_elevated() { return false; }
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd_of(h), Some(&mut pid)); }
        if pid == 0 { return false; }
        if let Some((known, at)) = self.elevated_pids.borrow().get(&pid)
            && at.elapsed() < ELEVATION_CACHE_TTL { return *known; }
        let elevated = unsafe {
            match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                Err(_) => false,
                Ok(process) => {
                    let mut token = HANDLE::default();
                    let result = if OpenProcessToken(process, TOKEN_QUERY, &mut token).is_err() {
                        true // a token we cannot even query belongs to a higher integrity level
                    } else {
                        let elevated = token_elevated(token);
                        let _ = CloseHandle(token);
                        elevated
                    };
                    let _ = CloseHandle(process);
                    result
                }
            }
        };
        let mut cache = self.elevated_pids.borrow_mut();
        if cache.len() > 512 { cache.clear(); }
        let changed = cache.insert(pid, (elevated, Instant::now())).is_none_or(|(old, _)| old != elevated);
        if elevated && changed { crate::app::log(&format!("ignoring elevated window {h:x} (pid {pid}): a standard-user Tilework cannot move it; start Tilework as administrator to include it")); }
        elevated
    }

    pub fn is_manageable(&self, h: isize, current_desktop: u128) -> bool {
        unsafe {
            let hw = hwnd_of(h);
            if !IsWindow(Some(hw)).as_bool() || !IsWindowVisible(hw).as_bool() && self.cloaked(h) != 2 {
                return false;
            }
            if self.is_elevated_window(h) {
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
            if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "TileworkOverlay" | "TileworkMain") {
                return false;
            }
            let mut r = RECT::default();
            !(GetWindowRect(hw, &mut r).is_err() || r.right - r.left < 80 || r.bottom - r.top < 40)
        }
    }

    pub fn all_top_level(&self) -> Vec<isize> {
        unsafe extern "system" fn cb(h: HWND, lp: LPARAM) -> BOOL {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let v = unsafe { &mut *(lp.0 as *mut Vec<isize>) };
                v.push(h.0 as isize);
                BOOL(1)
            })).unwrap_or_else(|_| { crate::app::callback_failed(); BOOL(0) })
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
            0 => self.desktop_cache.get().map(|(_, id)| id).unwrap_or_else(|| self.current_desktop()),
            id => id,
        }
    }

    /// Invalidate cached desktop evidence after a foreground or desktop-switch event.
    pub fn invalidate_desktop(&self) { self.desktop_cache.set(None); }

    pub fn current_desktop(&self) -> u128 {
        if let Some((time, id)) = self.desktop_cache.get() && time.elapsed() < Duration::from_millis(250) {
            return id;
        }
        let Some(vdm) = &self.vdm else { return 0 };
        let mut evidence = Vec::new();
        let foreground = id_of(unsafe { GetForegroundWindow() });
        for h in self.all_top_level() {
            unsafe {
                let hw = hwnd_of(h);
                let mut cloak = 0u32;
                if self.is_manageable(h, 0) && IsWindowVisible(hw).as_bool() && !IsIconic(hw).as_bool()
                    && DwmGetWindowAttribute(hw, DWMWA_CLOAKED, &mut cloak as *mut u32 as *mut c_void, 4).is_ok() && cloak == 0
                    && vdm.IsWindowOnCurrentVirtualDesktop(hw).map(|v| v.as_bool()).unwrap_or(false) {
                    let id = self.raw_desktop_id(h);
                    if id != 0 {
                        evidence.push((h, id));
                    }
                }
            }
        }
        let result = identify_desktop(foreground, &evidence, || explorer_current_desktop().unwrap_or(0));
        self.desktop_cache.set(Some((Instant::now(), result)));
        result
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
        // Full-screen coverage requires a borderless style. A normal maximized
        // window can cover a taskbar-free monitor and still needs adoption.
        let style = unsafe { GetWindowLongW(hwnd_of(h), GWL_STYLE) } as u32;
        if !fullscreen_style(unsafe { IsZoomed(hwnd_of(h)).as_bool() }, style) { return false; }
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
            ).map_err(|e| crate::app::log(&format!("SetWindowPos {h:x} failed: {e}"))).is_ok()
        }
    }
}

impl Win for Platform {
    fn visible_rect(&self, h: isize) -> Option<Rect> { Platform::visible_rect(self, h) }
    fn is_visible(&self, h: isize, current: u128) -> bool { Platform::is_visible(self, h, current) }
    fn is_ghost(&self, h: isize, current: u128) -> bool {
        if self.cloaked(h) != 2 { return false; }
        let desktop = self.raw_desktop_id(h);
        desktop == 0 || desktop == current
    }
    fn cursor_position(&self) -> Option<(i32, i32)> {
        let mut pt = windows::Win32::Foundation::POINT::default();
        unsafe { GetCursorPos(&mut pt).ok()?; }
        Some((pt.x, pt.y))
    }
    fn cursor_surface(&self, desktop: u128) -> Option<(SurfaceKey, Rect, i32, i32)> {
        let (x, y) = self.cursor_position()?;
        unsafe {
            let monitor = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if !GetMonitorInfoW(monitor, &mut info).as_bool() { return None; }
            let r = info.rcWork;
            let area = Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top);
            Some((SurfaceKey { desktop, monitor: monitor.0 as isize }, area, x, y))
        }
    }
    fn foreground_window(&self) -> isize {
        id_of(unsafe { GetForegroundWindow() })
    }
    fn resize_intent(&self, h: isize) -> Option<bool> {
        use windows::Win32::Foundation::WPARAM;
        let (x, y) = self.cursor_position()?;
        let packed = ((x as u16 as u32) | ((y as u16 as u32) << 16)) as i32 as isize;
        let mut result = 0usize;
        let sent = unsafe { SendMessageTimeoutW(hwnd_of(h), WM_NCHITTEST, WPARAM(0), LPARAM(packed), SMTO_ABORTIFHUNG, 100, Some(&mut result)) };
        if sent.0 == 0 { return None; }
        match result as u32 {
            HTCAPTION => Some(false),
            HTLEFT..=HTBOTTOMRIGHT => Some(true),
            _ => None,
        }
    }
    fn dpi(&self, h: isize) -> u32 {
        unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd_of(h)) }
    }
    fn move_size_active(&self, h: isize) -> Option<bool> {
        let thread = unsafe { GetWindowThreadProcessId(hwnd_of(h), None) };
        if thread == 0 { return None; }
        let mut info = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        unsafe { GetGUIThreadInfo(thread, &mut info).ok()?; }
        Some(info.flags.0 & GUI_INMOVESIZE.0 != 0)
    }
    fn is_maximized(&self, h: isize) -> bool { Platform::is_maximized(self, h) }
    fn is_minimized(&self, h: isize) -> bool { Platform::is_minimized(self, h) }
    fn is_fullscreen(&self, h: isize) -> bool { Platform::is_fullscreen(self, h) }
    fn is_hung(&self, h: isize) -> bool { Platform::is_hung(self, h) }
    fn place(&self, h: isize, target: Rect) -> bool { Platform::place(self, h, target) }
    fn desktop_id(&self, h: isize) -> u128 { Platform::desktop_id(self, h) }
    fn all_top_level(&self) -> Vec<isize> { Platform::all_top_level(self) }
    fn monitor_of(&self, h: isize) -> (isize, Rect) { Platform::monitor_of(self, h) }
    fn is_manageable(&self, h: isize, current: u128) -> bool { Platform::is_manageable(self, h, current) }
    fn title(&self, h: isize) -> String { Platform::title(self, h) }
    fn exe_name(&self, h: isize) -> String { Platform::exe_name(self, h) }
    fn class_name(&self, h: isize) -> String { Platform::class_name(self, h) }
    fn current_desktop(&self) -> u128 { Platform::current_desktop(self) }
    fn invalidate_desktop(&self) { Platform::invalidate_desktop(self); }
}

#[derive(Debug, PartialEq, Eq)]
enum PlacementStep { Wait, Restore, Position }

fn placement_step(hung: bool, minimized: bool, maximized: bool) -> PlacementStep {
    if hung || minimized { PlacementStep::Wait }
    else if maximized { PlacementStep::Restore }
    else { PlacementStep::Position }
}

/// Explorer exposes the active desktop here even when it contains no windows. This
/// is an undocumented shell detail, so unavailable or malformed data is ignored.
fn explorer_current_desktop() -> Option<u128> {
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_BINARY};
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::GetCurrentProcessId;
    let mut session = 0;
    let session_ok = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session).is_ok() };
    let base = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer";
    let mut paths = Vec::new();
    if session_ok { paths.push(format!(r"{base}\SessionInfo\{session}\VirtualDesktops")); }
    paths.push(format!(r"{base}\VirtualDesktops"));
    for path in paths {
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let mut guid = windows::core::GUID::zeroed();
        let mut len = std::mem::size_of_val(&guid) as u32;
        let status = unsafe { RegGetValueW(HKEY_CURRENT_USER, windows::core::PCWSTR(wide.as_ptr()), windows::core::w!("CurrentVirtualDesktop"), RRF_RT_REG_BINARY, None, Some(&mut guid as *mut _ as *mut c_void), Some(&mut len)) };
        if status.is_ok() && len as usize == std::mem::size_of_val(&guid) && guid.to_u128() != 0 {
            return Some(guid.to_u128());
        }
    }
    None
}

fn tracked_visibility(visible: bool, cloak: u32, desktop: u128, current: u128) -> bool {
    if cloak == 2 {
        // Unknown identity must not evict a possibly valid window on another desktop.
        desktop == 0 || current == 0 || desktop != current
    } else {
        visible
    }
}

fn fullscreen_style(maximized: bool, style: u32) -> bool {
    !maximized && style & WS_CAPTION.0 != WS_CAPTION.0 && style & WS_THICKFRAME.0 == 0
}

fn identify_desktop(foreground: isize, evidence: &[(isize, u128)], empty: impl FnOnce() -> u128) -> u128 {
    let mut counts = std::collections::HashMap::new();
    for &(_, id) in evidence.iter().filter(|(_, id)| *id != 0) {
        *counts.entry(id).or_insert(0usize) += 1;
    }
    let highest = counts.values().copied().max().unwrap_or(0);
    if highest < 2 {
        let shell = empty();
        if shell != 0 || counts.len() != 1 { return shell; }
        // Only the foreground window may break the tie with unavailable shell evidence.
        return evidence.iter().find_map(|&(h, id)| (h == foreground && id != 0).then_some(id)).unwrap_or(0);
    }
    let winners: Vec<_> = counts.into_iter().filter_map(|(id, n)| (n == highest).then_some(id)).collect();
    if winners.len() == 1 { return winners[0]; }
    if let Some((_, id)) = evidence.iter().find(|(h, id)| *h == foreground && winners.contains(id)) { return *id; }
    empty()
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
    std::panic::catch_unwind(|| {
        if matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT) {
            restore_owned_snap();
        }
    }).unwrap_or_else(|_| crate::app::callback_failed());
    BOOL(0)
}

/// Set Snap while preserving the original value in a recovery marker before disabling it.
pub fn managed_snap(on: bool) -> bool {
    let success = managed_snap_inner(on);
    if !success { crate::app::log("could not update managed Windows Snap state"); }
    success
}

fn managed_snap_inner(on: bool) -> bool {
    if SNAP_ORIGINAL.load(Ordering::SeqCst) == 0 { return false; }
    if on {
        // A session which started with Snap off must not enable it implicitly.
        return restore_owned_snap();
    }
    let Some(path) = marker_path() else { return false };
    let Ok(_guard) = crate::config::guard_parent(&path) else { return false };
    let marker_exists = match read_marker_file(&path) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => return false,
    };
    let Some(original) = ownership_original(marker_exists, SNAP_ORIGINAL.load(Ordering::SeqCst), read_snap) else { return false };
    SNAP_ORIGINAL.store(if original { 2 } else { 1 }, Ordering::SeqCst);
    let marker = if original { b"1" } else { b"0" };
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(windows)]
    { use std::os::windows::fs::OpenOptionsExt; options.custom_flags(0x0020_0000); }
    match options.open(&path) {
        Ok(mut file) => {
            use std::io::Write;
            if file.write_all(marker).is_err() || file.sync_all().is_err() { return false; }
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if read_marker_file(&path).ok().as_deref() != Some(marker.as_slice()) { return false; }
        }
        Err(_) => return false,
    }
    write_snap_logged(false)
}

// A current value is captured only when acquiring a new ownership marker.
fn ownership_original(marker_exists: bool, stored: u8, read: impl FnOnce() -> Option<bool>) -> Option<bool> {
    if marker_exists { match stored { 1 => Some(false), 2 => Some(true), _ => None } }
    else { read() }
}

fn read_marker_file(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    { use std::os::windows::fs::OpenOptionsExt; options.custom_flags(0x0020_0000); }
    let mut file = options.open(path)?;
    let metadata = file.metadata()?;
    #[cfg(windows)]
    let reparse = { use std::os::windows::fs::MetadataExt; metadata.file_attributes() & 0x400 != 0 };
    #[cfg(not(windows))]
    let reparse = metadata.file_type().is_symlink();
    if reparse { return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "reparse marker")); }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Restore the captured state; retain the marker if Windows rejects the change.
pub fn restore_owned_snap() -> bool {
    let original = match SNAP_ORIGINAL.load(Ordering::SeqCst) { 1 => false, 2 => true, _ => return false };
    let Some(path) = marker_path() else { return false };
    restore_marker(&path, Some(original), |on| write_snap(on) == SnapWrite::Applied)
}

/// Recovery is testable without changing Windows settings. An absent marker never writes Snap.
fn restore_marker(path: &std::path::Path, expected: Option<bool>, set: impl FnOnce(bool) -> bool) -> bool {
    let Ok(_guard) = crate::config::guard_parent(path) else { return false };
    let original = match read_marker_file(path) {
        Ok(bytes) => match parse_marker(&bytes) { Some(value) => value, None => return false },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return true,
        Err(_) => return false,
    };
    if expected.is_some_and(|value| value != original) || !set(original) || crate::config::safe_path(path).is_err() { return false; }
    match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

/// Explicit user recovery enables Snap regardless of the captured session state.
pub fn restore_snap_explicit() -> bool {
    let Some(path) = marker_path() else { return false };
    let Ok(_guard) = crate::config::guard_parent(&path) else { return false };
    match read_marker_file(&path) {
        Ok(_) => {},
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
        Err(_) => return false,
    }
    if !write_snap_logged(true) || crate::config::safe_path(&path).is_err() { return false; }
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
    use super::{ownership_original, parse_marker, restore_marker};

    #[test]
    fn ownership_recaptures_a_user_change_after_releasing_snap() {
        let first = ownership_original(false, 2, || Some(true)).unwrap();
        assert!(first);
        let while_owned = ownership_original(true, 2, || panic!("owned state must not be recaptured"));
        assert_eq!(while_owned, Some(true));
        let after_release = ownership_original(false, 2, || Some(false));
        assert_eq!(after_release, Some(false));
    }

    #[test]
    fn recovery_only_changes_snap_for_a_valid_marker_and_retains_failed_recovery() {
        let dir = std::env::temp_dir().join(format!("tilework-snap-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("snap.flag");
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
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn marker_only_accepts_the_stored_original_state() {
        assert_eq!(parse_marker(b"1"), Some(true));
        assert_eq!(parse_marker(b"0"), Some(false));
        assert_eq!(parse_marker(b""), None);
        assert_eq!(parse_marker(b"other"), None);
    }
    #[test]
    fn desktop_evidence_uses_agreement_before_foreground_and_registry_fallback() {
        use super::identify_desktop;
        let mut windows = vec![(1, 99), (2, 42), (3, 42)]; // Window 1 is pinned from desktop 99.
        assert_eq!(identify_desktop(0, &windows, || panic!("visible evidence exists")), 42);
        assert_eq!(identify_desktop(1, &windows, || 99), 42);
        windows[1].1 = 7; windows[2].1 = 7; // Switch desktop; the pinned window stays.
        assert_eq!(identify_desktop(2, &windows, || 0), 7);
        windows[2].1 = 99;
        assert_eq!(identify_desktop(2, &windows, || 0), 99);
        assert_eq!(identify_desktop(0, &[(1, 99)], || 42), 42);
        assert_eq!(identify_desktop(1, &[(1, 99)], || 42), 42);
        assert_eq!(identify_desktop(1, &[(1, 99)], || 0), 99);
        assert_eq!(identify_desktop(0, &[(1, 99)], || 0), 0);
        assert_eq!(identify_desktop(1, &[(1, 99), (2, 42)], || 0), 0);
        assert_eq!(identify_desktop(3, &[(1, 42), (2, 42), (3, 99), (4, 99)], || 7), 99);
        assert_eq!(identify_desktop(0, &[(1, 42), (2, 42), (3, 99), (4, 99)], || 7), 7);
        windows.clear(); // The empty desktop is resolved through shell evidence.
        assert_eq!(identify_desktop(0, &windows, || 7), 7);
    }

    #[test]
    fn tracked_visibility_preserves_other_desktop_windows() {
        use super::tracked_visibility;
        assert!(!tracked_visibility(false, 0, 0, 42));
        assert!(tracked_visibility(true, 0, 0, 42));
        assert!(tracked_visibility(false, 2, 99, 42));
        assert!(!tracked_visibility(true, 2, 42, 42));
    }

    #[test]
    fn maximized_or_resizable_borderless_windows_are_not_fullscreen() {
        use super::{fullscreen_style, WS_CAPTION, WS_THICKFRAME};
        assert!(!fullscreen_style(true, 0));
        assert!(!fullscreen_style(false, WS_THICKFRAME.0));
        assert!(!fullscreen_style(false, WS_CAPTION.0));
        assert!(fullscreen_style(false, 0));
    }

}

