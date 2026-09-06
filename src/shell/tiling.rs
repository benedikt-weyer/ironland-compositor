//! Automatic BSP ("dwindle"-style) tiling, similar to Hyprland's default layout.
//!
//! Each output owns its own tree of splits. Every tiled window is a leaf; mapping a
//! new window splits the currently focused leaf in two, and closing/untiling a
//! window collapses its sibling back into the parent's slot. Windows can be
//! individually floated (`toggle_floating`) to opt out of the layout entirely.

use std::cell::{Ref, RefCell, RefMut};

use smithay::{
    backend::input::InputTime,
    desktop::{Space, layer_map_for_output},
    input::pointer::MotionEvent,
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{IsAlive, Logical, Point, Rectangle, SERIAL_COUNTER},
    wayland::{compositor::with_states, shell::xdg::SurfaceCachedState},
};

use crate::{
    focus::KeyboardFocusTarget,
    shell::workspace::WorkspaceState,
    state::{AnvilState, Backend},
};

use super::WindowElement;

/// Gap between tiled windows, and between tiled windows and the output edges.
const GAP: i32 = 8;
/// How much a keyboard-driven resize changes a split's ratio per key press.
const RESIZE_STEP: f32 = 0.05;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

enum Node {
    Leaf(WindowElement),
    Split {
        /// `true` = children sit side by side (a split along x); `false` = stacked (split along y).
        vertical: bool,
        /// Fraction of the area given to `a`.
        ratio: f32,
        a: Box<Node>,
        b: Box<Node>,
    },
}

#[derive(Default)]
pub struct TilingLayout {
    root: Option<Node>,
    /// The most recently inserted/targeted leaf, used as the split target when
    /// there is no focused window to split against.
    last: Option<WindowElement>,
}

impl TilingLayout {
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    pub fn contains(&self, window: &WindowElement) -> bool {
        fn walk(node: &Node, window: &WindowElement) -> bool {
            match node {
                Node::Leaf(w) => w == window,
                Node::Split { a, b, .. } => walk(a, window) || walk(b, window),
            }
        }
        self.root.as_ref().is_some_and(|n| walk(n, window))
    }

