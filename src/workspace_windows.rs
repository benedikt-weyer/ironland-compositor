//! Server side of the `ironland-workspace-windows-v1` protocol (see
//! `crate::ironland_protocols::workspace_windows` for the generated
//! bindings and `protocols/ironland-workspace-windows-v1.xml` for why this
//! exists and its wire format).
//!
//! [`sync`] is the single entry point: call it whenever a window
//! maps/unmaps, changes title/app id, or moves workspace (the same set of
//! events that already drive [`crate::foreign_toplevel::sync`] and
//! [`crate::ext_workspace::ext_workspace_sync`] - call all three together).
//! It doesn't try to diff away redundant events, matching those two.

use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
    backend::{ClientId, GlobalId},
};
use smithay::wayland::{Dispatch2, GlobalDispatch2};

use crate::ironland_protocols::workspace_windows::ironland_workspace_windows_v1::{
    self, IronlandWorkspaceWindowsV1,
};

/// Implemented by the compositor state so this module can read window/
/// workspace state without depending on `AnvilState` (and its `Backend`
/// type parameter) directly.
pub trait WorkspaceWindowsHandler: 'static {
    fn workspace_windows_state(&mut self) -> &mut WorkspaceWindowsState;
    /// Every window's current (output name, workspace index, title, app id),
    /// if it's been assigned a home yet (see `shell::workspace::window_home`).
    fn windows_by_workspace(&self) -> Vec<(String, usize, String, String)>;
}

#[derive(Debug)]
pub struct WorkspaceWindowsState {
    global: GlobalId,
    instances: Vec<IronlandWorkspaceWindowsV1>,
}

impl WorkspaceWindowsState {
    pub fn new<D>(dh: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<IronlandWorkspaceWindowsV1, GlobalData> + 'static,
    {
        let global = dh.create_global::<D, IronlandWorkspaceWindowsV1, _>(1, GlobalData);
        WorkspaceWindowsState {
            global,
            instances: Vec::new(),
        }
    }

    #[allow(dead_code)]
    pub fn global(&self) -> GlobalId {
        self.global.clone()
    }
}

/// Global data for the `ironland_workspace_windows_v1` global (nothing to
/// carry - also reused as the per-instance user data, since the interface
/// carries no per-object state beyond what's already in
/// [`WorkspaceWindowsState::instances`]).
#[derive(Debug)]
pub struct GlobalData;

/// Sends every window's current workspace, as one `window` event per window
/// followed by `done`, to every bound client.
pub fn sync<D: WorkspaceWindowsHandler>(state: &mut D) {
    let windows = state.windows_by_workspace();
    for instance in &state.workspace_windows_state().instances {
        for (output, workspace, title, app_id) in &windows {
            instance.window(output.clone(), *workspace as u32, title.clone(), app_id.clone());
        }
        instance.done();
    }
}

impl<D> GlobalDispatch2<IronlandWorkspaceWindowsV1, D> for GlobalData
where
    D: WorkspaceWindowsHandler + Dispatch<IronlandWorkspaceWindowsV1, GlobalData>,
{
    fn bind(
        &self,
        state: &mut D,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<IronlandWorkspaceWindowsV1>,
        data_init: &mut DataInit<'_, D>,
    ) {
        let instance = data_init.init(resource, GlobalData);
        let windows = state.windows_by_workspace();
        for (output, workspace, title, app_id) in &windows {
            instance.window(output.clone(), *workspace as u32, title.clone(), app_id.clone());
        }
        instance.done();
        state.workspace_windows_state().instances.push(instance);
    }
}

impl<D: WorkspaceWindowsHandler> Dispatch2<IronlandWorkspaceWindowsV1, D> for GlobalData {
    fn request(
        &self,
        _state: &mut D,
        _client: &Client,
        _resource: &IronlandWorkspaceWindowsV1,
        request: ironland_workspace_windows_v1::Request,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        let ironland_workspace_windows_v1::Request::Destroy = request;
    }

    fn destroyed(&self, state: &mut D, _client: ClientId, resource: &IronlandWorkspaceWindowsV1) {
        state
            .workspace_windows_state()
            .instances
            .retain(|i| i != resource);
    }
}
