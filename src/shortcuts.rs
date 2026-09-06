//! Server side of the `ironland-shortcuts-v1` protocol (see
//! `crate::ironland_protocols::shortcuts` for the generated bindings and
//! `protocols/ironland-shortcuts-v1.xml` for the wire format).
//!
//! [`fire`] is the single entry point: `crate::input_handler` calls it with
//! the action name and press/release state whenever a configured keybinding
//! resolves to `KeyAction::Shortcut` (see `config::action_for_name`'s
//! `"shortcut:<name>"` convention), and it forwards to every bound client's
//! matching `ironland_shortcut_v1` object, if any.

use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
    backend::{ClientId, GlobalId},
};
use smithay::wayland::{Dispatch2, GlobalDispatch2};

use crate::ironland_protocols::shortcuts::{
    ironland_shortcut_v1::{self, IronlandShortcutV1},
    ironland_shortcuts_manager_v1::{self, IronlandShortcutsManagerV1},
};

/// Implemented by the compositor state so this module can fire shortcuts
/// without depending on `AnvilState` directly.
pub trait ShortcutsHandler: 'static {
    fn shortcuts_state(&mut self) -> &mut ShortcutsManagerState;
}

/// A single client's registration for one shortcut name.
#[derive(Debug)]
struct ShortcutEntry {
    name: String,
    resource: IronlandShortcutV1,
}

/// State of the `ironland_shortcuts_manager_v1` global: every
/// `ironland_shortcut_v1` object any client has created, across every
/// binding of the manager.
#[derive(Debug)]
pub struct ShortcutsManagerState {
    global: GlobalId,
    shortcuts: Vec<ShortcutEntry>,
}

impl ShortcutsManagerState {
    pub fn new<D>(dh: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<IronlandShortcutsManagerV1, ManagerGlobalData> + 'static,
    {
        let global = dh.create_global::<D, IronlandShortcutsManagerV1, _>(1, ManagerGlobalData);
        ShortcutsManagerState {
            global,
            shortcuts: Vec::new(),
        }
    }

    #[allow(dead_code)]
    pub fn global(&self) -> GlobalId {
        self.global.clone()
    }
}

/// Global data for the `ironland_shortcuts_manager_v1` global (nothing to
/// carry).
#[derive(Debug)]
pub struct ManagerGlobalData;

/// User data attached to a bound `ironland_shortcuts_manager_v1` resource
/// (nothing to carry - every created shortcut lives in
/// [`ShortcutsManagerState::shortcuts`], looked up by name).
#[derive(Debug)]
pub struct ManagerToken;

/// User data attached to an `ironland_shortcut_v1` resource (nothing to
/// carry beyond what's already in its [`ShortcutEntry`]).
#[derive(Debug)]
pub struct ShortcutToken;

/// Sends `pressed`/`released` to every client object registered for
/// `name`. A name nothing has registered (or that isn't bound to any
/// configured keybinding) is simply a no-op - see `config::Config::shortcuts`.
pub fn fire<D: ShortcutsHandler>(state: &mut D, name: &str, pressed: bool) {
    for entry in &state.shortcuts_state().shortcuts {
        if entry.name == name {
            if pressed {
                entry.resource.pressed();
            } else {
                entry.resource.released();
            }
        }
    }
}

impl<D> GlobalDispatch2<IronlandShortcutsManagerV1, D> for ManagerGlobalData
where
    D: ShortcutsHandler
        + Dispatch<IronlandShortcutsManagerV1, ManagerToken>
        + Dispatch<IronlandShortcutV1, ShortcutToken>,
{
    fn bind(
        &self,
        _state: &mut D,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<IronlandShortcutsManagerV1>,
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, ManagerToken);
    }
}

impl<D: ShortcutsHandler + Dispatch<IronlandShortcutV1, ShortcutToken>> Dispatch2<IronlandShortcutsManagerV1, D>
    for ManagerToken
{
    fn request(
        &self,
        state: &mut D,
        _client: &Client,
        _manager: &IronlandShortcutsManagerV1,
        request: ironland_shortcuts_manager_v1::Request,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            ironland_shortcuts_manager_v1::Request::GetShortcut { id, name } => {
                let resource = data_init.init(id, ShortcutToken);
                state
                    .shortcuts_state()
                    .shortcuts
                    .push(ShortcutEntry { name, resource });
            }
            ironland_shortcuts_manager_v1::Request::Destroy => {}
        }
    }
}

impl<D: ShortcutsHandler> Dispatch2<IronlandShortcutV1, D> for ShortcutToken {
    fn request(
        &self,
        _state: &mut D,
        _client: &Client,
        _resource: &IronlandShortcutV1,
        request: ironland_shortcut_v1::Request,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        let ironland_shortcut_v1::Request::Destroy = request;
    }

    fn destroyed(&self, state: &mut D, _client: ClientId, resource: &IronlandShortcutV1) {
        state
            .shortcuts_state()
            .shortcuts
            .retain(|entry| &entry.resource != resource);
    }
}
