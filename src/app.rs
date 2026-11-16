//! The tiling manager: reacts to window events, keeps one tree per surface (virtual desktop x
//! monitor), applies the layout and handles drag & drop with a live preview.

use crate::platform::{hwnd_of, id_of, set_snap, snap_enabled, Platform, SurfaceKey};
use crate::tree::{zone_at, DropZone, Edge, Hwnd, Rect, Tree};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::io::Write;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
use windows::Win32::Graphics::Gdi::{BeginPaint, CreateBitmap, CreateSolidBrush, DeleteObject, EndPaint, FillRect, GetMonitorInfoW, InvalidateRect, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT};
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
/// Cursor distance from the screen border that counts as "drop at the outer edge".
const EDGE_PX: i32 = 36;

fn close(a: Rect, b: Rect) -> bool {
    (a.x - b.x).abs() <= 1 && (a.y - b.y).abs() <= 1 && (a.w - b.w).abs() <= 1 && (a.h - b.h).abs() <= 1
}

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
    /// (surface, window under the cursor or None for an outer edge / empty monitor, zone)
    target: Option<(SurfaceKey, Option<Hwnd>, DropZone)>,
}

pub struct App {
    p: Platform,
    /// Virtual desktops on which tiling is switched off (every desktop is on by default).
    disabled: HashSet<u128>,
    /// State of Windows' own snapping before we started; restored on exit and on desktops without tiling.
    snap_original: bool,
    /// The virtual desktop that is currently shown.
    shown: u128,
    /// Minimum sizes some windows insist on, learned from windows that refuse to shrink.
    mins: HashMap<Hwnd, (i32, i32)>,
    /// Last target we placed a window at, and what we measured afterwards.
    placed: HashMap<Hwnd, (Rect, Option<Rect>)>,
    /// Windows that currently fit nowhere (they stay floating).
    floating: HashSet<Hwnd>,
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
    /// What the drag overlay paints: the screen area and (rectangle, is the dragged window) pairs.
    static OVERLAY: RefCell<(Rect, Vec<(Rect, bool)>)> = const { RefCell::new((Rect::new(0, 0, 0, 0), Vec::new())) };
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
        self.surfaces.iter().find(|(_, s)| s.tree.contains(h)).map(|(k, _)| *k)
    }

    /// Brings the trees in line with the windows that really exist, then lays out what changed.
    fn reconcile(&mut self, force_all: bool) {
        let cur = self.p.current_desktop();
        if cur != self.shown {
            log(&format!("desktop switch {:x} -> {:x} (tiling {})", self.shown, cur, if self.desktop_on(cur) { "on" } else { "off" }));
            self.shown = cur;
            self.sync_snap();
            self.update_tray();
        }
        let tops = self.p.all_top_level();
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

        // Windows that vanished, were minimised, moved to another desktop/monitor or whose desktop was switched off.
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
        self.placed.retain(|h, _| current.contains_key(h));
        self.floating.retain(|h| current.contains_key(h));

        // New windows, in z-order from back to front.
        let order: Vec<Hwnd> = tops.iter().rev().copied().filter(|h| current.contains_key(h)).collect();
        for h in order {
            let (key, area) = current[&h];
            if self.surface_containing(h).is_some() {
                continue;
            }
            let focused = self.focused;
            let info = format!("{h:x} {} ({})", self.p.title(h), self.p.exe_name(h));
            let mins = self.mins.clone();
            let s = self.surfaces.entry(key).or_insert_with(|| Surface { tree: Tree::new(), area });
            s.area = area;
            s.tree.mins = mins;
            if s.tree.add(h, area, focused) {
                dirty.insert(key);
                self.floating.remove(&h);
                log(&format!("added {info}"));
            } else if self.floating.insert(h) {
                log(&format!("no room, stays floating: {info}"));
            }
        }
        self.surfaces.retain(|_, s| !s.tree.is_empty());
        // Everything on the shown desktop is verified, so a desktop switch repairs what the
        // system moved while the windows were hidden.
        let keys: Vec<SurfaceKey> = self.surfaces.keys().copied().filter(|k| dirty.contains(k) || k.desktop == cur).collect();
        for key in keys {
            self.apply(key, false);
        }
    }

    fn apply(&mut self, key: SurfaceKey, force: bool) {
        let Some(s) = self.surfaces.get(&key) else { return };
        let layout = s.tree.layout(s.area);
        let mut learned = false;
        for (h, r) in layout {
            if self.p.is_fullscreen(h) {
                continue; // full screen stays full screen: never pull it back into its tile
            }
            if force {
                self.placed.remove(&h);
            }
            let vis = self.p.visible_rect(h);
            if vis.map(|v| close(v, r)).unwrap_or(false) {
                self.placed.insert(h, (r, vis));
                continue;
            }
            if let Some((pr, pv)) = self.placed.get(&h) {
                if *pr == r && *pv == vis {
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
                            log(&format!("{h:x} {} needs at least {}x{} (zone was {}x{})", self.p.title(h), new.0, new.1, r.w, r.h));
                            learned = true;
                        }
                    }
                    continue;
                }
            }
            self.p.place(h, r);
            self.placed.insert(h, (r, self.p.visible_rect(h)));
        }
        if learned {
            for s in self.surfaces.values_mut() {
                s.tree.mins = self.mins.clone();
            }
            // If the learned minimums do not fit the current arrangement, re-seat the demanding
            // windows (biggest first) where they squeeze nobody.
            let mins = &self.mins;
            if let Some(s) = self.surfaces.get_mut(&key) {
                let area = s.area;
                if s.tree.violations(area) > 0 {
                    let mut movers: Vec<Hwnd> = s.tree.leaves().into_iter().filter(|h| mins.contains_key(h)).collect();
                    movers.sort_by_key(|h| std::cmp::Reverse(mins[h].0 as i64 * 2000 + mins[h].1 as i64 * 1000));
                    for h in movers {
                        let before = s.tree.violations(area);
                        let mut c = s.tree.clone();
                        c.remove(h);
                        if c.add(h, area, None) && c.violations(area) < before {
                            s.tree = c;
                            log(&format!("re-seated {h:x} to respect minimum sizes"));
                        }
                        if s.tree.violations(area) == 0 {
                            break;
                        }
                    }
                }
            }
            self.apply(key, false);
        }
    }

    /// Windows' own snapping must not fight with us: it is off while tiling is on for the shown desktop.
    fn sync_snap(&self) {
        set_snap(if self.desktop_on(self.shown) { false } else { self.snap_original });
    }

    fn set_border(&mut self, h: Hwnd, color: u32) {
        unsafe {
            let c = COLORREF(color);
            let _ = DwmSetWindowAttribute(hwnd_of(h), DWMWA_BORDER_COLOR, &c as *const COLORREF as *const c_void, 4);
        }
    }

    fn on_focus(&mut self, h: Hwnd) {
        if self.surface_containing(h).is_none() {
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

    /// Switches tiling on or off for the desktop that is shown right now.
    fn toggle(&mut self) {
        let d = self.p.current_desktop();
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
        self.surfaces.retain(|k, _| k.desktop != d);
        self.placed.clear();
        self.sync_snap();
        if on {
            self.reconcile(true);
        }
        self.update_tray();
    }

    // ---------- drag & drop ----------

    fn drag_start(&mut self, h: Hwnd) {
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
        let mut preview: Option<Vec<(Hwnd, Rect)>> = None;
        match self.surfaces.get(&key) {
            Some(s) => {
                if let Some(side) = Self::edge_at(area, cx, cy) {
                    preview = s.tree.preview_edge_layout(area, h, side);
                    if preview.is_some() {
                        target = Some((key, None, side));
                    }
                } else if let Some((t, r)) = s.tree.layout(area).into_iter().find(|(id, r)| *id != h && r.contains(cx, cy)) {
                    let zone = zone_at(r, cx, cy);
                    preview = if key == home { s.tree.preview_layout(area, h, t, zone) } else { s.tree.preview_insert_layout(area, h, t, zone) };
                    if preview.is_some() {
                        target = Some((key, Some(t), zone));
                    }
                }
            }
            None => {
                // empty monitor: the window would fill it
                preview = Some(vec![(h, area)]);
                target = Some((key, None, DropZone::Center));
            }
        }
        if let Some(d) = &mut self.drag {
            d.target = target;
        }
        match preview {
            Some(l) => self.show_overlay(area, &l, h),
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
            self.apply(home, true);
            return;
        }
        let Some((key, target, zone)) = d.target else {
            log(&format!("drop {h:x}: no valid target, window goes back"));
            self.apply(home, true);
            return;
        };
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
        let on = self.desktop_on(self.shown);
        nid.hIcon = if on { self.icon_on } else { self.icon_off };
        let tip = if on { "CosmicTile: ON for this desktop (click to turn off)" } else { "CosmicTile: OFF for this desktop (click to turn on)" };
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
            let label = if self.desktop_on(self.shown) { w!("Turn tiling off for this desktop") } else { w!("Turn tiling on for this desktop") };
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
        set_snap(self.snap_original);
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
        let _ = SetLayeredWindowAttributes(overlay, COLORREF(0x00FF00FF), 200, LWA_COLORKEY | LWA_ALPHA);

        let platform = Platform::new();
        let app = App {
            p: platform,
            disabled: HashSet::new(),
            snap_original: snap_enabled(),
            shown: 0,
            mins: HashMap::new(),
            placed: HashMap::new(),
            floating: HashSet::new(),
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
