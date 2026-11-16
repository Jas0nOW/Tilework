//! Thin Windows layer: inspecting windows, monitors and virtual desktops, moving windows.

use crate::tree::Rect;
use std::ffi::c_void;
use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
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
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let vdm = CoCreateInstance::<_, IVirtualDesktopManager>(&VirtualDesktopManager, None, CLSCTX_ALL).ok();
            Self { vdm }
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
    pub fn is_manageable(&self, h: isize) -> bool {
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
            if GetWindowRect(hw, &mut r).is_err() || r.right - r.left < 80 || r.bottom - r.top < 40 {
                return false;
            }
            true
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

    /// The desktop that is currently shown, taken from any visible (not cloaked) window.
    pub fn current_desktop(&self) -> u128 {
        for h in self.all_top_level() {
            unsafe {
                let hw = hwnd_of(h);
                if IsWindowVisible(hw).as_bool() && !IsIconic(hw).as_bool() && self.cloaked(h) == 0 {
                    let id = self.raw_desktop_id(h);
                    if id != 0 {
                        return id;
                    }
                }
            }
        }
        0
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

    /// Moves a window so that its *visible* edges match `target`.
    pub fn place(&self, h: isize, target: Rect) {
        unsafe {
            let hw = hwnd_of(h);
            if IsZoomed(hw).as_bool() {
                let _ = ShowWindow(hw, SW_RESTORE);
            }
            let mut outer = RECT::default();
            if GetWindowRect(hw, &mut outer).is_err() {
                return;
            }
            let vis = self.visible_rect(h).unwrap_or(Rect::new(outer.left, outer.top, outer.right - outer.left, outer.bottom - outer.top));
            let (il, it) = (vis.x - outer.left, vis.y - outer.top);
            let (ir, ib) = (outer.right - vis.right(), outer.bottom - vis.bottom());
            let _ = SetWindowPos(
                hw,
                None,
                target.x - il,
                target.y - it,
                target.w + il + ir,
                target.h + it + ib,
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
        }
    }
}