    pub fn windows(&self) -> Vec<WindowElement> {
        fn walk(node: &Node, out: &mut Vec<WindowElement>) {
            match node {
                Node::Leaf(w) => out.push(w.clone()),
                Node::Split { a, b, .. } => {
                    walk(a, out);
                    walk(b, out);
                }
            }
        }
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            walk(root, &mut out);
        }
        out
    }

    /// Insert `window`, splitting the leaf for `target` (or the last-touched
    /// leaf, or an arbitrary leaf) in half.
    pub fn insert(&mut self, window: WindowElement, area: Rectangle<i32, Logical>, target: Option<&WindowElement>) {
        self.last = Some(window.clone());

        let Some(root) = self.root.take() else {
            self.root = Some(Node::Leaf(window));
            return;
        };

        let target = target
            .filter(|t| Self::tree_contains(&root, t))
            .cloned()
            .or_else(|| self.last.clone())
            .unwrap_or_else(|| Self::first_leaf(&root).clone());

        let target_rect = Self::layout_rec_rect(&root, area, &target).unwrap_or(area);
        let vertical = target_rect.size.w >= target_rect.size.h;

        let new_root = Self::replace_leaf(
            root,
            &target,
            Node::Split {
                vertical,
                ratio: 0.5,
                a: Box::new(Node::Leaf(target.clone())),
                b: Box::new(Node::Leaf(window)),
            },
        );
        self.root = Some(new_root);
    }

    fn tree_contains(node: &Node, window: &WindowElement) -> bool {
        match node {
            Node::Leaf(w) => w == window,
            Node::Split { a, b, .. } => Self::tree_contains(a, window) || Self::tree_contains(b, window),
        }
    }

    fn first_leaf(node: &Node) -> &WindowElement {
        match node {
            Node::Leaf(w) => w,
            Node::Split { a, .. } => Self::first_leaf(a),
        }
    }

    /// Replace the leaf holding `target` with `replacement`. `target` is assumed
    /// to be present in `node`.
    fn replace_leaf(node: Node, target: &WindowElement, replacement: Node) -> Node {
        match node {
            Node::Leaf(w) => {
                if &w == target {
                    replacement
                } else {
                    Node::Leaf(w)
                }
            }
            Node::Split { vertical, ratio, a, b } => {
                if Self::tree_contains(&a, target) {
                    Node::Split {
                        vertical,
                        ratio,
                        a: Box::new(Self::replace_leaf(*a, target, replacement)),
                        b,
                    }
                } else {
                    Node::Split {
                        vertical,
                        ratio,
                        a,
                        b: Box::new(Self::replace_leaf(*b, target, replacement)),
                    }
                }
            }
        }
    }

    /// Remove `window` from the tree, collapsing its sibling into its slot.
    pub fn remove(&mut self, window: &WindowElement) -> bool {
        if self.last.as_ref() == Some(window) {
            self.last = None;
        }
        let Some(root) = self.root.take() else {
            return false;
        };
        let (new_root, found) = Self::remove_rec(root, window);
        self.root = new_root;
        found
    }

    fn remove_rec(node: Node, target: &WindowElement) -> (Option<Node>, bool) {
        match node {
            Node::Leaf(w) => {
                if &w == target {
                    (None, true)
                } else {
                    (Some(Node::Leaf(w)), false)
                }
            }
            Node::Split { vertical, ratio, a, b } => {
                let (new_a, found_a) = Self::remove_rec(*a, target);
                if found_a {
                    return match new_a {
                        None => (Some(*b), true),
                        Some(na) => (Some(Node::Split { vertical, ratio, a: Box::new(na), b }), true),
                    };
                }
                // Not found under `a`; `remove_rec` hands back the subtree unchanged.
                let a = Box::new(new_a.expect("unchanged subtree is always returned"));
                let (new_b, found_b) = Self::remove_rec(*b, target);
                if found_b {
                    return match new_b {
                        None => (Some(*a), true),
                        Some(nb) => (Some(Node::Split { vertical, ratio, a, b: Box::new(nb) }), true),
                    };
                }
                let b = Box::new(new_b.expect("unchanged subtree is always returned"));
                (Some(Node::Split { vertical, ratio, a, b }), false)
            }
        }
    }

    /// Swap the positions of two tiled windows in the tree.
    pub fn swap(&mut self, a: &WindowElement, b: &WindowElement) {
        fn walk(node: &mut Node, a: &WindowElement, b: &WindowElement) {
            match node {
                Node::Leaf(w) => {
                    if w == a {
                        *w = b.clone();
                    } else if w == b {
                        *w = a.clone();
                    }
                }
                Node::Split { a: na, b: nb, .. } => {
                    walk(na, a, b);
                    walk(nb, a, b);
                }
            }
        }
        if let Some(root) = &mut self.root {
            walk(root, a, b);
        }
    }

    /// Grow (positive `delta`) or shrink (negative) the focused window's share
    /// of the nearest ancestor split matching `vertical`.
    pub fn adjust_ratio(&mut self, window: &WindowElement, vertical: bool, delta: f32) {
        fn walk(node: &mut Node, target: &WindowElement, vertical: bool, delta: f32) -> (bool, bool) {
            match node {
                Node::Leaf(w) => (&*w == target, false),
                Node::Split { vertical: v, ratio, a, b } => {
                    let (in_a, done_a) = walk(a, target, vertical, delta);
                    if in_a {
                        if !done_a && *v == vertical {
                            *ratio = (*ratio + delta).clamp(0.1, 0.9);
                            return (true, true);
                        }
                        return (true, done_a);
                    }
                    let (in_b, done_b) = walk(b, target, vertical, delta);
                    if in_b {
                        if !done_b && *v == vertical {
                            *ratio = (*ratio - delta).clamp(0.1, 0.9);
                            return (true, true);
                        }
                        return (true, done_b);
                    }
                    (false, false)
                }
            }
        }
        if let Some(root) = &mut self.root {
            walk(root, window, vertical, delta);
        }
    }

    /// Remove any windows that are no longer alive. Returns `true` if the tree changed.
    pub fn retain_alive(&mut self) -> bool {
        let dead: Vec<_> = self.windows().into_iter().filter(|w| !w.alive()).collect();
        let mut changed = false;
        for window in dead {
            changed |= self.remove(&window);
        }
        changed
    }

    /// Compute the on-screen rectangle for every tiled window within `area`.
    pub fn layout(&self, area: Rectangle<i32, Logical>) -> Vec<(WindowElement, Rectangle<i32, Logical>)> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            Self::layout_rec(root, area, &mut out);
        }
        out
    }

    fn layout_rec(node: &Node, area: Rectangle<i32, Logical>, out: &mut Vec<(WindowElement, Rectangle<i32, Logical>)>) {
        match node {
            Node::Leaf(w) => out.push((w.clone(), area)),
            Node::Split { vertical, ratio, a, b } => {
                let half_gap = GAP / 2;
                if *vertical {
                    let wa = ((area.size.w as f32) * ratio) as i32;
                    let area_a =
                        Rectangle::new(area.loc, (wa - half_gap, area.size.h).into());
                    let area_b = Rectangle::new(
                        Point::from((area.loc.x + wa + half_gap, area.loc.y)),
                        (area.size.w - wa - half_gap, area.size.h).into(),
                    );
                    Self::layout_rec(a, area_a, out);
                    Self::layout_rec(b, area_b, out);
                } else {
                    let ha = ((area.size.h as f32) * ratio) as i32;
                    let area_a =
                        Rectangle::new(area.loc, (area.size.w, ha - half_gap).into());
                    let area_b = Rectangle::new(
                        Point::from((area.loc.x, area.loc.y + ha + half_gap)),
                        (area.size.w, area.size.h - ha - half_gap).into(),
                    );
                    Self::layout_rec(a, area_a, out);
                    Self::layout_rec(b, area_b, out);
                }
            }
        }
    }

    fn layout_rec_rect(
        node: &Node,
        area: Rectangle<i32, Logical>,
        target: &WindowElement,
    ) -> Option<Rectangle<i32, Logical>> {
        let mut rects = Vec::new();
        Self::layout_rec(node, area, &mut rects);
        rects.into_iter().find(|(w, _)| w == target).map(|(_, r)| r)
    }
}

