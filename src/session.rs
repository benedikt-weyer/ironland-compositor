//! Announces the compositor's session to systemd/D-Bus so that
//! `systemd --user` units gated behind `graphical-session.target`
//! (notably `xdg-desktop-portal`, and with it every portal-backed file
//! chooser, screenshot, etc.) actually start. Without this, the target
//! stays inactive forever because nothing ever tells systemd a graphical
//! session has come up, and portal calls fail with something like
//! "Could not activate remote peer 'org.freedesktop.portal.Desktop':
//! startup job failed".

use std::process::Command;

/// `XDG_CURRENT_DESKTOP` value advertised to session services.
const DESKTOP_NAME: &str = "ironland-copositor";

/// Exports the session environment to `systemd --user` and D-Bus, then
/// starts `graphical-session.target`. Call this once the Wayland socket
/// is up and ready to accept clients.
pub fn announce_session_start(socket_name: Option<&str>) {
    if let Some(socket_name) = socket_name {
        // Safety: single-threaded at this point in startup, before any
        // other thread could be reading the environment.
        unsafe {
            std::env::set_var("WAYLAND_DISPLAY", socket_name);
        }
    }
    unsafe {
        std::env::set_var("XDG_CURRENT_DESKTOP", DESKTOP_NAME);
        std::env::set_var("XDG_SESSION_TYPE", "wayland");
    }

    const VARS: &[&str] = &["WAYLAND_DISPLAY", "XDG_CURRENT_DESKTOP", "XDG_SESSION_TYPE"];

    run("systemctl", |cmd| {
        cmd.arg("--user").arg("import-environment").args(VARS);
    });
    run("dbus-update-activation-environment", |cmd| {
        cmd.arg("--systemd").args(VARS);
    });
    // `graphical-session.target` itself refuses manual starts (it's meant to
    // be pulled in as a dependency, normally by a display manager's login
    // session). Not started standalone from a tty, ironland-copositor has
    // to kick it via NixOS's `nixos-fake-graphical-session.target`, its
    // documented stand-in for exactly this case. This is a no-op (and
    // harmless) on non-NixOS systems, where the unit simply doesn't exist.
    run("systemctl", |cmd| {
        cmd.arg("--user")
            .arg("start")
            .arg("nixos-fake-graphical-session.target");
    });
}

/// Tears the session back down, stopping session services that were
/// waiting on `graphical-session.target`. Call this on compositor exit.
pub fn announce_session_end() {
    run("systemctl", |cmd| {
        cmd.arg("--user").arg("stop").arg("graphical-session.target");
    });
}

fn run(program: &str, configure: impl FnOnce(&mut Command)) {
    let mut cmd = Command::new(program);
    configure(&mut cmd);
    match cmd.status() {
        Ok(status) if !status.success() => {
            tracing::warn!(program, ?status, "session setup command exited with an error");
        }
        Err(err) => {
            tracing::warn!(program, error = ?err, "failed to run session setup command");
        }
        Ok(_) => {}
    }
}
