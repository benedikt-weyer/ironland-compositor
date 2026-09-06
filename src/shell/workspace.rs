//! Virtual desktops ("workspaces"), layered on top of [`super::tiling`].
//!
//! Each output keeps its own set of workspaces - a workspace is just a
//! workspace-indexed slot: tiled windows live in [`super::tiling::TilingState`]
//! (already indexed by workspace), and floating windows are tracked here in
//! a small per-workspace registry, since unlike tiled windows their position
//! isn't recomputable from a layout tree and has to be remembered across a
//! hide/show cycle.
//!
//! Two things sit on top of that per-output storage:
//!
//! - **Mode** ([`crate::config::WorkspaceMode`]): in `PerMonitor` mode,
//!   switching workspaces only touches the output the switch was requested
//!   on. In `Combined` mode, every output is switched to the same index at
//!   once, so all monitors always show "workspace N" together (GNOME-style).
//!   Moving a window to an adjacent workspace always targets the window's
//!   own output in both modes - workspaces don't relocate windows across
//!   monitors, only across slots on their own.
//! - **Dynamic growth**: if enabled, navigating or moving a window past the
//!   last workspace creates a new one on demand, and trailing empty
//!   workspaces are pruned back down again automatically (GNOME-style).
//!   Otherwise the workspace count is fixed.

use std::cell::{RefCell, RefMut};
use std::time::Instant;

use smithay::{
    desktop::Space,
    output::Output,
    utils::{IsAlive, Logical, Point, SERIAL_COUNTER},
};

use crate::{
    config::{Config, WorkspaceMode},
    state::{AnvilState, Backend},
};

use super::{WindowElement, tiling};

/// How long the workspace-dot overlay stays on screen after a switch.
pub const OVERLAY_DURATION_MS: u64 = 1200;

/// Per-window bookkeeping: which output + workspace a window is currently
/// homed to, and (floating windows only) the location to restore it to when
/// that workspace becomes visible again.
#[derive(Default)]
struct WindowHome {
    output: RefCell<Option<Output>>,
    index: RefCell<usize>,
    floating_pos: RefCell<Option<Point<i32, Logical>>>,
}

impl WindowHome {
    fn get(window: &WindowElement) -> &WindowHome {
        window.user_data().insert_if_missing(WindowHome::default);
        window.user_data().get::<WindowHome>().unwrap()
    }
}

/// Per-output workspace bookkeeping: which workspace is active, how many
/// exist, and the floating windows homed to each one.
#[derive(Default)]
pub struct WorkspaceState {
    active: RefCell<usize>,
    count: RefCell<usize>,
    floating: RefCell<Vec<Vec<WindowElement>>>,
}

impl WorkspaceState {
    pub fn get(output: &Output) -> &WorkspaceState {
        output.user_data().insert_if_missing(WorkspaceState::default);
        let state = output.user_data().get::<WorkspaceState>().unwrap();
        // Lazily-created state (e.g. touched before `init_output` runs)
        // still needs at least one workspace to be usable.
        if *state.count.borrow() == 0 {
            *state.count.borrow_mut() = 1;
        }
        state
    }

    pub fn active(&self) -> usize {
        *self.active.borrow()
    }

    pub fn count(&self) -> usize {
        *self.count.borrow()
    }

    fn floating_slot(&self, idx: usize) -> RefMut<'_, Vec<WindowElement>> {
        let mut floating = self.floating.borrow_mut();
        if floating.len() <= idx {
            floating.resize_with(idx + 1, Vec::new);
        }
        RefMut::map(floating, |v| &mut v[idx])
    }

    fn floating_at(&self, idx: usize) -> Vec<WindowElement> {
        self.floating.borrow().get(idx).cloned().unwrap_or_default()
    }
}

/// Sets up workspace state for a newly connected output: how many
/// workspaces it starts with, and, in `Combined` mode, syncing its active
/// index/count to whatever the other outputs are already showing so a
/// hot-plugged monitor joins the same virtual desktop.
///
/// Takes `config`/`space` rather than `&AnvilState` so it can be called from
/// backend code that's already holding a disjoint mutable borrow of another
/// `AnvilState` field (e.g. a backend device entry) at the call site.
pub fn init_output(config: &Config, space: &Space<WindowElement>, output: &Output) {
    let settings = &config.workspaces;
    let (active, count) = if settings.mode == WorkspaceMode::Combined {
        space
            .outputs()
            .find(|o| *o != output)
            .map(|o| {
                let other = WorkspaceState::get(o);
                (other.active(), other.count())
            })
            .unwrap_or((0, settings.count.max(1)))
    } else {
        (0, settings.count.max(1))
    };
    let ws = WorkspaceState::get(output);
    *ws.active.borrow_mut() = active;
    *ws.count.borrow_mut() = count;
}

/// Registers a newly mapped window (tiled or floating) as belonging to
/// `output`'s currently active workspace.
pub fn assign_new_window(window: &WindowElement, output: &Output, floating: bool) {
    let idx = WorkspaceState::get(output).active();
    let home = WindowHome::get(window);
    *home.output.borrow_mut() = Some(output.clone());
    *home.index.borrow_mut() = idx;
    if floating {
        let mut slot = WorkspaceState::get(output).floating_slot(idx);
        if !slot.contains(window) {
            slot.push(window.clone());
        }
    }
}

