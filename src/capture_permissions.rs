//! Server side of the `ironland-capture-permissions-v1` protocol (see
//! `crate::ironland_protocols::capture_permissions` for the generated
//! bindings and `protocols/ironland-capture-permissions-v1.xml` for the
//! wire format).
//!
//! This is a read/write window into `crate::screencopy`'s in-memory grant
//! table (see that module's doc for what's actually in it and why it isn't
//! persisted) - in practice consumed by this compositor's companion shell
//! to show and let the user manage which executables can capture the
//! screen, in something nicer than editing nothing at all (there is no
//! other way to inspect or revoke a grant once made). Gated to the
//! privileged capture socket for the same reason `ironland-permission-
//! prompt-v1` is: nothing here should be visible to, let alone editable
//! by, an arbitrary client.
//!
//! [`sync`] is the entry point callers (`crate::state`) use to push a
//! changed/removed grant out to every bound listener - matching the
//! `sync()` naming convention this codebase's other server-side protocol
//! bridges (`crate::ext_workspace`, `crate::foreign_toplevel`) use.

use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
    backend::{ClientId, GlobalId},
};
use smithay::wayland::Dispatch2;
use smithay::wayland::GlobalDispatch2;

use crate::ironland_protocols::capture_permissions::ironland_capture_permissions_manager_v1::{
    self, IronlandCapturePermissionsManagerV1,
};

/// Implemented by the compositor state so this module can read/edit the
/// grant table and broadcast changes without depending on `AnvilState`
/// directly.
pub trait CapturePermissionsHandler: 'static {
    fn capture_permissions_state(&mut self) -> &mut CapturePermissionsState;

    /// Every grant currently on file, for a newly-bound client's initial
    /// burst of `entry` events.
    fn capture_grants(&self) -> Vec<(String, bool)>;

    /// Sets (adding or overwriting) `subject`'s decision and broadcasts the
    /// change (see [`sync_entry`]) to every bound listener, this request's
    /// own caller included - see the protocol doc.
    fn set_capture_grant(&mut self, subject: String, allowed: bool);

    /// Removes any decision on file for `subject`, broadcasting (see
    /// [`sync_removed`]) only if one actually existed to remove.
    fn forget_capture_grant(&mut self, subject: &str);
}

/// State of the `ironland_capture_permissions_manager_v1` global: every
/// bound client, to broadcast `entry`/`removed` events to.
#[derive(Debug, Default)]
pub struct CapturePermissionsState {
    global: Option<GlobalId>,
    listeners: Vec<IronlandCapturePermissionsManagerV1>,
}

impl CapturePermissionsState {
    pub fn new<D, F>(dh: &DisplayHandle, filter: F) -> Self
    where
        D: GlobalDispatch<IronlandCapturePermissionsManagerV1, ManagerGlobalData> + 'static,
        F: Fn(&Client) -> bool + Send + Sync + 'static,
    {
        let global = dh.create_global::<D, IronlandCapturePermissionsManagerV1, _>(
            1,
            ManagerGlobalData {
                filter: Box::new(filter),
            },
        );
        CapturePermissionsState {
            global: Some(global),
            listeners: Vec::new(),
        }
    }

    #[allow(dead_code)]
    pub fn global(&self) -> Option<GlobalId> {
        self.global.clone()
    }
}

/// Broadcasts `subject`'s new state to every bound listener. Called from
/// `crate::state` after any change to the grant table - a resolved prompt
/// (`PermissionPromptHandler::capture_grant_resolved`) or a listener's own
/// `set_grant` request.
pub fn sync_entry<D: CapturePermissionsHandler>(state: &mut D, subject: &str, allowed: bool) {
    for listener in &state.capture_permissions_state().listeners {
        listener.entry(subject.to_string(), allowed as u32);
    }
}

/// Broadcasts that `subject` was forgotten. See [`sync_entry`].
pub fn sync_removed<D: CapturePermissionsHandler>(state: &mut D, subject: &str) {
    for listener in &state.capture_permissions_state().listeners {
        listener.removed(subject.to_string());
    }
}

/// Global data for the `ironland_capture_permissions_manager_v1` global:
/// the client filter it's gated behind.
#[allow(missing_debug_implementations)]
pub struct ManagerGlobalData {
    filter: Box<dyn Fn(&Client) -> bool + Send + Sync>,
}

impl std::fmt::Debug for ManagerGlobalData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagerGlobalData").finish_non_exhaustive()
    }
}

/// User data attached to a bound `ironland_capture_permissions_manager_v1`
/// resource (nothing to carry - it's tracked in
/// [`CapturePermissionsState::listeners`] instead).
#[derive(Debug)]
pub struct ManagerToken;

impl<D> GlobalDispatch2<IronlandCapturePermissionsManagerV1, D> for ManagerGlobalData
where
    D: CapturePermissionsHandler + Dispatch<IronlandCapturePermissionsManagerV1, ManagerToken>,
{
    fn bind(
        &self,
        state: &mut D,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<IronlandCapturePermissionsManagerV1>,
        data_init: &mut DataInit<'_, D>,
    ) {
        let resource = data_init.init(resource, ManagerToken);
        for (subject, allowed) in state.capture_grants() {
            resource.entry(subject, allowed as u32);
        }
        state
            .capture_permissions_state()
            .listeners
            .push(resource);
    }

    fn can_view(&self, client: &Client) -> bool {
        (self.filter)(client)
    }
}

impl<D> Dispatch2<IronlandCapturePermissionsManagerV1, D> for ManagerToken
where
    D: CapturePermissionsHandler,
{
    fn request(
        &self,
        state: &mut D,
        _client: &Client,
        _manager: &IronlandCapturePermissionsManagerV1,
        request: ironland_capture_permissions_manager_v1::Request,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            ironland_capture_permissions_manager_v1::Request::SetGrant { subject, allowed } => {
                state.set_capture_grant(subject, allowed != 0);
            }
            ironland_capture_permissions_manager_v1::Request::Forget { subject } => {
                state.forget_capture_grant(&subject);
            }
            ironland_capture_permissions_manager_v1::Request::Destroy => {}
        }
    }

    fn destroyed(
        &self,
        state: &mut D,
        _client: ClientId,
        resource: &IronlandCapturePermissionsManagerV1,
    ) {
        state
            .capture_permissions_state()
            .listeners
            .retain(|listener| listener != resource);
    }
}
