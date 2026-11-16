//! The tiling manager: reacts to window events, keeps one tree per surface (virtual desktop x
//! monitor), applies the layout and handles drag & drop with a live preview.

use crate::config::Config;
use crate::platform::{hwnd_of, id_of, managed_snap, prepare_snap_recovery, restore_owned_snap, Platform, SurfaceKey};
use std::time::{Duration, Instant};
use crate::tree::{zone_at, DropZone, Edge, Hwnd, Rect, Tree};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::io::Write;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
use windows::Win32::Graphics::Gdi::{BeginPaint, CreateBitmap, CreateSolidBrush, DeleteObject, EndPaint, FillRect, GetMonitorInfoW, InvalidateRect, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, RegisterHotKey, MOD_NOREPEAT, MOD_WIN, VK_LBUTTON, VK_RBUTTON, VK_MBUTTON, VK_XBUTTON1, VK_XBUTTON2};
use windows::Win32::UI::Shell::{ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW};
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_EVENT: u32 = WM_APP + 1;
const WM_TRAY: u32 = WM_APP + 2;
const WM_PENDING: u32 = WM_APP + 3;
const TIMER_RECONCILE: usize = 1;
const TIMER_DRAG: usize = 2;
const TIMER_POLL: usize = 3;
const TIMER_HOVER: usize = 4;
const HOTKEY_TOGGLE: i32 = 1;
const MENU_TOGGLE: usize = 1;
const MENU_EXIT: usize = 2;
const MENU_OPEN_CONFIG: usize = 3;
const MENU_FFM_OFF: usize = 10;
const MENU_FFM_BASE: usize = 20;
const FFM_PRESETS: [u64; 5] = [150, 300, 500, 800, 1200];
const ACCENT: u32 = 0x00FF823C; // BGR: a clear blue
const DWMWA_BORDER_COLOR: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(34);
const COLOR_DEFAULT: u32 = 0xFFFF_FFFF;
/// Cursor distance from the screen border that counts as "drop at the outer edge".
const EDGE_PX: i32 = 36;

fn close(a: Rect, b: Rect) -> bool {
    (a.x - b.x).abs() <= 1 && (a.y - b.y).abs() <= 1 && (a.w - b.w).abs() <= 1 && (a.h - b.h).abs() <= 1
}

struct Placement {
    target: Rect,
    measured: Option<Rect>,
    requested: Instant,
}

impl Placement {
    fn new(target: Rect) -> Self { Self { target, measured: None, requested: Instant::now() } }

    fn suspend(&mut self) {
        self.measured = None;
        self.requested = Instant::now();
    }

    /// Ignore early async results; require two later polls with identical visible geometry.
    fn observe(&mut self, visible: Option<Rect>, elapsed: Duration) -> bool {
        if elapsed < Duration::from_millis(500) { self.measured = None; return false; }
        let stable = visible.is_some() && self.measured == visible;
        self.measured = visible;
        stable
    }
}

struct Surface {
    tree: Tree,
    area: Rect,
}

struct Drag {
    hwnd: Hwnd,
    key: SurfaceKey,
    start: Rect,
    resize: bool,
    released_since: Option<Instant>,
    /// (surface, window under the cursor or None for an outer edge / empty monitor, zone)
    target: Option<(SurfaceKey, Option<Hwnd>, DropZone)>,
}

#[derive(Clone, Copy, Default)]
struct PendingEvents {
    reconcile: bool,
    refresh: bool,
    tray: bool,
}

impl PendingEvents {
    const fn new() -> Self { Self { reconcile: false, refresh: false, tray: false } }

    fn record(&mut self, msg: u32, taskbar_created: u32) {
        self.reconcile |= matches!(msg, WM_EVENT | WM_TIMER | WM_PENDING);
        self.refresh |= matches!(msg, WM_DISPLAYCHANGE | WM_SETTINGCHANGE);
        self.tray |= taskbar_created != 0 && msg == taskbar_created;
    }

    fn any(self) -> bool { self.reconcile || self.refresh || self.tray }
}

pub struct App {
    p: Platform,
    /// Virtual desktops on which tiling is switched off (every desktop is on by default).
    disabled: HashSet<u128>,
    cfg: Config,
    /// Window under the cursor, the focused window at that moment, and since when.
    hover: Option<(Hwnd, Hwnd, Instant)>,
    /// The virtual desktop that is currently shown.
    shown: u128,
    desktop_error_logged: bool,
    /// Minimum sizes some windows insist on, learned from windows that refuse to shrink.
    mins: HashMap<Hwnd, (i32, i32)>,
    /// Last target we placed a window at, and what we measured afterwards.
    placed: HashMap<Hwnd, Placement>,
    /// Windows that currently fit nowhere (they stay floating).
    floating: HashSet<Hwnd>,
    surfaces: HashMap<SurfaceKey, Surface>,
    focused: Option<Hwnd>,
    bordered: Option<Hwnd>,
    border_original: HashMap<Hwnd, u32>,
    taskbar_created: u32,
    drag: Option<Drag>,
    overlay: HWND,
    main: HWND,
    filter: Option<String>,
    icon_on: HICON,
    icon_off: HICON,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static MAIN_HANDLE: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    static PENDING: Cell<PendingEvents> = const { Cell::new(PendingEvents::new()) };
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
    static MENU_OPEN: Cell<bool> = const { Cell::new(false) };
    /// What the drag overlay paints: the screen area and (rectangle, is the dragged window) pairs.
    static OVERLAY: RefCell<(Rect, Vec<(Rect, bool)>)> = const { RefCell::new((Rect::new(0, 0, 0, 0), Vec::new())) };
}

fn clean_text(text: &str, limit: usize) -> String {
    text.chars().take(limit).map(|c| if c.is_control() { ' ' } else { c }).collect()
}

#[cfg(test)]
pub fn log(_msg: &str) {}

#[cfg(not(test))]
pub fn log(msg: &str) {
    static WRITER: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = WRITER.lock().unwrap_or_else(|e| e.into_inner());
    let Some(dir) = Config::path().and_then(|p| p.parent().map(std::path::Path::to_path_buf)) else { return };
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("cosmictile.log");
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let msg = clean_text(msg, 500);
    let line = format!("[{}:{:02}:{:02}] {msg}\n", (t / 3600) % 24, (t / 60) % 60, t % 60);
    let _ = append_log(&path, &line);
}

fn append_log(path: &std::path::Path, line: &str) -> std::io::Result<()> {
    if std::fs::metadata(path).map(|m| m.len().saturating_add(line.len() as u64) > 1_048_576).unwrap_or(false) {
        let backup = path.with_extension("log.1");
        match std::fs::remove_file(&backup) {
            Ok(()) => {},
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
            Err(e) => return Err(e),
        }
        std::fs::rename(path, backup)?;
    }
    std::fs::OpenOptions::new().create(true).append(true).open(path)?.write_all(line.as_bytes())
}

