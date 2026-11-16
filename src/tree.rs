//! Zone tree for one monitor on one virtual desktop.
//!
//! The screen is divided into *zones*. A container holds up to [`MAX_ZONES`] zones side by side
//! (horizontal) or stacked (vertical); every zone is either a window or another container.
//! Dropping a window on the edge of another window only changes the area of that window, all
//! other windows keep their place. Zone sizes stay equal until the user resizes them.

pub const MAX_ZONES: usize = 4;
const MIN_ZONE_PX: i32 = 80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }
}

/// `Horizontal`: zones side by side. `Vertical`: zones stacked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropZone {
    Center,
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

pub type Hwnd = isize;

#[derive(Clone, Debug)]
pub enum Node {
    Leaf(Hwnd),
    Container {
        orient: Orientation,
        children: Vec<Node>,
        weights: Vec<f64>,
        /// true until the user resizes a zone: new zones then get an equal share.
        auto: bool,
    },
}

#[derive(Clone, Debug, Default)]
pub struct Tree {
    root: Option<Node>,
}

impl Tree {
    pub fn new() -> Self {
        Self { root: None }
    }

    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    pub fn leaves(&self) -> Vec<Hwnd> {
        let mut out = Vec::new();
        if let Some(n) = &self.root {
            collect(n, &mut out);
        }
        out
    }

    pub fn contains(&self, h: Hwnd) -> bool {
        self.leaves().contains(&h)
    }

    /// Adds a window next to the focused window (or the largest one), along its longer side.
    pub fn add(&mut self, h: Hwnd, area: Rect, focused: Option<Hwnd>) {
        if self.contains(h) {
            return;
        }
        if self.root.is_none() {
            self.root = Some(Node::Leaf(h));
            return;
        }
        let layout = self.layout(area);
        let target = focused
            .filter(|f| self.contains(*f))
            .or_else(|| layout.iter().max_by_key(|(_, r)| r.w as i64 * r.h as i64).map(|(id, _)| *id))
            .expect("tree is not empty");
        let r = layout.iter().find(|(id, _)| *id == target).map(|(_, r)| *r).unwrap();
        let zone = match self.root.as_ref().and_then(|n| parent_row(n, target)) {
            Some((Orientation::Horizontal, n)) if n < MAX_ZONES => DropZone::Right,
            Some((Orientation::Vertical, n)) if n < MAX_ZONES => DropZone::Bottom,
            _ if r.w >= r.h => DropZone::Right,
            _ => DropZone::Bottom,
        };
        self.insert_around(target, h, zone);
    }

    /// Removes a window; the freed space goes to its neighbours.
    pub fn remove(&mut self, h: Hwnd) -> bool {
        let Some(root) = self.root.take() else { return false };
        let before = self.leaves_of(&root).len();
        self.root = remove_node(root, h);
        let after = self.root.as_ref().map(|n| self.leaves_of(n).len()).unwrap_or(0);
        before != after
    }

    fn leaves_of(&self, n: &Node) -> Vec<Hwnd> {
        let mut v = Vec::new();
        collect(n, &mut v);
        v
    }

    pub fn swap(&mut self, a: Hwnd, b: Hwnd) -> bool {
        if a == b || !self.contains(a) || !self.contains(b) {
            return false;
        }
        if let Some(root) = &mut self.root {
            swap_ids(root, a, b);
        }
        true
    }

    /// Moves a window next to another one (or swaps them for the centre zone).
    pub fn move_to(&mut self, dragged: Hwnd, target: Hwnd, zone: DropZone) -> bool {
        if dragged == target || !self.contains(dragged) || !self.contains(target) {
            return false;
        }
        if zone == DropZone::Center {
            return self.swap(dragged, target);
        }
        self.remove(dragged);
        self.insert_around(target, dragged, zone);
        true
    }

    /// Inserts a window that is not part of this tree yet next to `target`.
    pub fn insert(&mut self, added: Hwnd, target: Hwnd, zone: DropZone) {
        if self.root.is_none() {
            self.root = Some(Node::Leaf(added));
        } else if self.contains(target) && !self.contains(added) {
            self.insert_around(target, added, zone);
        }
    }

    /// Rectangle a foreign window would get if it were inserted next to `target`.
    pub fn preview_insert(&self, area: Rect, added: Hwnd, target: Hwnd, zone: DropZone) -> Option<Rect> {
        let mut c = self.clone();
        c.insert(added, target, zone);
        c.rect_of(area, added)
    }