/// Every one of an output's tiling trees, one per workspace, indexed by
/// workspace number. Grows lazily as higher workspace indices are touched;
/// entries are never removed (an empty [`TilingLayout`] is cheap and
/// `retain_alive`/pruning elsewhere deals with staleness).
#[derive(Default)]
pub struct TilingState(RefCell<Vec<TilingLayout>>);

impl TilingState {
    fn get(output: &Output) -> &TilingState {
        output.user_data().insert_if_missing(TilingState::default);
        output.user_data().get::<TilingState>().unwrap()
    }

    fn ensure_len(&self, idx: usize) {
        let mut v = self.0.borrow_mut();
        if v.len() <= idx {
            v.resize_with(idx + 1, TilingLayout::default);
        }
    }

    /// Borrows workspace `idx`'s tiling tree for `output`, growing storage as needed.
    pub fn tree(output: &Output, idx: usize) -> Ref<'_, TilingLayout> {
        let state = Self::get(output);
        state.ensure_len(idx);
        Ref::map(state.0.borrow(), |v| &v[idx])
    }

    /// Mutably borrows workspace `idx`'s tiling tree for `output`, growing storage as needed.
    pub fn tree_mut(output: &Output, idx: usize) -> RefMut<'_, TilingLayout> {
        let state = Self::get(output);
        state.ensure_len(idx);
        RefMut::map(state.0.borrow_mut(), |v| &mut v[idx])
    }

    /// Every workspace's tiling tree for `output`, in workspace-index order.
    pub fn all(output: &Output) -> Ref<'_, Vec<TilingLayout>> {
        Self::get(output).0.borrow()
    }

    /// How many workspace slots have been touched for `output` so far.
    pub fn len(output: &Output) -> usize {
        Self::get(output).0.borrow().len()
    }
}

