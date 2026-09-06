#![warn(rust_2018_idioms)]
#![allow(clippy::collapsible_match)]
// If no backend is enabled, a large portion of the codebase is unused.
// So silence this useless warning for the CI.
#![cfg_attr(
    not(any(feature = "winit", feature = "x11", feature = "udev")),
    allow(dead_code, unused_imports)
)]

pub mod config;
#[cfg(any(feature = "udev", feature = "xwayland"))]
pub mod cursor;
pub mod drawing;
pub mod focus;
pub mod font;
pub mod input_handler;
pub mod launcher;
#[cfg(feature = "libei")]
pub mod libei;
pub mod render;
pub mod shell;
pub mod state;
#[cfg(feature = "udev")]
pub mod udev;
#[cfg(feature = "winit")]
pub mod winit;
#[cfg(feature = "x11")]
pub mod x11;
pub mod ext_workspace;

pub use state::{AnvilState, ClientState};