fn set_timer(main: HWND, id: usize, ms: u32) {
    unsafe { if SetTimer(Some(main), id, ms, None) == 0 { log(&format!("could not create timer {id}")); } }
}

fn notify(action: windows::Win32::UI::Shell::NOTIFY_ICON_MESSAGE, data: &NOTIFYICONDATAW) {
    unsafe { if !Shell_NotifyIconW(action, data).as_bool() { log("tray icon operation failed"); } }
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
            let frame = !(3..N - 3).contains(&x) || !(3..N - 3).contains(&y);
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
    fn desktop_on(&self, d: u128) -> bool {
        !self.disabled.contains(&d)
    }

    fn managed(&self, h: Hwnd, cur: u128) -> bool {
        if !self.p.is_manageable(h, cur) {
            return false;
        }
        match &self.filter {
            Some(f) => self.p.title(h).contains(f.as_str()) || self.p.exe_name(h).eq_ignore_ascii_case(f),
            None => true,
        }
    }

    fn surface_containing(&self, h: Hwnd) -> Option<SurfaceKey> {
        find_enabled_surface(&self.surfaces, &self.disabled, h)
    }

    /// Brings the trees in line with the windows that really exist, then lays out what changed.
    fn reconcile(&mut self, force_all: bool) {
        let cur = self.p.current_desktop();
        if cur == 0 {
            self.cancel_drag();
            let was_known = self.shown != 0;
            self.shown = 0;
            self.hover = None;
            if let Some(h) = self.bordered.take() { self.set_border(h, COLOR_DEFAULT); }
            self.sync_snap();
            if was_known { self.update_tray(); }
            if !self.desktop_error_logged { log("current virtual desktop unavailable; tiling suspended"); self.desktop_error_logged = true; }
            return;
        }
        if cur != self.shown {
            self.cancel_drag();
            log(&format!("desktop switch {:x} -> {:x} (tiling {})", self.shown, cur, if self.desktop_on(cur) { "on" } else { "off" }));
            self.shown = cur;
            self.sync_snap();
            self.update_tray();
        }
        let tops = self.p.all_top_level();
        self.mins.retain(|h, _| tops.contains(h));
        self.border_original.retain(|h, _| tops.contains(h));
        let current: HashMap<Hwnd, (SurfaceKey, Rect)> = tops
            .iter()
            .rev()
            .copied()
            .filter(|&h| self.managed(h, cur) && (!self.p.is_fullscreen(h) || self.surface_containing(h).is_some()))
            .map(|h| (h, self.p.surface_of(h)))
            .filter(|&(_, (k, _))| self.desktop_on(k.desktop))
            .collect();
        let mut dirty: HashSet<SurfaceKey> = HashSet::new();
        if force_all {
            dirty.extend(self.surfaces.keys().copied());
        }
        for s in self.surfaces.values_mut() {
            s.tree.mins = self.mins.clone();
        }

        // Paused desktops retain their manual trees until enabled again.
        for (key, s) in self.surfaces.iter_mut() {
            if self.disabled.contains(&key.desktop) { continue; }
            s.area = current.values().find(|(k, _)| k == key).map(|(_, a)| *a).unwrap_or(s.area);
            for h in s.tree.leaves() {
                if current.get(&h).map(|(k, _)| k) != Some(key) {
                    s.tree.remove(h);
                    dirty.insert(*key);
                    log(&format!("removed {h:x}"));
                }
            }
        }
        let paused: HashSet<Hwnd> = self.surfaces.iter().filter(|(k, _)| self.disabled.contains(&k.desktop)).flat_map(|(_, s)| s.tree.leaves()).collect();
        self.placed.retain(|h, _| current.contains_key(h) || paused.contains(h));
        self.floating.retain(|h| current.contains_key(h));

        // New windows, in z-order from back to front.
        let order: Vec<Hwnd> = tops.iter().rev().copied().filter(|h| current.contains_key(h)).collect();
        for h in order {
            let (key, area) = current[&h];
            if self.surface_containing(h).is_some() {
                continue;
            }
            let focused = self.focused;
            let info = format!("{h:x} {} ({})", clean_text(&self.p.title(h), 40), self.p.exe_name(h));
            let mins = self.mins.clone();
            let s = self.surfaces.entry(key).or_insert_with(|| Surface { tree: Tree::new(), area });
            s.area = area;
            s.tree.mins = mins;
            if s.tree.add(h, area, focused) {
                dirty.insert(key);
                self.floating.remove(&h);
                log(&format!("added {info}"));
            } else if self.floating.insert(h) {
                log(&format!("no room, stays floating: {h:x}"));
            }
        }
        self.surfaces.retain(|_, s| !s.tree.is_empty());
        // Everything on the shown desktop is verified, so a desktop switch repairs what the
        // system moved while the windows were hidden.
        let keys: Vec<SurfaceKey> = self.surfaces.keys().copied().filter(|k| dirty.contains(k) || k.desktop == cur).collect();
        for key in keys {
            self.apply(key, force_all);
        }
    }

    fn apply(&mut self, key: SurfaceKey, force: bool) {
        if self.shown == 0 || key.desktop != self.shown || !self.desktop_on(key.desktop) { return; }
        let Some(s) = self.surfaces.get(&key) else { return };
        let layout = s.tree.layout(s.area);
        let mut learned = false;
        for (h, r) in layout {
            let vis = self.p.visible_rect(h);
            let was_placed = self.placed.get(&h).map(|p| p.target);
            if suspend_placement(self.p.is_maximized(h), self.p.is_fullscreen(h), was_placed, vis) || self.p.is_minimized(h) || self.p.is_hung(h) {
                if let Some(placement) = self.placed.get_mut(&h) { placement.suspend(); }
                continue;
            }
            if force {
                self.placed.remove(&h);
            }
            if vis.map(|v| close(v, r)).unwrap_or(false) {
                self.placed.insert(h, Placement::new(r));
                continue;
            }
            if let Some(placement) = self.placed.get_mut(&h) && placement.target == r {
                let elapsed = placement.requested.elapsed();
                if !placement.observe(vis, elapsed) { continue; }
                {
                    // Same target, same result as last time: the window refuses this size.
                    // Remember what it needs; the layout then gives it a zone of that size.
                    if let Some(v) = vis {
                        let old = self.mins.get(&h).copied().unwrap_or((0, 0));
                        let mut new = old;
                        if v.w > r.w + 2 {
                            new.0 = new.0.max(v.w);
                        }
                        if v.h > r.h + 2 {
                            new.1 = new.1.max(v.h);
                        }
                        if new != old {
                            self.mins.insert(h, new);
                            log(&format!("{h:x} needs at least {}x{} (zone was {}x{})", new.0, new.1, r.w, r.h));
                            learned = true;
                        }
                    }
                    continue;
                }
            }
            if self.p.place(h, r) {
                // The request is pending; only later stable polls can teach a minimum.
                self.placed.insert(h, Placement::new(r));
            }
        }
        if learned {
            for s in self.surfaces.values_mut() {
                s.tree.mins = self.mins.clone();
            }
            if let Some(s) = self.surfaces.get_mut(&key) && s.tree.reseat(s.area) {
                log("re-seated surface to respect minimum sizes");
            }
            self.apply(key, false);
        }
    }

    /// Windows' own snapping must not fight with us: it is off while tiling is on for the shown desktop.
    fn sync_snap(&self) {
        managed_snap(self.shown == 0 || !self.desktop_on(self.shown));
    }

    fn set_border(&mut self, h: Hwnd, color: u32) {
        unsafe {
            let color = if color == COLOR_DEFAULT {
                self.border_original.remove(&h).unwrap_or(COLOR_DEFAULT)
            } else {
                self.border_original.entry(h).or_insert_with(|| {
                    let mut original = COLOR_DEFAULT;
                    let _ = DwmGetWindowAttribute(hwnd_of(h), DWMWA_BORDER_COLOR, &mut original as *mut u32 as *mut c_void, 4);
                    original
                });
                color
            };
            let c = COLORREF(color);
            let _ = DwmSetWindowAttribute(hwnd_of(h), DWMWA_BORDER_COLOR, &c as *const COLORREF as *const c_void, 4);
        }
    }

    fn on_focus(&mut self, h: Hwnd) {
        if !self.desktop_on(self.shown) || self.surface_containing(h).is_none() {
            self.focused = None;
            if let Some(prev) = self.bordered.take() { self.set_border(prev, COLOR_DEFAULT); }
            return;
        }
        self.focused = Some(h);
        if let Some(prev) = self.bordered.take() && prev != h { self.set_border(prev, COLOR_DEFAULT); }
        self.set_border(h, ACCENT);
        self.bordered = Some(h);
    }

    /// Switches tiling on or off for the desktop that is shown right now.
    fn toggle(&mut self) {
        let d = self.p.current_desktop();
        if d == 0 { return; }
        if !self.disabled.remove(&d) {
            self.disabled.insert(d);
        }
        let on = self.desktop_on(d);
        log(&format!("tiling {} for desktop {d:x}", if on { "ON" } else { "OFF" }));
        self.shown = d;
        self.drag = None;
        unsafe {
            let _ = KillTimer(Some(self.main), TIMER_DRAG);
        }
        self.hide_overlay();
        if let Some(b) = self.bordered.take() {
            self.set_border(b, COLOR_DEFAULT);
        }
        self.sync_snap();
        if on {
            self.reconcile(true);
        }
        self.update_tray();
    }

    fn cancel_drag(&mut self) {
        self.drag = None;
        unsafe { let _ = KillTimer(Some(self.main), TIMER_DRAG); }
        self.hide_overlay();
    }

    fn drag_desktop_valid(&self, desktop: u128) -> bool {
        valid_drag_desktop(desktop, self.shown, self.p.current_desktop(), self.desktop_on(desktop))
    }

    fn poll_drag_release(&mut self) {
        if let Some(d) = &self.drag {
            if !self.drag_desktop_valid(d.key.desktop) { self.cancel_drag(); }
            else {
                let down = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) < 0 };
                let d = self.drag.as_mut().expect("drag was checked");
                if released_for_timeout(&mut d.released_since, down, Instant::now()) {
                    let h = d.hwnd;
                    self.drag_end(h);
                }
            }
        }
    }

    fn poll_tick(&mut self) {
        self.poll_drag_release();
        if self.drag.is_none() { self.reconcile(false); }
    }

    fn refresh_surfaces(&mut self) {
        if self.p.current_desktop() == 0 { self.reconcile(true); return; }
        self.cancel_drag();
        let tops = self.p.all_top_level();
        let old = std::mem::take(&mut self.surfaces);
        for (_, mut surface) in old {
            let Some(h) = surface.tree.leaves().into_iter().find(|h| tops.contains(h)) else { continue };
            let (key, area) = self.p.surface_of(h);
            if key.desktop == 0 { continue; }
            surface.area = area;
            if let Some(existing) = self.surfaces.get_mut(&key) {
                for h in surface.tree.leaves() { existing.tree.add(h, area, None); }
            } else { self.surfaces.insert(key, surface); }
        }
        self.reconcile(true);
    }

    // ---------- drag & drop ----------

    fn drag_start(&mut self, h: Hwnd) {
        let Some(key) = self.surface_containing(h) else { return };
        if !self.drag_desktop_valid(key.desktop) { return; }
        let Some(v) = self.p.visible_rect(h) else { return };
        log(&format!("drag start {h:x}"));
        self.drag = Some(Drag { hwnd: h, key, start: v, resize: false, released_since: None, target: None });
        set_timer(self.main, TIMER_DRAG, 30);
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

    /// Outer screen edge the cursor is at (if any): dropping there creates a full-length zone.
    fn edge_at(area: Rect, cx: i32, cy: i32) -> Option<DropZone> {
        let cands = [
            (cx - area.x, DropZone::Left),
            (area.right() - 1 - cx, DropZone::Right),
            (cy - area.y, DropZone::Top),
            (area.bottom() - 1 - cy, DropZone::Bottom),
        ];
        cands.into_iter().filter(|(d, _)| *d < EDGE_PX).min_by_key(|(d, _)| *d).map(|(_, z)| z)
    }

    fn drag_tick(&mut self) {
        let Some(d) = &self.drag else { return };
        let (h, home) = (d.hwnd, d.key);
        if !self.drag_desktop_valid(home.desktop) { self.cancel_drag(); return; }
        let visible = self.p.visible_rect(h);
        let cursor = self.cursor_surface(self.shown);
        let preview = update_drag(self.drag.as_mut().expect("drag was checked"), visible, cursor, &self.surfaces);
        match preview {
            Some((bounds, layout)) => self.show_overlay(bounds, &layout, h),
            None => self.hide_overlay(),
        }
    }

    fn drag_end(&mut self, h: Hwnd) {
        if self.drag.as_ref().map(|d| d.hwnd) != Some(h) { return; }
        // Release can arrive before the first 30 ms tick or after the last cursor move.
        self.drag_tick();
        let Some(d) = self.drag.take() else { return };
        unsafe {
            let _ = KillTimer(Some(self.main), TIMER_DRAG);
        }
        self.hide_overlay();
        if d.hwnd != h {
            return;
        }
        let home = d.key;
        if !self.drag_desktop_valid(home.desktop) { return; }
        if d.resize {
            if let (Some(v), Some(s)) = (self.p.visible_rect(h), self.surfaces.get_mut(&home))
                && let Some(old) = s.tree.rect_of(s.area, h) {
                    let area = s.area;
                    let tol = 3;
                    if (v.x - old.x).abs() > tol { s.tree.set_edge(area, h, Edge::Left, v.x); }
                    if (v.right() - old.right()).abs() > tol { s.tree.set_edge(area, h, Edge::Right, v.right()); }
                    if (v.y - old.y).abs() > tol { s.tree.set_edge(area, h, Edge::Top, v.y); }
                    if (v.bottom() - old.bottom()).abs() > tol { s.tree.set_edge(area, h, Edge::Bottom, v.bottom()); }
            }
            log(&format!("resize end {h:x}"));
            self.apply(home, true);
            return;
        }
        let Some((key, target, zone)) = d.target else {
            log(&format!("drop {h:x}: no valid target, window goes back"));
            self.apply(home, true);
            return;
        };
        let cur = self.p.current_desktop();
        let valid = cur == key.desktop && self.desktop_on(cur)
            && self.surfaces.get(&home).map(|s| s.tree.contains(h)).unwrap_or(false)
            && self.managed(h, cur) && self.p.desktop_id(h) == cur
            && target_in_tree(self.surfaces.get(&key).map(|s| &s.tree), target)
            && target.map(|t| self.managed(t, cur) && self.p.surface_of(t).0 == key).unwrap_or(true);
        if !valid {
            self.apply(home, true);
            return;
        }
        log(&format!("drop {h:x} -> surface {:x} target {:?} {zone:?}", key.monitor, target.map(|t| format!("{t:x}"))));
        match (target, zone) {
            (Some(t), z) if key == home => {
                if let Some(s) = self.surfaces.get_mut(&home) {
                    s.tree.move_to(h, t, z);
                }
            }
            (None, z) if key == home && z != DropZone::Center => {
                if let Some(s) = self.surfaces.get_mut(&home) {
                    s.tree.move_edge(h, z);
                }
            }
            (t, z) => {
                if let Some(s) = self.surfaces.get_mut(&home) {
                    s.tree.remove(h);
                }
                match (self.surfaces.get_mut(&key), t) {
                    (Some(s), Some(t)) => s.tree.insert(h, t, z),
                    (Some(s), None) => s.tree.insert_edge(h, z),
                    (None, _) => {
                        if let Some((_, area, _, _)) = self.cursor_surface(key.desktop) {
                            let mut tree = Tree::new();
                            tree.add(h, area, None);
                            self.surfaces.insert(key, Surface { tree, area });
                        }
                    }
                }
            }
        }
        self.surfaces.retain(|_, s| !s.tree.is_empty());
        self.apply(home, true);
        if key != home {
            self.apply(key, true);
        }
    }

    // ---------- focus follows mouse ----------

    fn is_tracked_here(&self, h: Hwnd) -> bool {
        self.surfaces.iter().any(|(k, s)| k.desktop == self.shown && s.tree.contains(h))
    }

    /// Gives the keyboard focus to the tiled window under the cursor once the cursor has rested
    /// on it for the configured time. Does nothing while a menu, the Start menu, a dialog or any
    /// window we do not manage has the focus, while a mouse button is held, or while dragging.
    fn hover_tick(&mut self) {
        if self.shown == 0 || !self.cfg.focus_follows_mouse || !self.desktop_on(self.shown) || self.drag.is_some() || MENU_OPEN.with(Cell::get) {
            self.hover = None;
            return;
        }
        unsafe {
            if [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON, VK_XBUTTON1, VK_XBUTTON2].iter().any(|k| GetAsyncKeyState(k.0 as i32) < 0) {
                self.hover = None;
                return;
            }
            let mut pt = POINT::default();
            if GetCursorPos(&mut pt).is_err() {
                return;
            }
            let under = id_of(GetAncestor(WindowFromPoint(pt), GA_ROOT));
            let fg = id_of(GetForegroundWindow());
            if self.p.is_fullscreen(fg) || self.p.is_maximized(fg) || foreground_has_menu(hwnd_of(fg)) {
                self.hover = None;
                return;
            }
            if under == 0 || under == fg || !self.is_tracked_here(under) {
                self.hover = None;
                return;
            }
            let fg_ok = fg == 0 || self.is_tracked_here(fg) || matches!(self.p.class_name(fg).as_str(), "Progman" | "WorkerW");
            if !fg_ok {
                self.hover = None;
                return;
            }
            match self.hover {
                Some((h, f, since)) if h == under && f == fg => {
                    if since.elapsed() >= Duration::from_millis(self.cfg.focus_delay_ms) {
                        if self.p.current_desktop() != self.shown { self.hover = None; return; }
                        self.focus_window(under);
                        self.hover = Some((under, fg, Instant::now()));
                    }
                }
                _ => self.hover = Some((under, fg, Instant::now())),
            }
        }
    }

    fn focus_window(&self, h: Hwnd) -> bool {
        unsafe { SetForegroundWindow(hwnd_of(h)).as_bool() }
    }

    // ---------- overlay & tray ----------

    /// Shows the complete layout that a drop would produce: every window as a thin frame, the
    /// dragged window highlighted. That way it is clear what happens to the neighbours too.
    fn show_overlay(&self, area: Rect, layout: &[(Hwnd, Rect)], dragged: Hwnd) {
        let items: Vec<(Rect, bool)> = layout.iter().map(|(h, r)| (*r, *h == dragged)).collect();
        let changed = OVERLAY.with(|o| {
            let mut o = o.borrow_mut();
            if o.0 == area && o.1 == items {
                false
            } else {
                *o = (area, items);
                true
            }
        });
        unsafe {
            let _ = SetWindowPos(self.overlay, Some(HWND_TOPMOST), area.x, area.y, area.w, area.h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            if changed {
                let _ = InvalidateRect(Some(self.overlay), None, true);
            }
        }
    }

    fn hide_overlay(&self) {
        unsafe {
            let _ = ShowWindow(self.overlay, SW_HIDE);
        }
    }

    fn tray_data(&self) -> NOTIFYICONDATAW {
        let mut nid = NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: self.main, uID: 1, uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP, uCallbackMessage: WM_TRAY, ..Default::default() };
        let on = self.shown != 0 && self.desktop_on(self.shown);
        nid.hIcon = if on { self.icon_on } else { self.icon_off };
        let tip = if self.shown == 0 { "CosmicTile: paused (desktop identity unavailable)" }
            else if on { "CosmicTile: ON for this desktop (click to turn off)" }
            else { "CosmicTile: OFF for this desktop (click to turn on)" };
        for (i, c) in tip.encode_utf16().take(127).enumerate() {
            nid.szTip[i] = c;
        }
        nid
    }

    fn update_tray(&self) {
        notify(NIM_MODIFY, &self.tray_data());
    }

    fn menu_command(&mut self, cmd: usize) {
        let app = self;
        unsafe {
            match cmd {
                MENU_TOGGLE => app.toggle(),
                MENU_OPEN_CONFIG => {
                    app.cfg.ensure_exists();
                    if let Some(path) = Config::path() {
                        let path = wide(&path.to_string_lossy());
                        ShellExecuteW(None, w!("open"), PCWSTR(path.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
                    }
                }
                MENU_FFM_OFF => {
                    app.cfg.focus_follows_mouse = false;
                    app.cfg.save();
                }
                c if (MENU_FFM_BASE..MENU_FFM_BASE + FFM_PRESETS.len()).contains(&c) => {
                    app.cfg.focus_follows_mouse = true;
                    app.cfg.focus_delay_ms = FFM_PRESETS[c - MENU_FFM_BASE];
                    app.cfg.save();
                    log(&format!("focus follows mouse after {} ms", app.cfg.focus_delay_ms));
                }
                MENU_EXIT => {
                    PostQuitMessage(0);
                }
                _ => {}
            }
        }
    }

    fn shutdown(&mut self) {
        restore_owned_snap();
        if let Some(b) = self.bordered.take() {
            self.set_border(b, COLOR_DEFAULT);
        }
        notify(NIM_DELETE, &self.tray_data());
        log("exit");
    }
}

fn foreground_has_menu(foreground: HWND) -> bool {
    unsafe extern "system" fn visit(hwnd: HWND, lp: LPARAM) -> windows::core::BOOL {
        let mut class = [0u16; 32];
        let len = unsafe { GetClassNameW(hwnd, &mut class) } as usize;
        if String::from_utf16_lossy(&class[..len]) == "#32768" && unsafe { IsWindowVisible(hwnd).as_bool() } {
            unsafe { *(lp.0 as *mut bool) = true; }
            return windows::core::BOOL(0);
        }
        windows::core::BOOL(1)
    }
    let mut found = false;
    unsafe { let thread = GetWindowThreadProcessId(foreground, None); if thread != 0 { let _ = EnumThreadWindows(thread, Some(visit), LPARAM(&mut found as *mut bool as isize)); } }
    found
}

fn show_menu() {
    if MENU_OPEN.with(|m| m.replace(true)) { return; }
    let snapshot = APP.with(|a| a.try_borrow().ok().and_then(|g| g.as_ref().map(|app| (app.cfg.clone(), app.main, app.desktop_on(app.shown)))));
    if let Some((cfg, main, on)) = snapshot {
        let cmd = unsafe { build_and_track_menu(&cfg, main, on) };
        APP.with(|a| { if let Ok(mut g) = a.try_borrow_mut() && let Some(app) = g.as_mut() { app.menu_command(cmd); } });
    }
    MENU_OPEN.with(|m| m.set(false));
}

unsafe fn build_and_track_menu(cfg: &Config, main: HWND, on: bool) -> usize {
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { log("could not create tray menu"); return 0; };
            let Ok(focus_menu) = CreatePopupMenu() else { let _ = DestroyMenu(menu); log("could not create focus menu"); return 0; };
            let check = |on: bool| if on { MF_STRING | MF_CHECKED } else { MF_STRING };
            if let Err(e) = AppendMenuW(focus_menu, check(!cfg.focus_follows_mouse), MENU_FFM_OFF, w!("Off")) { log(&format!("could not append tray menu item: {e}")); }
            for (i, ms) in FFM_PRESETS.iter().enumerate() {
                let label = wide(&format!("After {ms} ms{}", if *ms == 300 { " (default)" } else { "" }));
                let on = cfg.focus_follows_mouse && cfg.focus_delay_ms == *ms;
                if let Err(e) = AppendMenuW(focus_menu, check(on), MENU_FFM_BASE + i, PCWSTR(label.as_ptr())) { log(&format!("could not append tray menu item: {e}")); }
            }
            let label = if on { w!("Turn tiling off for this desktop") } else { w!("Turn tiling on for this desktop") };
            if let Err(e) = AppendMenuW(menu, MF_STRING, MENU_TOGGLE, label) { log(&format!("could not append tray menu item: {e}")); }
            if let Err(e) = AppendMenuW(menu, MF_POPUP, focus_menu.0 as usize, w!("Focus follows mouse")) {
                // The parent owns the submenu only after a successful append.
                let _ = DestroyMenu(focus_menu);
                log(&format!("could not append tray menu item: {e}"));
            }
            if let Err(e) = AppendMenuW(menu, MF_STRING, MENU_OPEN_CONFIG, w!("Open settings file")) { log(&format!("could not append tray menu item: {e}")); }
            if let Err(e) = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()) { log(&format!("could not append tray menu item: {e}")); }
            if let Err(e) = AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("Exit CosmicTile")) { log(&format!("could not append tray menu item: {e}")); }
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = SetForegroundWindow(main);
            let cmd = TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_RETURNCMD, pt.x, pt.y, Some(0), main, None).0 as usize;
            let _ = DestroyMenu(menu);
            cmd
        }
}

