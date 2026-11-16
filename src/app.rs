//! The tiling manager: reacts to window events, keeps one tree per surface (virtual desktop x
//! monitor), applies the layout and handles drag & drop with a live preview.

use crate::platform::{hwnd_of, id_of, Platform, SurfaceKey};
use crate::tree::{zone_at, DropZone, Edge, Hwnd, Rect, Tree};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::io::Write;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
use windows::Win32::Graphics::Gdi::{CreateBitmap, CreateSolidBrush, DeleteObject, GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, MOD_NOREPEAT, MOD_WIN};
use windows::Win32::UI::Shell::{Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW};
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_EVENT: u32 = WM_APP + 1;
const WM_TRAY: u32 = WM_APP + 2;
const TIMER_RECONCILE: usize = 1;
const TIMER_DRAG: usize = 2;
const TIMER_POLL: usize = 3;
const HOTKEY_TOGGLE: i32 = 1;
const MENU_TOGGLE: usize = 1;
const MENU_EXIT: usize = 2;
const ACCENT: u32 = 0x00FF823C; // BGR: a clear blue
const DWMWA_BORDER_COLOR: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(34);
const COLOR_DEFAULT: u32 = 0xFFFF_FFFF;

struct Surface {
    tree: Tree,
    area: Rect,
}

struct Drag {
    hwnd: Hwnd,
    key: SurfaceKey,
    w0: i32,
    h0: i32,
    resize: bool,
    target: Option<(SurfaceKey, Hwnd, DropZone)>,
}