    fn insert_around(&mut self, target: Hwnd, added: Hwnd, zone: DropZone) {
        if let Some(root) = self.root.take() {
            self.root = Some(insert_node(root, target, added, zone));
        }
    }

    /// Rectangle of every window.
    pub fn layout(&self, area: Rect) -> Vec<(Hwnd, Rect)> {
        let mut out = Vec::new();
        if let Some(n) = &self.root {
            layout_node(n, area, &mut out);
        }
        out
    }

    pub fn rect_of(&self, area: Rect, h: Hwnd) -> Option<Rect> {
        self.layout(area).into_iter().find(|(id, _)| *id == h).map(|(_, r)| r)
    }

    /// The exact rectangle the dragged window would get if it were dropped here.
    pub fn preview(&self, area: Rect, dragged: Hwnd, target: Hwnd, zone: DropZone) -> Option<Rect> {
        let mut c = self.clone();
        if !c.move_to(dragged, target, zone) {
            return None;
        }
        c.rect_of(area, dragged)
    }

    /// Moves the divider that forms the given edge of a window (or of the whole zone the window
    /// is part of) to a new coordinate. Returns false for outer edges without a divider.
    pub fn set_edge(&mut self, area: Rect, h: Hwnd, edge: Edge, coordinate: i32) -> bool {
        match &mut self.root {
            Some(root) => matches!(set_edge_node(root, area, h, edge, coordinate), EdgeState::Done),
            None => false,
        }
    }

    /// Makes all zones of every container equal again.
    pub fn equalize(&mut self) {
        if let Some(root) = &mut self.root {
            equalize_node(root);
        }
    }
}

/// Which part of a window the cursor is over: the middle swaps, the outer band splits that side.
pub fn zone_at(r: Rect, x: i32, y: i32) -> DropZone {
    let fx = (x - r.x) as f64 / r.w as f64;
    let fy = (y - r.y) as f64 / r.h as f64;
    if fx > 0.28 && fx < 0.72 && fy > 0.28 && fy < 0.72 {
        return DropZone::Center;
    }
    let (dl, dr, dt, db) = (fx, 1.0 - fx, fy, 1.0 - fy);
    let m = dl.min(dr).min(dt).min(db);
    if m == dl {
        DropZone::Left
    } else if m == dr {
        DropZone::Right
    } else if m == dt {
        DropZone::Top
    } else {
        DropZone::Bottom
    }
}

/// Orientation and zone count of the container that directly holds `h`.
fn parent_row(n: &Node, h: Hwnd) -> Option<(Orientation, usize)> {
    if let Node::Container { orient, children, .. } = n {
        if children.iter().any(|c| matches!(c, Node::Leaf(id) if *id == h)) {
            return Some((*orient, children.len()));
        }
        return children.iter().find_map(|c| parent_row(c, h));
    }
    None
}

fn collect(n: &Node, out: &mut Vec<Hwnd>) {
    match n {
        Node::Leaf(h) => out.push(*h),
        Node::Container { children, .. } => children.iter().for_each(|c| collect(c, out)),
    }
}

fn equal(n: usize) -> Vec<f64> {
    vec![1.0; n]
}

/// Removes a leaf; containers with a single child dissolve, nested containers of the same
/// orientation merge into their parent when there is room.
fn remove_node(n: Node, h: Hwnd) -> Option<Node> {
    match n {
        Node::Leaf(id) if id == h => None,
        Node::Leaf(_) => Some(n),
        Node::Container { orient, children, weights, auto } => {
            let mut nc = Vec::new();
            let mut nw = Vec::new();
            for (c, w) in children.into_iter().zip(weights) {
                if let Some(c) = remove_node(c, h) {
                    nc.push(c);
                    nw.push(w);
                }
            }
            normalize(orient, nc, nw, auto)
        }
    }
}

fn normalize(orient: Orientation, children: Vec<Node>, weights: Vec<f64>, auto: bool) -> Option<Node> {
    match children.len() {
        0 => None,
        1 => children.into_iter().next(),
        _ => {
            let mut nc: Vec<Node> = Vec::new();
            let mut nw: Vec<f64> = Vec::new();
            let mut auto = auto;
            let total_len = children.len();
            for (c, w) in children.into_iter().zip(weights) {
                match c {
                    Node::Container { orient: o2, children: c2, weights: w2, auto: a2 } if o2 == orient && nc.len() + c2.len() + (total_len - nc.len() - 1) <= MAX_ZONES => {
                        let sum: f64 = w2.iter().sum();
                        for (cc, ww) in c2.into_iter().zip(w2) {
                            nc.push(cc);
                            nw.push(w * ww / sum);
                        }
                        auto &= a2;
                    }
                    other => {
                        nc.push(other);
                        nw.push(w);
                    }
                }
            }
            if auto {
                nw = equal(nc.len());
            }
            Some(Node::Container { orient, children: nc, weights: nw, auto })
        }
    }
}