unsafe extern "system" fn win_event_proc(_h: HWINEVENTHOOK, event: u32, hwnd: HWND, id_object: i32, id_child: i32, _thread: u32, _time: u32) {
    if id_object != 0 || id_child != 0 || hwnd.0.is_null() {
        return;
    }
    let main = MAIN_HANDLE.with(Cell::get);
    if !main.is_invalid() {
        unsafe { if PostMessageW(Some(main), WM_EVENT, WPARAM(event as usize), LPARAM(hwnd.0 as isize)).is_err() { queue_pending(WM_EVENT); } }
    }
}

unsafe extern "system" fn overlay_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            paint_overlay(hwnd);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn paint_overlay(hwnd: HWND) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let key = CreateSolidBrush(COLORREF(0x00FF00FF)); // transparent colour key
        let fill = CreateSolidBrush(COLORREF(ACCENT));
        let grey = CreateSolidBrush(COLORREF(0x00E0E0E0));
        let white = CreateSolidBrush(COLORREF(0x00FFFFFF));
        FillRect(hdc, &rc, key);
        OVERLAY.with(|o| {
            let (origin, items) = &*o.borrow();
            for (r, highlight) in items {
                let inner = RECT { left: r.x - origin.x + 3, top: r.y - origin.y + 3, right: r.right() - origin.x - 3, bottom: r.bottom() - origin.y - 3 };
                if *highlight {
                    FillRect(hdc, &inner, fill);
                }
                let t = 3;
                let line = if *highlight { white } else { grey };
                for strip in [
                    RECT { bottom: inner.top + t, ..inner },
                    RECT { top: inner.bottom - t, ..inner },
                    RECT { right: inner.left + t, ..inner },
                    RECT { left: inner.right - t, ..inner },
                ] {
                    FillRect(hdc, &strip, line);
                }
            }
        });
        for b in [key, fill, grey, white] {
            let _ = DeleteObject(b.into());
        }
        let _ = EndPaint(hwnd, &ps);
    }
}