/// Records that `window` just became floating, having been pulled out of
/// `output`'s workspace `idx` tiling tree. Tracks its current on-screen
/// position (if mapped) so it reappears there next time that workspace is shown.
pub fn mark_floating<B: Backend>(state: &AnvilState<B>, window: &WindowElement, output: &Output, idx: usize) {
    let home = WindowHome::get(window);
    *home.output.borrow_mut() = Some(output.clone());
    *home.index.borrow_mut() = idx;
    {
        let mut slot = WorkspaceState::get(output).floating_slot(idx);
        if !slot.contains(window) {
            slot.push(window.clone());
        }
    }
    if let Some(loc) = state.space.element_location(window) {
        *home.floating_pos.borrow_mut() = Some(loc);
    }
}

/// Drops dead windows from every output's floating registry. Tiled windows
/// are cleaned up by [`tiling::cleanup_dead`] (which calls this too).
pub fn cleanup_dead<B: Backend>(state: &AnvilState<B>) {
    for output in state.space.outputs() {
        let ws = WorkspaceState::get(output);
        for slot in ws.floating.borrow_mut().iter_mut() {
            slot.retain(|w| w.alive());
        }
    }
}

/// The next workspace index `delta` steps from `cur`, or `None` if that step
/// is out of bounds (below zero, or past the last workspace in fixed mode).
fn target_index(cur: usize, count: usize, delta: i32, dynamic: bool) -> Option<usize> {
    let next = cur as i32 + delta;
    if next < 0 {
        return None;
    }
    let next = next as usize;
    if !dynamic && next >= count {
        return None;
    }
    Some(next)
}

fn show_overlay<B: Backend>(state: &mut AnvilState<B>) {
    if state.config.workspaces.overlay {
        state.workspace_overlay_shown = Some(Instant::now());
    }
}

/// Unmaps every window (tiled or floating) belonging to `output`'s
/// workspace `idx`, remembering floating windows' positions first.
fn hide_workspace<B: Backend>(state: &mut AnvilState<B>, output: &Output, idx: usize) {
    let tiled = tiling::TilingState::tree(output, idx).windows();
    for window in &tiled {
        if window.alive() {
            state.space.unmap_elem(window);
        }
    }

    let floating = WorkspaceState::get(output).floating_at(idx);
    for window in &floating {
        if !window.alive() {
            continue;
        }
        if let Some(loc) = state.space.element_location(window) {
            *WindowHome::get(window).floating_pos.borrow_mut() = Some(loc);
        }
        state.space.unmap_elem(window);
    }
}

/// Maps every (alive) window belonging to `output`'s workspace `idx` back
/// into the space: tiled windows are reflowed, floating ones restored to
/// their last known position.
fn show_workspace<B: Backend>(state: &mut AnvilState<B>, output: &Output, idx: usize) {
    // `apply_layout` reflows `output`'s *active* workspace, which by the
    // time this runs is already `idx` (the caller updates `active` first).
    tiling::apply_layout(state, output);

    let ws = WorkspaceState::get(output);
    if let Some(slot) = ws.floating.borrow_mut().get_mut(idx) {
        slot.retain(|w| w.alive());
    }
    let floating = ws.floating_at(idx);
    for window in floating {
        let pos = WindowHome::get(&window).floating_pos.borrow().unwrap_or_default();
        state.space.map_element(window, pos, false);
    }
}

fn focus_first_in_workspace<B: Backend>(state: &mut AnvilState<B>, output: &Output, idx: usize) {
    let candidate = tiling::TilingState::tree(output, idx)
        .windows()
        .into_iter()
        .find(|w| w.alive())
        .or_else(|| WorkspaceState::get(output).floating_at(idx).into_iter().find(|w| w.alive()));

    match candidate {
        Some(window) => tiling::raise_and_focus(state, &window),
        None => {
            if let Some(keyboard) = state.seat.get_keyboard() {
                let serial = SERIAL_COUNTER.next_serial();
                keyboard.set_focus(state, None, serial);
            }
        }
    }
}

/// Drops trailing empty workspaces (dynamic mode only), down to a minimum of
/// one and never below the active one.
fn prune_trailing_empty(output: &Output) {
    let ws = WorkspaceState::get(output);
    let active = ws.active();
    let mut count = ws.count();
    while count > 1 && count - 1 != active {
        let idx = count - 1;
        let tiled_empty = tiling::TilingState::tree(output, idx).is_empty();
        let floating_empty = ws.floating_at(idx).is_empty();
        if tiled_empty && floating_empty {
            count -= 1;
        } else {
            break;
        }
    }
    *ws.count.borrow_mut() = count;
}

