#![warn(rust_2018_idioms)]
#![allow(clippy::collapsible_match)]
// If no backend is enabled, a large portion of the codebase is unused.
// So silence this useless warning for the CI.
#![cfg_attr(
    not(any(feature = "winit", feature = "x11", feature = "udev")),
    allow(dead_code, unused_imports)
)]

// The config schema/file-I/O itself lives in the `ironland-config` crate
// (a separate, smithay-free workspace member so `ironlandctl` and the
// settings GUI can share it without a heavy build) - re-exported here under
// its old name so every existing `crate::config::Foo` reference keeps
// working unchanged.
pub use ironland_config as config;
pub mod keybindings;
pub mod border;
#[cfg(any(feature = "udev", feature = "xwayland"))]
pub mod cursor;
pub mod drawing;
pub mod focus;
pub mod focus_grab;
pub mod font;
pub mod foreign_toplevel;
pub mod frame_capture;
pub mod input_handler;
pub mod ironland_protocols;
pub mod capture_permissions;
pub mod launcher;
#[cfg(feature = "libei")]
pub mod libei;
pub mod perf_overlay;
pub mod permission_prompt;
pub mod render;
pub mod rounded_corners;
pub mod screencopy;
#[cfg(feature = "udev")]
pub mod session;
pub mod shell;
pub mod state;
#[cfg(feature = "udev")]
pub mod udev;
pub mod wallpaper;
#[cfg(feature = "winit")]
pub mod winit;
#[cfg(feature = "x11")]
pub mod x11;
pub mod ext_workspace;
pub mod shortcuts;
pub mod workspace_windows;

pub use state::{AnvilState, ClientState};