fn queue_pending(msg: u32) {
    PENDING.with(|p| {
        let mut pending = p.get();
        pending.record(msg, TASKBAR_CREATED.with(Cell::get));
        p.set(pending);
    });
}

fn drain_pending(app: &mut App) {
    let pending = PENDING.with(Cell::take);
    if pending.tray { notify(NIM_ADD, &app.tray_data()); }
    if pending.refresh { app.refresh_surfaces(); }
    else { app.poll_tick(); }
    // A deferred foreground event should also repair the accent on an unmanaged window.
    app.on_focus(id_of(unsafe { GetForegroundWindow() }));
}

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_QUERYENDSESSION { return LRESULT(1); }
    if msg == WM_ENDSESSION && wp.0 != 0 { restore_owned_snap(); unsafe { PostQuitMessage(0); } return LRESULT(0); }
    if msg == WM_CLOSE { unsafe { PostQuitMessage(0); } return LRESULT(0); }
    if msg == WM_TRAY && lp.0 as u32 == WM_RBUTTONUP { show_menu(); return LRESULT(0); }
    let handled = APP.with(|a| {
        let Ok(mut guard) = a.try_borrow_mut() else {
            queue_pending(msg);
            return false;
        };
        let Some(app) = guard.as_mut() else { return false };
        if app.taskbar_created != 0 && msg == app.taskbar_created {
            notify(NIM_ADD, &app.tray_data());
            return true;
        }
        match msg {
            WM_EVENT => {
                let h = lp.0;
                match wp.0 as u32 {
                    0x0003 => app.on_focus(h),
                    0x000A => app.drag_start(h),
                    0x000B => app.drag_end(h),
                    _ => set_timer(app.main, TIMER_RECONCILE, 80),
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
                TIMER_DRAG => { app.drag_tick(); app.poll_drag_release(); },
                TIMER_HOVER => app.hover_tick(),
                TIMER_POLL => { if PENDING.with(|p| p.get().any()) { drain_pending(app); } else { app.poll_tick(); } },
                _ => {}
            },
            WM_PENDING => drain_pending(app),
            WM_DISPLAYCHANGE | WM_SETTINGCHANGE => app.refresh_surfaces(),
            WM_HOTKEY => app.toggle(),
            WM_TRAY if lp.0 as u32 == WM_LBUTTONUP => app.toggle(),
            _ => return false,
        }
        true
    });
    if handled && PENDING.with(|p| p.get().any()) {
        unsafe { let _ = PostMessageW(Some(hwnd), WM_PENDING, WPARAM(0), LPARAM(0)); }
    }
    if handled {
        LRESULT(0)
    } else {
        unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
    }
}

