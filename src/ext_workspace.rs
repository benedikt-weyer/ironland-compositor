//! Server side of the `ext-workspace-v1` protocol.
//!
//! This is what lets a Wayland client (a shell panel like molunga-shell) show
//! and switch the active workspace without a compositor-specific IPC: it's a
//! thin read/write projection of the workspace state already tracked in
//! [`crate::shell::workspace`], not a separate source of truth.
//!
//! One [`ExtWorkspaceGroupHandleV1`] is advertised per output, containing
//! that output's workspaces (named "1", "2", ... in index order) as
//! [`ExtWorkspaceHandleV1`] objects. Only the `active` state and the
//! `activate` request are implemented - workspaces are never hidden/urgent,
//! and creating/removing/reassigning workspaces from the client side isn't
//! supported (the compositor already grows/prunes them on its own, see
//! `shell::workspace`'s module docs), so those capability bits are left
//! unset and their requests are ignored, per the protocol's own contract for
//! unadvertised capabilities.
//!
//! [`sync`] is the single entry point: call it after any change to
//! workspace count, active index, or the output set, and it brings every
//! bound client up to date (creating/removing protocol objects as needed,
//! refreshing `state`, and finishing with `done`). It intentionally doesn't
//! try to diff away redundant `state` events - `sync` only runs at discrete
//! change points (never per frame), so the extra events are negligible.
//!
//! One known gap: `output_enter` is only sent for `wl_output` resources the
//! client has already bound at the time its group is created. A client that
//! binds `wl_output` for a given output *after* that (rather than during its
//! initial registry burst, which is how every client we care about behaves)
//! won't see that output on the group. Handling that properly would mean
//! hooking `wl_output` bind events too, which isn't worth it for this case.

use smithay::{
    output::Output,
    reexports::{
        wayland_protocols::ext::workspace::v1::server::{
            ext_workspace_group_handle_v1::{ExtWorkspaceGroupHandleV1, GroupCapabilities},
            ext_workspace_handle_v1::{ExtWorkspaceHandleV1, Request as WsRequest, State as WsState, WorkspaceCapabilities},
            ext_workspace_manager_v1::{ExtWorkspaceManagerV1, Request as ManagerRequest},
        },
        wayland_server::{
            backend::{ClientId, GlobalId},
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
        },
    },
    wayland::{Dispatch2, GlobalDispatch2},
};

use crate::{
    shell::workspace::WorkspaceState,
    state::{AnvilState, Backend},
};

/// Implemented by the compositor state so this module can drive workspace
/// switches from the `activate` request without depending on
/// [`AnvilState`] (and its `Backend` type parameter) directly.
pub trait WorkspaceManagerHandler: 'static {
    fn workspace_manager_state(&mut self) -> &mut WorkspaceManagerState;
    /// Handle a client's `ext_workspace_handle_v1.activate` request for the
    /// workspace at `index` on `output`.
    fn activate_workspace(&mut self, output: &Output, index: usize);
}

impl<B: Backend> WorkspaceManagerHandler for AnvilState<B> {
    fn workspace_manager_state(&mut self) -> &mut WorkspaceManagerState {
        &mut self.workspace_manager_state
    }

    fn activate_workspace(&mut self, output: &Output, index: usize) {
        crate::shell::workspace::activate_workspace(self, output, index);
        ext_workspace_sync(self);
    }
}

/// A single workspace object created for one bound client, tied to the
/// output+index it represents.
#[derive(Debug)]
struct WorkspaceEntry {
    resource: ExtWorkspaceHandleV1,
    index: usize,
}

/// A single workspace group (one per output) created for one bound client.
#[derive(Debug)]
struct GroupEntry {
    output: Output,
    resource: ExtWorkspaceGroupHandleV1,
    workspaces: Vec<WorkspaceEntry>,
}

/// One client's binding of the `ext_workspace_manager_v1` global, and every
/// group/workspace protocol object created for it so far.
#[derive(Debug)]
struct Instance {
    manager: ExtWorkspaceManagerV1,
    groups: Vec<GroupEntry>,
}

/// State of the `ext_workspace_manager_v1` global.
#[derive(Debug)]
pub struct WorkspaceManagerState {
    global: GlobalId,
    instances: Vec<Instance>,
}

impl WorkspaceManagerState {
    pub fn new<D>(dh: &DisplayHandle) -> Self
    where
        D: WorkspaceManagerHandler + GlobalDispatch<ExtWorkspaceManagerV1, ManagerGlobalData>,
    {
        let global = dh.create_global::<D, ExtWorkspaceManagerV1, _>(1, ManagerGlobalData);
        WorkspaceManagerState {
            global,
            instances: Vec::new(),
        }
    }