pub(crate) fn tiling_area(space: &Space<WindowElement>, output: &Output) -> Rectangle<i32, Logical> {
    let geo = space.output_geometry(output).unwrap_or_default();
    let map = layer_map_for_output(output);
    let zone = map.non_exclusive_zone();
    Rectangle::new(geo.loc + zone.loc, zone.size)
}

/// Finds which output's tiling tree (and at which workspace index) contains
/// `window`, searching every workspace, not just the active one - a window
/// can be tiled on a hidden workspace (e.g. its client closed it in the
/// background).
pub(crate) fn locate<BackendData: Backend>(
    state: &AnvilState<BackendData>,
    window: &WindowElement,
) -> Option<(Output, usize)> {
    for output in state.space.outputs() {
        for (idx, layout) in TilingState::all(output).iter().enumerate() {
            if layout.contains(window) {
                return Some((output.clone(), idx));
            }
        }
    }
    None
}

pub(crate) fn current_focused_window<BackendData: Backend>(
    state: &AnvilState<BackendData>,
) -> Option<WindowElement> {
    let keyboard = state.seat.get_keyboard()?;
    match keyboard.current_focus()? {
        KeyboardFocusTarget::Window(w) => Some(WindowElement(w)),
        _ => None,
    }
}

/// Re-applies a tiled window's own layout on every commit of its surface.
///
/// A client is supposed to honor a configure's suggested size, but not every
/// toolkit's very first (pre-map) commit reliably does - some render at
/// their own preferred/minimum content size regardless of what was
/// suggested, only picking up server-driven resizes correctly once they're
/// already mapped. Re-checking here, on each real commit, makes the tile the
/// window was assigned the source of truth continuously rather than only at
/// insert time: `apply_layout` already no-ops (no configure sent) once the
/// window's pending size already matches its tile, so this is cheap once
/// the window has caught up.
pub(crate) fn resync_committed_window<BackendData: Backend>(state: &mut AnvilState<BackendData>, surface: &WlSurface) {
    let Some(window) = state.window_for_surface(surface) else {
        return;
    };
    let Some((output, _idx)) = locate(state, &window) else {
        return;
    };
    apply_layout(state, &output);
}

pub(crate) fn raise_and_focus<BackendData: Backend>(state: &mut AnvilState<BackendData>, window: &WindowElement) {
    state.space.raise_element(window, true);
    if let Some(keyboard) = state.seat.get_keyboard() {
        let serial = SERIAL_COUNTER.next_serial();
        keyboard.set_focus(state, Some(window.clone().into()), serial);
    }
    warp_pointer_to(state, window);
}

/// Moves the pointer to the center of `window`, if `focus.mouse_follows_focus`
/// is enabled. Every caller of [`raise_and_focus`] is a focus change that
/// didn't originate from the pointer itself (a workspace switch, cycling
/// windows, a newly mapped window taking focus, activating a window from the
/// dock) - `focus.follows_mouse` only ever runs from real pointer-motion
/// input events (see `input_handler.rs`), so the two settings can't end up
/// fighting each other through this.
fn warp_pointer_to<BackendData: Backend>(state: &mut AnvilState<BackendData>, window: &WindowElement) {
    if !state.config.focus.mouse_follows_focus {
        return;
    }
    let Some(geo) = state.space.element_geometry(window) else {
        return;
    };
    let center = Point::<i32, Logical>::from((geo.loc.x + geo.size.w / 2, geo.loc.y + geo.size.h / 2))
        .to_f64();

    let pointer = state.pointer.clone();
    pointer.set_location(center);
    let under = state.surface_under(center);
    pointer.motion(
        state,
        under,
        &MotionEvent {
            location: center,
            serial: SERIAL_COUNTER.next_serial(),
            time: InputTime::now(),
        },
    );
    pointer.frame(state);
}

