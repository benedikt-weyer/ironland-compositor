//! Config schema and file I/O for ironland-compositor's `config.toml`.
//!
//! Settings are read from the first of these that exists, in order:
//!
//! 1. `$IRONLAND_COMPOSITOR_CONFIG` (an explicit path, mainly for testing)
//! 2. `$XDG_CONFIG_HOME/ironland-compositor/config.toml` (or `~/.config/...`)
//! 3. `/etc/ironland-compositor/config.toml` (written by the NixOS module)
//!
//! None of these existing is not an error: callers fall back to the
//! defaults below, which reproduce the shortcuts that used to be
//! hardcoded. A malformed file is reported to the caller rather than
//! silently ignored here - [`Config::load`] logs and falls back to
//! defaults (matching the compositor's own live-reload tolerance), while
//! [`Config::try_load`] surfaces the parse error for a caller (`ironlandctl`,
//! the settings GUI) that wants to tell the user about it instead.
//!
//! This crate is deliberately free of any Wayland/smithay dependency so it
//! can be shared, as a lightweight build, between the compositor itself,
//! `ironlandctl`, and (indirectly, via `ironlandctl`) the Go settings GUI.
//! Anything that needs a keysym or geometry type - parsing a keybinding
//! spec into a modifiers+keysym pair, resolving an output's on-screen
//! position - lives in the compositor crate's own `src/keybindings.rs`
//! instead, built on top of the plain-data types here.

use std::{collections::HashMap, env, fs, path::PathBuf};

use serde::{Deserialize, Serialize};

/// Keyboard layout settings, passed straight through to xkbcommon.
///
/// An empty string for any field means "let xkbcommon fall back to its
/// `XKB_DEFAULT_*` environment variables / built-in default".
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct KeyboardSettings {
    pub rules: String,
    pub model: String,
    pub layout: String,
    pub variant: String,
    pub options: String,
}

/// Where to place an output relative to another, already-placed one, or at
/// an explicit logical position. Kept separate from [`OutputSettings::mirror_of`]:
/// mirroring takes priority over `position` when both are set for the same
/// output.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum OutputPosition {
    RightOf { right_of: String },
    LeftOf { left_of: String },
    Above { above: String },
    Below { below: String },
    Absolute { x: i32, y: i32 },
}

/// Per-output settings, keyed by connector name (e.g. `"eDP-1"`,
/// `"HDMI-A-1"`) in the `[outputs.*]` config table.
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
#[serde(default)]
pub struct OutputSettings {
    /// Marks this the primary monitor. At most one output should set this;
    /// if several do, which one wins is unspecified.
    pub primary: bool,
    /// Requested vertical refresh rate in millihertz. The compositor picks
    /// the closest advertised rate at the monitor's preferred resolution.
    pub refresh_rate: Option<i32>,
    /// Name of another output to duplicate ("mirror"/"clone" this one onto),
    /// by placing this output at that output's position. Takes priority over
    /// `position`.
    pub mirror_of: Option<String>,
    /// Where to place this output when it isn't mirroring another one.
    /// Defaults to auto-placement (stacked to the right of every other
    /// placed output), matching pre-existing behavior.
    pub position: Option<OutputPosition>,
}

/// Whether workspaces are independent per output ("split") or shared across
/// every connected output ("combined", i.e. switching workspaces moves every
/// monitor to the same slot at once, GNOME-style).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMode {
    #[default]
    PerMonitor,
    Combined,
}

/// Workspace settings: how many virtual desktops exist, whether outputs
/// share them or each gets their own, and whether the on-screen dot
/// indicator (shown briefly on switch) is enabled.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct WorkspaceSettings {
    pub mode: WorkspaceMode,
    /// How many workspaces each output (or the whole session, in `Combined`
    /// mode) starts with.
    pub count: usize,
    /// If true, the workspace count isn't fixed at `count`: switching or
    /// moving a window past the last workspace creates a new one on demand,
    /// and trailing empty workspaces are dropped again automatically.
    pub dynamic: bool,
    /// Whether to flash a row of dots (like GNOME's workspace switcher) on
    /// screen briefly whenever the active workspace changes.
    pub overlay: bool,
    /// How long windows take to slide to/from the next workspace on a
    /// switch, in milliseconds. `0` disables the animation - the switch is
    /// instant, as it was before this setting existed.
    pub transition_ms: u32,
}