    #[allow(dead_code)]
    pub fn global(&self) -> GlobalId {
        self.global.clone()
    }
}

/// Global data for the `ext_workspace_manager_v1` global (nothing to carry).
#[derive(Debug)]
pub struct ManagerGlobalData;

/// User data attached to a bound `ext_workspace_manager_v1` resource
/// (nothing to carry - the instance's state lives in
/// [`WorkspaceManagerState::instances`], looked up by resource identity).
#[derive(Debug)]
pub struct ManagerToken;

/// User data attached to a `ext_workspace_group_handle_v1` resource. Not
/// currently read (its requests are all ignored), but kept for parity with
/// [`WorkspaceData`] and in case a future request needs it.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct GroupData {
    output: Output,
}

/// User data attached to a `ext_workspace_handle_v1` resource.
#[derive(Debug, Clone)]
pub struct WorkspaceData {
    output: Output,
    index: usize,
}

/// Recomputes and pushes the full workspace state to every bound client.
/// Call this after anything that changes workspace count, active index, or
/// the output set.
pub fn ext_workspace_sync<D>(state: &mut D)
where
    D: WorkspaceManagerHandler
        + Dispatch<ExtWorkspaceManagerV1, ManagerToken>
        + Dispatch<ExtWorkspaceGroupHandleV1, GroupData>
        + Dispatch<ExtWorkspaceHandleV1, WorkspaceData>
        + AsOutputsAndDisplay
        + crate::workspace_windows::WorkspaceWindowsHandler,
{
    let dh = state.display_handle();
    let outputs = state.workspace_outputs();
    let proto = state.workspace_manager_state();

    for instance in &mut proto.instances {
        let Ok(client) = dh.get_client(instance.manager.id()) else {
            continue;
        };
        sync_instance::<D>(&dh, &client, instance, &outputs);
        instance.manager.done();
    }

    // A workspace switch or a window moving workspace doesn't necessarily
    // touch any window's title/app id, so `foreign_toplevel::sync_with_focus`
    // won't always run alongside this - piggyback the workspace-windows
    // snapshot here too rather than threading a separate call through every
    // caller of this function.
    crate::workspace_windows::sync(state);
}

/// Brings one client's groups/workspaces in line with the current
/// `(output, active, count)` snapshot: creates missing objects, tears down
/// ones for outputs/workspaces that no longer exist, and refreshes `state`
/// on everything that remains. Does not send `done` - the caller does that
/// once, after this returns (bind and [`ext_workspace_sync`] both do this,
/// the latter for every instance in one pass).
fn sync_instance<D>(
    dh: &DisplayHandle,
    client: &Client,
    instance: &mut Instance,
    outputs: &[(Output, usize, usize)],
) where
    D: WorkspaceManagerHandler
        + Dispatch<ExtWorkspaceManagerV1, ManagerToken>
        + Dispatch<ExtWorkspaceGroupHandleV1, GroupData>
        + Dispatch<ExtWorkspaceHandleV1, WorkspaceData>,
{
    // Drop groups for outputs that no longer exist.
    instance.groups.retain(|group| {
        let still_present = outputs.iter().any(|(o, _, _)| o == &group.output);
        if !still_present {
            for ws in &group.workspaces {
                ws.resource.removed();
            }
            group.resource.removed();
        }
        still_present
    });

    for (output, active, count) in outputs {
        let group = match instance.groups.iter_mut().find(|g| &g.output == output) {
            Some(g) => g,
            None => {
                let Ok(resource) = client.create_resource::<ExtWorkspaceGroupHandleV1, _, D>(
                    dh,
                    instance.manager.version(),
                    GroupData { output: output.clone() },
                ) else {
                    continue;
                };
                instance.manager.workspace_group(&resource);
                resource.capabilities(GroupCapabilities::empty());
                for wl_output in output.client_outputs(client) {
                    resource.output_enter(&wl_output);
                }
                instance.groups.push(GroupEntry {
                    output: output.clone(),
                    resource,
                    workspaces: Vec::new(),
                });
                instance.groups.last_mut().unwrap()
            }
        };

        // Drop trailing workspaces the output pruned.
        while group.workspaces.len() > *count {
            let ws = group.workspaces.pop().unwrap();
            ws.resource.removed();
            group.resource.workspace_leave(&ws.resource);
        }

        // Create newly grown workspaces.
        while group.workspaces.len() < *count {
            let index = group.workspaces.len();
            let Ok(resource) = client.create_resource::<ExtWorkspaceHandleV1, _, D>(
                dh,
                instance.manager.version(),
                WorkspaceData {
                    output: output.clone(),
                    index,
                },
            ) else {
                break;
            };
            instance.manager.workspace(&resource);
            group.resource.workspace_enter(&resource);
            resource.name((index + 1).to_string());
            resource.capabilities(WorkspaceCapabilities::Activate);
            group.workspaces.push(WorkspaceEntry { resource, index });
        }

        // Refresh active state on everything that's left.
        for ws in &group.workspaces {
            let state = if ws.index == *active {
                WsState::Active
            } else {
                WsState::empty()
            };
            ws.resource.state(state);
        }
    }
}