/// Re-flow every tiled window on `output`'s *active* workspace to match its
/// current tree. Hidden workspaces are left untouched (and are up to date
/// whenever they're next shown by [`crate::shell::workspace`]).
pub fn apply_layout<BackendData: Backend>(state: &mut AnvilState<BackendData>, output: &Output) {
    let idx = WorkspaceState::get(output).active();
    let area = tiling_area(&state.space, output);
    let rects = TilingState::tree(output, idx).layout(area);
    for (window, rect) in rects {
        #[allow(irrefutable_let_patterns)]
        if let Some(toplevel) = window.0.toplevel() {
            let changed = toplevel.with_pending_state(|s| {
                if s.size != Some(rect.size) {
                    s.size = Some(rect.size);
                    true
                } else {
                    false
                }
            });
            if changed && toplevel.is_initial_configure_sent() {
                toplevel.send_pending_configure();
            }
        }
        state.space.map_element(window, rect.loc, false);
    }
}

/// Should this newly-created toplevel participate in tiling at all?
/// Dialogs (windows with a parent) and fixed-size utility windows stay floating.
pub fn should_tile(window: &WindowElement) -> bool {
    #[allow(irrefutable_let_patterns)]
    let Some(toplevel) = window.0.toplevel() else {
        return false;
    };
    if toplevel.parent().is_some() {
        return false;
    }
    let (min, max) = with_states(toplevel.wl_surface(), |states| {
        let mut cached = states.cached_state.get::<SurfaceCachedState>();
        let state = cached.current();
        (state.min_size, state.max_size)
    });
    let fixed_size = min.w > 0 && min.h > 0 && min == max;
    !fixed_size
}

/// Insert a newly mapped window into the tiling tree of the output it belongs on.
pub fn tile_new_window<BackendData: Backend>(
    state: &mut AnvilState<BackendData>,
    window: &WindowElement,
    pointer_location: Point<f64, Logical>,
) {
    let Some(output) = state
        .space
        .output_under(pointer_location)
        .next()
        .or_else(|| state.space.outputs().next())
        .cloned()
    else {
        return;
    };

    let idx = WorkspaceState::get(&output).active();
    let target = current_focused_window(state).filter(|w| TilingState::tree(&output, idx).contains(w));

    crate::shell::workspace::assign_new_window(window, &output, false);

    let area = tiling_area(&state.space, &output);
    TilingState::tree_mut(&output, idx).insert(window.clone(), area, target.as_ref());

    apply_layout(state, &output);
    raise_and_focus(state, window);
}

/// Remove `window` from whichever output/workspace's tiling tree contains
/// it (wherever that is - it need not be the active/visible one), and
/// re-flow that output's active workspace if it was affected. The window
/// becomes floating, tracked at the workspace it was pulled out of. Returns
/// `true` if it was tiled.
pub fn untile_window<BackendData: Backend>(state: &mut AnvilState<BackendData>, window: &WindowElement) -> bool {
    let Some((output, idx)) = locate(state, window) else {
        return false;
    };
    let removed = TilingState::tree_mut(&output, idx).remove(window);
    if removed {
        crate::shell::workspace::mark_floating(state, window, &output, idx);
        if idx == WorkspaceState::get(&output).active() {
            apply_layout(state, &output);
        }
    }
    removed
}

/// Toggle whether `window` participates in tiling.
pub fn toggle_floating<BackendData: Backend>(state: &mut AnvilState<BackendData>, window: &WindowElement) {
    if untile_window(state, window) {
        return;
    }
    tile_new_window(state, window, state.pointer.current_location());
}

