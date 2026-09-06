//! Server side of the `ironland-focus-grab-v1` protocol (see
//! `crate::ironland_protocols::focus_grab` for the generated bindings and
//! `protocols/ironland-focus-grab-v1.xml` for the wire format).
//!
//! [`check`] is the single entry point: `crate::input_handler` calls it
//! with the surface (if any) that a pointer click or key press actually
//! landed on, and it clears (and forgets) every active grab whose surface
//! doesn't match.
//!
//! Deliberately compares surface identity only, with no allowance for a
//! grabbed surface's popups/subsurfaces: every known caller passes a
//! single-surface Quickshell `WlrLayershell` window, which doesn't create
//! separate `wl_surface`s for its own internal popups (unlike, say, an
//! `xdg_popup`-based combo box), so there's nothing to climb an ancestor
//! chain for. A future caller that does need that would have to extend
//! this.

use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
    backend::{ClientId, GlobalId},
    protocol::wl_surface::WlSurface,
};
use smithay::wayland::{Dispatch2, GlobalDispatch2};

use crate::ironland_protocols::focus_grab::{
    ironland_focus_grab_manager_v1::{self, IronlandFocusGrabManagerV1},
    ironland_focus_grab_v1::{self, IronlandFocusGrabV1},
};

/// Implemented by the compositor state so this module can check/clear
/// grabs without depending on `AnvilState` directly.
pub trait FocusGrabHandler: 'static {
    fn focus_grab_state(&mut self) -> &mut FocusGrabManagerState;
}

/// One active grab: the surface it's limited to, and the protocol object to
/// notify when it clears.
#[derive(Debug)]
struct GrabEntry {
    surface: WlSurface,
    resource: IronlandFocusGrabV1,
}

/// State of the `ironland_focus_grab_manager_v1` global: every grab
/// currently active, across every client.
#[derive(Debug)]
pub struct FocusGrabManagerState {
    global: GlobalId,
    grabs: Vec<GrabEntry>,
}

impl FocusGrabManagerState {
    pub fn new<D>(dh: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<IronlandFocusGrabManagerV1, ManagerGlobalData> + 'static,
    {
        let global = dh.create_global::<D, IronlandFocusGrabManagerV1, _>(1, ManagerGlobalData);
        FocusGrabManagerState {
            global,
            grabs: Vec::new(),
        }
    }

    #[allow(dead_code)]
    pub fn global(&self) -> GlobalId {
        self.global.clone()
    }
}

/// Global data for the `ironland_focus_grab_manager_v1` global (nothing to
/// carry).
#[derive(Debug)]
pub struct ManagerGlobalData;

/// User data attached to a bound `ironland_focus_grab_manager_v1` resource
/// (nothing to carry - every created grab lives in
/// [`FocusGrabManagerState::grabs`]).
#[derive(Debug)]
pub struct ManagerToken;

/// User data attached to an `ironland_focus_grab_v1` resource (nothing to
/// carry beyond what's already in its [`GrabEntry`]).
#[derive(Debug)]
pub struct GrabToken;

/// Clears every active grab whose surface isn't `focused`. A click or key
/// press with no surface under it at all (`focused: None` - empty desktop,
/// or a compositor-drawn element with no `wl_surface`) also clears
/// everything, matching "outside any grabbed surface".
pub fn check<D: FocusGrabHandler>(state: &mut D, focused: Option<&WlSurface>) {
    let grabs = &mut state.focus_grab_state().grabs;
    if grabs.is_empty() {
        return;
    }

    let mut cleared = Vec::new();
    grabs.retain(|entry| {
        if Some(&entry.surface) == focused {
            true
        } else {
            cleared.push(entry.resource.clone());
            false
        }
    });

    for resource in cleared {
        resource.cleared();
    }
}

impl<D> GlobalDispatch2<IronlandFocusGrabManagerV1, D> for ManagerGlobalData
where
    D: FocusGrabHandler
        + Dispatch<IronlandFocusGrabManagerV1, ManagerToken>
        + Dispatch<IronlandFocusGrabV1, GrabToken>,
{
    fn bind(
        &self,
        _state: &mut D,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<IronlandFocusGrabManagerV1>,
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, ManagerToken);
    }
}

impl<D: FocusGrabHandler + Dispatch<IronlandFocusGrabV1, GrabToken>> Dispatch2<IronlandFocusGrabManagerV1, D>
    for ManagerToken
{
    fn request(
        &self,
        state: &mut D,
        _client: &Client,
        _manager: &IronlandFocusGrabManagerV1,
        request: ironland_focus_grab_manager_v1::Request,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            ironland_focus_grab_manager_v1::Request::Grab { id, surface } => {
                let resource = data_init.init(id, GrabToken);
                state.focus_grab_state().grabs.push(GrabEntry { surface, resource });
            }
            ironland_focus_grab_manager_v1::Request::Destroy => {}
        }
    }
}

impl<D: FocusGrabHandler> Dispatch2<IronlandFocusGrabV1, D> for GrabToken {
    fn request(
        &self,
        _state: &mut D,
        _client: &Client,
        _resource: &IronlandFocusGrabV1,
        request: ironland_focus_grab_v1::Request,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        let ironland_focus_grab_v1::Request::Destroy = request;
    }

    fn destroyed(&self, state: &mut D, _client: ClientId, resource: &IronlandFocusGrabV1) {
        state
            .focus_grab_state()
            .grabs
            .retain(|entry| &entry.resource != resource);
    }
}