impl Default for WorkspaceSettings {
    fn default() -> Self {
        WorkspaceSettings {
            mode: WorkspaceMode::default(),
            count: 4,
            dynamic: false,
            overlay: true,
            transition_ms: 220,
        }
    }
}

/// Mouse cursor theme settings: `None` means "fall back to the
/// `XCURSOR_THEME`/`XCURSOR_SIZE` environment variable, or the compositor's
/// own built-in default if that isn't set either".
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
#[serde(default)]
pub struct CursorSettings {
    pub theme: Option<String>,
    pub size: Option<u32>,
}

/// Background blur shown through translucent application surfaces.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct BlurSettings {
    pub enabled: bool,
    /// Gaussian standard deviation in logical pixels.
    pub radius: u32,
}

impl Default for BlurSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            radius: 12,
        }
    }
}

/// Rounded corners on window content, drawn via a GLES shader. Off by
/// default, matching the sharp-cornered windows this compositor had before
/// the feature existed.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct CornersSettings {
    pub enabled: bool,
    /// Corner radius in logical pixels.
    pub radius: u32,
}

impl Default for CornersSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            radius: 12,
        }
    }
}

/// Space between tiled windows, and between tiled windows and the output
/// edges. Both default to reproducing this compositor's hardcoded behavior
/// before either was configurable: an 8px gap between windows, and no gap
/// at the screen edge.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct GapsSettings {
    pub inner: u32,
    pub outer: u32,
}

impl Default for GapsSettings {
    fn default() -> Self {
        Self { inner: 8, outer: 0 }
    }
}

/// A highlight border drawn around the currently focused window only (not
/// every tiled window). Off by default. `gradient_color` unset draws a
/// solid `color` border; set, it draws a two-stop linear gradient from
/// `color` to `gradient_color` along `angle` degrees, similar to
/// Hyprland's `col.active_border`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct BorderSettings {
    pub enabled: bool,
    /// Border thickness in logical pixels.
    pub thickness: u32,
    /// `#rrggbb` or `#rrggbbaa`.
    pub color: String,
    /// `#rrggbb` or `#rrggbbaa`; unset means a solid `color` border.
    pub gradient_color: Option<String>,
    /// Gradient direction in degrees. Ignored when `gradient_color` is unset.
    pub angle: f32,
}

impl Default for BorderSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            thickness: 2,
            color: "#89b4fa".to_string(),
            gradient_color: None,
            angle: 45.0,
        }
    }
}

/// Pointer/keyboard-focus interaction. Both default off, matching the
/// click-to-focus behavior this compositor had before either existed.
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
#[serde(default)]
pub struct FocusSettings {
    /// If true, moving the pointer over a window focuses it, without
    /// needing a click. Hovering empty space (no window under the
    /// pointer) leaves the current focus alone rather than clearing it.
    pub follows_mouse: bool,
    /// If true, a focus change that didn't come from the pointer itself
    /// (switching workspaces, cycling windows, a newly mapped window
    /// taking focus, activating a window from the dock) warps the pointer
    /// to the center of the newly focused window.
    pub mouse_follows_focus: bool,
}