pub struct App {
    p: Platform,
    enabled: bool,
    surfaces: HashMap<SurfaceKey, Surface>,
    focused: Option<Hwnd>,
    bordered: Option<Hwnd>,
    drag: Option<Drag>,
    overlay: HWND,
    main: HWND,
    filter: Option<String>,
    icon_on: HICON,
    icon_off: HICON,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

pub fn log(msg: &str) {
    let dir = std::env::var("LOCALAPPDATA").map(|d| format!("{d}\\CosmicTile")).unwrap_or_else(|_| ".".into());
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(format!("{dir}\\cosmictile.log")) {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = writeln!(f, "[{}:{:02}:{:02}] {msg}", (t / 3600) % 24, (t / 60) % 60, t % 60);
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn make_icon(on: bool) -> HICON {
    const N: usize = 32;
    let (r, g, b) = if on { (46u8, 204u8, 113u8) } else { (130u8, 130u8, 130u8) };
    let mut px = vec![0u32; N * N];
    for y in 0..N {
        for x in 0..N {
            let frame = x < 3 || x >= N - 3 || y < 3 || y >= N - 3;
            let vline = (15..17).contains(&x) && y < N / 2 + 1;
            let hline = (N / 2 - 1..N / 2 + 1).contains(&y) && x >= 15;
            if frame || vline || hline {
                px[y * N + x] = 0xFF00_0000 | (r as u32) << 16 | (g as u32) << 8 | b as u32;
            }
        }
    }
    unsafe {
        let color = CreateBitmap(N as i32, N as i32, 1, 32, Some(px.as_ptr() as *const c_void));
        let mask = CreateBitmap(N as i32, N as i32, 1, 1, None);
        let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info).unwrap_or_default();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}

impl App {
    fn managed(&self, h: Hwnd) -> bool {
        if !self.p.is_manageable(h) {
            return false;
        }
        match &self.filter {
            Some(f) => self.p.title(h).contains(f.as_str()) || self.p.exe_name(h).eq_ignore_ascii_case(f),
            None => true,
        }
    }

    fn surface_containing(&self, h: Hwnd) -> Option<SurfaceKey> {
        self.surfaces.iter().find(|(_, s)| s.tree.contains(h)).map(|(k, _)| *k)
    }

    /// Brings the trees in line with the windows that really exist, then lays out what changed.
    fn reconcile(&mut self, force_all: bool) {
        if !self.enabled {
            return;
        }
        let current: HashMap<Hwnd, (SurfaceKey, Rect)> =
            self.p.all_top_level().into_iter().rev().filter(|&h| self.managed(h)).map(|h| (h, self.p.surface_of(h))).collect();
        let mut dirty: HashSet<SurfaceKey> = HashSet::new();
        if force_all {
            dirty.extend(self.surfaces.keys().copied());
        }

        // Windows that vanished, were minimised or moved to another desktop/monitor.
        for (key, s) in self.surfaces.iter_mut() {
            s.area = current.values().find(|(k, _)| k == key).map(|(_, a)| *a).unwrap_or(s.area);
            for h in s.tree.leaves() {
                if current.get(&h).map(|(k, _)| k) != Some(key) {
                    s.tree.remove(h);
                    dirty.insert(*key);
                    log(&format!("removed {h:x}"));
                }
            }
        }
        // New windows, in z-order from back to front.
        let order: Vec<Hwnd> = self.p.all_top_level().into_iter().rev().filter(|h| current.contains_key(h)).collect();
        for h in order {
            let (key, area) = current[&h];
            if self.surface_containing(h).is_some() {
                continue;
            }
            let focused = self.focused;
            let s = self.surfaces.entry(key).or_insert_with(|| Surface { tree: Tree::new(), area });
            s.area = area;
            s.tree.add(h, area, focused);
            dirty.insert(key);
            log(&format!("added {h:x} {} ({})", self.p.title(h), self.p.exe_name(h)));
        }
        self.surfaces.retain(|_, s| !s.tree.is_empty());
        for key in dirty {
            self.apply(key);
        }
    }

    fn apply(&self, key: SurfaceKey) {
        let Some(s) = self.surfaces.get(&key) else { return };
        for (h, r) in s.tree.layout(s.area) {
            let same = self.p.visible_rect(h).map(|v| (v.x - r.x).abs() <= 1 && (v.y - r.y).abs() <= 1 && (v.w - r.w).abs() <= 1 && (v.h - r.h).abs() <= 1).unwrap_or(false);
            if !same {
                self.p.place(h, r);
            }
        }
    }

    fn set_border(&mut self, h: Hwnd, color: u32) {
        unsafe {
            let c = COLORREF(color);
            let _ = DwmSetWindowAttribute(hwnd_of(h), DWMWA_BORDER_COLOR, &c as *const COLORREF as *const c_void, 4);
        }
    }

    fn on_focus(&mut self, h: Hwnd) {
        if !self.enabled || self.surface_containing(h).is_none() {
            return;
        }
        self.focused = Some(h);
        if let Some(prev) = self.bordered.take() {
            if prev != h {
                self.set_border(prev, COLOR_DEFAULT);
            }
        }
        self.set_border(h, ACCENT);
        self.bordered = Some(h);
    }

    fn toggle(&mut self) {
        self.enabled = !self.enabled;
        log(&format!("tiling {}", if self.enabled { "ON" } else { "OFF" }));
        self.drag = None;
        self.hide_overlay();
        if let Some(b) = self.bordered.take() {
            self.set_border(b, COLOR_DEFAULT);
        }
        if self.enabled {
            self.surfaces.clear();
            self.reconcile(true);
        }
        self.update_tray();
    }

    // ---------- drag & drop ----------

    fn drag_start(&mut self, h: Hwnd) {
        if !self.enabled {
            return;
        }
        let Some(key) = self.surface_containing(h) else { return };
        let Some(v) = self.p.visible_rect(h) else { return };
        log(&format!("drag start {h:x}"));
        self.drag = Some(Drag { hwnd: h, key, w0: v.w, h0: v.h, resize: false, target: None });
        unsafe {
            SetTimer(Some(self.main), TIMER_DRAG, 30, None);
        }
    }

    fn cursor_surface(&self, desktop: u128) -> Option<(SurfaceKey, Rect, i32, i32)> {
        unsafe {
            let mut pt = POINT::default();
            GetCursorPos(&mut pt).ok()?;
            let m = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(m, &mut mi);
            let w = mi.rcWork;
            Some((SurfaceKey { desktop, monitor: m.0 as isize }, Rect::new(w.left, w.top, w.right - w.left, w.bottom - w.top), pt.x, pt.y))
        }
    }

    fn drag_tick(&mut self) {
        let Some(d) = &self.drag else { return };
        let (h, w0, h0, home) = (d.hwnd, d.w0, d.h0, d.key);
        if d.resize {
            return;
        }
        if let Some(v) = self.p.visible_rect(h) {
            if (v.w - w0).abs() > 4 || (v.h - h0).abs() > 4 {
                if let Some(d) = &mut self.drag {
                    d.resize = true;
                }
                self.hide_overlay();
                return;
            }
        }
        let Some((key, area, cx, cy)) = self.cursor_surface(home.desktop) else { return };
        let mut target = None;
        let mut preview = None;
        match self.surfaces.get(&key) {
            Some(s) => {
                let hit = s.tree.layout(area).into_iter().find(|(id, r)| *id != h && r.contains(cx, cy));
                if let Some((t, r)) = hit {
                    let zone = zone_at(r, cx, cy);
                    preview = if key == home { s.tree.preview(area, h, t, zone) } else { s.tree.preview_insert(area, h, t, zone) };
                    target = Some((key, t, zone));
                }
            }
            None => {
                // empty monitor: the window would fill it
                preview = Some(area);
            }
        }
        if let Some(d) = &mut self.drag {
            d.target = target;
        }
        match preview {
            Some(r) => self.show_overlay(r),
            None => self.hide_overlay(),
        }
    }

    fn drag_end(&mut self, h: Hwnd) {
        let Some(d) = self.drag.take() else { return };
        unsafe {
            let _ = KillTimer(Some(self.main), TIMER_DRAG);
        }
        self.hide_overlay();
        if d.hwnd != h {
            return;
        }
        let home = d.key;
        if d.resize {
            if let (Some(v), Some(s)) = (self.p.visible_rect(h), self.surfaces.get_mut(&home)) {
                if let Some(old) = s.tree.rect_of(s.area, h) {
                    let area = s.area;
                    let tol = 3;
                    if (v.x - old.x).abs() > tol { s.tree.set_edge(area, h, Edge::Left, v.x); }
                    if (v.right() - old.right()).abs() > tol { s.tree.set_edge(area, h, Edge::Right, v.right()); }
                    if (v.y - old.y).abs() > tol { s.tree.set_edge(area, h, Edge::Top, v.y); }
                    if (v.bottom() - old.bottom()).abs() > tol { s.tree.set_edge(area, h, Edge::Bottom, v.bottom()); }
                }
            }
            log(&format!("resize end {h:x}"));
            self.apply(home);
            return;
        }
        match d.target {
            Some((key, t, zone)) if key == home => {
                if let Some(s) = self.surfaces.get_mut(&home) {
                    s.tree.move_to(h, t, zone);
                }
                log(&format!("drop {h:x} -> {t:x} {zone:?}"));
                self.apply(home);
            }
            Some((key, t, zone)) => {
                if let Some(s) = self.surfaces.get_mut(&home) {
                    s.tree.remove(h);
                }
                if let Some(s) = self.surfaces.get_mut(&key) {
                    s.tree.insert(h, t, zone);
                }
                log(&format!("drop {h:x} on other monitor -> {t:x} {zone:?}"));
                self.surfaces.retain(|_, s| !s.tree.is_empty());
                self.apply(home);
                self.apply(key);
            }
            None => self.apply(home), // snap back
        }
    }

    // ---------- overlay & tray ----------

    fn show_overlay(&self, r: Rect) {
        unsafe {
            let _ = SetWindowPos(self.overlay, Some(HWND_TOPMOST), r.x, r.y, r.w, r.h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
    }

    fn hide_overlay(&self) {
        unsafe {
            let _ = ShowWindow(self.overlay, SW_HIDE);
        }
    }

    fn tray_data(&self) -> NOTIFYICONDATAW {
        let mut nid = NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: self.main, uID: 1, uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP, uCallbackMessage: WM_TRAY, ..Default::default() };
        nid.hIcon = if self.enabled { self.icon_on } else { self.icon_off };
        let tip = if self.enabled { "CosmicTile: ON (click to turn off)" } else { "CosmicTile: OFF (click to turn on)" };
        for (i, c) in tip.encode_utf16().take(127).enumerate() {
            nid.szTip[i] = c;
        }
        nid
    }

    fn update_tray(&self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &self.tray_data());
        }
    }

    fn show_menu(&mut self) {
        unsafe {
            let menu = CreatePopupMenu().unwrap();
            let label = if self.enabled { w!("Turn tiling off") } else { w!("Turn tiling on") };
            let _ = AppendMenuW(menu, MF_STRING, MENU_TOGGLE, label);
            let _ = AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("Exit CosmicTile"));
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = SetForegroundWindow(self.main);
            let cmd = TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_RETURNCMD, pt.x, pt.y, Some(0), self.main, None).0 as usize;
            let _ = DestroyMenu(menu);
            match cmd {
                MENU_TOGGLE => self.toggle(),
                MENU_EXIT => {
                    self.shutdown();
                    PostQuitMessage(0);
                }
                _ => {}
            }
        }
    }