/// Re-flow every output's tiling tree, e.g. after output geometry changed
/// (scale, rotation, added/removed monitors).
pub fn retile_all_outputs<BackendData: Backend>(state: &mut AnvilState<BackendData>) {
    let outputs: Vec<Output> = state.space.outputs().cloned().collect();
    for output in outputs {
        apply_layout(state, &output);
    }
}

/// Drop dead windows from every output's tiling tree (every workspace, not
/// just the active one) and re-flow whichever active workspaces changed. If
/// the currently focused window is among the casualties, focus moves to its
/// nearest surviving neighbor in the same tiling tree, falling back to
/// whatever else is left on that workspace. Returns whether any window was
/// removed.
pub fn cleanup_dead<BackendData: Backend>(state: &mut AnvilState<BackendData>) -> bool {
    let outputs: Vec<Output> = state.space.outputs().cloned().collect();
    let mut changed = false;

    let dead_focus = current_focused_window(state).filter(|w| !w.alive());
    let fallback = dead_focus.as_ref().and_then(|focused| {
        let (output, idx) = locate(state, focused)?;
        let area = tiling_area(&state.space, &output);
        let rects = TilingState::tree(&output, idx).layout(area);
        let replacement = nearest_neighbor(&rects, focused);
        Some((output, idx, replacement))
    });

    for output in &outputs {
        let active = WorkspaceState::get(output).active();
        let mut active_changed = false;
        for idx in 0..TilingState::len(output) {
            let workspace_changed = TilingState::tree_mut(output, idx).retain_alive();
            changed |= workspace_changed;
            active_changed |= workspace_changed && idx == active;
        }
        if active_changed {
            apply_layout(state, output);
        }
    }
    changed |= crate::shell::workspace::cleanup_dead(state);

    if dead_focus.is_some() {
        match fallback.as_ref().and_then(|(_, _, w)| w.clone()).filter(|w| w.alive()) {
            Some(target) => raise_and_focus(state, &target),
            None => match fallback {
                Some((output, idx, _)) => crate::shell::workspace::focus_first_in_workspace(state, &output, idx),
                None => {
                    if let Some(keyboard) = state.seat.get_keyboard() {
                        let serial = SERIAL_COUNTER.next_serial();
                        keyboard.set_focus(state, None, serial);
                    }
                }
            },
        }
    }

    changed
}

/// Move keyboard focus to the tiled window neighboring the currently focused
/// one in `dir`. No-op if the focused window isn't tiled. If there is no
/// neighbor in that direction on the current output, hops to the adjacent
/// monitor in `dir` (per the monitor layout) instead, focusing a window there.
pub fn focus_direction<BackendData: Backend>(state: &mut AnvilState<BackendData>, dir: Direction) {
    let Some(focused) = current_focused_window(state) else {
        return;
    };
    let Some((output, idx)) = locate(state, &focused) else {
        return;
    };
    let area = tiling_area(&state.space, &output);
    let rects = TilingState::tree(&output, idx).layout(area);
    if let Some(target) = neighbor(&rects, &focused, dir) {
        raise_and_focus(state, &target);
        return;
    }
    if let Some(next_output) = output_in_direction(state, &output, dir) {
        let next_idx = WorkspaceState::get(&next_output).active();
        crate::shell::workspace::focus_first_in_workspace(state, &next_output, next_idx);
    }
}

/// Swap the currently focused tiled window with its neighbor in `dir`.
pub fn swap_direction<BackendData: Backend>(state: &mut AnvilState<BackendData>, dir: Direction) {
    let Some(focused) = current_focused_window(state) else {
        return;
    };
    let Some((output, idx)) = locate(state, &focused) else {
        return;
    };
    let area = tiling_area(&state.space, &output);
    let rects = TilingState::tree(&output, idx).layout(area);
    let Some(target) = neighbor(&rects, &focused, dir) else {
        return;
    };
    TilingState::tree_mut(&output, idx).swap(&focused, &target);
    apply_layout(state, &output);
    raise_and_focus(state, &focused);
}