/// GUI/CLI-only appearance state: the compositor itself has no notion of a
/// color scheme, so this isn't part of [`Config`] - the compositor's
/// [`RawConfig`]-equivalent parser simply ignores an `[appearance]` table it
/// doesn't know about. Kept in the same `config.toml` purely so a dark/light
/// mode toggle (in `ironlandctl` or the settings GUI) remembers its state
/// across runs.
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
#[serde(default)]
pub struct AppearanceSettings {
    pub dark_mode: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
struct RawConfig {
    keyboard: KeyboardSettings,
    terminal: Option<String>,
    browser: Option<String>,
    file_manager: Option<String>,
    top_bar: bool,
    wallpaper: Option<String>,
    blur: BlurSettings,
    corners: CornersSettings,
    gaps: GapsSettings,
    border: BorderSettings,
    cursor: CursorSettings,
    focus: FocusSettings,
    shortcuts: HashMap<String, Vec<String>>,
    outputs: HashMap<String, OutputSettings>,
    workspaces: WorkspaceSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    pub keyboard: KeyboardSettings,
    pub terminal: String,
    /// Command spawned by the `open_browser` action.
    pub browser: String,
    /// Command spawned by the `open_file_manager` action.
    pub file_manager: String,
    /// Whether windows may get a compositor-drawn header bar ("top bar") for
    /// server-side decoration. Off by default: a client's request for
    /// server-side decoration is overridden back to client-side, so no
    /// header bar is drawn regardless of what individual clients ask for.
    pub top_bar: bool,
    /// Path to an image file (PNG/JPEG/WebP) to use as the desktop
    /// background, scaled and center-cropped to cover each output. `None`
    /// (the default, and also the fallback if the path fails to load) uses
    /// the compositor's built-in default wallpaper.
    pub wallpaper: Option<String>,
    /// Gaussian backdrop blur settings for translucent application windows.
    pub blur: BlurSettings,
    /// Rounded corner settings for window content (see [`CornersSettings`]).
    pub corners: CornersSettings,
    /// Gap between tiled windows and between them and the output edges (see
    /// [`GapsSettings`]).
    pub gaps: GapsSettings,
    /// Highlight border around the focused window (see [`BorderSettings`]).
    pub border: BorderSettings,
    /// Mouse cursor theme/size (see [`CursorSettings`]).
    pub cursor: CursorSettings,
    /// Pointer/keyboard-focus interaction (see [`FocusSettings`]).
    pub focus: FocusSettings,
    /// action name -> key combos, e.g. `"toggle_launcher" -> ["ctrl+space"]`.
    /// Always fully populated: entries not overridden by the config file
    /// keep their built-in default.
    pub shortcuts: HashMap<String, Vec<String>>,
    /// connector name (e.g. `"eDP-1"`) -> settings for that output. Outputs
    /// not present here use [`OutputSettings::default`] (auto-placed,
    /// extended, not primary).
    pub outputs: HashMap<String, OutputSettings>,
    /// Virtual desktop settings (see [`WorkspaceSettings`]).
    pub workspaces: WorkspaceSettings,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            keyboard: KeyboardSettings::default(),
            terminal: default_terminal(),
            browser: default_browser(),
            file_manager: default_file_manager(),
            top_bar: false,
            wallpaper: None,
            blur: BlurSettings::default(),
            corners: CornersSettings::default(),
            gaps: GapsSettings::default(),
            border: BorderSettings::default(),
            cursor: CursorSettings::default(),
            focus: FocusSettings::default(),
            shortcuts: default_shortcuts(),
            outputs: HashMap::new(),
            workspaces: WorkspaceSettings::default(),
        }
    }
}

/// [`Config`] plus the GUI/CLI-only [`AppearanceSettings`], as it round-trips
/// through `config.toml` end to end. `ironlandctl`'s `show --json`/
/// `defaults --json`/`apply` and the settings GUI all speak this shape.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct FullConfig {
    #[serde(flatten)]
    pub config: Config,
    #[serde(default)]
    pub appearance: AppearanceSettings,
}

fn default_terminal() -> String {
    "weston-terminal".to_string()
}

fn default_browser() -> String {
    "brave".to_string()
}

fn default_file_manager() -> String {
    "iron-file".to_string()
}