fn insert_node(n: Node, target: Hwnd, added: Hwnd, zone: DropZone) -> Node {
    let orient_needed = if matches!(zone, DropZone::Left | DropZone::Right) { Orientation::Horizontal } else { Orientation::Vertical };
    let added_first = matches!(zone, DropZone::Left | DropZone::Top);
    let pair = |t: Node, a: Node| if added_first { vec![a, t] } else { vec![t, a] };
    match n {
        Node::Leaf(id) if id == target => Node::Container { orient: orient_needed, children: pair(Node::Leaf(id), Node::Leaf(added)), weights: equal(2), auto: true },
        Node::Leaf(_) => n,
        Node::Container { orient, mut children, mut weights, auto } => {
            if let Some(i) = children.iter().position(|c| matches!(c, Node::Leaf(id) if *id == target)) {
                if orient == orient_needed && children.len() < MAX_ZONES {
                    // another zone next to the target
                    let at = if added_first { i } else { i + 1 };
                    children.insert(at, Node::Leaf(added));
                    if auto {
                        weights = equal(children.len());
                    } else {
                        let half = weights[i] / 2.0;
                        weights[i] = half;
                        weights.insert(at, half);
                    }
                    return Node::Container { orient, children, weights, auto };
                }
                // area full or other direction: only the target's zone is split further
                let t = children.remove(i);
                children.insert(i, Node::Container { orient: orient_needed, children: pair(t, Node::Leaf(added)), weights: equal(2), auto: true });
                return Node::Container { orient, children, weights, auto };
            }
            let children = children.into_iter().map(|c| insert_node(c, target, added, zone)).collect();
            Node::Container { orient, children, weights, auto }
        }
    }
}

fn swap_ids(n: &mut Node, a: Hwnd, b: Hwnd) {
    match n {
        Node::Leaf(id) => {
            if *id == a {
                *id = b;
            } else if *id == b {
                *id = a;
            }
        }
        Node::Container { children, .. } => children.iter_mut().for_each(|c| swap_ids(c, a, b)),
    }
}

fn equalize_node(n: &mut Node) {
    if let Node::Container { children, weights, auto, .. } = n {
        *weights = equal(children.len());
        *auto = true;
        children.iter_mut().for_each(equalize_node);
    }
}

/// Pixel size of every zone along the container's axis; the last zone takes the rounding rest.
fn zone_sizes(weights: &[f64], total: i32) -> Vec<i32> {
    let sum: f64 = weights.iter().sum();
    let mut sizes = Vec::with_capacity(weights.len());
    let mut used = 0;
    let mut acc = 0.0;
    for (i, w) in weights.iter().enumerate() {
        acc += w;
        let end = if i + 1 == weights.len() { total } else { (total as f64 * acc / sum).round() as i32 };
        sizes.push(end - used);
        used = end;
    }
    sizes
}

fn child_rects(orient: Orientation, weights: &[f64], r: Rect) -> Vec<Rect> {
    let total = if orient == Orientation::Horizontal { r.w } else { r.h };
    let mut pos = 0;
    zone_sizes(weights, total)
        .into_iter()
        .map(|s| {
            let cr = if orient == Orientation::Horizontal { Rect::new(r.x + pos, r.y, s, r.h) } else { Rect::new(r.x, r.y + pos, r.w, s) };
            pos += s;
            cr
        })
        .collect()
}

fn layout_node(n: &Node, r: Rect, out: &mut Vec<(Hwnd, Rect)>) {
    match n {
        Node::Leaf(h) => out.push((*h, r)),
        Node::Container { orient, children, weights, .. } => {
            for (c, cr) in children.iter().zip(child_rects(*orient, weights, r)) {
                layout_node(c, cr, out);
            }
        }
    }
}

#[derive(PartialEq, Eq)]
enum EdgeState {
    NotHere,
    Pending,
    Done,
}