pub fn run(filter: Option<String>) {
    if !prepare_snap_recovery() { log("Snap recovery unavailable; Snap setting will be left unchanged"); }
    let cfg = Config::load();
    crate::tree::set_min_zone(cfg.min_zone_width, cfg.min_zone_height);
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap();
        let class_main = w!("CosmicTileMain");
        let class_overlay = w!("CosmicTileOverlay");
        let accent_brush = CreateSolidBrush(COLORREF(ACCENT));
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(main_proc), hInstance: hinst.into(), lpszClassName: class_main, ..Default::default() });
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(overlay_proc), hInstance: hinst.into(), lpszClassName: class_overlay, hbrBackground: accent_brush, ..Default::default() });
        let main = CreateWindowExW(WINDOW_EX_STYLE(0), class_main, w!("CosmicTile"), WS_OVERLAPPED, 0, 0, 0, 0, None, None, Some(hinst.into()), None).unwrap();
        MAIN_HANDLE.with(|h| h.set(main));
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
        let _ = SetLayeredWindowAttributes(overlay, COLORREF(0x00FF00FF), 200, LWA_COLORKEY | LWA_ALPHA);

        let platform = Platform::new();
        let taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated"));
        if taskbar_created == 0 { log("could not register TaskbarCreated message"); }
        TASKBAR_CREATED.with(|id| id.set(taskbar_created));
        let app = App {
            p: platform,
            disabled: HashSet::new(),
            cfg,
            hover: None,
            shown: 0,
            desktop_error_logged: false,
            mins: HashMap::new(),
            placed: HashMap::new(),
            floating: HashSet::new(),
            surfaces: HashMap::new(),
            focused: None,
            bordered: None,
            border_original: HashMap::new(),
            taskbar_created,
            drag: None,
            overlay,
            main,
            filter,
            icon_on: make_icon(true),
            icon_off: make_icon(false),
        };
        let nid = app.tray_data();
        notify(NIM_ADD, &nid);
        APP.with(|a| *a.borrow_mut() = Some(app));

        // foreground, move/size start+end, minimise start+end, destroy, show, cloak, uncloak
        let mut hooks = Vec::new();
        for ev in [0x0003u32, 0x000A, 0x000B, 0x0016, 0x0017, 0x8001, 0x8002, 0x8017, 0x8018] {
            let hook = SetWinEventHook(ev, ev, None, Some(win_event_proc), 0, 0, 0);
            if hook.is_invalid() { log(&format!("could not register window hook {ev:x}")); }
            else { hooks.push(hook); }
        }
        if RegisterHotKey(Some(main), HOTKEY_TOGGLE, MOD_WIN | MOD_NOREPEAT, 'Y' as u32).is_err() {
            log("could not register Win+Y");
        }
        set_timer(main, TIMER_POLL, 1500);
        set_timer(main, TIMER_HOVER, 50);
        log("started");
        APP.with(|a| {
            if let Some(app) = a.borrow_mut().as_mut() {
                app.reconcile(true);
            }
        });

        let mut msg = MSG::default();
        loop {
            let result = GetMessageW(&mut msg, None, 0, 0).0;
            if result <= 0 {
                if result < 0 { log("GetMessageW failed"); }
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        for hook in hooks { let _ = UnhookWinEvent(hook); }
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::UnregisterHotKey(Some(main), HOTKEY_TOGGLE);
        for timer in [TIMER_RECONCILE, TIMER_DRAG, TIMER_POLL, TIMER_HOVER] { let _ = KillTimer(Some(main), timer); }
        APP.with(|a| { if let Some(mut app) = a.borrow_mut().take() {
            app.shutdown();
            let _ = DestroyIcon(app.icon_on);
            let _ = DestroyIcon(app.icon_off);
        } });
        let _ = DestroyWindow(overlay);
        let _ = DestroyWindow(main);
        MAIN_HANDLE.with(|h| h.set(HWND(std::ptr::null_mut())));
        let _ = DeleteObject(accent_brush.into());
    }
}