/// The shortcuts the compositor shipped with before it became configurable.
/// Kept as the baseline so an empty/missing/partial config file still
/// behaves exactly as before.
pub fn default_shortcuts() -> HashMap<String, Vec<String>> {
    [
        ("quit", vec!["super+alt+backspace", "super+q"]),
        ("run_terminal", vec!["super+c"]),
        ("toggle_launcher", vec!["ctrl+space"]),
        // Fires the shell's own "launcher" shortcut (see caelestia-shell's
        // `modules/Shortcuts.qml`) via the `ironland-shortcuts-v1`
        // `shortcut:<name>` escape hatch - not the compositor's built-in
        // launcher above.
        ("shortcut:launcher", vec!["super"]),
        ("open_browser", vec!["super+b"]),
        ("open_file_manager", vec!["super+f"]),
        ("toggle_floating", vec!["super+shift+space"]),
        ("kill_window", vec!["super+x"]),
        // `focus_left`/`focus_right` used to default to bare `super+left`/
        // `super+right`, but those combos now switch workspaces (see
        // `workspace_left`/`workspace_right` below), so focus-by-direction
        // moved to `super+ctrl+left/right` for the left/right pair only;
        // up/down were never in the way and keep their original combo.
        ("focus_left", vec!["super+ctrl+left"]),
        ("focus_right", vec!["super+ctrl+right"]),
        ("focus_up", vec!["super+up"]),
        ("focus_down", vec!["super+down"]),
        ("swap_left", vec!["super+shift+left"]),
        ("swap_right", vec!["super+shift+right"]),
        ("swap_up", vec!["super+shift+up"]),
        ("swap_down", vec!["super+shift+down"]),
        // Likewise, `resize_left`/`resize_right` moved off `super+alt+left/
        // right`, which now moves the focused window to an adjacent
        // workspace (see `move_workspace_left`/`move_workspace_right`).
        ("resize_left", vec!["super+ctrl+shift+left"]),
        ("resize_right", vec!["super+ctrl+shift+right"]),
        ("resize_up", vec!["super+alt+up"]),
        ("resize_down", vec!["super+alt+down"]),
        ("workspace_left", vec!["super+left"]),
        ("workspace_right", vec!["super+right"]),
        ("move_workspace_left", vec!["super+alt+left"]),
        ("move_workspace_right", vec!["super+alt+right"]),
        ("scale_up", vec!["super+shift+p"]),
        ("scale_down", vec!["super+shift+m"]),
        ("toggle_preview", vec!["super+shift+w"]),
        ("rotate_output", vec!["super+shift+r"]),
        ("toggle_tint", vec!["super+shift+t"]),
        ("toggle_decorations", vec!["super+shift+d"]),
    ]
    .into_iter()
    .map(|(name, keys)| {
        (
            name.to_string(),
            keys.into_iter().map(String::from).collect(),
        )
    })
    .collect()
}

/// All action names the compositor understands, for validation and for
/// `ironlandctl`/the settings GUI. Kept next to `default_shortcuts` so the
/// two can't drift.
pub fn known_actions() -> Vec<&'static str> {
    [
        "quit",
        "run_terminal",
        "toggle_launcher",
        "open_browser",
        "open_file_manager",
        "toggle_floating",
        "kill_window",
        "focus_left",
        "focus_right",
        "focus_up",
        "focus_down",
        "swap_left",
        "swap_right",
        "swap_up",
        "swap_down",
        "resize_left",
        "resize_right",
        "resize_up",
        "resize_down",
        "workspace_left",
        "workspace_right",
        "move_workspace_left",
        "move_workspace_right",
        "scale_up",
        "scale_down",
        "toggle_preview",
        "rotate_output",
        "toggle_tint",
        "toggle_decorations",
    ]
    .to_vec()
}

