//! The tiling manager: reacts to window events, keeps one tree per surface (virtual desktop x
//! monitor), applies the layout and handles drag & drop with a live preview.

use crate::config::Config;
use crate::platform::{hwnd_of, id_of, managed_snap, prepare_snap_recovery, restore_owned_snap, Platform, SurfaceKey, Win};
use std::time::{Duration, Instant};
use crate::tree::{zone_at, DropZone, Edge, Hwnd, Rect, Tree};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::c_void;
use std::io::Write;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
use windows::Win32::Graphics::Gdi::{BeginPaint, CreateBitmap, DeleteObject, EndPaint, FillRect, GetStockObject, RedrawWindow, SetDCBrushColor, DC_BRUSH, RDW_INVALIDATE, RDW_UPDATENOW, HBRUSH, HDC, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, RegisterHotKey, MOD_NOREPEAT, MOD_WIN, VK_LBUTTON, VK_RBUTTON, VK_MBUTTON, VK_XBUTTON1, VK_XBUTTON2};
use windows::Win32::UI::Shell::{ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW};
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_EVENT: u32 = WM_APP + 1;
const WM_TRAY: u32 = WM_APP + 2;
const WM_PENDING: u32 = WM_APP + 3;
/// A worker thread left a tray balloon text in `update::take_note`.
const WM_UPDATE_NOTE: u32 = WM_APP + 4;
/// A worker verified the installer; Tilework shuts down and the installer is started.
const WM_UPDATE_INSTALL: u32 = WM_APP + 5;
/// Tray callback sent when the user clicks a balloon (`NIN_BALLOONUSERCLICK`).
const NIN_BALLOONUSERCLICK: u32 = WM_USER + 5;
const TIMER_RECONCILE: usize = 1;
const TIMER_DRAG: usize = 2;
const TIMER_POLL: usize = 3;
const TIMER_HOVER: usize = 4;
const TIMER_UPDATE: usize = 5;
const HOTKEY_TOGGLE: i32 = 1;
const MENU_TOGGLE: usize = 1;
const MENU_EXIT: usize = 2;
const MENU_OPEN_CONFIG: usize = 3;
const MENU_AUTOSTART: usize = 4;
const MENU_CHECK_UPDATE: usize = 5;
const MENU_RESTART_ADMIN: usize = 6;
const MENU_ADMIN_INFO: usize = 7;
const MENU_INSTALL_UPDATE: usize = 8;
const MENU_AUTO_UPDATE: usize = 9;
const MENU_FFM_OFF: usize = 10;
const MENU_FFM_BASE: usize = 20;
const FFM_PRESETS: [u64; 5] = [150, 300, 500, 800, 1200];
const ACCENT: u32 = 0x00FF823C; // BGR: a clear blue
const DWMWA_BORDER_COLOR: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(34);
const COLOR_DEFAULT: u32 = 0xFFFF_FFFF;
/// Colour key of the layered overlays: pixels painted in it are fully transparent.
const COLOR_KEY: u32 = 0x00FF00FF;
/// Cursor distance from the screen border that counts as "drop at the outer edge".
const EDGE_PX: i32 = 36;

fn close(a: Rect, b: Rect) -> bool {
    (a.x - b.x).abs() <= 1 && (a.y - b.y).abs() <= 1 && (a.w - b.w).abs() <= 1 && (a.h - b.h).abs() <= 1
}

struct Placement {
    target: Rect,
    measured: Option<Rect>,
    requested: Instant,
    retries: u8,
    confirmation: Option<Rect>,
}

impl Placement {
    fn new(target: Rect) -> Self { Self { target, measured: None, requested: Instant::now(), retries: 0, confirmation: None } }

    fn suspend(&mut self) {
        self.confirmation = None;
        self.retries = 0;
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

struct MonitorMove {
    from: SurfaceKey,
    to: SurfaceKey,
    started: Instant,
}

struct LearnedMinimum {
    size: (i32, i32),
    dpi: u32,
}

struct Drag {
    hwnd: Hwnd,
    key: SurfaceKey,
    start: Rect,
    resize: bool,
    move_only: bool,
    intent: Option<bool>,
    dpi: u32,
    monitor_changed: bool,
    released_since: Option<Instant>,
    /// (surface, window under the cursor or None for an outer edge / empty monitor, zone)
    target: Option<(SurfaceKey, Option<Hwnd>, DropZone)>,
    /// The last chosen target and where the cursor was when it was chosen (hysteresis against flicker).
    last: Option<(DropTarget, (i32, i32))>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingEvent { msg: u32, wp: usize, lp: isize }

pub struct App<P: Win = Platform> {
    p: P,
    /// Virtual desktops on which tiling is switched off (every desktop is on by default).
    disabled: HashSet<u128>,
    cfg: Config,
    /// Window under the cursor, the focused window at that moment, and since when.
    hover: Option<(Hwnd, Hwnd, Instant)>,
    hover_attempted: Option<Hwnd>,
    hover_foreground: Option<(Hwnd, (i32, i32))>,
    hover_attempt_position: Option<(i32, i32)>,
    ghost_seen: HashSet<Hwnd>,
    reserved: HashSet<Hwnd>,
    /// The virtual desktop that is currently shown.
    shown: u128,
    desktop_error_logged: bool,
    dispatch_desktop: Cell<Option<u128>>,
    dispatching: bool,
    /// Minimum sizes some windows insist on, learned from windows that refuse to shrink.
    mins: HashMap<Hwnd, LearnedMinimum>,
    /// Last target we placed a window at, and what we measured afterwards.
    placed: HashMap<Hwnd, Placement>,
    monitor_moves: HashMap<Hwnd, MonitorMove>,
    /// Windows that currently fit nowhere (they stay floating).
    floating: HashSet<Hwnd>,
    surfaces: HashMap<SurfaceKey, Surface>,
    last_tracked_focus: Option<Hwnd>,
    bordered: Option<Hwnd>,
    border_original: HashMap<Hwnd, u32>,
    taskbar_created: u32,
    drag: Option<Drag>,
    overlay: Overlay,
    focus_border: Overlay,
    /// The window whose own location and visibility changes are followed (0 = none), and the
    /// process those per-process event hooks are registered for.
    followed: Hwnd,
    follow_pid: u32,
    follow_hooks: Vec<HWINEVENTHOOK>,
    main: HWND,
    filter: Option<String>,
    icon_on: HICON,
    icon_off: HICON,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static MAIN_HANDLE: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    static PENDING: RefCell<VecDeque<PendingEvent>> = const { RefCell::new(VecDeque::new()) };
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
    static MENU_OPEN: Cell<bool> = const { Cell::new(false) };
    static TRAY_ADDED: Cell<bool> = const { Cell::new(false) };
    /// Mirror of `App::followed` for the event hook callback, which cannot borrow the app.
    static FOLLOWED: Cell<isize> = const { Cell::new(0) };
    /// What the drag overlay paints: the screen area and (rectangle, is the dragged window) pairs.
    static OVERLAY: RefCell<(Rect, Vec<(Rect, bool)>)> = const { RefCell::new((Rect::new(0, 0, 0, 0), Vec::new())) };
}

/// What an overlay window shows right now, as far as Tilework knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OverlayState {
    /// The system may have moved or shown it (display change): the next update must not be skipped.
    Unknown,
    Hidden,
    Shown(Rect),
}

/// Win32 work an overlay update needs: reposition/raise the window, and repaint its content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OverlayPlan { position: bool, repaint: bool }

/// Decides the minimum Win32 work for showing an overlay at `area`. `raise` re-asserts the topmost
/// z-order; `repaint` means the content changed. A move with the same size keeps the content.
fn overlay_plan(state: OverlayState, area: Rect, raise: bool, repaint: bool) -> OverlayPlan {
    let same_size = matches!(state, OverlayState::Shown(r) if (r.w, r.h) == (area.w, area.h));
    OverlayPlan { position: raise || state != OverlayState::Shown(area), repaint: repaint || !same_size }
}

/// A click-through, never-activating layered window that remembers what it shows, so repeated
/// updates with the same geometry cost no window-manager calls at all.
struct Overlay {
    hwnd: HWND,
    state: OverlayState,
    /// Window-manager calls issued (SetWindowPos, RedrawWindow, ShowWindow), for diagnostics.
    calls: u32,
}

impl Overlay {
    fn new(hwnd: HWND) -> Self { Self { hwnd, state: OverlayState::Hidden, calls: 0 } }

    fn show(&mut self, area: Rect, raise: bool, repaint: bool) {
        if area.w <= 0 || area.h <= 0 { self.hide(); return; }
        let plan = overlay_plan(self.state, area, raise, repaint);
        self.state = OverlayState::Shown(area);
        self.calls += u32::from(plan.position) + u32::from(plan.repaint);
        if self.hwnd.is_invalid() { return; }
        unsafe {
            if plan.position {
                let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), area.x, area.y, area.w, area.h, SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_NOOWNERZORDER);
            }
            // Paint now rather than when the message loop gets to it, so a moved or resized overlay
            // never shows stale content. No erase: painting fully covers the client area.
            if plan.repaint { let _ = RedrawWindow(Some(self.hwnd), None, None, RDW_INVALIDATE | RDW_UPDATENOW); }
        }
    }

    /// Idempotent: hiding a hidden overlay issues no call.
    fn hide(&mut self) {
        if self.state == OverlayState::Hidden { return; }
        self.state = OverlayState::Hidden;
        self.calls += 1;
        if self.hwnd.is_invalid() { return; }
        unsafe { let _ = ShowWindow(self.hwnd, SW_HIDE); }
    }

    /// The system may have moved, resized or shown the window behind our back.
    fn forget(&mut self) { self.state = OverlayState::Unknown; }
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
    if crate::config::safe_path(&dir).is_err() { return; }
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("tilework.log");
    let date = unsafe { windows::Win32::System::SystemInformation::GetSystemTime() };
    let msg = clean_text(msg, 500);
    let line = format!("[{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC] {msg}\n", date.wYear, date.wMonth, date.wDay, date.wHour, date.wMinute, date.wSecond);
    let _ = append_log(&path, &line);
}

fn append_log(path: &std::path::Path, line: &str) -> std::io::Result<()> {
    let _guard = crate::config::guard_parent(path)?;
    crate::config::safe_path(path)?;
    crate::config::safe_path(&path.with_extension("log.1"))?;
    if std::fs::metadata(path).map(|m| m.len().saturating_add(line.len() as u64) > 1_048_576).unwrap_or(false) {
        let backup = path.with_extension("log.1");
        match std::fs::remove_file(&backup) {
            Ok(()) => {},
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
            Err(e) => return Err(e),
        }
        std::fs::rename(path, backup)?;
    }
    {
        use std::os::windows::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new().create(true).append(true).custom_flags(0x00200000).open(path)?;
        use std::os::windows::{fs::MetadataExt, io::AsRawHandle};
        use windows::Win32::Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION};
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(windows::Win32::Foundation::HANDLE(file.as_raw_handle()), &mut info).map_err(std::io::Error::other)?; }
        if info.nNumberOfLinks > 1 { return Err(std::io::Error::other("hard-linked log file")); }
        if file.metadata()?.file_attributes() & 0x400 != 0 { return Err(std::io::Error::other("reparse point log file")); }
        file.write_all(line.as_bytes())
    }
}

fn set_timer(main: HWND, id: usize, ms: u32) {
    if main.is_invalid() { return; }
    unsafe { if SetTimer(Some(main), id, ms, None) == 0 { log(&format!("could not create timer {id}")); } }
}