/// A monitor-filling tile is not an app entering full screen. Test before clearing placement.
fn suspend_placement(maximized: bool, fullscreen: bool, placed: Option<Rect>, visible: Option<Rect>) -> bool {
    (maximized && placed.is_some()) || (fullscreen && !placed.zip(visible).map(|(p, v)| close(p, v)).unwrap_or(false))
}

/// Shared by timer updates and release: failed observations invalidate the previous target.
fn update_drag(d: &mut Drag, visible: Option<Rect>, cursor: Option<(SurfaceKey, Rect, i32, i32)>, surfaces: &HashMap<SurfaceKey, Surface>) -> Option<(Rect, Vec<(Hwnd, Rect)>)> {
    d.target = None;
    let visible = visible?;
    d.resize |= is_resize(d.start, visible);
    if d.resize { return None; }
    let (key, area, cx, cy) = cursor?;
    let h = d.hwnd;
    let (target, mut layout) = match surfaces.get(&key) {
        Some(s) => {
            if let Some(side) = App::edge_at(area, cx, cy) {
                ((key, None, side), s.tree.preview_edge_layout(area, h, side)?)
            } else {
                let (target, rect) = s.tree.layout(area).into_iter().find(|(id, r)| *id != h && r.contains(cx, cy))?;
                let zone = zone_at(rect, cx, cy);
                let preview = if key == d.key { s.tree.preview_layout(area, h, target, zone) }
                    else { s.tree.preview_insert_layout(area, h, target, zone) };
                ((key, Some(target), zone), preview?)
            }
        }
        None => ((key, None, DropZone::Center), vec![(h, area)]),
    };
    d.target = Some(target);
    let mut bounds = area;
    if key != d.key && let Some(source) = surfaces.get(&d.key) {
        let mut tree = source.tree.clone();
        tree.remove(h);
        layout.extend(tree.layout(source.area));
        bounds = union(area, source.area);
    }
    Some((bounds, layout))
}

