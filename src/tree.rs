//! Binary split tree for one monitor on one virtual desktop.
//!
//! Every window is a leaf and every split divides its area in two. This is what makes
//! COSMIC-style placement possible: dropping a window on the edge of another window only splits
//! that one window's area, all other windows keep their place.

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

/// `Horizontal`: children side by side (A left, B right). `Vertical`: stacked (A top, B bottom).
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
    Split { orient: Orientation, ratio: f64, a: Box<Node>, b: Box<Node> },
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

    /// Adds a window by splitting the focused window (or the largest one) along its longer side.
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
        let zone = if r.w >= r.h { DropZone::Right } else { DropZone::Bottom };
        self.insert_around(target, h, zone);
    }

    /// Removes a window; its sibling takes the place of the parent split.
    pub fn remove(&mut self, h: Hwnd) -> bool {
        let Some(root) = self.root.take() else { return false };
        let (new_root, removed) = remove_node(root, h);
        self.root = new_root;
        removed
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

    /// Rectangle of every window. Gaps are not needed for the default look (gap 0).
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

    /// Moves the divider that forms the given edge of a window to a new coordinate.
    pub fn set_edge(&mut self, area: Rect, h: Hwnd, edge: Edge, coordinate: i32) -> bool {
        let horizontal = matches!(edge, Edge::Left | Edge::Right);
        let leaf_in_a = matches!(edge, Edge::Right | Edge::Bottom);
        match &mut self.root {
            Some(root) => matches!(set_edge_node(root, area, h, horizontal, leaf_in_a, coordinate), EdgeState::Done),
            None => false,
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

fn collect(n: &Node, out: &mut Vec<Hwnd>) {
    match n {
        Node::Leaf(h) => out.push(*h),
        Node::Split { a, b, .. } => {
            collect(a, out);
            collect(b, out);
        }
    }
}

fn remove_node(n: Node, h: Hwnd) -> (Option<Node>, bool) {
    match n {
        Node::Leaf(id) if id == h => (None, true),
        Node::Leaf(_) => (Some(n), false),
        Node::Split { orient, ratio, a, b } => {
            let (na, ra) = remove_node(*a, h);
            if ra {
                return (Some(match na {
                    Some(na) => Node::Split { orient, ratio, a: Box::new(na), b },
                    None => *b,
                }), true);
            }
            let (nb, rb) = remove_node(*b, h);
            let a = na.expect("left child unchanged when nothing removed");
            if rb {
                return (Some(match nb {
                    Some(nb) => Node::Split { orient, ratio, a: Box::new(a), b: Box::new(nb) },
                    None => a,
                }), true);
            }
            (Some(Node::Split { orient, ratio, a: Box::new(a), b: Box::new(nb.expect("right child unchanged")) }), false)
        }
    }
}

fn insert_node(n: Node, target: Hwnd, added: Hwnd, zone: DropZone) -> Node {
    match n {
        Node::Leaf(id) if id == target => {
            let orient = if matches!(zone, DropZone::Left | DropZone::Right) { Orientation::Horizontal } else { Orientation::Vertical };
            let (first, second) = if matches!(zone, DropZone::Left | DropZone::Top) { (added, target) } else { (target, added) };
            Node::Split { orient, ratio: 0.5, a: Box::new(Node::Leaf(first)), b: Box::new(Node::Leaf(second)) }
        }
        Node::Leaf(_) => n,
        Node::Split { orient, ratio, a, b } => Node::Split {
            orient,
            ratio,
            a: Box::new(insert_node(*a, target, added, zone)),
            b: Box::new(insert_node(*b, target, added, zone)),
        },
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
        Node::Split { a: l, b: r, .. } => {
            swap_ids(l, a, b);
            swap_ids(r, a, b);
        }
    }
}

fn child_rects(orient: Orientation, ratio: f64, r: Rect) -> (Rect, Rect) {
    match orient {
        Orientation::Horizontal => {
            let wa = (r.w as f64 * ratio).round() as i32;
            (Rect::new(r.x, r.y, wa, r.h), Rect::new(r.x + wa, r.y, r.w - wa, r.h))
        }
        Orientation::Vertical => {
            let ha = (r.h as f64 * ratio).round() as i32;
            (Rect::new(r.x, r.y, r.w, ha), Rect::new(r.x, r.y + ha, r.w, r.h - ha))
        }
    }
}

fn layout_node(n: &Node, r: Rect, out: &mut Vec<(Hwnd, Rect)>) {
    match n {
        Node::Leaf(h) => out.push((*h, r)),
        Node::Split { orient, ratio, a, b } => {
            let (ra, rb) = child_rects(*orient, *ratio, r);
            layout_node(a, ra, out);
            layout_node(b, rb, out);
        }
    }
}

#[derive(PartialEq, Eq)]
enum EdgeState {
    NotHere,
    Pending,
    Done,
}

fn set_edge_node(n: &mut Node, r: Rect, h: Hwnd, horizontal: bool, leaf_in_a: bool, coord: i32) -> EdgeState {
    match n {
        Node::Leaf(id) => {
            if *id == h { EdgeState::Pending } else { EdgeState::NotHere }
        }
        Node::Split { orient, ratio, a, b } => {
            let (ra, rb) = child_rects(*orient, *ratio, r);
            let mut in_a = true;
            let mut st = set_edge_node(a, ra, h, horizontal, leaf_in_a, coord);
            if st == EdgeState::NotHere {
                in_a = false;
                st = set_edge_node(b, rb, h, horizontal, leaf_in_a, coord);
            }
            if st == EdgeState::Pending && (*orient == Orientation::Horizontal) == horizontal && in_a == leaf_in_a {
                let new_ratio = if horizontal { (coord - r.x) as f64 / r.w as f64 } else { (coord - r.y) as f64 / r.h as f64 };
                *ratio = new_ratio.clamp(0.1, 0.9);
                return EdgeState::Done;
            }
            st
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
    fn second_window_splits_the_wide_screen_in_two_halves() {
        let t = tree_of(&[1, 2]);
        assert_eq!(rect(&t, 1), Rect::new(0, 0, 960, 1080));
        assert_eq!(rect(&t, 2), Rect::new(960, 0, 960, 1080));
    }

    #[test]
    fn new_windows_split_only_the_focused_window() {
        let t = tree_of(&[1, 2, 3]); // 3 splits window 2 (focused), window 1 is untouched
        assert_eq!(rect(&t, 1), Rect::new(0, 0, 960, 1080));
        assert_eq!(rect(&t, 2), Rect::new(960, 0, 960, 540));
        assert_eq!(rect(&t, 3), Rect::new(960, 540, 960, 540));
    }

    #[test]
    fn four_windows_can_form_a_2x2_grid() {
        let mut t = tree_of(&[1, 2, 3]);
        t.add(4, SCREEN, Some(1)); // split window 1 (left half) vertically
        assert_eq!(rect(&t, 1), Rect::new(0, 0, 960, 540));
        assert_eq!(rect(&t, 4), Rect::new(0, 540, 960, 540));
        assert_eq!(rect(&t, 2), Rect::new(960, 0, 960, 540));
        assert_eq!(rect(&t, 3), Rect::new(960, 540, 960, 540));
    }

    #[test]
    fn one_left_and_three_stacked_right_is_possible() {
        let mut t = tree_of(&[1, 2]);
        t.add(3, SCREEN, Some(2));
        // split window 3 (bottom right, 960x540) along its longer side -> side by side, so force a vertical stack:
        t.move_to(3, 2, DropZone::Bottom);
        t.add(4, SCREEN, Some(3));
        t.move_to(4, 3, DropZone::Bottom);
        assert_eq!(rect(&t, 1), Rect::new(0, 0, 960, 1080));
        for id in [2, 3, 4] {
            assert_eq!(rect(&t, id).x, 960);
            assert_eq!(rect(&t, id).w, 960);
        }
    }

    #[test]
    fn removing_a_window_gives_its_space_to_the_sibling_only() {
        let mut t = tree_of(&[1, 2, 3]);
        let before_1 = rect(&t, 1);
        assert!(t.remove(3));
        assert_eq!(rect(&t, 1), before_1);
        assert_eq!(rect(&t, 2), Rect::new(960, 0, 960, 1080));
        assert!(!t.remove(3));
    }

    #[test]
    fn removing_the_last_window_empties_the_tree() {
        let mut t = tree_of(&[1]);
        assert!(t.remove(1));
        assert!(t.is_empty());
    }

    #[test]
    fn dropping_on_an_edge_splits_only_the_target_window() {
        let mut t = tree_of(&[1, 2, 3]);
        let r1 = rect(&t, 1);
        // drag 3 onto the left edge of window 2
        assert!(t.move_to(3, 2, DropZone::Left));
        assert_eq!(rect(&t, 1), r1, "window 1 must not move");
        assert_eq!(rect(&t, 3), Rect::new(960, 0, 480, 1080));
        assert_eq!(rect(&t, 2), Rect::new(1440, 0, 480, 1080));
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
        let t = tree_of(&[1, 2, 3]);
        for zone in [DropZone::Left, DropZone::Right, DropZone::Top, DropZone::Bottom, DropZone::Center] {
            let predicted = t.preview(SCREEN, 3, 1, zone).unwrap();
            let mut real = t.clone();
            real.move_to(3, 1, zone);
            assert_eq!(predicted, rect(&real, 3), "zone {zone:?}");
        }
        // preview must not change the original tree
        assert_eq!(rect(&t, 3), Rect::new(960, 540, 960, 540));
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
    fn resizing_an_edge_moves_the_divider() {
        let mut t = tree_of(&[1, 2]);
        // window 1's right edge dragged to x = 480
        assert!(t.set_edge(SCREEN, 1, Edge::Right, 480));
        assert_eq!(rect(&t, 1), Rect::new(0, 0, 480, 1080));
        assert_eq!(rect(&t, 2), Rect::new(480, 0, 1440, 1080));
        // an outer edge has no divider
        assert!(!t.set_edge(SCREEN, 1, Edge::Left, 100));
    }

    #[test]
    fn resize_changes_only_the_neighbours_of_that_divider() {
        let mut t = tree_of(&[1, 2, 3]);
        assert!(t.set_edge(SCREEN, 2, Edge::Bottom, 300));
        assert_eq!(rect(&t, 1), Rect::new(0, 0, 960, 1080));
        assert_eq!(rect(&t, 2).h, 300);
        assert_eq!(rect(&t, 3), Rect::new(960, 300, 960, 780));
    }

    #[test]
    fn adding_the_same_window_twice_is_ignored() {
        let mut t = tree_of(&[1, 2]);
        t.add(2, SCREEN, None);
        assert_eq!(t.leaves().len(), 2);
    }
}