fn notify(action: windows::Win32::UI::Shell::NOTIFY_ICON_MESSAGE, data: &NOTIFYICONDATAW) {
    if data.hWnd.is_invalid() { return; }
    unsafe { if !Shell_NotifyIconW(action, data).as_bool() { log("tray icon operation failed"); } }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Modal message from a worker thread or before the window exists.
pub fn message_box(title: &str, text: &str) {
    let (title, text) = (wide(title), wide(text));
    unsafe { MessageBoxW(None, PCWSTR(text.as_ptr()), PCWSTR(title.as_ptr()), MB_OK | MB_ICONWARNING | MB_SETFOREGROUND); }
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

impl<P: Win> App<P> {
    fn current_desktop(&self) -> u128 {
        if let Some(d) = self.dispatch_desktop.get() { return d; }
        let d = self.p.current_desktop();
        if self.dispatching { self.dispatch_desktop.set(Some(d)); }
        d
    }
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

    fn tree_minima(&self) -> HashMap<Hwnd, (i32, i32)> {
        self.mins.iter().map(|(&h, min)| (h, min.size)).collect()
    }

    /// Brings the trees in line with the windows that really exist, then lays out what changed.
    fn reconcile(&mut self, force_all: bool) {
        let cur = self.current_desktop();
        if cur == 0 {
            self.cancel_drag();
            let was_known = self.shown != 0;
            self.shown = 0;
            self.hover = None;
            if let Some(h) = self.bordered.take() { self.set_border(h, COLOR_DEFAULT); }
            self.hide_focus_border();
            self.sync_snap();
            if was_known { self.update_tray(); }
            if !self.desktop_error_logged { log("current virtual desktop unavailable; tiling suspended"); self.desktop_error_logged = true; }
            return;
        }
        self.desktop_error_logged = false;
        if cur != self.shown {
            self.cancel_drag();
            log(&format!("desktop switch {:x} -> {:x} (tiling {})", self.shown, cur, if self.desktop_on(cur) { "on" } else { "off" }));
            self.shown = cur;
            self.sync_snap();
            self.update_tray();
        }
        let tops = self.p.all_top_level();
        let ghosts: HashSet<_> = tops.iter().copied().filter(|h| self.p.is_ghost(*h, cur)).collect();
        let gone_ghosts: HashSet<_> = ghosts.intersection(&self.ghost_seen).copied().collect();
        self.ghost_seen = ghosts;
        let live: HashSet<_> = tops.iter().copied().filter(|h| !gone_ghosts.contains(h)
            && (self.p.is_visible(*h, cur) || self.ghost_seen.contains(h))).collect();
        self.reserved.retain(|h| live.contains(h));
        for h in self.placed.keys().copied().filter(|h| live.contains(h)) {
            if suspend_placement(self.p.is_maximized(h), self.p.is_fullscreen(h), self.placed.get(&h).map(|p| p.target), self.p.visible_rect(h)) {
                self.reserved.insert(h);
            }
        }
        self.mins.retain(|h, min| {
            if !tops.contains(h) { return false; }
            let dpi = self.p.dpi(*h);
            if dpi != 0 && dpi != min.dpi {
                self.placed.remove(h);
                false
            } else { true }
        });
        self.border_original.retain(|h, _| tops.contains(h));
        let mut current: HashMap<Hwnd, (SurfaceKey, Rect)> = tops
            .iter()
            .rev()
            .copied()
            .filter(|&h| {
                let tracked = self.surfaces.values().any(|s| s.tree.contains(h));
                live.contains(&h) && ((tracked && (self.p.is_minimized(h) || self.p.is_maximized(h) || self.p.is_fullscreen(h) || self.reserved.contains(&h)))
                    || tracked && self.ghost_seen.contains(&h) || self.managed(h, cur) && !self.p.is_fullscreen(h))
            })
            .map(|h| (h, self.p.surface_of(h)))
            .filter(|&(_, (k, _))| self.desktop_on(k.desktop))
            .collect();
        // An asynchronous placement may still report the source monitor after a committed drop.
        self.monitor_moves.retain(|h, pending| {
            let Some(observed) = current.get_mut(h) else { return false; };
            let Some(destination) = self.surfaces.get(&pending.to) else { return false; };
            if observed.0 != pending.from || pending.started.elapsed() >= Duration::from_secs(3) {
                return false;
            }
            *observed = (pending.to, destination.area);
            true
        });
        let mut dirty: HashSet<SurfaceKey> = HashSet::new();
        if force_all {
            dirty.extend(self.surfaces.keys().copied());
        }
        let minima = self.tree_minima();
        for s in self.surfaces.values_mut() {
            s.tree.mins = minima.clone();
            s.tree.skipped = tops.iter().copied().filter(|h| self.p.is_minimized(*h)).collect();
        }

        // Paused desktops retain their manual trees until enabled again.
        for (key, s) in self.surfaces.iter_mut() {
            if self.disabled.contains(&key.desktop) {
                for h in s.tree.leaves() { if !tops.contains(&h) { s.tree.remove(h); } }
                continue;
            }
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
            let focused = self.last_tracked_focus;
            let info = if self.cfg.verbose_log { format!("{h:x} {} ({})", clean_text(&self.p.title(h), 40), self.p.exe_name(h)) } else { format!("{h:x}") };
            let s = self.surfaces.entry(key).or_insert_with(|| Surface { tree: Tree::new(), area });
            s.area = area;
            s.tree.mins = minima.clone();
            s.tree.skipped = tops.iter().copied().filter(|h| self.p.is_minimized(*h)).collect();
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
        self.refresh_focus();
    }

    fn apply(&mut self, key: SurfaceKey, force: bool) {
        if self.shown == 0 || key.desktop != self.shown || !self.desktop_on(key.desktop) { return; }
        let Some(s) = self.surfaces.get_mut(&key) else { return };
        // A monitor query that fails mid display change reports an empty work area. Laying out
        // into it would shrink every window to its minimum; wait for the next poll instead.
        if s.area.w <= 0 || s.area.h <= 0 {
            log(&format!("apply skipped for monitor {:x}: empty work area", key.monitor));
            return;
        }
        s.tree.skipped = s.tree.leaves().into_iter().filter(|h| self.p.is_minimized(*h)).collect();
        let area = s.area;
        let layout = s.tree.layout(area);
        let mut learned = false;
        for (h, r) in layout {
            let vis = self.p.visible_rect(h);
            let was_placed = self.placed.get(&h).map(|p| p.target);
            if suspend_placement(self.p.is_maximized(h), self.p.is_fullscreen(h), was_placed, vis) {
                self.reserved.insert(h);
            }
            if self.p.is_minimized(h) || self.p.is_hung(h) || self.reserved.contains(&h) && (self.p.is_maximized(h) || self.p.is_fullscreen(h)) {
                if force { log(&format!("apply skipped {h:x}: minimized={} hung={} reserved={}", self.p.is_minimized(h), self.p.is_hung(h), self.reserved.contains(&h))); }
                if let Some(p) = self.placed.get_mut(&h) { p.suspend(); }
                continue;
            }
            if self.reserved.remove(&h) { self.placed.remove(&h); }
            if vis.is_some_and(|v| close(v, r)) {
                self.placed.insert(h, Placement::new(r));
                continue;
            }
            if force {
                self.placed.remove(&h);
                log(&format!("apply forced {h:x} target={}x{}@{},{} now={}", r.w, r.h, r.x, r.y,
                    vis.map(|v| format!("{}x{}@{},{}", v.w, v.h, v.x, v.y)).unwrap_or_else(|| "none".into())));
            }
            if let Some(p) = self.placed.get_mut(&h) && p.target == r {
                let elapsed = p.requested.elapsed();
                let stable = p.observe(vis, elapsed); // Exactly one observation per poll.
                if elapsed < Duration::from_millis(500) { continue; }
                let refusal = vis.filter(|v| (v.x - r.x).abs() <= 1 && (v.y - r.y).abs() <= 1 && (v.w > r.w + 2 || v.h > r.h + 2));
                if stable && p.confirmation == refusal && let Some(v) = refusal {
                    let dpi = self.p.dpi(h);
                    let old = self.mins.get(&h).map(|min| min.size).unwrap_or((0, 0));
                    let new = (old.0.max(if v.w > r.w + 2 { v.w.min(area.w) } else { 0 }), old.1.max(if v.h > r.h + 2 { v.h.min(area.h) } else { 0 }));
                    if old != new && dpi != 0 {
                        self.mins.insert(h, LearnedMinimum { size: new, dpi });
                        learned = true;
                        log(&format!("{h:x} needs at least {}x{} at {dpi} DPI", new.0, new.1));
                    }
                    continue;
                }
                if refusal.is_some() && !stable { continue; }
                // Retry a displaced window, or explicitly confirm a possible size refusal.
                // Three quick attempts are followed by a cooldown, then a fresh budget.
                if p.retries >= 3 && elapsed < Duration::from_secs(30) { continue; }
                if self.p.place(h, r) {
                    let retries = if p.retries >= 3 { 1 } else { p.retries + 1 };
                    let mut next = Placement::new(r);
                    next.retries = retries;
                    next.confirmation = refusal;
                    *p = next;
                }
                continue;
            }
            let placed_ok = self.p.place(h, r);
            if force || !placed_ok { log(&format!("place {h:x} -> {}x{}@{},{} accepted={placed_ok}", r.w, r.h, r.x, r.y)); }
            if placed_ok { self.placed.insert(h, Placement::new(r)); }
        }
        if learned {
            let minima = self.tree_minima();
            for s in self.surfaces.values_mut() { s.tree.mins = minima.clone(); }
            if let Some(s) = self.surfaces.get_mut(&key) {
                s.tree.reseat(s.area);
                // Place changed targets only; never sample a window twice in this poll.
                for (h, r) in s.tree.layout(s.area) {
                    if self.placed.get(&h).is_some_and(|p| p.target == r) || self.reserved.contains(&h) || self.p.is_minimized(h) || self.p.is_hung(h) { continue; }
                    if self.p.place(h, r) { self.placed.insert(h, Placement::new(r)); }
                }
            }
        }
    }

    /// Windows' own snapping must not fight with us: it is off while tiling is on for the shown desktop.
    fn sync_snap(&self) {
        if self.main.is_invalid() { return; }
        managed_snap(self.shown == 0 || !self.desktop_on(self.shown));
    }

    fn set_border(&mut self, h: Hwnd, color: u32) {
        if self.main.is_invalid() { return; }
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

    fn observe_foreground(&mut self, h: Hwnd) {
        if self.hover_foreground.map(|(fg, _)| fg) != Some(h) {
            self.hover_foreground = self.p.cursor_position().map(|pos| (h, pos));
            self.hover = None;
        }
    }

    fn refresh_focus(&mut self) {
        let h = self.p.foreground_window();
        self.observe_foreground(h);
        let tracked = h != 0 && self.shown != 0 && self.current_desktop() == self.shown && self.desktop_on(self.shown) && self.is_tracked_here(h);
        // Followed even while maximized or minimized, so that restoring it brings the border back.
        self.follow(if tracked { h } else { 0 });
        if !tracked || self.p.is_minimized(h) || self.p.is_maximized(h) || self.p.is_fullscreen(h) {
            if let Some(prev) = self.bordered.take() { self.set_border(prev, COLOR_DEFAULT); }
            self.hide_focus_border();
            return;
        }
        self.last_tracked_focus = Some(h);
        let new_target = self.bordered != Some(h);
        if new_target {
            if let Some(prev) = self.bordered.replace(h) { self.set_border(prev, COLOR_DEFAULT); }
            self.set_border(h, ACCENT);
        }
        if self.drag.is_some() { self.hide_focus_border(); }
        else { self.show_focus_border(h, new_target); }
    }

    /// Registers location and visibility hooks for the process of `h` (0 = none), so the focus
    /// border follows the window at once instead of on the next poll. Hooks are per process to keep
    /// the event volume low; re-registered only when the process changes.
    fn follow(&mut self, h: Hwnd) {
        self.followed = h;
        FOLLOWED.with(|f| f.set(h));
        if self.main.is_invalid() { return; }
        let mut pid = 0u32;
        if h != 0 { unsafe { GetWindowThreadProcessId(hwnd_of(h), Some(&mut pid)); } }
        if pid == self.follow_pid && (pid == 0 || !self.follow_hooks.is_empty()) { return; }
        for hook in self.follow_hooks.drain(..) { unsafe { let _ = UnhookWinEvent(hook); } }
        self.follow_pid = pid;
        if pid == 0 { return; }
        // object hide, object location change
        for ev in [0x8003u32, 0x800B] {
            let hook = unsafe { SetWinEventHook(ev, ev, None, Some(follow_event_proc), pid, 0, WINEVENT_OUTOFCONTEXT) };
            if hook.is_invalid() { log(&format!("could not follow window events {ev:x} of process {pid}")); }
            else { self.follow_hooks.push(hook); }
        }
    }

    /// The followed window moved, resized, was hidden, cloaked or destroyed: update the border now.
    fn follow_focus(&mut self, h: Hwnd, event: u32) {
        if h == 0 || h != self.followed || self.drag.is_some() { return; }
        if matches!(event, 0x8001 | 0x8003 | 0x8017) {
            if let Some(b) = self.bordered.take() { self.set_border(b, COLOR_DEFAULT); }
            self.hide_focus_border();
            return;
        }
        if self.bordered == Some(h) && !self.p.is_minimized(h) && !self.p.is_maximized(h) && !self.p.is_fullscreen(h) {
            self.show_focus_border(h, false);
        } else {
            self.refresh_focus();
        }
    }

    /// Switches tiling on or off for the desktop that is shown right now.
    fn toggle(&mut self) {
        let d = self.current_desktop();
        if d == 0 { self.reconcile(false); self.update_tray(); return; }
        if !self.disabled.remove(&d) {
            self.disabled.insert(d);
        }
        let on = self.desktop_on(d);
        log(&format!("tiling {} for desktop {d:x}", if on { "ON" } else { "OFF" }));
        self.shown = d;
        self.cancel_drag();
        if let Some(b) = self.bordered.take() {
            self.set_border(b, COLOR_DEFAULT);
        }
        self.hide_focus_border();
        self.sync_snap();
        if on {
            self.reconcile(true);
        }
        self.update_tray();
    }

    fn cancel_drag(&mut self) {
        self.drag = None;
        if !self.main.is_invalid() { unsafe { let _ = KillTimer(Some(self.main), TIMER_DRAG); } }
        self.hide_overlay();
    }

    fn drag_desktop_valid(&self, desktop: u128) -> bool {
        valid_drag_desktop(desktop, self.shown, self.current_desktop(), self.desktop_on(desktop))
    }

    fn poll_drag_release(&mut self) {
        if let Some(d) = &self.drag {
            if !self.drag_desktop_valid(d.key.desktop) { self.cancel_drag(); }
            else {
                let swapped = unsafe { GetSystemMetrics(SM_SWAPBUTTON) != 0 };
                let button = if swapped { VK_RBUTTON } else { VK_LBUTTON };
                let down = unsafe { GetAsyncKeyState(button.0 as i32) < 0 };
                let released = drag_released(self.p.move_size_active(d.hwnd), down);
                let d = self.drag.as_mut().expect("drag was checked");
                if released_for_timeout(&mut d.released_since, !released, Instant::now()) {
                    let h = d.hwnd;
                    self.drag_end(h);
                }
            }
        }
    }

    fn poll_tick(&mut self) {
        if !TRAY_ADDED.with(Cell::get) { self.add_tray(true); }
        self.poll_drag_release();
        if self.drag.is_none() { self.reconcile(false); }
    }

    fn refresh_surfaces(&mut self) {
        self.monitor_moves.clear();
        self.overlay.forget();
        self.focus_border.forget();
        if self.current_desktop() == 0 { self.reconcile(true); return; }
        self.cancel_drag();
        let tops = self.p.all_top_level();
        let old = std::mem::take(&mut self.surfaces);
        let mut old: Vec<_> = old.into_iter().collect();
        old.sort_by_key(|(old_key, surface)| {
            !surface.tree.leaves().into_iter().find(|h| tops.contains(h))
                .is_some_and(|h| self.p.surface_of(h).0 == *old_key)
        });
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
        let move_only = self.p.is_maximized(h);
        let v = if move_only { self.surfaces[&key].tree.rect_of(self.surfaces[&key].area, h).unwrap_or(v) } else { v };
        let intent = self.p.resize_intent(h);
        log(&format!("drag start {h:x} class={} rect={}x{}@{},{} intent={intent:?} move_only={move_only}", self.p.class_name(h), v.w, v.h, v.x, v.y));
        self.hide_focus_border();
        self.overlay.calls = 0;
        self.drag = Some(Drag { hwnd: h, key, start: v, resize: false, move_only, intent, dpi: self.p.dpi(h), monitor_changed: false, released_since: None, target: None, last: None });
        set_timer(self.main, TIMER_DRAG, 30);
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

    fn drag_tick(&mut self) -> Option<Rect> {
        let d = self.drag.as_ref()?;
        let (h, home) = (d.hwnd, d.key);
        if !self.drag_desktop_valid(home.desktop) { self.cancel_drag(); return None; }
        let visible = self.p.visible_rect(h);
        let changed = self.p.monitor_of(h).0 != home.monitor || self.p.dpi(h) != d.dpi;
        if let Some(d) = self.drag.as_mut() {
            d.monitor_changed |= changed;
            if d.monitor_changed { d.resize = false; }
        }
        let cursor = self.p.cursor_surface(self.shown);
        let preview = update_drag(self.drag.as_mut().expect("drag was checked"), visible, cursor, &self.surfaces);
        match preview {
            Some((bounds, layout)) => self.show_overlay(bounds, &layout, h),
            None => self.hide_overlay(),
        }
        cursor.map(|(_, area, _, _)| area)
    }

    fn drag_end(&mut self, h: Hwnd) {
        if self.drag.as_ref().map(|d| d.hwnd) != Some(h) { return; }
        self.finish_drag(h);
        // drag_start hid the focus border; every outcome (drop, resize, snap back) brings it back,
        // also when the window ends exactly at its tile and no location event follows.
        self.refresh_focus();
    }

    fn finish_drag(&mut self, h: Hwnd) {
        // Release can arrive before the first 30 ms tick or after the last cursor move.
        let drop_area = self.drag_tick();
        let Some(d) = self.drag.take() else { return };
        self.cancel_drag();
        if d.hwnd != h {
            return;
        }
        let home = d.key;
        if !self.drag_desktop_valid(home.desktop) { return; }
        log(&format!("drag end {h:x} start={}x{}@{},{} end={} resize={} intent={:?} target={:?} overlay_calls={}", d.start.w, d.start.h, d.start.x, d.start.y,
            self.p.visible_rect(h).map(|r| format!("{}x{}@{},{}", r.w, r.h, r.x, r.y)).unwrap_or_default(), d.resize, d.intent, d.target.map(|t| t.2), self.overlay.calls));
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
        let cur = self.current_desktop();
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
                        if let Some(area) = drop_area {
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
            let observed = self.p.surface_of(h).0;
            if observed.desktop == key.desktop && observed.monitor != key.monitor {
                self.monitor_moves.insert(h, MonitorMove { from: observed, to: key, started: Instant::now() });
            }
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
            self.observe_foreground(fg);
            if self.hover_attempted != Some(under) { self.hover_attempted = None; }
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
            if self.hover_ready(under, fg, Instant::now()) { self.focus_window(under); }
        }
    }

    fn hover_ready(&mut self, under: Hwnd, fg: Hwnd, now: Instant) -> bool {
        self.observe_foreground(fg);
        if self.hover_attempted != Some(under) { self.hover_attempted = None; }
        let Some(pos) = self.p.cursor_position() else { self.hover = None; return false };
        if self.hover_foreground.is_none_or(|(_, anchor)| !cursor_moved(anchor, pos))
            || self.hover_attempt_position.is_some_and(|anchor| !cursor_moved(anchor, pos)) {
            self.hover = None;
            return false;
        }
        if self.hover_attempted != Some(under) { self.hover_attempted = None; }
        match self.hover {
            Some((h, f, since)) if h == under && f == fg => {
                if now.saturating_duration_since(since) < Duration::from_millis(self.cfg.focus_delay_ms) || self.hover_attempted == Some(under) { return false; }
                if self.current_desktop() != self.shown { self.hover = None; return false; }
                self.hover_attempted = Some(under);
                self.hover_attempt_position = Some(pos);
                true
            }
            _ => { self.hover = Some((under, fg, now)); false }
        }
    }

    fn focus_window(&self, h: Hwnd) -> bool {
        unsafe {
            use windows::Win32::UI::Input::KeyboardAndMouse::{SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEINPUT, MOUSEEVENTF_MOVE};
            let input = INPUT { r#type: INPUT_MOUSE, Anonymous: INPUT_0 { mi: MOUSEINPUT { dwFlags: MOUSEEVENTF_MOVE, ..Default::default() } } };
            if SendInput(&[input], std::mem::size_of::<INPUT>() as i32) != 1 { return false; }
            SetForegroundWindow(hwnd_of(h)).as_bool()
        }
    }

    // ---------- overlay & tray ----------

    /// Shows the complete layout that a drop would produce: every window as a thin frame, the
    /// dragged window highlighted. That way it is clear what happens to the neighbours too.
    fn show_overlay(&mut self, area: Rect, layout: &[(Hwnd, Rect)], dragged: Hwnd) {
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
        // A new target re-asserts the topmost position; an unchanged preview costs nothing.
        self.overlay.show(area, changed, changed);
    }

    fn hide_overlay(&mut self) {
        self.overlay.hide();
    }

    /// Frames the focused window. Unchanged geometry costs no call; only a new target is raised.
    fn show_focus_border(&mut self, h: Hwnd, raise: bool) {
        match self.p.visible_rect(h) {
            Some(r) => self.focus_border.show(r, raise, false),
            None => self.focus_border.hide(),
        }
    }

    fn hide_focus_border(&mut self) {
        self.focus_border.hide();
    }

    fn tray_data(&self) -> NOTIFYICONDATAW {
        let mut nid = NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: self.main, uID: 1, uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP, uCallbackMessage: WM_TRAY, ..Default::default() };
        let on = self.shown != 0 && self.desktop_on(self.shown);
        nid.hIcon = if on { self.icon_on } else { self.icon_off };
        let tip = if self.shown == 0 { "Tilework: paused (desktop identity unavailable)" }
            else if on { "Tilework: ON for this desktop (click to turn off)" }
            else { "Tilework: OFF for this desktop (click to turn on)" };
        let tip = if crate::platform::self_elevated() { format!("{tip} (Administrator)") } else { tip.to_string() };
        for (i, c) in tip.encode_utf16().take(127).enumerate() {
            nid.szTip[i] = c;
        }
        nid
    }

    fn show_balloon(&self, title: &str, text: &str) {
        let mut nid = self.tray_data();
        nid.uFlags |= NIF_INFO;
        nid.dwInfoFlags = NIIF_INFO;
        for (dst, src) in [(&mut nid.szInfoTitle[..63], title), (&mut nid.szInfo[..255], text)] {
            for (i, c) in src.encode_utf16().take(dst.len()).enumerate() { dst[i] = c; }
        }
        notify(NIM_MODIFY, &nid);
    }

    /// Hourly: starts a background check when enabled; the worker decides whether one is due.
    fn update_tick(&self) {
        set_timer(self.main, TIMER_UPDATE, crate::update::RECHECK_TICK_MS);
        if self.cfg.check_updates { check_update_async(self.main, UpdateCheck::Scheduled); }
    }

    fn update_tray(&self) {
        notify(NIM_MODIFY, &self.tray_data());
    }

    /// Explorer can time out while it is busy at sign-in, and then no TaskbarCreated follows: the
    /// tray icon (the only way to exit) would be missing for the whole session. A failed add is
    /// retried from the poll; NIM_MODIFY detects an add that timed out but took effect anyway.
    fn add_tray(&self, retry: bool) {
        if self.main.is_invalid() { return; }
        let data = self.tray_data();
        let added = unsafe { Shell_NotifyIconW(NIM_ADD, &data).as_bool() || Shell_NotifyIconW(NIM_MODIFY, &data).as_bool() };
        TRAY_ADDED.with(|t| t.set(added));
        if !added && !retry { log("could not add the tray icon; retrying"); }
        if added && retry { log("tray icon added after retry"); }
    }

    fn reload_saved_settings(&mut self, result: std::io::Result<()>) {
        match result { Ok(()) => self.cfg = Config::load(), Err(e) => log(&format!("settings update failed: {e}")) }
    }

    /// Modal UI (message boxes, the UAC prompt) is returned as a follow-up and shown by the caller
    /// after the app borrow ends: its nested message loop would otherwise queue every event.
    fn menu_command(&mut self, cmd: usize) -> Option<MenuFollowUp> {
        let app = self;
        unsafe {
            match cmd {
                MENU_TOGGLE => app.toggle(),
                MENU_AUTOSTART => {
                    let enabled = !crate::setup::is_autostart_enabled();
                    match crate::setup::set_autostart(enabled) {
                        Ok(()) => log(if enabled { "autostart entry registered" } else { "autostart entry removed" }),
                        Err(e) => {
                            log(&e);
                            return Some(MenuFollowUp::AutostartError(e));
                        }
                    }
                }
                MENU_CHECK_UPDATE => check_update_async(app.main, UpdateCheck::Manual),
                MENU_INSTALL_UPDATE => install_available_update(app.main),
                MENU_AUTO_UPDATE => {
                    app.cfg.check_updates = !app.cfg.check_updates;
                    app.reload_saved_settings(Config::update_keys(&[("check_updates", if app.cfg.check_updates { "true" } else { "false" })]));
                }
                MENU_RESTART_ADMIN => return Some(MenuFollowUp::RestartAsAdministrator(app.filter.clone())),
                MENU_OPEN_CONFIG => {
                    app.cfg.ensure_exists();
                    return Config::path().map(MenuFollowUp::OpenConfig);
                }
                MENU_FFM_OFF => {
                    app.cfg.focus_follows_mouse = false;
                    app.reload_saved_settings(Config::update_keys(&[("focus_follows_mouse", "false")]));
                }
                c if (MENU_FFM_BASE..MENU_FFM_BASE + FFM_PRESETS.len()).contains(&c) => {
                    app.cfg.focus_follows_mouse = true;
                    app.cfg.focus_delay_ms = FFM_PRESETS[c - MENU_FFM_BASE];
                    app.reload_saved_settings(Config::update_keys(&[("focus_follows_mouse", "true"), ("focus_delay_ms", &app.cfg.focus_delay_ms.to_string())]));
                    log(&format!("focus follows mouse after {} ms", app.cfg.focus_delay_ms));
                }
                MENU_EXIT => {
                    PostQuitMessage(0);
                }
                _ => {}
            }
        }
        None
    }

    fn shutdown(&mut self) {
        restore_owned_snap();
        self.cancel_drag();
        if let Some(b) = self.bordered.take() {
            self.set_border(b, COLOR_DEFAULT);
        }
        self.hide_focus_border();
        self.follow(0);
        notify(NIM_DELETE, &self.tray_data());
        log("exit");
    }
}

fn foreground_has_menu(foreground: HWND) -> bool {
    unsafe extern "system" fn visit(hwnd: HWND, lp: LPARAM) -> windows::core::BOOL {
        callback_guard(|| {
        let mut class = [0u16; 32];
        let len = unsafe { GetClassNameW(hwnd, &mut class) } as usize;
        if class[..len] == [35, 51, 50, 55, 54, 56] && unsafe { IsWindowVisible(hwnd).as_bool() } {
            unsafe { *(lp.0 as *mut bool) = true; }
            return windows::core::BOOL(0);
        }
        windows::core::BOOL(1)
        })
    }
    let mut found = false;
    unsafe { let thread = GetWindowThreadProcessId(foreground, None); if thread != 0 { let _ = EnumThreadWindows(thread, Some(visit), LPARAM(&mut found as *mut bool as isize)); } }
    found
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UpdateCheck { Manual, Scheduled }

fn open_release_page(url: &str) {
    let url = wide(url);
    unsafe {
        let result = ShellExecuteW(None, w!("open"), PCWSTR(url.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
        if result.0 as isize <= 32 {
            log("could not open the official Tilework release page");
            message_box("Tilework Update", "Could not open the browser. Visit github.com/Jas0nOW/Tilework/releases.");
        }
    }
}

fn window_from(raw: isize) -> HWND { HWND(raw as *mut c_void) }

/// Download, verify and hand over to the installer (worker thread, blocking). Tilework itself only
/// exits after the installer is verified; the installer is started from `main` once the
/// single-instance mutex is released. Portable copies cannot be replaced by the per-user installer,
/// so they get the release page instead. An elevated Tilework never starts the installer (it would
/// run as administrator, which the per-user installer rejects, and the browser must not open
/// elevated either): it only shows the address and explains how to update.
fn install_update(main: isize, tag: &str) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static INSTALLING: AtomicBool = AtomicBool::new(false);
    let Some(page) = crate::update::installer_url(tag).map(|_| format!("{}/tag/{tag}", crate::setup::RELEASES_URL)) else { return };
    if crate::platform::self_elevated() {
        message_box("Tilework Update", &format!("Tilework {tag} is available.\nStart Tilework without administrator rights to install it from the tray, or download it from:\n{page}"));
        return;
    }
    if !crate::setup::is_running_installed() { open_release_page(&page); return; }
    if INSTALLING.swap(true, Ordering::SeqCst) { return; }
    crate::update::set_note("Tilework", &format!("Downloading and verifying {tag}..."));
    unsafe { let _ = PostMessageW(Some(window_from(main)), WM_UPDATE_NOTE, WPARAM(0), LPARAM(0)); }
    match crate::update::download_and_verify(tag) {
        Ok(path) => {
            log(&format!("update {tag} verified; shutting down for the installer"));
            crate::update::set_installer(path);
            unsafe { let _ = PostMessageW(Some(window_from(main)), WM_UPDATE_INSTALL, WPARAM(0), LPARAM(0)); }
        }
        Err(e) => {
            INSTALLING.store(false, Ordering::SeqCst);
            log(&format!("update {tag} not installed: {e}"));
            message_box("Tilework Update", &format!("The update was not installed because {e}.
Nothing was changed. You can download it from github.com/Jas0nOW/Tilework/releases."));
        }
    }
}

fn install_available_update(main: HWND) {
    let Some(tag) = crate::update::available_tag() else { return };
    let main = main.0 as isize;
    if let Err(e) = std::thread::Builder::new().spawn(move || install_update(main, &tag)) {
        log(&format!("could not start the update installation: {e}"));
    }
}

fn check_update_async(main: HWND, mode: UpdateCheck) {
    use crate::setup::UpdateStatus;
    use crate::update::{self, UpdateState};
    use std::sync::atomic::{AtomicBool, Ordering};

    static CHECKING: AtomicBool = AtomicBool::new(false);
    if CHECKING.swap(true, Ordering::Relaxed) { return; }
    let main = main.0 as isize;
    if let Err(e) = std::thread::Builder::new().spawn(move || {
        let mut state = update::load_state();
        let now = update::unix_now();
        if mode == UpdateCheck::Scheduled && !update::should_check_now(state.last_check, now) {
            CHECKING.store(false, Ordering::Relaxed);
            return;
        }
        let status = crate::setup::check_for_update();
        // A failed check is retried in an hour instead of waiting a full day.
        state.last_check = Some(if matches!(status, UpdateStatus::Error(_)) { now.saturating_sub(update::DAY_SECS - 3600) } else { now });
        // The per-user installer can only be started by a non-elevated, installed Tilework.
        let elevated = crate::platform::self_elevated();
        let installed = crate::setup::is_running_installed() && !elevated;
        let mut dialog: Option<(String, Option<String>, MESSAGEBOX_STYLE)> = None;
        match status {
            UpdateStatus::Available { tag, release_url } => {
                update::set_available(Some(tag.clone()));
                let notify_user = mode == UpdateCheck::Scheduled && update::should_notify(&state, &tag, now);
                if mode == UpdateCheck::Manual || notify_user {
                    state = UpdateState { notified_tag: Some(tag.clone()), notified_at: Some(now), ..state };
                }
                if notify_user {
                    let action = if elevated { "start Tilework without administrator rights to install it" }
                        else if installed { "click to install" } else { "click to open the release page" };
                    update::set_note(&format!("Tilework {tag} is available"), &format!("Tilework {tag} is available - {action}."));
                    unsafe { let _ = PostMessageW(Some(window_from(main)), WM_UPDATE_NOTE, WPARAM(0), LPARAM(0)); }
                } else if mode == UpdateCheck::Manual {
                    dialog = Some(if elevated {
                        (format!("Tilework {tag} is available.\nStart Tilework without administrator rights to install it from the tray, or download it from:\n{release_url}"), None, MB_OK | MB_ICONINFORMATION)
                    } else if installed {
                        (format!("Tilework {tag} is available.

Yes: download, verify and install it now (Tilework restarts).
No: open the release page.
Cancel: not now."), Some(tag), MB_YESNOCANCEL | MB_ICONINFORMATION)
                    } else {
                        (format!("Tilework {tag} is available. Open the official release page?"), Some(release_url), MB_YESNO | MB_ICONINFORMATION)
                    });
                }
            }
            UpdateStatus::UpToDate => { update::set_available(None); if mode == UpdateCheck::Manual { dialog = Some(("Tilework is up to date.".into(), None, MB_OK | MB_ICONINFORMATION)); } }
            UpdateStatus::NoReleaseFound => { if mode == UpdateCheck::Manual { dialog = Some(("No public stable release found.".into(), None, MB_OK | MB_ICONINFORMATION)); } }
            UpdateStatus::Error(e) => {
                log(&format!("update check failed: {e}"));
                if mode == UpdateCheck::Manual { dialog = Some((format!("Could not check for updates:
{e}"), None, MB_OK | MB_ICONWARNING)); }
            }
        }
        update::save_state(&state);
        CHECKING.store(false, Ordering::Relaxed);
        if let Some((message, target, buttons)) = dialog {
            let text = wide(&message);
            let answer = unsafe { MessageBoxW(None, PCWSTR(text.as_ptr()), w!("Tilework Update"), buttons | MB_SETFOREGROUND) };
            // With three buttons `target` is the tag (install / release page); with two it is the URL.
            let three = buttons == (MB_YESNOCANCEL | MB_ICONINFORMATION);
            match (answer, target) {
                (IDYES, Some(tag)) if three => install_update(main, &tag),
                (IDNO, Some(tag)) if three => open_release_page(&format!("{}/tag/{tag}", crate::setup::RELEASES_URL)),
                (IDYES, Some(url)) if !three => open_release_page(&url),
                _ => {}
            }
        }
    }) {
        CHECKING.store(false, Ordering::Relaxed);
        log(&format!("could not start update check: {e}"));
    }
}

fn open_config(path: &std::path::Path) {
    let path = wide(&path.to_string_lossy());
    unsafe {
        if windows::Win32::UI::Shell::IsUserAnAdmin().as_bool() {
            let _ = MessageBoxW(None, PCWSTR(path.as_ptr()), w!("Settings path (edit from a non-elevated editor)"), MB_OK);
        } else {
            ShellExecuteW(None, w!("open"), PCWSTR(path.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
        }
    }
}

enum MenuFollowUp {
    OpenConfig(std::path::PathBuf),
    AutostartError(String),
    RestartAsAdministrator(Option<String>),
}

fn show_menu() {
    if MENU_OPEN.with(|m| m.replace(true)) { return; }
    let snapshot = APP.with(|a| a.try_borrow().ok().and_then(|g| g.as_ref().map(|app| (app.cfg.clone(), app.main, app.desktop_on(app.shown)))));
    if let Some((cfg, main, on)) = snapshot {
        let cmd = unsafe { build_and_track_menu(&cfg, main, on) };
        let action = APP.with(|a| a.try_borrow_mut().ok().and_then(|mut g| g.as_mut().and_then(|app| app.menu_command(cmd))));
        match action {
            Some(MenuFollowUp::OpenConfig(path)) => open_config(&path),
            Some(MenuFollowUp::AutostartError(e)) => unsafe {
                let message = wide(&e);
                MessageBoxW(Some(main), PCWSTR(message.as_ptr()), w!("Tilework Autostart"), MB_OK | MB_ICONERROR);
            },
            // Only a started elevated instance (UAC accepted) may replace this one.
            Some(MenuFollowUp::RestartAsAdministrator(filter)) if restart_as_administrator(filter.as_deref()) => unsafe { PostQuitMessage(0); },
            _ => {}
        }
    }
    MENU_OPEN.with(|m| m.set(false));
}

/// Starts an elevated copy of this exe via the UAC prompt. The copy waits for this instance to release
/// the single-instance mutex (`--replace`). Returns true only when it was started; a cancelled prompt
/// leaves this instance running.
fn restart_as_administrator(filter: Option<&str>) -> bool {
    use windows::Win32::Foundation::ERROR_CANCELLED;
    use windows::Win32::UI::Shell::{ShellExecuteExW, SHELLEXECUTEINFOW};
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => { log(&format!("restart as administrator: cannot resolve own path: {e}")); return false; }
    };
    let mut args = String::from("--replace");
    if let Some(f) = filter { args.push_str(&format!(" --filter \"{}\"", f.replace('"', ""))); }
    let file = wide(&exe.to_string_lossy());
    let params = wide(&args);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    match unsafe { ShellExecuteExW(&mut info) } {
        Ok(()) => { log("restarting as administrator"); true }
        Err(e) if e.code() == ERROR_CANCELLED.to_hresult() => { log("restart as administrator cancelled"); false }
        Err(e) => { log(&format!("restart as administrator failed: {e}")); false }
    }
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

            if crate::setup::is_running_installed() {
                let autostart_on = crate::setup::is_autostart_enabled();
                if let Err(e) = AppendMenuW(menu, check(autostart_on), MENU_AUTOSTART, w!("Start with Windows")) {
                    log(&format!("could not append tray menu item: {e}"));
                }
            }
            if let Some(tag) = crate::update::available_tag() {
                let verb = if crate::platform::self_elevated() { "Update available:" } else if crate::setup::is_running_installed() { "Install update" } else { "Download update" };
                let label = wide(&format!("{verb} {tag}"));
                if let Err(e) = AppendMenuW(menu, MF_STRING, MENU_INSTALL_UPDATE, PCWSTR(label.as_ptr())) {
                    log(&format!("could not append tray menu item: {e}"));
                }
            }
            if let Err(e) = AppendMenuW(menu, MF_STRING, MENU_CHECK_UPDATE, w!("Check for updates...")) {
                log(&format!("could not append tray menu item: {e}"));
            }
            if let Err(e) = AppendMenuW(menu, check(cfg.check_updates), MENU_AUTO_UPDATE, w!("Check for updates automatically")) {
                log(&format!("could not append tray menu item: {e}"));
            }

            if let Err(e) = AppendMenuW(menu, MF_STRING, MENU_OPEN_CONFIG, w!("Open settings file")) { log(&format!("could not append tray menu item: {e}")); }
            if crate::platform::self_elevated() {
                if let Err(e) = AppendMenuW(menu, MF_STRING | MF_GRAYED, MENU_ADMIN_INFO, w!("Running as administrator")) { log(&format!("could not append tray menu item: {e}")); }
            } else if let Err(e) = AppendMenuW(menu, MF_STRING, MENU_RESTART_ADMIN, w!("Restart as administrator")) {
                log(&format!("could not append tray menu item: {e}"));
            }
            if let Err(e) = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()) { log(&format!("could not append tray menu item: {e}")); }
            if let Err(e) = AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("Exit Tilework")) { log(&format!("could not append tray menu item: {e}")); }
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = SetForegroundWindow(main);
            let cmd = TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_RETURNCMD, pt.x, pt.y, Some(0), main, None).0 as usize;
            let _ = DestroyMenu(menu);
            cmd
        }
}

unsafe extern "system" fn win_event_proc(_h: HWINEVENTHOOK, event: u32, hwnd: HWND, id_object: i32, id_child: i32, _thread: u32, _time: u32) {
    callback_guard(|| {
    if id_object != 0 || id_child != 0 || hwnd.0.is_null() && event != 0x0020 {
        return;
    }
    let main = MAIN_HANDLE.with(Cell::get);
    if !main.is_invalid() {
        unsafe { if PostMessageW(Some(main), WM_EVENT, WPARAM(event as usize), LPARAM(hwnd.0 as isize)).is_err() { queue_pending(WM_EVENT, WPARAM(event as usize), LPARAM(hwnd.0 as isize)); } }
    }
    });
}

/// Per-process hook: forwards only the followed window's own location/visibility changes.
unsafe extern "system" fn follow_event_proc(_h: HWINEVENTHOOK, event: u32, hwnd: HWND, id_object: i32, id_child: i32, _thread: u32, _time: u32) {
    callback_guard(|| {
    if id_object != 0 || id_child != 0 || hwnd.0.is_null() || id_of(hwnd) != FOLLOWED.with(Cell::get) {
        return;
    }
    let main = MAIN_HANDLE.with(Cell::get);
    if !main.is_invalid() {
        unsafe { if PostMessageW(Some(main), WM_EVENT, WPARAM(event as usize), LPARAM(hwnd.0 as isize)).is_err() { queue_pending(WM_EVENT, WPARAM(event as usize), LPARAM(hwnd.0 as isize)); } }
    }
    });
}

unsafe extern "system" fn overlay_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    callback_guard(|| match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_PAINT => {
            paint_overlay(hwnd);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    })
}

/// Fills with the DC's stock brush: no GDI object is created per paint, so none can leak.
fn fill(hdc: HDC, rect: &RECT, color: u32) {
    unsafe {
        SetDCBrushColor(hdc, COLORREF(color));
        FillRect(hdc, rect, HBRUSH(GetStockObject(DC_BRUSH).0));
    }
}

fn paint_overlay(hwnd: HWND) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        fill(hdc, &rc, COLOR_KEY);
        OVERLAY.with(|o| {
            let (origin, items) = &*o.borrow();
            for (r, highlight) in items {
                let inner = RECT { left: r.x - origin.x + 3, top: r.y - origin.y + 3, right: r.right() - origin.x - 3, bottom: r.bottom() - origin.y - 3 };
                if *highlight {
                    fill(hdc, &inner, ACCENT);
                }
                let t = 3;
                let line = if *highlight { 0x00FFFFFF } else { 0x00E0E0E0 };
                for strip in [
                    RECT { bottom: inner.top + t, ..inner },
                    RECT { top: inner.bottom - t, ..inner },
                    RECT { right: inner.left + t, ..inner },
                    RECT { left: inner.right - t, ..inner },
                ] {
                    fill(hdc, &strip, line);
                }
            }
        });
        let _ = EndPaint(hwnd, &ps);
    }
}

unsafe extern "system" fn focus_border_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    callback_guard(|| match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_PAINT => {
            paint_focus_border(hwnd);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    })
}

fn paint_focus_border(hwnd: HWND) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        fill(hdc, &rc, COLOR_KEY);
        let t = 3;
        for strip in [
            RECT { left: 0, top: 0, right: rc.right, bottom: t },
            RECT { left: 0, top: rc.bottom - t, right: rc.right, bottom: rc.bottom },
            RECT { left: 0, top: 0, right: t, bottom: rc.bottom },
            RECT { left: rc.right - t, top: 0, right: rc.right, bottom: rc.bottom },
        ] {
            fill(hdc, &strip, ACCENT);
        }
        let _ = EndPaint(hwnd, &ps);
    }
}

fn queue_pending(msg: u32, wp: WPARAM, lp: LPARAM) -> bool {
    if matches!(msg, WM_EVENT | WM_TIMER | WM_UPDATE_NOTE | WM_HOTKEY | WM_TRAY | WM_DISPLAYCHANGE | WM_SETTINGCHANGE) || msg == TASKBAR_CREATED.with(Cell::get) && msg != 0 {
        PENDING.with(|p| p.borrow_mut().push_back(PendingEvent { msg, wp: wp.0, lp: lp.0 }));
        return true;
    }
    false
}

fn dispatch<P: Win>(app: &mut App<P>, event: PendingEvent) -> bool {
    let PendingEvent { msg, wp, lp } = event;
    if app.taskbar_created != 0 && msg == app.taskbar_created { app.add_tray(false); return true; }
    if msg == WM_EVENT && matches!(wp as u32, 0x0003 | 0x0020 | 0x8017 | 0x8018) { app.p.invalidate_desktop(); }
    if msg == WM_TIMER && wp == TIMER_HOVER && !app.cfg.focus_follows_mouse { return true; }
    app.dispatch_desktop.set(None);
    app.dispatching = true;
    match msg {
        WM_EVENT => match wp as u32 {
            0x0003 => { app.refresh_focus(); set_timer(app.main, TIMER_RECONCILE, 80); },
            0x0020 => { app.reconcile(false); },
            0x000A => app.drag_start(lp),
            0x000B => app.drag_end(lp),
            0x800B => app.follow_focus(lp, 0x800B),
            ev @ (0x8001 | 0x8003 | 0x8017) => { app.follow_focus(lp, ev); set_timer(app.main, TIMER_RECONCILE, 80); },
            _ => set_timer(app.main, TIMER_RECONCILE, 80),
        },
        WM_TIMER => match wp {
            TIMER_RECONCILE => { unsafe { let _ = KillTimer(Some(app.main), TIMER_RECONCILE); } if app.drag.is_none() { app.reconcile(false); } },
            TIMER_DRAG => { let _ = app.drag_tick(); app.poll_drag_release(); },
            TIMER_HOVER => app.hover_tick(),
            TIMER_POLL => app.poll_tick(),
            TIMER_UPDATE => app.update_tick(),
            _ => {},
        },
        WM_DISPLAYCHANGE => app.refresh_surfaces(),
        WM_SETTINGCHANGE if wp == SPI_SETWORKAREA.0 as usize => app.refresh_surfaces(),
        WM_HOTKEY if wp == HOTKEY_TOGGLE as usize => app.toggle(),
        WM_TRAY if lp as u32 == WM_LBUTTONUP => app.toggle(),
        WM_TRAY if lp as u32 == NIN_BALLOONUSERCLICK => install_available_update(app.main),
        WM_UPDATE_NOTE => if let Some((title, text)) = crate::update::take_note() { app.show_balloon(&title, &text); },
        _ => { app.dispatch_desktop.set(None); app.dispatching = false; return false; },
    };
    app.dispatch_desktop.set(None); app.dispatching = false;
    true
}

pub(crate) fn callback_failed() {
    restore_owned_snap();
    log("panic contained at Windows callback; shutting down");
    APP.with(|a| { if let Ok(mut g) = a.try_borrow_mut() && let Some(app) = g.as_mut() { app.shutdown(); } });
    unsafe { PostQuitMessage(1); }
}

fn callback_guard<T: Default>(f: impl FnOnce() -> T) -> T {
    contain_callback(f, callback_failed)
}

fn contain_callback<T: Default>(f: impl FnOnce() -> T, cleanup: impl FnOnce()) -> T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(_) => { cleanup(); T::default() },
    }
}

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    callback_guard(|| main_message(hwnd, msg, wp, lp))
}

fn main_message(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_QUERYENDSESSION { return LRESULT(1); }
    if msg == WM_ENDSESSION && wp.0 != 0 { restore_owned_snap(); unsafe { PostQuitMessage(0); } return LRESULT(0); }
    if msg == WM_CLOSE || msg == WM_UPDATE_INSTALL { unsafe { PostQuitMessage(0); } return LRESULT(0); }
    if msg == WM_PENDING {
        let events = PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()));
        for e in events { main_message(hwnd, e.msg, WPARAM(e.wp), LPARAM(e.lp)); }
        return LRESULT(0);
    }
    if msg == WM_TRAY && lp.0 as u32 == WM_RBUTTONUP {
        if APP.with(|a| a.try_borrow_mut().is_err()) { queue_pending(msg, wp, lp); }
        else { show_menu(); }
        return LRESULT(0);
    }
    let handled = APP.with(|a| {
        let Ok(mut guard) = a.try_borrow_mut() else { return queue_pending(msg, wp, lp); };
        guard.as_mut().is_some_and(|app| dispatch(app, PendingEvent { msg, wp: wp.0, lp: lp.0 }))
    });
    if PENDING.with(|p| !p.borrow().is_empty()) && APP.with(|a| a.try_borrow_mut().is_ok()) {
        unsafe { let _ = PostMessageW(Some(hwnd), WM_PENDING, WPARAM(0), LPARAM(0)); }
    }
    if handled { LRESULT(0) } else { unsafe { DefWindowProcW(hwnd, msg, wp, lp) } }
}

pub fn run(filter: Option<String>) {
    if !prepare_snap_recovery() { log("Snap recovery unavailable; Snap setting will be left unchanged"); }
    let cfg = Config::load();
    crate::tree::set_min_zone(cfg.min_zone_width, cfg.min_zone_height);
    unsafe {
        // A failure here (exhausted desktop heap or USER handle quota, no interactive desktop)
        // must end the start-up cleanly and leave a trace, not panic. Snap is still untouched.
        let hinst = match GetModuleHandleW(None) {
            Ok(h) => h,
            Err(e) => { log(&format!("cannot start: no module handle: {e}")); return; }
        };
        let class_main = w!("TileworkMain");
        let class_overlay = w!("TileworkOverlay");
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(main_proc), hInstance: hinst.into(), lpszClassName: class_main, ..Default::default() });
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(overlay_proc), hInstance: hinst.into(), lpszClassName: class_overlay, ..Default::default() });
        let main = match CreateWindowExW(WINDOW_EX_STYLE(0), class_main, w!("Tilework"), WS_OVERLAPPED, 0, 0, 0, 0, None, None, Some(hinst.into()), None) {
            Ok(h) => h,
            Err(e) => { log(&format!("cannot start: could not create the main window: {e}")); return; }
        };
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
        // Without an overlay, tiling still works; only the drag preview is missing.
        .unwrap_or_else(|e| { log(&format!("could not create the drag preview window: {e}")); HWND::default() });
        let _ = SetLayeredWindowAttributes(overlay, COLORREF(COLOR_KEY), 200, LWA_COLORKEY | LWA_ALPHA);

        let class_focus = w!("TileworkFocusBorder");
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(focus_border_proc), hInstance: hinst.into(), lpszClassName: class_focus, ..Default::default() });
        let focus_border = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class_focus,
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
        .unwrap_or_else(|e| { log(&format!("could not create the focus border window: {e}")); HWND::default() });
        let _ = SetLayeredWindowAttributes(focus_border,COLORREF(COLOR_KEY), 0, LWA_COLORKEY);

        let platform = Platform::new();
        let taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated"));
        if taskbar_created == 0 { log("could not register TaskbarCreated message"); }
        else if ChangeWindowMessageFilterEx(main, taskbar_created, MSGFLT_ALLOW, None).is_err() {
            log("could not allow TaskbarCreated through the elevated message filter");
        }
        TASKBAR_CREATED.with(|id| id.set(taskbar_created));
        let app = App {
            p: platform,
            disabled: HashSet::new(),
            cfg,
            hover: None,
            hover_attempted: None, hover_foreground: None, hover_attempt_position: None, ghost_seen: HashSet::new(),
            reserved: HashSet::new(),
            shown: 0,
            desktop_error_logged: false,
            dispatch_desktop: Cell::new(None), dispatching: false,
            mins: HashMap::new(),
            placed: HashMap::new(),
            monitor_moves: HashMap::new(),
            floating: HashSet::new(),
            surfaces: HashMap::new(),
            last_tracked_focus: None,
            bordered: None,
            border_original: HashMap::new(),
            taskbar_created,
            drag: None,
            overlay: Overlay::new(overlay),
            focus_border: Overlay::new(focus_border),
            followed: 0, follow_pid: 0, follow_hooks: Vec::new(),
            main,
            filter,
            icon_on: make_icon(true),
            icon_off: make_icon(false),
        };
        app.add_tray(false);
        APP.with(|a| *a.borrow_mut() = Some(app));

        // foreground, move/size start+end, minimise start+end, destroy, show, cloak, uncloak
        let mut hooks = Vec::new();
        for ev in [0x0003u32, 0x000A, 0x000B, 0x0016, 0x0017, 0x0020, 0x8001, 0x8002, 0x8017, 0x8018] {
            let hook = SetWinEventHook(ev, ev, None, Some(win_event_proc), 0, 0, 0);
            if hook.is_invalid() { log(&format!("could not register window hook {ev:x}")); }
            else { hooks.push(hook); }
        }
        if RegisterHotKey(Some(main), HOTKEY_TOGGLE, MOD_WIN | MOD_NOREPEAT, 'Y' as u32).is_err() {
            log("could not register Win+Y");
        }
        set_timer(main, TIMER_POLL, 1500);
        set_timer(main, TIMER_HOVER, 50);
        set_timer(main, TIMER_UPDATE, crate::update::FIRST_CHECK_DELAY_MS);
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
        for timer in [TIMER_RECONCILE, TIMER_DRAG, TIMER_POLL, TIMER_HOVER, TIMER_UPDATE] { let _ = KillTimer(Some(main), timer); }
        APP.with(|a| { if let Some(mut app) = a.borrow_mut().take() {
            app.shutdown();
            let _ = DestroyIcon(app.icon_on);
            let _ = DestroyIcon(app.icon_off);
        } });
        let _ = DestroyWindow(overlay);
        let _ = DestroyWindow(focus_border);
        let _ = DestroyWindow(main);
        MAIN_HANDLE.with(|h| h.set(HWND(std::ptr::null_mut())));
    }
}