fn target_in_tree(tree: Option<&Tree>, target: Option<Hwnd>) -> bool {
    target.map(|h| tree.is_some_and(|tree| tree.contains(h))).unwrap_or(true)
}

/// Paused trees keep their reserved layout but must not claim windows on an enabled desktop.
fn find_enabled_surface(surfaces: &HashMap<SurfaceKey, Surface>, disabled: &HashSet<u128>, h: Hwnd) -> Option<SurfaceKey> {
    surfaces.iter().find(|(key, surface)| !disabled.contains(&key.desktop) && surface.tree.contains(h)).map(|(key, _)| *key)
}

fn valid_drag_desktop(home: u128, shown: u128, current: u128, enabled: bool) -> bool {
    home != 0 && home == shown && home == current && enabled
}

fn released_for_timeout(since: &mut Option<Instant>, down: bool, now: Instant) -> bool {
    if down { *since = None; return false; }
    now.saturating_duration_since(*since.get_or_insert(now)) >= Duration::from_millis(500)
}

fn is_resize(start: Rect, now: Rect) -> bool {
    let moved = |a: i32, b: i32| (i64::from(a) - i64::from(b)).abs() > 3;
    (moved(start.x, now.x) && !moved(start.right(), now.right()))
        || (moved(start.right(), now.right()) && !moved(start.x, now.x))
        || (moved(start.y, now.y) && !moved(start.bottom(), now.bottom()))
        || (moved(start.bottom(), now.bottom()) && !moved(start.y, now.y))
}