/// Grow the currently focused tiled window towards `dir` by shrinking its neighbor.
pub fn resize_tiled<BackendData: Backend>(state: &mut AnvilState<BackendData>, dir: Direction) {
    let Some(focused) = current_focused_window(state) else {
        return;
    };
    let Some((output, idx)) = locate(state, &focused) else {
        return;
    };
    let vertical = matches!(dir, Direction::Left | Direction::Right);
    let grow = matches!(dir, Direction::Right | Direction::Down);
    let delta = if grow { RESIZE_STEP } else { -RESIZE_STEP };
    TilingState::tree_mut(&output, idx).adjust_ratio(&focused, vertical, delta);
    apply_layout(state, &output);
}

fn center(rect: &Rectangle<i32, Logical>) -> (i32, i32) {
    (rect.loc.x + rect.size.w / 2, rect.loc.y + rect.size.h / 2)
}

/// Score how well `to` sits in `dir` relative to `from`, lower being closer.
/// `None` if `to` isn't in that direction at all.
fn directional_score(dir: Direction, from: (i32, i32), to: (i32, i32)) -> Option<i64> {
    let (fx, fy) = from;
    let (tx, ty) = to;
    let matches_dir = match dir {
        Direction::Left => tx < fx,
        Direction::Right => tx > fx,
        Direction::Up => ty < fy,
        Direction::Down => ty > fy,
    };
    if !matches_dir {
        return None;
    }
    let (primary, secondary) = match dir {
        Direction::Left | Direction::Right => ((tx - fx).abs(), (ty - fy).abs()),
        Direction::Up | Direction::Down => ((ty - fy).abs(), (tx - fx).abs()),
    };
    Some(primary as i64 * 4 + secondary as i64)
}

fn neighbor(
    rects: &[(WindowElement, Rectangle<i32, Logical>)],
    focused: &WindowElement,
    dir: Direction,
) -> Option<WindowElement> {
    let focused_rect = rects.iter().find(|(w, _)| w == focused)?.1;
    let from = center(&focused_rect);

    let mut best: Option<(i64, WindowElement)> = None;
    for (w, r) in rects {
        if w == focused {
            continue;
        }
        let Some(score) = directional_score(dir, from, center(r)) else {
            continue;
        };
        if best.as_ref().is_none_or(|(best_score, _)| score < *best_score) {
            best = Some((score, w.clone()));
        }
    }
    best.map(|(_, w)| w)
}

/// The output adjacent to `from` in `dir`, per the outputs' arranged
/// positions in `state.space` (i.e. the monitor layout), if any.
fn output_in_direction<BackendData: Backend>(
    state: &AnvilState<BackendData>,
    from: &Output,
    dir: Direction,
) -> Option<Output> {
    let from_geo = state.space.output_geometry(from)?;
    let origin = center(&from_geo);

    let mut best: Option<(i64, Output)> = None;
    for output in state.space.outputs() {
        if output == from {
            continue;
        }
        let Some(geo) = state.space.output_geometry(output) else {
            continue;
        };
        let Some(score) = directional_score(dir, origin, center(&geo)) else {
            continue;
        };
        if best.as_ref().is_none_or(|(best_score, _)| score < *best_score) {
            best = Some((score, output.clone()));
        }
    }
    best.map(|(_, o)| o)
}

/// The tiled window in `rects` geometrically closest to `focused`, in any
/// direction. Used to pick a replacement focus when `focused` is about to
/// disappear (e.g. it was just closed).
fn nearest_neighbor(
    rects: &[(WindowElement, Rectangle<i32, Logical>)],
    focused: &WindowElement,
) -> Option<WindowElement> {
    let focused_rect = rects.iter().find(|(w, _)| w == focused)?.1;
    let (fx, fy) = center(&focused_rect);
    rects
        .iter()
        .filter(|(w, _)| w != focused)
        .min_by_key(|(_, r)| {
            let (cx, cy) = center(r);
            let dx = (cx - fx) as i64;
            let dy = (cy - fy) as i64;
            dx * dx + dy * dy
        })
        .map(|(w, _)| w.clone())
}