/// Small seam so [`ext_workspace_sync`] doesn't need to know about
/// [`AnvilState`]/[`Backend`] directly.
pub trait AsOutputsAndDisplay {
    fn display_handle(&self) -> DisplayHandle;
    /// `(output, active workspace index, workspace count)` for every output.
    fn workspace_outputs(&self) -> Vec<(Output, usize, usize)>;
}

impl<B: Backend> AsOutputsAndDisplay for AnvilState<B> {
    fn display_handle(&self) -> DisplayHandle {
        self.display_handle.clone()
    }

    fn workspace_outputs(&self) -> Vec<(Output, usize, usize)> {
        self.space
            .outputs()
            .map(|o| {
                let ws = WorkspaceState::get(o);
                (o.clone(), ws.active(), ws.count())
            })
            .collect()
    }
}

impl<D> GlobalDispatch2<ExtWorkspaceManagerV1, D> for ManagerGlobalData
where
    D: WorkspaceManagerHandler
        + AsOutputsAndDisplay
        + Dispatch<ExtWorkspaceManagerV1, ManagerToken>
        + Dispatch<ExtWorkspaceGroupHandleV1, GroupData>
        + Dispatch<ExtWorkspaceHandleV1, WorkspaceData>,
{
    fn bind(
        &self,
        state: &mut D,
        dh: &DisplayHandle,
        client: &Client,
        resource: New<ExtWorkspaceManagerV1>,
        data_init: &mut DataInit<'_, D>,
    ) {
        let manager = data_init.init(resource, ManagerToken);
        let mut instance = Instance {
            manager,
            groups: Vec::new(),
        };
        let outputs = state.workspace_outputs();
        sync_instance::<D>(dh, client, &mut instance, &outputs);
        instance.manager.done();
        state.workspace_manager_state().instances.push(instance);
    }
}

impl<D: WorkspaceManagerHandler> Dispatch2<ExtWorkspaceManagerV1, D> for ManagerToken {
    fn request(
        &self,
        _state: &mut D,
        client: &Client,
        manager: &ExtWorkspaceManagerV1,
        request: ManagerRequest,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            ManagerRequest::Stop => {
                manager.finished();
                let _ = client;
            }
            ManagerRequest::Commit => {}
            _ => {}
        }
    }

    fn destroyed(&self, state: &mut D, _client: ClientId, resource: &ExtWorkspaceManagerV1) {
        state
            .workspace_manager_state()
            .instances
            .retain(|instance| &instance.manager != resource);
    }
}

impl<D: WorkspaceManagerHandler> Dispatch2<ExtWorkspaceGroupHandleV1, D> for GroupData {
    fn request(
        &self,
        _state: &mut D,
        _client: &Client,
        _group: &ExtWorkspaceGroupHandleV1,
        request: smithay::reexports::wayland_protocols::ext::workspace::v1::server::ext_workspace_group_handle_v1::Request,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        use smithay::reexports::wayland_protocols::ext::workspace::v1::server::ext_workspace_group_handle_v1::Request as GroupRequest;
        match request {
            // Not advertised in `capabilities`, so ignored per protocol.
            GroupRequest::CreateWorkspace { .. } => {}
            GroupRequest::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(&self, state: &mut D, _client: ClientId, resource: &ExtWorkspaceGroupHandleV1) {
        for instance in &mut state.workspace_manager_state().instances {
            instance.groups.retain(|g| &g.resource != resource);
        }
    }
}

impl<D: WorkspaceManagerHandler> Dispatch2<ExtWorkspaceHandleV1, D> for WorkspaceData {
    fn request(
        &self,
        state: &mut D,
        _client: &Client,
        _workspace: &ExtWorkspaceHandleV1,
        request: WsRequest,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            WsRequest::Activate => state.activate_workspace(&self.output, self.index),
            // Not advertised in `capabilities`, so ignored per protocol.
            WsRequest::Deactivate | WsRequest::Assign { .. } | WsRequest::Remove => {}
            WsRequest::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(&self, state: &mut D, client: ClientId, resource: &ExtWorkspaceHandleV1) {
        let _ = client;
        for instance in &mut state.workspace_manager_state().instances {
            for group in &mut instance.groups {
                group.workspaces.retain(|w| &w.resource != resource);
            }
        }
    }
}