/// A monitor-filling tile is not an app entering full screen. Test before clearing placement.
fn suspend_placement(maximized: bool, fullscreen: bool, placed: Option<Rect>, visible: Option<Rect>) -> bool {
    (maximized && placed.is_some()) || (fullscreen && !placed.zip(visible).map(|(p, v)| close(p, v)).unwrap_or(false))
}

/// Shared by timer updates and release: failed observations invalidate the previous target.
/// Cursor jitter within this many pixels of where a drop target was chosen does not change it.
const STICKY_PX: i32 = 14;

type DropTarget = (SurfaceKey, Option<Hwnd>, DropZone);

fn preview_of(surfaces: &HashMap<SurfaceKey, Surface>, d: &Drag, area: Rect, target: DropTarget) -> Option<Vec<(Hwnd, Rect)>> {
    let (key, over, zone) = target;
    let h = d.hwnd;
    match (surfaces.get(&key), over) {
        (Some(s), None) => s.tree.preview_edge_layout(area, h, zone),
        (Some(s), Some(t)) if key == d.key => s.tree.preview_layout(area, h, t, zone),
        (Some(s), Some(t)) => s.tree.preview_insert_layout(area, h, t, zone),
        (None, _) => Some(vec![(h, area)]),
    }
}

fn update_drag(d: &mut Drag, visible: Option<Rect>, cursor: Option<(SurfaceKey, Rect, i32, i32)>, surfaces: &HashMap<SurfaceKey, Surface>) -> Option<(Rect, Vec<(Hwnd, Rect)>)> {
    d.target = None;
    let visible = visible?;
    // Move or resize is decided by what the window does and then kept for the whole drag: a resize
    // that comes back to its start edge must not turn into a move (and a drop) half way.
    if d.move_only || d.monitor_changed {
        d.resize = false;
    } else {
        d.resize |= d.intent == Some(true) || is_resize(d.start, visible);
    }
    if d.resize { return None; }
    let (key, area, cx, cy) = cursor?;
    let h = d.hwnd;
    let candidate: DropTarget = match surfaces.get(&key) {
        Some(s) => {
            let layout = s.tree.layout(area);
            if let Some(side) = App::<Platform>::edge_at(area, cx, cy) {
                (key, None, side)
            } else if key != d.key && layout.is_empty() {
                // Every tile of that monitor is minimized: it looks empty, so the window fills it.
                (key, None, DropZone::Center)
            } else {
                let (target, rect) = layout.into_iter().find(|(id, r)| *id != h && r.contains(cx, cy))?;
                (key, Some(target), zone_at(rect, cx, cy))
            }
        }
        None => (key, None, DropZone::Center),
    };
    // On the border between two zones the cursor jitters between two targets; keep the previous one
    // until the cursor has clearly left the spot where it was chosen.
    let (chosen, layout) = match d.last {
        Some((prev, (px, py))) if prev != candidate && prev.0 == key && (cx - px).abs() <= STICKY_PX && (cy - py).abs() <= STICKY_PX => {
            match preview_of(surfaces, d, area, prev) {
                Some(layout) => (prev, layout),
                None => (candidate, preview_of(surfaces, d, area, candidate)?),
            }
        }
        _ => (candidate, preview_of(surfaces, d, area, candidate)?),
    };
    if d.last.map(|(t, _)| t) != Some(chosen) { d.last = Some((chosen, (cx, cy))); }
    let mut layout = layout;
    d.target = Some(chosen);
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

fn cursor_moved(a: (i32, i32), b: (i32, i32)) -> bool {
    (i64::from(a.0) - i64::from(b.0)).abs() > 4 || (i64::from(a.1) - i64::from(b.1)).abs() > 4
}

/// A drag is over only when neither the window's own move/size loop nor the mouse button says it
/// continues. Console windows do not report GUI_INMOVESIZE while the button is held.
fn drag_released(move_size_active: Option<bool>, primary_down: bool) -> bool {
    !(move_size_active.unwrap_or(false) || primary_down)
}

fn is_resize(start: Rect, now: Rect) -> bool {
    // Uniform DPI scaling can leave a top/bottom edge fixed during a move.
    if start.w != now.w && start.h != now.h
        && (i64::from(start.w) * i64::from(now.h) - i64::from(now.w) * i64::from(start.h)).abs()
            <= i64::from(start.w.max(start.h)).max(1) * 3 { return false; }
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

    pub(super) fn drag_fixture() -> (Drag, HashMap<SurfaceKey, Surface>, Rect) {
        let area = Rect::new(0, 0, 1920, 1080);
        let key = SurfaceKey { desktop: 1, monitor: 1 };
        let mut tree = Tree::new();
        tree.add(1, area, None);
        tree.add(2, area, None);
        let drag = Drag { hwnd: 1, key, start: tree.rect_of(area, 1).unwrap(), resize: false, move_only: false, intent: None, dpi: 96, monitor_changed: false, released_since: None, target: None, last: None };
        (drag, HashMap::from([(key, Surface { tree, area })]), area)
    }
    #[test]
    fn a_held_button_keeps_the_drag_alive_even_if_the_window_reports_no_move_loop() {
        assert!(!drag_released(Some(false), true), "console windows: loop flag false, button down");
        assert!(!drag_released(Some(true), false), "touch or pen: loop active, no button");
        assert!(!drag_released(None, true));
        assert!(drag_released(Some(false), false));
        assert!(drag_released(None, false));
    }

    #[test]
    fn target_does_not_flicker_on_a_zone_border() {
        let (mut drag, surfaces, area) = drag_fixture();
        let key = drag.key;
        let start = drag.start;
        // Window 2 fills the right half; x=1440,y=540 is its centre. Move to its left-edge zone and jitter.
        update_drag(&mut drag, Some(start), Some((key, area, 1440, 540)), &surfaces).unwrap();
        let first = drag.target;
        for dx in [-6, 6, -10, 8, -4] {
            update_drag(&mut drag, Some(start), Some((key, area, 1440 + dx, 540 + dx)), &surfaces).unwrap();
            assert_eq!(drag.target, first, "small jitter must keep the chosen target");
        }
    }

    #[test]
    fn a_resize_stays_a_resize_when_it_returns_to_the_start_edge() {
        let (mut drag, surfaces, area) = drag_fixture();
        let key = drag.key;
        let start = drag.start;
        let grown = Rect::new(start.x, start.y - 80, start.w, start.h + 80);
        assert!(update_drag(&mut drag, Some(grown), Some((key, area, 1440, 540)), &surfaces).is_none());
        assert!(drag.resize);
        // Back at the original size: still a resize, so no move preview and no drop target appears.
        assert!(update_drag(&mut drag, Some(start), Some((key, area, 1440, 540)), &surfaces).is_none());
        assert!(drag.resize && drag.target.is_none());
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
        let dir = std::env::temp_dir().join(format!("tilework-log-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events.log");
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
        std::fs::remove_dir(dir).unwrap();
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

}


#[cfg(test)]
mod scenarios {
    use super::*;

    #[derive(Clone)]
    struct Window {
        rect: Rect, minimum: (i32, i32), maximized: bool, minimized: bool,
        fullscreen: bool, hung: bool, styled: bool, desktop: u128, ignores: bool, hidden: bool,
    }
    struct FakeWin {
        windows: RefCell<HashMap<Hwnd, Window>>,
        area: Cell<Rect>, desktop: Cell<u128>, desktop_queries: Cell<usize>, requests: RefCell<Vec<(Hwnd, Rect)>>, cursor: Cell<(i32, i32)>,
        foreground: Cell<Hwnd>,
        dpi: Cell<u32>,
    }
    impl Win for FakeWin {
        fn visible_rect(&self, h: Hwnd) -> Option<Rect> { self.windows.borrow().get(&h).map(|w| w.rect) }
        fn is_maximized(&self, h: Hwnd) -> bool { self.windows.borrow()[&h].maximized }
        fn is_minimized(&self, h: Hwnd) -> bool { self.windows.borrow()[&h].minimized }
        fn is_fullscreen(&self, h: Hwnd) -> bool { self.windows.borrow()[&h].fullscreen }
        fn is_hung(&self, h: Hwnd) -> bool { self.windows.borrow()[&h].hung }
        fn place(&self, h: Hwnd, r: Rect) -> bool {
            self.requests.borrow_mut().push((h, r));
            let mut windows = self.windows.borrow_mut(); let w = windows.get_mut(&h).unwrap();
            if w.hung || w.minimized { return false; }
            if w.maximized { w.maximized = false; return false; }
            if !w.ignores { w.rect = Rect { w: r.w.max(w.minimum.0), h: r.h.max(w.minimum.1), ..r }; }
            true
        }
        fn desktop_id(&self, h: Hwnd) -> u128 { self.windows.borrow()[&h].desktop }
        fn all_top_level(&self) -> Vec<Hwnd> { let mut v: Vec<_> = self.windows.borrow().keys().copied().collect(); v.sort(); v }
        fn monitor_of(&self, h: Hwnd) -> (isize, Rect) { self.fake_monitor(self.windows.borrow()[&h].rect.x) }
        fn is_manageable(&self, h: Hwnd, _: u128) -> bool { let w = &self.windows.borrow()[&h]; w.styled && !w.minimized }
        fn is_visible(&self, h: Hwnd, _: u128) -> bool { !self.windows.borrow()[&h].hidden }
        fn title(&self, _: Hwnd) -> String { "test".into() }
        fn exe_name(&self, _: Hwnd) -> String { "test.exe".into() }
        fn class_name(&self, _: Hwnd) -> String { "test".into() }
        fn current_desktop(&self) -> u128 { self.desktop_queries.set(self.desktop_queries.get() + 1); self.desktop.get() }
        fn invalidate_desktop(&self) {}
        fn cursor_position(&self) -> Option<(i32, i32)> { Some(self.cursor.get()) }
        fn cursor_surface(&self, desktop: u128) -> Option<(SurfaceKey, Rect, i32, i32)> {
            let (x, y) = self.cursor.get();
            let (monitor, area) = self.fake_monitor(x);
            Some((SurfaceKey { desktop, monitor }, area, x, y))
        }
        fn foreground_window(&self) -> Hwnd { self.foreground.get() }
        fn dpi(&self, _: Hwnd) -> u32 { self.dpi.get() }
    }
    impl FakeWin {
        fn fake_monitor(&self, x: i32) -> (isize, Rect) {
            let primary = self.area.get();
            if x < primary.right() { (1, primary) }
            else { (2, Rect { x: primary.right(), ..primary }) }
        }
    }
    fn app() -> (App<FakeWin>, SurfaceKey) {
        let area = Rect::new(0, 0, 1920, 1080);
        let mut windows = HashMap::new();
        for h in [1, 2] { windows.insert(h, Window { rect: Rect::new(50, 50, 1000, 700), minimum: (0, 0), maximized: false, minimized: false, fullscreen: false, hung: false, styled: true, desktop: 1, ignores: false, hidden: false }); }
        let p = FakeWin { windows: RefCell::new(windows), area: Cell::new(area), desktop: Cell::new(1), desktop_queries: Cell::new(0), requests: RefCell::new(Vec::new()), cursor: Cell::new((0, 0)), foreground: Cell::new(0), dpi: Cell::new(96) };
        let (drag, surfaces, _) = super::tests::drag_fixture();
        let key = drag.key;
        (App { p, disabled: HashSet::new(), cfg: Config::default(), hover: None, hover_attempted: None, hover_foreground: None, hover_attempt_position: None, ghost_seen: HashSet::new(), reserved: HashSet::new(), shown: 1, desktop_error_logged: false, dispatch_desktop: Cell::new(None), dispatching: false, mins: HashMap::new(), placed: HashMap::new(), monitor_moves: HashMap::new(), floating: HashSet::new(), surfaces, last_tracked_focus: None, bordered: None, border_original: HashMap::new(), taskbar_created: 0, drag: None, overlay: Overlay::new(HWND::default()), focus_border: Overlay::new(HWND::default()), followed: 0, follow_pid: 0, follow_hooks: Vec::new(), main: HWND::default(), filter: None, icon_on: HICON::default(), icon_off: HICON::default() }, key)
    }
    fn poll(app: &mut App<FakeWin>, key: SurfaceKey) {
        for p in app.placed.values_mut() { p.requested = Instant::now() - Duration::from_secs(1); }
        app.apply(key, false);
    }

    #[test]
    fn saved_position_drift_and_a_lost_drag_start_are_repaired() {
        let (mut a, key) = app(); a.apply(key, false);
        let tile = a.placed[&1].target;
        for rect in [Rect { x: 300, y: 200, ..tile }, Rect { x: 200, w: 1400, ..tile }] {
            a.p.windows.borrow_mut().get_mut(&1).unwrap().rect = rect;
            poll(&mut a, key);
            assert_eq!(a.p.visible_rect(1), Some(tile));
            assert!(a.mins.is_empty());
        }
    }
    #[test]
    fn refusal_requires_a_replacement_and_two_separate_followup_polls() {
        let (mut a, key) = app(); a.p.windows.borrow_mut().get_mut(&1).unwrap().minimum = (1100, 0);
        a.apply(key, false);
        poll(&mut a, key); assert!(a.mins.is_empty());
        let requests = a.p.requests.borrow().len();
        poll(&mut a, key); assert!(a.mins.is_empty());
        assert!(a.p.requests.borrow().len() > requests);
        poll(&mut a, key); assert!(a.mins.is_empty());
        poll(&mut a, key); assert_eq!(a.mins[&1].size.0, 1100);
        assert_eq!(a.p.visible_rect(1).unwrap().w, 1100);
    }
    #[test]
    fn dpi_rescaling_and_self_resize_do_not_teach_minimums() {
        let (mut a, key) = app(); a.apply(key, false);
        let tile = a.placed[&1].target;
        a.p.windows.borrow_mut().get_mut(&1).unwrap().rect = Rect { x: 40, w: 1440, h: 1620, ..tile };
        poll(&mut a, key);
        assert_eq!(a.p.visible_rect(1), Some(tile));
        // A one-off self resize at the tile origin must survive an explicit re-place to count.
        a.p.windows.borrow_mut().get_mut(&1).unwrap().rect = Rect { w: 1300, ..tile };
        for _ in 0..5 { poll(&mut a, key); }
        assert!(a.mins.is_empty());
        assert_eq!(a.p.visible_rect(1), Some(tile));
        // A real work-area/DPI change produces a new target without learning the old scale.
        a.p.area.set(Rect::new(0, 0, 2560, 1440));
        a.reconcile(false);
        assert_eq!(a.p.visible_rect(1), a.surfaces[&key].tree.rect_of(a.p.area.get(), 1));
        assert!(a.mins.is_empty());
    }
    #[test]
    fn learning_is_capped_to_the_work_area_and_does_not_resample_unchanged_targets() {
        let (mut a, key) = app();
        a.p.windows.borrow_mut().get_mut(&1).unwrap().minimum = (5000, 5000);
        a.apply(key, false);
        for _ in 0..4 { poll(&mut a, key); }
        assert_eq!(a.mins[&1].size, (1920, 1080));
        assert_eq!(a.placed[&1].measured, None); // Changed target was placed, not sampled again.
    }
    #[test]
    fn refusing_position_uses_a_bounded_retry_budget_then_backoff() {
        let (mut a, key) = app(); a.p.windows.borrow_mut().get_mut(&1).unwrap().ignores = true;
        a.apply(key, false);
        for _ in 0..10 { poll(&mut a, key); }
        assert_eq!(a.p.requests.borrow().iter().filter(|(h, _)| *h == 1).count(), 4);
        assert!(a.mins.is_empty());
        a.placed.get_mut(&1).unwrap().requested = Instant::now() - Duration::from_secs(31);
        a.apply(key, false);
        assert_eq!(a.p.requests.borrow().iter().filter(|(h, _)| *h == 1).count(), 5);
    }
    #[test]
    fn a_learned_pixel_minimum_is_discarded_when_window_dpi_changes() {
        let (mut a, key) = app();
        a.p.dpi.set(144);
        a.p.windows.borrow_mut().get_mut(&1).unwrap().minimum = (1100, 0);
        a.apply(key, false);
        for _ in 0..4 { poll(&mut a, key); }
        assert_eq!(a.mins[&1].size, (1100, 0));
        assert_eq!(a.mins[&1].dpi, 144);

        a.p.dpi.set(96);
        a.p.windows.borrow_mut().get_mut(&1).unwrap().minimum = (0, 0);
        a.reconcile(false);
        assert!(a.mins.is_empty());
        assert!(a.surfaces[&key].tree.mins.is_empty());
    }
    #[test]
    fn maximization_survives_minimize_restore_and_win_d_preserves_weights() {
        let (mut a, key) = app();
        let area = a.surfaces[&key].area;
        a.surfaces.get_mut(&key).unwrap().tree.set_edge(area, 1, Edge::Right, 700);
        a.apply(key, false);
        let layout = a.surfaces[&key].tree.layout(area);
        { let mut w = a.p.windows.borrow_mut(); w.get_mut(&1).unwrap().maximized = true; w.get_mut(&1).unwrap().rect = area; }
        a.reconcile(false); assert!(a.reserved.contains(&1));
        let count = a.p.requests.borrow().len();
        { let mut w = a.p.windows.borrow_mut(); for window in w.values_mut() { window.minimized = true; window.maximized = false; } }
        a.reconcile(false);
        // Minimised windows keep their place and weights but take no space.
        assert!(a.surfaces[&key].tree.layout(area).is_empty());
        assert_eq!(a.surfaces[&key].tree.leaves().len(), 2);
        { let mut w = a.p.windows.borrow_mut(); for window in w.values_mut() { window.minimized = false; } w.get_mut(&1).unwrap().maximized = true; }
        a.reconcile(false);
        assert_eq!(a.p.requests.borrow().len(), count);
        assert_eq!(a.surfaces[&key].tree.layout(area), layout);
        a.p.windows.borrow_mut().get_mut(&1).unwrap().maximized = false;
        poll(&mut a, key); assert_eq!(a.p.visible_rect(1), Some(layout[0].1));
    }
    #[test]
    fn fullscreen_style_loss_keeps_tile_and_marker_is_pruned_only_on_destroy() {
        let (mut a, key) = app(); a.apply(key, false);
        let layout = a.surfaces[&key].tree.layout(a.surfaces[&key].area);
        { let mut ws = a.p.windows.borrow_mut(); let w = ws.get_mut(&1).unwrap(); w.fullscreen = true; w.styled = false; w.rect = a.p.area.get(); }
        a.reconcile(false); assert_eq!(a.surfaces[&key].tree.layout(a.p.area.get()), layout);
        assert!(a.reserved.contains(&1));
        a.p.windows.borrow_mut().get_mut(&1).unwrap().minimized = true;
        a.reconcile(false); assert!(a.reserved.contains(&1));
        a.p.windows.borrow_mut().remove(&1); a.reconcile(false);
        assert!(!a.reserved.contains(&1)); assert!(!a.surfaces[&key].tree.contains(1));
    }
    #[test]
    fn focus_uses_actual_foreground_and_leaves_fullscreen_windows_alone() {
        let (mut a, key) = app();
        a.p.foreground.set(1);
        a.reconcile(false);
        assert_eq!(a.bordered, Some(1));

        // A delayed event names window 1, but 2 now owns the keyboard focus.
        a.p.foreground.set(2);
        dispatch(&mut a, PendingEvent { msg: WM_EVENT, wp: 0x0003, lp: 1 });
        assert_eq!(a.bordered, Some(2));

        a.p.windows.borrow_mut().get_mut(&2).unwrap().fullscreen = true;
        a.reconcile(false);
        assert_eq!(a.bordered, None);
        assert!(a.surfaces[&key].tree.contains(2));

        a.p.windows.borrow_mut().get_mut(&2).unwrap().fullscreen = false;
        a.reconcile(false);
        assert_eq!(a.bordered, Some(2));

        a.p.desktop.set(2);
        a.reconcile(false);
        assert_eq!(a.bordered, None);
    }
    #[test]
    fn newly_adopted_maximized_window_restores_on_a_separate_placement() {
        let (mut a, key) = app();
        a.p.windows.borrow_mut().get_mut(&1).unwrap().maximized = true;
        a.p.windows.borrow_mut().get_mut(&1).unwrap().rect = a.p.area.get();
        a.apply(key, false); assert!(!a.p.is_maximized(1)); assert!(!a.placed.contains_key(&1));
        a.apply(key, false); assert!(a.placed.contains_key(&1));
        assert_eq!(a.p.visible_rect(1), a.surfaces[&key].tree.rect_of(a.p.area.get(), 1));
    }
    #[test]
    fn dragging_a_maximized_tile_is_move_only_and_desktop_switch_cancels_it() {
        let (mut a, key) = app(); a.apply(key, false);
        a.p.windows.borrow_mut().get_mut(&1).unwrap().maximized = true;
        a.drag_start(1);
        let drag = a.drag.as_mut().unwrap();
        assert_eq!(drag.start, a.surfaces[&key].tree.rect_of(a.p.area.get(), 1).unwrap());
        update_drag(drag, Some(Rect::new(300, 0, 1200, 700)), Some((key, a.p.area.get(), 1400, 500)), &a.surfaces);
        assert!(!drag.resize); assert!(drag.target.is_some());
        a.p.desktop.set(2); a.poll_drag_release(); assert!(a.drag.is_none());
    }
    #[test]
    fn a_cross_monitor_drop_survives_delayed_native_placement_and_eventually_reconciles_failures() {
        for placement_succeeds in [true, false] {
            let (mut a, home) = app();
            a.apply(home, false);
            a.p.windows.borrow_mut().get_mut(&1).unwrap().ignores = true;
            a.drag_start(1);
            let destination = SurfaceKey { monitor: 2, ..home };
            a.p.cursor.set((2200, 500));
            a.drag_end(1);
            assert_eq!(a.surface_containing(1), Some(destination));
            assert_eq!(a.p.monitor_of(1).0, home.monitor);
            a.reconcile(false);
            assert_eq!(a.surface_containing(1), Some(destination));
            if placement_succeeds {
                a.p.windows.borrow_mut().get_mut(&1).unwrap().rect = a.surfaces[&destination].area;
            } else {
                a.monitor_moves.get_mut(&1).unwrap().started -= Duration::from_secs(4);
            }
            a.reconcile(false);
            assert_eq!(a.surface_containing(1), Some(if placement_succeeds { destination } else { home }));
            assert!(a.monitor_moves.is_empty());
        }
    }
    #[test]
    fn deferred_events_keep_drag_identity_and_each_toggle() {
        let (mut a, _) = app(); a.apply(SurfaceKey { desktop: 1, monitor: 1 }, false);
        PENDING.with(|p| p.borrow_mut().clear());
        queue_pending(WM_EVENT, WPARAM(0x000A), LPARAM(1));
        queue_pending(WM_EVENT, WPARAM(0x000B), LPARAM(2)); // Other window must not end this drag.
        let first = PENDING.with(|p| p.borrow_mut().pop_front().unwrap()); dispatch(&mut a, first);
        assert_eq!(a.drag.as_ref().unwrap().hwnd, 1);
        let second = PENDING.with(|p| p.borrow_mut().pop_front().unwrap()); dispatch(&mut a, second);
        assert!(a.drag.is_some());
        queue_pending(WM_HOTKEY, WPARAM(HOTKEY_TOGGLE as usize), LPARAM(0));
        queue_pending(WM_TRAY, WPARAM(0), LPARAM(WM_LBUTTONUP as isize));
        let events = PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()));
        for e in events { dispatch(&mut a, e); }
        assert!(!a.disabled.contains(&1)); assert!(a.drag.is_none());
    }
    #[test]
    fn empty_desktop_toggle_and_unrelated_setting_notification() {
        let (mut a, _) = app(); a.p.windows.borrow_mut().clear();
        dispatch(&mut a, PendingEvent { msg: WM_HOTKEY, wp: HOTKEY_TOGGLE as usize, lp: 0 });
        assert!(a.disabled.contains(&1));
        assert!(!dispatch(&mut a, PendingEvent { msg: WM_SETTINGCHANGE, wp: 0, lp: 0 }));
    }
    #[test]
    fn failed_hover_focus_is_attempted_once_until_a_different_window_is_hovered() {
        let (mut a, _) = app(); let now = Instant::now();
        a.p.cursor.set((0, 0));
        assert!(!a.hover_ready(1, 2, now)); // anchors the cursor at the foreground change
        a.p.cursor.set((50, 50));
        assert!(!a.hover_ready(1, 2, now + Duration::from_millis(10))); // cursor moved: arms
        assert!(a.hover_ready(1, 2, now + Duration::from_millis(310)));
        // Simulate Windows refusing focus: the foreground remains unchanged over many ticks.
        for seconds in 1..20 { assert!(!a.hover_ready(1, 2, now + Duration::from_secs(seconds))); }
        assert!(!a.hover_ready(2, 1, now + Duration::from_secs(21)));
        assert!(!a.hover_ready(1, 2, now + Duration::from_secs(22)));
        a.p.cursor.set((300, 300));
        assert!(!a.hover_ready(1, 2, now + Duration::from_secs(23))); // arms after the move
        assert!(a.hover_ready(1, 2, now + Duration::from_secs(24)));
    }
    #[test]
    fn each_dispatch_uses_one_desktop_query_even_when_drag_validation_repeats() {
        let (mut a, key) = app(); a.apply(key, false);
        dispatch(&mut a, PendingEvent { msg: WM_EVENT, wp: 0x000A, lp: 1 });
        let queries = a.p.desktop_queries.get();
        // Both drag_tick and poll_drag_release validate the desktop in this dispatch.
        dispatch(&mut a, PendingEvent { msg: WM_TIMER, wp: TIMER_DRAG, lp: 0 });
        assert_eq!(a.p.desktop_queries.get(), queries + 1);
        assert!(a.dispatch_desktop.get().is_none());
    }
    #[test]
    fn hard_linked_log_cannot_modify_the_link_target() {
        let dir = std::env::temp_dir().join(format!("tilework-hardlink-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("sentinel"); let log = dir.join("events.log");
        std::fs::write(&target, "unchanged").unwrap();
        std::fs::hard_link(&target, &log).unwrap();
        assert!(append_log(&log, "event").is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "unchanged");
        std::fs::remove_file(log).unwrap(); std::fs::remove_file(target).unwrap(); std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn a_panicking_callback_returns_safely_and_runs_cleanup() {
        let (mut a, _) = app(); a.reserved.insert(1);
        let result: LRESULT = contain_callback(|| panic!("simulated callback panic"), || { a.reserved.clear(); });
        assert_eq!(result, LRESULT(0));
        assert!(a.reserved.is_empty());
    }

    #[test]
    fn overlay_updates_only_issue_the_window_calls_that_change_something() {
        let r = Rect::new(0, 0, 800, 600);
        let plan = |position, repaint| OverlayPlan { position, repaint };
        assert_eq!(overlay_plan(OverlayState::Hidden, r, false, false), plan(true, true));
        assert_eq!(overlay_plan(OverlayState::Unknown, r, false, false), plan(true, true));
        assert_eq!(overlay_plan(OverlayState::Shown(r), r, false, false), plan(false, false));
        assert_eq!(overlay_plan(OverlayState::Shown(r), Rect { x: 50, ..r }, false, false), plan(true, false));
        assert_eq!(overlay_plan(OverlayState::Shown(r), Rect { w: 700, ..r }, false, false), plan(true, true));
        assert_eq!(overlay_plan(OverlayState::Shown(r), r, true, false), plan(true, false));
        assert_eq!(overlay_plan(OverlayState::Shown(r), r, false, true), plan(false, true));

        let mut o = Overlay::new(HWND::default());
        o.hide(); o.hide();
        assert_eq!(o.calls, 0, "hiding a hidden overlay is free");
        o.show(r, false, false); o.show(r, false, false);
        assert_eq!((o.calls, o.state), (2, OverlayState::Shown(r)));
        o.show(Rect { w: 0, ..r }, false, false);
        assert_eq!((o.calls, o.state), (3, OverlayState::Hidden), "an empty rectangle hides");
        o.forget(); o.hide();
        assert_eq!(o.calls, 4, "after a display change the next hide is never skipped");
    }

    #[test]
    fn an_unchanged_drag_preview_costs_no_window_calls_per_tick() {
        let (mut a, key) = app(); a.apply(key, false);
        a.drag_start(1);
        let start = a.drag.as_ref().unwrap().start;
        a.p.windows.borrow_mut().get_mut(&1).unwrap().rect = Rect { x: start.x + 300, ..start };
        a.p.cursor.set((1400, 500));
        for _ in 0..50 { a.drag_tick(); }
        // Previously every 30 ms tick repositioned the overlay: 50 SetWindowPos + 1 InvalidateRect.
        assert_eq!(a.overlay.calls, 2, "one show and one paint for the whole steady preview");
        assert!(matches!(a.overlay.state, OverlayState::Shown(_)));

        // A new target repaints once and re-asserts the z-order once.
        a.p.cursor.set((1400, 1060));
        for _ in 0..10 { a.drag_tick(); }
        assert_eq!(a.overlay.calls, 4);

        // Every cancel path hides, and repeated hides are free.
        a.cancel_drag(); a.cancel_drag(); a.hide_overlay();
        assert_eq!((a.overlay.calls, a.overlay.state), (5, OverlayState::Hidden));
    }

    #[test]
    fn a_resize_drag_does_not_hide_the_overlay_on_every_tick() {
        let (mut a, key) = app(); a.apply(key, false);
        a.drag_start(1);
        let start = a.drag.as_ref().unwrap().start;
        a.p.windows.borrow_mut().get_mut(&1).unwrap().rect = Rect { w: start.w + 200, ..start };
        for _ in 0..50 { a.drag_tick(); }
        assert_eq!((a.overlay.calls, a.overlay.state), (0, OverlayState::Hidden));
    }

    #[test]
    fn focus_border_follows_its_window_and_skips_unchanged_refreshes() {
        let (mut a, key) = app(); a.apply(key, false);
        a.p.foreground.set(1);
        a.refresh_focus();
        let r1 = a.p.visible_rect(1).unwrap();
        assert_eq!((a.focus_border.state, a.followed), (OverlayState::Shown(r1), 1));
        let calls = a.focus_border.calls;
        for _ in 0..20 { a.refresh_focus(); a.reconcile(false); }
        assert_eq!(a.focus_border.calls, calls, "periodic refreshes of an unchanged border are free");

        // The window is moved asynchronously: its location event moves the border without a poll.
        let moved = Rect { x: r1.x + 10, w: r1.w - 10, ..r1 };
        a.p.windows.borrow_mut().get_mut(&1).unwrap().rect = moved;
        dispatch(&mut a, PendingEvent { msg: WM_EVENT, wp: 0x800B, lp: 1 });
        assert_eq!(a.focus_border.state, OverlayState::Shown(moved));
        // Events of other windows are ignored.
        a.p.windows.borrow_mut().get_mut(&1).unwrap().rect = r1;
        dispatch(&mut a, PendingEvent { msg: WM_EVENT, wp: 0x800B, lp: 2 });
        assert_eq!(a.focus_border.state, OverlayState::Shown(moved));

        // Maximize hides it at once; restoring brings it back, still followed meanwhile.
        a.p.windows.borrow_mut().get_mut(&1).unwrap().maximized = true;
        dispatch(&mut a, PendingEvent { msg: WM_EVENT, wp: 0x800B, lp: 1 });
        assert_eq!((a.focus_border.state, a.bordered, a.followed), (OverlayState::Hidden, None, 1));
        a.p.windows.borrow_mut().get_mut(&1).unwrap().maximized = false;
        dispatch(&mut a, PendingEvent { msg: WM_EVENT, wp: 0x800B, lp: 1 });
        assert_eq!((a.focus_border.state, a.bordered), (OverlayState::Shown(r1), Some(1)));

        // Destroy, hide and cloak remove a stale border immediately, before the reconcile timer.
        for ev in [0x8001, 0x8003, 0x8017] {
            a.refresh_focus();
            assert_eq!(a.bordered, Some(1));
            dispatch(&mut a, PendingEvent { msg: WM_EVENT, wp: ev, lp: 1 });
            assert_eq!((a.focus_border.state, a.bordered), (OverlayState::Hidden, None), "event {ev:x}");
        }

        // A drag hides the border and ignores the dragged window's location events.
        a.refresh_focus();
        a.drag_start(1);
        dispatch(&mut a, PendingEvent { msg: WM_EVENT, wp: 0x800B, lp: 1 });
        assert_eq!(a.focus_border.state, OverlayState::Hidden);
    }

    #[test]
    fn a_monitor_whose_tiles_are_all_minimized_accepts_a_drop_like_an_empty_one() {
        let (mut a, home) = app(); a.apply(home, false);
        // Window 3 lives alone on the second monitor and is minimized there.
        a.p.windows.borrow_mut().insert(3, Window { rect: Rect::new(2000, 50, 800, 600), minimum: (0, 0), maximized: false, minimized: false, fullscreen: false, hung: false, styled: true, desktop: 1, ignores: false, hidden: false });
        a.reconcile(false);
        let second = SurfaceKey { monitor: 2, ..home };
        assert!(a.surfaces[&second].tree.contains(3));
        a.p.windows.borrow_mut().get_mut(&3).unwrap().minimized = true;
        a.reconcile(false);
        a.drag_start(1);
        a.p.cursor.set((2900, 500));
        a.drag_end(1);
        assert_eq!(a.surface_containing(1), Some(second));
        // The dropped window fills the monitor; the minimized one keeps its reserved tile.
        let s = &a.surfaces[&second];
        assert_eq!(s.tree.layout(s.area), vec![(1, s.area)]);
        assert!(s.tree.contains(3));
    }

    #[test]
    fn the_focus_border_returns_after_a_drag_that_ends_without_a_drop() {
        let (mut a, key) = app(); a.apply(key, false);
        a.p.foreground.set(1);
        a.refresh_focus();
        // A resize that the layout accepts exactly: no placement, so no location event follows.
        a.drag_start(1);
        let start = a.drag.as_ref().unwrap().start;
        let resized = Rect { w: start.w - 200, ..start };
        a.p.windows.borrow_mut().get_mut(&1).unwrap().rect = resized;
        a.drag_end(1);
        assert_eq!(a.focus_border.state, OverlayState::Shown(resized));
        // A move released over nothing snaps back and also restores the border.
        a.drag_start(1);
        a.p.cursor.set((resized.x + 100, resized.y + 300));
        a.drag_end(1);
        assert_eq!(a.focus_border.state, OverlayState::Shown(resized));
    }

    #[test]
    fn display_change_and_shutdown_leave_no_overlay_behind() {
        let (mut a, key) = app(); a.apply(key, false);
        a.p.foreground.set(1);
        a.refresh_focus();
        a.drag_start(2);
        a.p.windows.borrow_mut().get_mut(&2).unwrap().rect.x += 300;
        a.p.cursor.set((300, 500));
        a.drag_tick();
        assert!(matches!(a.overlay.state, OverlayState::Shown(_)));
        dispatch(&mut a, PendingEvent { msg: WM_DISPLAYCHANGE, wp: 0, lp: 0 });
        assert!(a.drag.is_none());
        assert_eq!(a.overlay.state, OverlayState::Hidden);
        // The border is re-shown unconditionally since the system may have moved it.
        assert!(matches!(a.focus_border.state, OverlayState::Shown(_)));
        a.shutdown();
        assert_eq!((a.overlay.state, a.focus_border.state, a.followed), (OverlayState::Hidden, OverlayState::Hidden, 0));
    }

    // ---------- robustness ----------

    #[test]
    fn restart_as_administrator_is_deferred_until_the_app_borrow_ends() {
        let (mut a, _) = app(); a.filter = Some("probe".into());
        // The UAC prompt runs a nested message loop; it must not run inside menu_command.
        assert!(matches!(a.menu_command(MENU_RESTART_ADMIN), Some(MenuFollowUp::RestartAsAdministrator(Some(f))) if f == "probe"));
    }

    #[test]
    fn an_unreadable_monitor_work_area_never_squeezes_windows() {
        let (mut a, key) = app(); a.apply(key, false);
        let tiles: Vec<_> = [1, 2].iter().map(|h| a.p.visible_rect(*h)).collect();
        // GetMonitorInfoW failing during a monitor hot-unplug reports an empty work area.
        a.p.area.set(Rect::new(0, 0, 0, 0));
        a.p.requests.borrow_mut().clear();
        a.reconcile(true);
        assert!(a.p.requests.borrow().is_empty(), "no placement into an empty area: {:?}", a.p.requests.borrow());
        assert_eq!([1, 2].iter().map(|h| a.p.visible_rect(*h)).collect::<Vec<_>>(), tiles);
    }

}