fn union(a: Rect, b: Rect) -> Rect {
    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
    Rect::new(x, y, a.right().max(b.right()) - x, a.bottom().max(b.bottom()) - y)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paused_layout_does_not_block_adoption_after_a_window_changes_desktop() {
        let (drag, mut surfaces, area) = drag_fixture();
        let home = drag.key;
        let disabled = HashSet::from([home.desktop]);
        surfaces.get_mut(&home).unwrap().tree.set_edge(area, drag.hwnd, Edge::Right, 700);
        let reserved = surfaces[&home].tree.layout(area);
        assert_eq!(find_enabled_surface(&surfaces, &disabled, drag.hwnd), None);
        let new_key = SurfaceKey { desktop: 2, ..home };
        let mut tree = Tree::new();
        tree.add(drag.hwnd, area, None);
        surfaces.insert(new_key, Surface { tree, area });
        assert_eq!(find_enabled_surface(&surfaces, &disabled, drag.hwnd), Some(new_key));
        assert_eq!(surfaces[&home].tree.layout(area), reserved);
    }
    #[test]
    fn busy_callbacks_preserve_geometry_and_explorer_recovery_until_drained() {
        let mut pending = PendingEvents::new();
        pending.record(WM_DISPLAYCHANGE, 40000);
        pending.record(WM_TIMER, 40000);
        pending.record(40000, 40000);
        pending.record(WM_EVENT, 40000);
        assert!(pending.refresh && pending.reconcile && pending.tray);
        let cell = Cell::new(pending);
        let drained = cell.take();
        assert!(drained.refresh && drained.tray);
        assert!(!cell.get().any());
        let mut next = cell.get();
        next.record(WM_SETTINGCHANGE, 40000);
        cell.set(next);
        assert!(cell.get().refresh); // events arriving during drain belong to the next drain
    }
    #[test]
    fn lost_drag_end_is_recovered_only_after_continuous_button_release() {
        let now = Instant::now();
        let mut since = None;
        assert!(!released_for_timeout(&mut since, false, now));
        assert!(!released_for_timeout(&mut since, false, now + Duration::from_millis(499)));
        assert!(released_for_timeout(&mut since, false, now + Duration::from_millis(500)));
        assert!(!released_for_timeout(&mut since, true, now + Duration::from_millis(501)));
        assert!(since.is_none());
        assert!(!released_for_timeout(&mut since, false, now + Duration::from_millis(600)));
    }
    #[test]
    fn desktop_switch_unavailable_identity_or_pausing_invalidates_drag() {
        assert!(valid_drag_desktop(1, 1, 1, true));
        assert!(!valid_drag_desktop(1, 1, 2, true));
        assert!(!valid_drag_desktop(1, 2, 2, true));
        assert!(!valid_drag_desktop(1, 1, 0, true));
        assert!(!valid_drag_desktop(0, 0, 0, true));
        assert!(!valid_drag_desktop(1, 1, 1, false));
    }
    fn drag_fixture() -> (Drag, HashMap<SurfaceKey, Surface>, Rect) {
        let area = Rect::new(0, 0, 1920, 1080);
        let key = SurfaceKey { desktop: 1, monitor: 1 };
        let mut tree = Tree::new();
        tree.add(1, area, None);
        tree.add(2, area, None);
        let drag = Drag { hwnd: 1, key, start: tree.rect_of(area, 1).unwrap(), resize: false, released_since: None, target: None };
        (drag, HashMap::from([(key, Surface { tree, area })]), area)
    }
    #[test]
    fn release_without_a_timer_tick_uses_current_cursor_and_geometry() {
        let (mut drag, surfaces, area) = drag_fixture();
        let key = drag.key;
        let start = drag.start;
        let preview = update_drag(&mut drag, Some(start), Some((key, area, 1440, 540)), &surfaces).unwrap();
        assert_eq!(drag.target, Some((key, Some(2), DropZone::Center)));
        assert_eq!(preview.1.iter().find(|(id, _)| *id == 1).unwrap().1.x, 960);
        // A final cursor movement replaces the old target before committing.
        update_drag(&mut drag, Some(start), Some((key, area, 1440, 100)), &surfaces).unwrap();
        assert_eq!(drag.target, Some((key, Some(2), DropZone::Top)));
    }
    #[test]
    fn short_resize_and_failed_release_observations_clear_stale_targets() {
        let (mut drag, surfaces, area) = drag_fixture();
        let key = drag.key;
        let start = drag.start;
        update_drag(&mut drag, Some(start), Some((key, area, 1440, 540)), &surfaces).unwrap();
        assert!(update_drag(&mut drag, Some(start), None, &surfaces).is_none());
        assert!(drag.target.is_none());
        update_drag(&mut drag, Some(start), Some((key, area, 1440, 540)), &surfaces).unwrap();
        assert!(update_drag(&mut drag, None, Some((key, area, 1440, 540)), &surfaces).is_none());
        assert!(drag.target.is_none());
        let resized = Rect { w: start.w + 8, ..start };
        assert!(update_drag(&mut drag, Some(resized), Some((key, area, 1440, 540)), &surfaces).is_none());
        assert!(drag.resize);
        assert!(drag.target.is_none());
    }
    #[test]
    fn cross_monitor_preview_includes_the_reflowed_source() {
        let (mut drag, surfaces, area) = drag_fixture();
        let dest = SurfaceKey { monitor: 2, ..drag.key };
        let dest_area = Rect::new(1920, 0, area.w, area.h);
        let start = drag.start;
        let (bounds, layout) = update_drag(&mut drag, Some(start), Some((dest, dest_area, 2500, 500)), &surfaces).unwrap();
        assert_eq!(bounds, Rect::new(0, 0, 3840, 1080));
        assert!(layout.contains(&(1, dest_area)));
        assert!(layout.contains(&(2, area)));
    }
    #[test]
    fn log_rotation_keeps_one_backup_and_caps_the_active_file() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join(format!("log-test-{}.log", std::process::id()));
        let backup = path.with_extension("log.1");
        std::fs::write(&path, vec![b'x'; 1_048_576]).unwrap();
        append_log(&path, "new event\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new event\n");
        assert_eq!(std::fs::metadata(&backup).unwrap().len(), 1_048_576);
        std::fs::write(&path, vec![b'y'; 1_048_576]).unwrap();
        append_log(&path, "next event\n").unwrap();
        assert_eq!(std::fs::read(&backup).unwrap()[0], b'y');
        std::fs::remove_file(path).unwrap();
        std::fs::remove_file(backup).unwrap();
    }
    #[test]
    fn a_disappearing_drop_target_cancels_the_drop() {
        let mut tree = Tree::new();
        tree.add(1, Rect::new(0, 0, 1920, 1080), None);
        assert!(target_in_tree(Some(&tree), Some(1)));
        tree.remove(1);
        assert!(!target_in_tree(Some(&tree), Some(1)));
        assert!(!target_in_tree(None, Some(1)));
        assert!(target_in_tree(None, None)); // an empty monitor is a valid target
    }
    #[test]
    fn delayed_async_placement_cannot_teach_the_old_size_on_early_polls() {
        let target = Rect::new(0, 0, 600, 500);
        let old = Rect::new(0, 0, 1200, 900);
        let mut p = Placement::new(target);
        assert!(!p.observe(Some(old), Duration::from_millis(80)));
        assert!(!p.observe(Some(old), Duration::from_millis(160)));
        assert!(!p.observe(Some(target), Duration::from_millis(600)));
        assert!(p.observe(Some(target), Duration::from_millis(1500)));
        let refused = Rect::new(0, 0, 800, 500);
        assert!(!p.observe(Some(refused), Duration::from_millis(3000)));
        assert!(p.observe(Some(refused), Duration::from_millis(4500)));
    }
    #[test]
    fn suspended_measurements_do_not_count_as_consecutive_eligible_polls() {
        let target = Rect::new(0, 0, 600, 500);
        let refused = Rect::new(0, 0, 800, 500);
        let mut placement = Placement::new(target);
        assert!(!placement.observe(Some(refused), Duration::from_secs(1)));
        // A maximized, minimized or hung poll interrupts the measurement sequence.
        placement.suspend();
        assert!(!placement.observe(Some(refused), Duration::from_millis(100)));
        assert!(!placement.observe(Some(refused), Duration::from_secs(1)));
        assert!(placement.observe(Some(refused), Duration::from_secs(2)));
    }
    #[test]
    fn log_text_has_no_controls_and_titles_are_bounded() {
        assert_eq!(clean_text("one\ntwo\r\tthree", 7), "one two");
        assert_eq!(clean_text(&"x".repeat(100), 40).len(), 40);
    }
    #[test]
    fn resize_compares_edges_to_the_drag_start() {
        let start = Rect::new(20, 30, 900, 600);
        assert!(!is_resize(start, Rect::new(50, 60, 900, 600)));
        assert!(is_resize(start, Rect::new(20, 30, 908, 600)));
        assert!(is_resize(start, Rect::new(12, 30, 908, 600)));
        assert!(is_resize(start, Rect::new(20, 22, 900, 608)));
        assert!(!is_resize(start, Rect::new(20, 30, 902, 600)));
    }
    #[test]
    fn placed_maximized_and_fullscreen_windows_keep_their_reservation() {
        let tile = Rect::new(0, 0, 1920, 1080);
        assert!(!suspend_placement(false, true, Some(tile), Some(tile)));
        assert!(suspend_placement(false, true, Some(Rect::new(0, 0, 960, 1080)), Some(tile)));
        assert!(suspend_placement(true, false, Some(tile), Some(tile)));
        assert!(!suspend_placement(true, false, None, Some(tile)));
        assert!(!suspend_placement(false, false, Some(tile), Some(tile)));
    }
}