/// True for an action name of the form `"shortcut:<name>"`, the escape
/// hatch that lets `[shortcuts]` bind a key to an `ironland-shortcuts-v1`
/// name instead of one of the compositor's own fixed [`known_actions`].
/// Kept separate from `known_actions` since the set of valid `<name>`s is
/// whatever a client has registered at runtime, not something this module
/// can enumerate.
pub fn is_shortcut_action(action: &str) -> bool {
    action.starts_with("shortcut:")
}

/// The paths the compositor (and `ironlandctl`) check for a config file, in
/// priority order: an explicit test override, the user's own config, then
/// the system-wide file a NixOS module may have written.
pub fn config_search_path() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Ok(explicit) = env::var("IRONLAND_COMPOSITOR_CONFIG") {
        paths.push(PathBuf::from(explicit));
    }

    if let Some(user_path) = user_config_path() {
        paths.push(user_path);
    }

    paths.push(PathBuf::from("/etc/ironland-compositor/config.toml"));

    paths
}

/// Where a user's own config lives: `$XDG_CONFIG_HOME/ironland-compositor/
/// config.toml`, falling back to `~/.config/...`. This is deliberately the
/// same path regardless of `$IRONLAND_COMPOSITOR_CONFIG` - that variable is
/// an explicit override for reads (mainly for testing), not somewhere a
/// normal user's edits (via `ironlandctl` or the settings GUI) should land.
/// `None` only if neither `XDG_CONFIG_HOME` nor `HOME` is set.
pub fn user_config_path() -> Option<PathBuf> {
    let config_home = env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|_| env::var("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok()?;
    Some(config_home.join("ironland-compositor/config.toml"))
}

impl Config {
    /// Loads settings from the first config file found on
    /// [`config_search_path`], falling back to [`Config::default`] if none
    /// exist or the one found doesn't parse.
    pub fn load() -> Config {
        match Self::try_load() {
            Ok((config, path)) => {
                if let Some(path) = path {
                    tracing::info!(path = %path.display(), "Loaded compositor config");
                }
                config
            }
            Err(err) => {
                tracing::warn!(%err, "Failed to load compositor config, using defaults");
                Config::default()
            }
        }
    }

    /// Reads the current effective config without silently replacing a
    /// malformed file with defaults. Live reload uses this so a temporary
    /// typo never wipes the running configuration; it simply retries after
    /// the next file change. `ironlandctl` also uses this directly, to
    /// surface a parse error instead of silently falling back like
    /// [`Config::load`].
    pub fn try_load() -> Result<(Config, Option<PathBuf>), String> {
        for path in config_search_path() {
            let contents = match fs::read_to_string(&path) {
                Ok(contents) => contents,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => {
                    tracing::warn!(path = %path.display(), %err, "Failed to read compositor config, skipping it");
                    continue;
                }
            };

            let raw: RawConfig = match toml::from_str(&contents) {
                Ok(raw) => raw,
                Err(err) => {
                    return Err(format!("failed to parse {}: {err}", path.display()));
                }
            };

            let mut shortcuts = default_shortcuts();
            shortcuts.extend(raw.shortcuts);

            return Ok((
                Config {
                    keyboard: raw.keyboard,
                    terminal: raw.terminal.unwrap_or_else(default_terminal),
                    browser: raw.browser.unwrap_or_else(default_browser),
                    file_manager: raw.file_manager.unwrap_or_else(default_file_manager),
                    top_bar: raw.top_bar,
                    wallpaper: raw.wallpaper,
                    blur: raw.blur,
                    corners: raw.corners,
                    gaps: raw.gaps,
                    border: raw.border,
                    cursor: raw.cursor,
                    focus: raw.focus,
                    shortcuts,
                    outputs: raw.outputs,
                    workspaces: raw.workspaces,
                },
                Some(path),
            ));
        }

        Ok((Config::default(), None))
    }

    /// Settings for the output named `name`, or the all-default settings
    /// (auto-placed, extended, not primary) if it isn't configured.
    pub fn output_settings(&self, name: &str) -> OutputSettings {
        self.outputs.get(name).cloned().unwrap_or_default()
    }

    /// Name of the output marked `primary = true`, if any. If more than one
    /// is marked primary, which one wins is unspecified.
    pub fn primary_output_name(&self) -> Option<&str> {
        self.outputs
            .iter()
            .find(|(_, settings)| settings.primary)
            .map(|(name, _)| name.as_str())
    }
}

impl AppearanceSettings {
    /// Reads just the `[appearance]` table from the first config file found
    /// on [`config_search_path`] - kept separate from [`Config::try_load`]
    /// since the compositor's own schema has no notion of this table.
    pub fn load() -> AppearanceSettings {
        for path in config_search_path() {
            let Ok(contents) = fs::read_to_string(&path) else {
                continue;
            };
            #[derive(Deserialize, Default)]
            #[serde(default)]
            struct WithAppearance {
                appearance: AppearanceSettings,
            }
            return toml::from_str::<WithAppearance>(&contents)
                .map(|w| w.appearance)
                .unwrap_or_default();
        }
        AppearanceSettings::default()
    }
}

impl FullConfig {
    /// The effective config plus appearance, exactly as `ironlandctl show`
    /// and the settings GUI see it.
    pub fn load() -> FullConfig {
        FullConfig {
            config: Config::load(),
            appearance: AppearanceSettings::load(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_shortcuts_override_only_the_named_action() {
        let mut shortcuts = HashMap::new();
        shortcuts.insert("quit".to_string(), vec!["ctrl+alt+q".to_string()]);
        let raw = RawConfig {
            keyboard: KeyboardSettings::default(),
            terminal: None,
            browser: None,
            file_manager: None,
            top_bar: false,
            wallpaper: None,
            blur: BlurSettings::default(),
            corners: CornersSettings::default(),
            gaps: GapsSettings::default(),
            border: BorderSettings::default(),
            cursor: CursorSettings::default(),
            focus: FocusSettings::default(),
            shortcuts,
            outputs: HashMap::new(),
            workspaces: WorkspaceSettings::default(),
        };

        let mut merged = default_shortcuts();
        merged.extend(raw.shortcuts);

        assert_eq!(merged["quit"], vec!["ctrl+alt+q"]);
        assert_eq!(merged["toggle_launcher"], vec!["ctrl+space"]);
    }

    #[test]
    fn unknown_tables_are_ignored() {
        // `[appearance]` is written by `ironlandctl`/the settings GUI for
        // their own dark-mode toggle, which the compositor itself has no
        // use for. Serde's default behavior (no `deny_unknown_fields`) is
        // what makes this safe to add there without needing a matching
        // field in `RawConfig`.
        let raw: Result<RawConfig, _> = toml::from_str("[appearance]\ndark_mode = true\n");
        assert!(raw.is_ok(), "unexpected parse error: {:?}", raw.err());
    }

    #[test]
    fn top_bar_defaults_to_disabled_but_can_be_enabled() {
        let raw: RawConfig = toml::from_str("").unwrap();
        assert!(!raw.top_bar);

        let raw: RawConfig = toml::from_str("top_bar = true").unwrap();
        assert!(raw.top_bar);
    }

    #[test]
    fn wallpaper_defaults_to_none_but_can_be_set() {
        let raw: RawConfig = toml::from_str("").unwrap();
        assert_eq!(raw.wallpaper, None);

        let raw: RawConfig = toml::from_str(r#"wallpaper = "/home/user/wallpaper.png""#).unwrap();
        assert_eq!(raw.wallpaper.as_deref(), Some("/home/user/wallpaper.png"));
    }

    #[test]
    fn parses_relative_and_absolute_positions() {
        let toml = r#"
            [outputs."HDMI-A-1"]
            position = { right_of = "eDP-1" }

            [outputs."DP-1"]
            position = { x = 100, y = 200 }

            [outputs."eDP-1"]
            primary = true
        "#;
        let raw: RawConfig = toml::from_str(toml).unwrap();
        assert_eq!(
            raw.outputs["HDMI-A-1"].position,
            Some(OutputPosition::RightOf {
                right_of: "eDP-1".to_string()
            })
        );
        assert_eq!(
            raw.outputs["DP-1"].position,
            Some(OutputPosition::Absolute { x: 100, y: 200 })
        );
        assert!(raw.outputs["eDP-1"].primary);
    }

    #[test]
    fn workspace_settings_default_to_split_per_monitor() {
        let raw: RawConfig = toml::from_str("").unwrap();
        assert_eq!(raw.workspaces, WorkspaceSettings::default());
        assert_eq!(raw.workspaces.mode, WorkspaceMode::PerMonitor);
        assert_eq!(raw.workspaces.count, 4);
        assert!(!raw.workspaces.dynamic);
        assert!(raw.workspaces.overlay);
    }

    #[test]
    fn output_refresh_rate_parses_in_millihertz() {
        let raw: RawConfig = toml::from_str("[outputs.DP-1]\nrefresh_rate = 144000\n").unwrap();
        assert_eq!(raw.outputs["DP-1"].refresh_rate, Some(144_000));
    }

    #[test]
    fn blur_defaults_off_and_parses() {
        assert_eq!(
            BlurSettings::default(),
            BlurSettings {
                enabled: false,
                radius: 12
            }
        );
        let raw: RawConfig = toml::from_str("[blur]\nenabled = true\nradius = 20\n").unwrap();
        assert_eq!(
            raw.blur,
            BlurSettings {
                enabled: true,
                radius: 20
            }
        );
    }

    #[test]
    fn corners_default_off_and_parse() {
        assert_eq!(
            CornersSettings::default(),
            CornersSettings {
                enabled: false,
                radius: 12
            }
        );
        let raw: RawConfig = toml::from_str("[corners]\nenabled = true\nradius = 8\n").unwrap();
        assert_eq!(
            raw.corners,
            CornersSettings {
                enabled: true,
                radius: 8
            }
        );
    }

    #[test]
    fn cursor_settings_default_to_none_but_can_be_set() {
        let raw: RawConfig = toml::from_str("").unwrap();
        assert_eq!(raw.cursor, CursorSettings::default());
        assert_eq!(raw.cursor.theme, None);
        assert_eq!(raw.cursor.size, None);

        let raw: RawConfig = toml::from_str("[cursor]\ntheme = \"Adwaita\"\nsize = 32\n").unwrap();
        assert_eq!(raw.cursor.theme.as_deref(), Some("Adwaita"));
        assert_eq!(raw.cursor.size, Some(32));
    }

    #[test]
    fn focus_settings_default_off_and_parse() {
        let raw: RawConfig = toml::from_str("").unwrap();
        assert_eq!(raw.focus, FocusSettings::default());
        assert!(!raw.focus.follows_mouse);
        assert!(!raw.focus.mouse_follows_focus);

        let raw: RawConfig =
            toml::from_str("[focus]\nfollows_mouse = true\nmouse_follows_focus = true\n").unwrap();
        assert!(raw.focus.follows_mouse);
        assert!(raw.focus.mouse_follows_focus);
    }

    #[test]
    fn workspace_settings_parse_from_config() {
        let toml = r#"
            [workspaces]
            mode = "combined"
            count = 6
            dynamic = true
            overlay = false
        "#;
        let raw: RawConfig = toml::from_str(toml).unwrap();
        assert_eq!(raw.workspaces.mode, WorkspaceMode::Combined);
        assert_eq!(raw.workspaces.count, 6);
        assert!(raw.workspaces.dynamic);
        assert!(!raw.workspaces.overlay);
    }

    #[test]
    fn full_config_round_trips_through_json() {
        let full = FullConfig {
            config: Config::default(),
            appearance: AppearanceSettings { dark_mode: true },
        };
        let json = serde_json::to_string(&full).unwrap();
        let back: FullConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(full, back);
    }
}