fn set_active<B: Backend>(state: &mut AnvilState<B>, output: &Output, new_idx: usize) {
    let old_idx = WorkspaceState::get(output).active();
    if old_idx == new_idx {
        return;
    }

    hide_workspace(state, output, old_idx);

    let ws = WorkspaceState::get(output);
    *ws.active.borrow_mut() = new_idx;
    if new_idx + 1 > ws.count() {
        *ws.count.borrow_mut() = new_idx + 1;
    }

    show_workspace(state, output, new_idx);
    focus_first_in_workspace(state, output, new_idx);

    if state.config.workspaces.dynamic {
        prune_trailing_empty(output);
    }
}

/// Switches `output` (and, in `Combined` mode, every output) `delta`
/// workspaces over (-1 = previous, +1 = next). No-op if that would go out
/// of bounds.
pub fn switch_workspace<B: Backend>(state: &mut AnvilState<B>, output: &Output, delta: i32) {
    let ws = WorkspaceState::get(output);
    let Some(new_idx) = target_index(ws.active(), ws.count(), delta, state.config.workspaces.dynamic) else {
        return;
    };

    let outputs: Vec<Output> = if state.config.workspaces.mode == WorkspaceMode::Combined {
        state.space.outputs().cloned().collect()
    } else {
        vec![output.clone()]
    };
    for o in &outputs {
        set_active(state, o, new_idx);
    }

    show_overlay(state);
}

/// Moves the currently focused window `delta` workspaces over on its own
/// output (-1 = previous, +1 = next). No-op if nothing is focused or that
/// would go out of bounds.
pub fn move_focused_window<B: Backend>(state: &mut AnvilState<B>, delta: i32) {
    let Some(window) = tiling::current_focused_window(state) else {
        return;
    };

    let (output, was_tiled) = match tiling::locate(state, &window) {
        Some((o, _idx)) => (o, true),
        None => match WindowHome::get(&window).output.borrow().clone() {
            Some(o) => (o, false),
            None => return,
        },
    };

    let ws = WorkspaceState::get(&output);
    let active = ws.active();
    let Some(target_idx) = target_index(active, ws.count(), delta, state.config.workspaces.dynamic) else {
        return;
    };
    if target_idx == active {
        return;
    }

    // Pull the window out of its current (active) slot without leaving it
    // registered there.
    if was_tiled {
        tiling::TilingState::tree_mut(&output, active).remove(&window);
    } else {
        WorkspaceState::get(&output).floating_slot(active).retain(|w| w != &window);
    }

    let home = WindowHome::get(&window);
    *home.output.borrow_mut() = Some(output.clone());
    *home.index.borrow_mut() = target_idx;
    if target_idx + 1 > WorkspaceState::get(&output).count() {
        *WorkspaceState::get(&output).count.borrow_mut() = target_idx + 1;
    }

    if was_tiled {
        let area = tiling::tiling_area(&state.space, &output);
        tiling::TilingState::tree_mut(&output, target_idx).insert(window.clone(), area, None);
        // Reflows the source workspace (still active) to close the gap the
        // window left; the destination tree is applied whenever it's shown.
        tiling::apply_layout(state, &output);
        state.space.unmap_elem(&window);
    } else {
        WorkspaceState::get(&output).floating_slot(target_idx).push(window.clone());
        if let Some(loc) = state.space.element_location(&window) {
            *home.floating_pos.borrow_mut() = Some(loc);
        }
        state.space.unmap_elem(&window);
    }

    focus_first_in_workspace(state, &output, active);

    if state.config.workspaces.dynamic {
        prune_trailing_empty(&output);
    }

    show_overlay(state);
}

/// Activates workspace `idx` on `output`, e.g. in response to the
/// `ext-workspace-v1` protocol's `activate` request. Applies the same
/// `Combined`-mode fan-out as [`switch_workspace`]. `idx` is expected to
/// name a workspace that already exists (protocol clients only ever hold
/// handles for workspaces we've told them about), so unlike
/// [`switch_workspace`]/[`move_focused_window`] this doesn't validate it
/// against `count`/`dynamic` first.
pub fn activate_workspace<B: Backend>(state: &mut AnvilState<B>, output: &Output, idx: usize) {
    let outputs: Vec<Output> = if state.config.workspaces.mode == WorkspaceMode::Combined {
        state.space.outputs().cloned().collect()
    } else {
        vec![output.clone()]
    };
    for o in &outputs {
        set_active(state, o, idx);
    }
    show_overlay(state);
}

/// `(active, count)` for `output`'s workspaces, for the dot overlay.
pub fn overlay_info(output: &Output) -> (usize, usize) {
    let ws = WorkspaceState::get(output);
    (ws.active(), ws.count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_index_steps_within_bounds() {
        assert_eq!(target_index(1, 4, 1, false), Some(2));
        assert_eq!(target_index(1, 4, -1, false), Some(0));
    }

    #[test]
    fn target_index_fixed_mode_clamps_at_edges() {
        assert_eq!(target_index(0, 4, -1, false), None);
        assert_eq!(target_index(3, 4, 1, false), None);
    }

    #[test]
    fn target_index_dynamic_mode_grows_past_the_last_workspace() {
        assert_eq!(target_index(3, 4, 1, true), Some(4));
        // Still can't go negative even when dynamic.
        assert_eq!(target_index(0, 4, -1, true), None);
    }
}