    fn shutdown(&mut self) {
        if let Some(b) = self.bordered.take() {
            self.set_border(b, COLOR_DEFAULT);
        }
        unsafe {
            let nid = self.tray_data();
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
        }
        log("exit");
    }
}

unsafe extern "system" fn win_event_proc(_h: HWINEVENTHOOK, event: u32, hwnd: HWND, id_object: i32, id_child: i32, _thread: u32, _time: u32) {
    if id_object != 0 || id_child != 0 || hwnd.0.is_null() {
        return;
    }
    let main = APP.with(|a| a.try_borrow().ok().and_then(|g| g.as_ref().map(|app| app.main)));
    if let Some(main) = main {
        unsafe {
            let _ = PostMessageW(Some(main), WM_EVENT, WPARAM(event as usize), LPARAM(hwnd.0 as isize));
        }
    }
}

unsafe extern "system" fn overlay_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let handled = APP.with(|a| {
        let Ok(mut guard) = a.try_borrow_mut() else { return false };
        let Some(app) = guard.as_mut() else { return false };
        match msg {
            WM_EVENT => {
                let h = lp.0 as isize;
                match wp.0 as u32 {
                    0x0003 => app.on_focus(h),
                    0x000A => app.drag_start(h),
                    0x000B => app.drag_end(h),
                    _ => unsafe {
                        SetTimer(Some(app.main), TIMER_RECONCILE, 80, None);
                    },
                }
            }
            WM_TIMER => match wp.0 {
                TIMER_RECONCILE => {
                    unsafe {
                        let _ = KillTimer(Some(app.main), TIMER_RECONCILE);
                    }
                    if app.drag.is_none() {
                        app.reconcile(false);
                    }
                }
                TIMER_DRAG => app.drag_tick(),
                TIMER_POLL => {
                    if app.drag.is_none() {
                        app.reconcile(false);
                    }
                }
                _ => {}
            },
            WM_HOTKEY => app.toggle(),
            WM_TRAY => match lp.0 as u32 {
                WM_LBUTTONUP => app.toggle(),
                WM_RBUTTONUP => app.show_menu(),
                _ => {}
            },
            _ => return false,
        }
        true
    });
    if handled {
        LRESULT(0)
    } else {
        unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
    }
}