fn set_edge_node(n: &mut Node, r: Rect, h: Hwnd, edge: Edge, coord: i32) -> EdgeState {
    match n {
        Node::Leaf(id) => {
            if *id == h { EdgeState::Pending } else { EdgeState::NotHere }
        }
        Node::Container { orient, children, weights, auto } => {
            let rects = child_rects(*orient, weights, r);
            for i in 0..children.len() {
                match set_edge_node(&mut children[i], rects[i], h, edge, coord) {
                    EdgeState::NotHere => continue,
                    EdgeState::Done => return EdgeState::Done,
                    EdgeState::Pending => {
                        let horizontal_edge = matches!(edge, Edge::Left | Edge::Right);
                        if (*orient == Orientation::Horizontal) != horizontal_edge {
                            return EdgeState::Pending;
                        }
                        // divider between zone `d` and `d + 1`
                        let d = match edge {
                            Edge::Right | Edge::Bottom if i + 1 < children.len() => i,
                            Edge::Left | Edge::Top if i > 0 => i - 1,
                            _ => return EdgeState::Pending,
                        };
                        let (start, total) = if horizontal_edge { (rects[d].x, r.w) } else { (rects[d].y, r.h) };
                        let pair = if horizontal_edge { rects[d].w + rects[d + 1].w } else { rects[d].h + rects[d + 1].h };
                        let min = MIN_ZONE_PX.min(pair / 2);
                        let left = (coord - start).clamp(min, pair - min);
                        let mut px: Vec<f64> = rects.iter().map(|rc| if horizontal_edge { rc.w as f64 } else { rc.h as f64 }).collect();
                        px[d] = left as f64;
                        px[d + 1] = (pair - left) as f64;
                        let _ = total;
                        *weights = px;
                        *auto = false;
                        return EdgeState::Done;
                    }
                }
            }
            EdgeState::NotHere
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect::new(0, 0, 1920, 1080);

    fn rect(t: &Tree, h: Hwnd) -> Rect {
        t.rect_of(SCREEN, h).unwrap_or_else(|| panic!("window {h} not in tree"))
    }

    /// Opens windows one after another; every new window is added next to the previous one.
    fn tree_of(ids: &[Hwnd]) -> Tree {
        let mut t = Tree::new();
        let mut last = None;
        for &i in ids {
            t.add(i, SCREEN, last);
            last = Some(i);
        }
        t
    }

    #[test]
    fn single_window_fills_the_screen() {
        assert_eq!(rect(&tree_of(&[1]), 1), SCREEN);
    }

    #[test]
    fn two_windows_share_the_screen_in_halves() {
        let t = tree_of(&[1, 2]);
        assert_eq!(rect(&t, 1), Rect::new(0, 0, 960, 1080));
        assert_eq!(rect(&t, 2), Rect::new(960, 0, 960, 1080));
    }

    #[test]
    fn three_windows_become_three_equal_columns() {
        let t = tree_of(&[1, 2, 3]);
        assert_eq!(rect(&t, 1), Rect::new(0, 0, 640, 1080));
        assert_eq!(rect(&t, 2), Rect::new(640, 0, 640, 1080));
        assert_eq!(rect(&t, 3), Rect::new(1280, 0, 640, 1080));
    }

    #[test]
    fn four_windows_become_four_long_columns() {
        let t = tree_of(&[1, 2, 3, 4]);
        for (i, id) in [1, 2, 3, 4].iter().enumerate() {
            assert_eq!(rect(&t, *id), Rect::new(480 * i as i32, 0, 480, 1080));
        }
    }

    #[test]
    fn fifth_window_only_splits_the_zone_of_window_four() {
        let t = tree_of(&[1, 2, 3, 4, 5]);
        for id in [1, 2, 3] {
            assert_eq!(rect(&t, id), Rect::new(480 * (id as i32 - 1), 0, 480, 1080), "window {id} must not move");
        }
        assert_eq!(rect(&t, 4), Rect::new(1440, 0, 480, 540));
        assert_eq!(rect(&t, 5), Rect::new(1440, 540, 480, 540));
    }

    #[test]
    fn sixth_window_becomes_a_third_zone_below_window_four() {
        let t = tree_of(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(rect(&t, 1), Rect::new(0, 0, 480, 1080));
        assert_eq!(rect(&t, 4), Rect::new(1440, 0, 480, 360));
        assert_eq!(rect(&t, 5), Rect::new(1440, 360, 480, 360));
        assert_eq!(rect(&t, 6), Rect::new(1440, 720, 480, 360));
    }

    #[test]
    fn a_zone_never_holds_more_than_four_windows() {
        let t = tree_of(&[1, 2, 3, 4, 5, 6, 7, 8, 9]);
        fn max_children(n: &Node) -> usize {
            match n {
                Node::Leaf(_) => 0,
                Node::Container { children, .. } => children.len().max(children.iter().map(max_children).max().unwrap_or(0)),
            }
        }
        assert!(max_children(t.root.as_ref().unwrap()) <= MAX_ZONES);
        assert_eq!(t.leaves().len(), 9);
    }

    #[test]
    fn closing_a_window_gives_the_space_to_its_neighbours() {
        let mut t = tree_of(&[1, 2, 3, 4]);
        assert!(t.remove(2));
        assert_eq!(rect(&t, 1), Rect::new(0, 0, 640, 1080));
        assert_eq!(rect(&t, 3), Rect::new(640, 0, 640, 1080));
        assert_eq!(rect(&t, 4), Rect::new(1280, 0, 640, 1080));
        assert!(!t.remove(2));
    }

    #[test]
    fn closing_the_second_window_of_a_nested_zone_lets_the_first_fill_it() {
        let mut t = tree_of(&[1, 2, 3, 4, 5]);
        assert!(t.remove(5));
        assert_eq!(rect(&t, 4), Rect::new(1440, 0, 480, 1080));
        assert_eq!(rect(&t, 1), Rect::new(0, 0, 480, 1080));
    }

    #[test]
    fn closing_the_last_window_empties_the_tree() {
        let mut t = tree_of(&[1]);
        assert!(t.remove(1));
        assert!(t.is_empty());
    }

    #[test]
    fn dropping_on_the_bottom_edge_of_a_window_splits_only_that_window() {
        let mut t = tree_of(&[1, 2, 3]);
        assert!(t.move_to(1, 2, DropZone::Bottom));
        // 1 leaves the row; 2 and 3 share the screen, 2's zone is split into 2 (top) and 1 (bottom)
        assert_eq!(rect(&t, 2), Rect::new(0, 0, 960, 540));
        assert_eq!(rect(&t, 1), Rect::new(0, 540, 960, 540));
        assert_eq!(rect(&t, 3), Rect::new(960, 0, 960, 1080));
    }

    #[test]
    fn dropping_on_a_side_edge_reorders_the_zones_in_a_row() {
        let mut t = tree_of(&[1, 2, 3, 4]);
        assert!(t.move_to(4, 2, DropZone::Left)); // order becomes 1, 4, 2, 3
        let order: Vec<Hwnd> = t.layout(SCREEN).into_iter().map(|(id, _)| id).collect();
        assert_eq!(order, vec![1, 4, 2, 3]);
        assert_eq!(rect(&t, 4), Rect::new(480, 0, 480, 1080));
    }

    #[test]
    fn dropping_on_the_side_of_a_full_row_splits_only_the_target_zone() {
        let mut t = tree_of(&[1, 2, 3, 4]);
        t.add(5, SCREEN, Some(1)); // 5 attaches to window 1 (full row -> nested)
        assert!(t.move_to(5, 3, DropZone::Left)); // row is full again after the move -> nested in 3's zone
        assert_eq!(rect(&t, 1), Rect::new(0, 0, 480, 1080));
        assert_eq!(rect(&t, 5), Rect::new(960, 0, 240, 1080));
        assert_eq!(rect(&t, 3), Rect::new(1200, 0, 240, 1080));
    }

    #[test]
    fn dropping_in_the_centre_swaps_two_windows() {
        let mut t = tree_of(&[1, 2, 3]);
        let (r1, r3) = (rect(&t, 1), rect(&t, 3));
        assert!(t.move_to(1, 3, DropZone::Center));
        assert_eq!(rect(&t, 1), r3);
        assert_eq!(rect(&t, 3), r1);
    }

    #[test]
    fn preview_matches_the_real_result_exactly() {
        let t = tree_of(&[1, 2, 3, 4, 5]);
        for target in [1, 3, 4] {
            for zone in [DropZone::Left, DropZone::Right, DropZone::Top, DropZone::Bottom, DropZone::Center] {
                let predicted = t.preview(SCREEN, 5, target, zone).unwrap();
                let mut real = t.clone();
                real.move_to(5, target, zone);
                assert_eq!(predicted, rect(&real, 5), "target {target} zone {zone:?}");
            }
        }
        assert_eq!(rect(&t, 5), Rect::new(1440, 540, 480, 540), "preview must not change the tree");
    }

    #[test]
    fn invalid_moves_do_nothing() {
        let mut t = tree_of(&[1, 2]);
        assert!(!t.move_to(1, 1, DropZone::Left));
        assert!(!t.move_to(1, 99, DropZone::Left));
        assert!(!t.move_to(99, 1, DropZone::Left));
        assert_eq!(rect(&t, 1), Rect::new(0, 0, 960, 1080));
    }

    #[test]
    fn zones_follow_the_cursor_position() {
        let r = Rect::new(0, 0, 1000, 1000);
        assert_eq!(zone_at(r, 500, 500), DropZone::Center);
        assert_eq!(zone_at(r, 20, 500), DropZone::Left);
        assert_eq!(zone_at(r, 980, 500), DropZone::Right);
        assert_eq!(zone_at(r, 500, 20), DropZone::Top);
        assert_eq!(zone_at(r, 500, 980), DropZone::Bottom);
    }

    #[test]
    fn dragging_a_divider_resizes_the_two_neighbouring_zones() {
        let mut t = tree_of(&[1, 2, 3]);
        assert!(t.set_edge(SCREEN, 1, Edge::Right, 400));
        assert_eq!(rect(&t, 1), Rect::new(0, 0, 400, 1080));
        assert_eq!(rect(&t, 2), Rect::new(400, 0, 880, 1080));
        assert_eq!(rect(&t, 3), Rect::new(1280, 0, 640, 1080), "the third zone keeps its size");
    }

    #[test]
    fn the_left_edge_of_a_window_moves_the_divider_to_its_left_neighbour() {
        let mut t = tree_of(&[1, 2, 3]);
        assert!(t.set_edge(SCREEN, 3, Edge::Left, 1000));
        assert_eq!(rect(&t, 2), Rect::new(640, 0, 360, 1080));
        assert_eq!(rect(&t, 3), Rect::new(1000, 0, 920, 1080));
    }

    #[test]
    fn outer_edges_have_no_divider() {
        let mut t = tree_of(&[1, 2]);
        assert!(!t.set_edge(SCREEN, 1, Edge::Left, 100));
        assert!(!t.set_edge(SCREEN, 2, Edge::Right, 1800));
        assert!(!t.set_edge(SCREEN, 1, Edge::Top, 100));
    }

    #[test]
    fn dragging_the_divider_of_a_whole_zone_moves_it_for_all_windows_in_it() {
        let mut t = tree_of(&[1, 2, 3, 4, 5, 6]); // right-most zone holds 4, 5, 6 stacked
        assert!(t.set_edge(SCREEN, 5, Edge::Left, 1200)); // left edge of a window inside the stack
        for id in [4, 5, 6] {
            assert_eq!(rect(&t, id).x, 1200);
            assert_eq!(rect(&t, id).w, 720);
        }
        assert_eq!(rect(&t, 3), Rect::new(960, 0, 240, 1080));
    }

    #[test]
    fn inner_dividers_only_move_inside_their_own_zone() {
        let mut t = tree_of(&[1, 2, 3, 4, 5, 6]);
        assert!(t.set_edge(SCREEN, 4, Edge::Bottom, 200));
        assert_eq!(rect(&t, 4).h, 200);
        assert_eq!(rect(&t, 5).y, 200);
        assert_eq!(rect(&t, 6), Rect::new(1440, 720, 480, 360), "the third window keeps its size");
        assert_eq!(rect(&t, 1), Rect::new(0, 0, 480, 1080));
    }

    #[test]
    fn zones_stay_equal_until_resized_then_new_windows_only_split_the_target() {
        let mut t = tree_of(&[1, 2]);
        t.set_edge(SCREEN, 1, Edge::Right, 600);
        t.add(3, SCREEN, Some(2)); // 2 is 1320 wide -> 3 attaches to the right, splitting only 2's zone
        assert_eq!(rect(&t, 1), Rect::new(0, 0, 600, 1080), "custom size is kept");
        assert_eq!(rect(&t, 2), Rect::new(600, 0, 660, 1080));
        assert_eq!(rect(&t, 3), Rect::new(1260, 0, 660, 1080));
    }

    #[test]
    fn equalize_resets_all_zone_sizes() {
        let mut t = tree_of(&[1, 2, 3]);
        t.set_edge(SCREEN, 1, Edge::Right, 300);
        t.equalize();
        assert_eq!(rect(&t, 1).w, 640);
        assert_eq!(rect(&t, 2).w, 640);
    }

    #[test]
    fn adding_the_same_window_twice_is_ignored() {
        let mut t = tree_of(&[1, 2]);
        t.add(2, SCREEN, None);
        assert_eq!(t.leaves().len(), 2);
    }
}