pub fn run(filter: Option<String>) {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap();
        let class_main = w!("CosmicTileMain");
        let class_overlay = w!("CosmicTileOverlay");
        let accent_brush = CreateSolidBrush(COLORREF(ACCENT));
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(main_proc), hInstance: hinst.into(), lpszClassName: class_main, ..Default::default() });
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(overlay_proc), hInstance: hinst.into(), lpszClassName: class_overlay, hbrBackground: accent_brush, ..Default::default() });
        let main = CreateWindowExW(WINDOW_EX_STYLE(0), class_main, w!("CosmicTile"), WS_OVERLAPPED, 0, 0, 0, 0, None, None, Some(hinst.into()), None).unwrap();
        let overlay = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class_overlay,
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            10,
            10,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .unwrap();
        let _ = SetLayeredWindowAttributes(overlay, COLORREF(0), 110, LWA_ALPHA);

        let platform = Platform::new();
        let app = App {
            p: platform,
            enabled: true,
            surfaces: HashMap::new(),
            focused: None,
            bordered: None,
            drag: None,
            overlay,
            main,
            filter,
            icon_on: make_icon(true),
            icon_off: make_icon(false),
        };
        let nid = app.tray_data();
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
        APP.with(|a| *a.borrow_mut() = Some(app));

        // foreground, move/size start+end, minimise start+end, destroy, show, cloak, uncloak
        for ev in [0x0003u32, 0x000A, 0x000B, 0x0016, 0x0017, 0x8001, 0x8002, 0x8017, 0x8018] {
            SetWinEventHook(ev, ev, None, Some(win_event_proc), 0, 0, 0);
        }
        if RegisterHotKey(Some(main), HOTKEY_TOGGLE, MOD_WIN | MOD_NOREPEAT, 'Y' as u32).is_err() {
            log("could not register Win+Y");
        }
        SetTimer(Some(main), TIMER_POLL, 1500, None);
        log("started");
        APP.with(|a| {
            if let Some(app) = a.borrow_mut().as_mut() {
                app.reconcile(true);
            }
        });

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = id_of(main);
    }
}
