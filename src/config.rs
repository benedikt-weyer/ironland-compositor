//! User-facing settings: keyboard layout and keyboard shortcuts.
//!
//! Settings are read from the first of these that exists, in order:
//!
//! 1. `$IRONLAND_COMPOSITOR_CONFIG` (an explicit path, mainly for testing)
//! 2. `$XDG_CONFIG_HOME/ironland-copositor/config.toml` (or `~/.config/...`)
//! 3. `/etc/ironland-copositor/config.toml` (written by the NixOS module)
//!
//! None of these existing is not an error: the compositor falls back to the
//! defaults below, which reproduce the shortcuts that used to be hardcoded.
//! A malformed file is logged and ignored rather than treated as fatal,
//! since a typo in a config file shouldn't stop the compositor from
//! starting.

use std::{collections::HashMap, env, fs, path::PathBuf};

use serde::Deserialize;
use smithay::{
    input::keyboard::{Keysym, ModifiersState, XkbConfig, xkb},
    utils::{Logical, Point, Rectangle, Size},
};
use tracing::warn;

/// Keyboard layout settings, passed straight through to xkbcommon.
///
/// An empty string for any field means "let xkbcommon fall back to its
/// `XKB_DEFAULT_*` environment variables / built-in default", matching
/// [`XkbConfig`]'s own default behavior.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct KeyboardSettings {
    pub rules: String,
    pub model: String,
    pub layout: String,
    pub variant: String,
    pub options: String,
}

impl KeyboardSettings {
    pub fn to_xkb_config(&self) -> XkbConfig<'_> {
        XkbConfig {
            rules: &self.rules,
            model: &self.model,
            layout: &self.layout,
            variant: &self.variant,
            options: if self.options.is_empty() {
                None
            } else {
                Some(self.options.clone())
            },
        }
    }
}

/// Which modifiers a keybinding requires. Caps lock/num lock/level3-4 shift
/// are deliberately not part of a binding's identity: only ctrl/alt/shift/
/// logo distinguish one shortcut from another here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyModifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool,
}

impl KeyModifiers {
    pub fn matches(&self, mods: &ModifiersState) -> bool {
        self.ctrl == mods.ctrl
            && self.alt == mods.alt
            && self.shift == mods.shift
            && self.logo == mods.logo
    }
}

/// Where to place an output relative to another, already-placed one, or at
/// an explicit logical position. Kept separate from [`OutputSettings::mirror_of`]:
/// mirroring takes priority over `position` when both are set for the same
/// output.
#[derive(Debug, Clone, Deserialize, PartialEq)]
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
#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
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

/// Resolves where a newly-connecting output named `name`, with logical size
/// `size`, should be placed, given its [`OutputSettings`] and the outputs
/// already placed in the space (`name -> current geometry`).
///
/// Falls back to auto-placement (and logs a warning) if a referenced output
/// (`mirror_of`/`right_of`/etc.) hasn't connected yet.
pub fn resolve_output_position(
    settings: &OutputSettings,
    name: &str,
    size: Size<i32, Logical>,
    placed: &[(String, Rectangle<i32, Logical>)],
) -> Point<i32, Logical> {
    let find = |target: &str| {
        placed
            .iter()
            .find(|(n, _)| n == target)
            .map(|(_, rect)| *rect)
    };

    if let Some(target) = settings.mirror_of.as_deref() {
        match find(target) {
            Some(rect) => return rect.loc,
            None => warn!(
                output = name,
                mirror_of = target,
                "Mirror target not connected yet, using auto placement"
            ),
        }
    }

    match &settings.position {
        Some(OutputPosition::Absolute { x, y }) => return (*x, *y).into(),
        Some(OutputPosition::RightOf { right_of }) => match find(right_of) {
            Some(rect) => return (rect.loc.x + rect.size.w, rect.loc.y).into(),
            None => warn!(
                output = name,
                right_of, "Reference output not connected yet, using auto placement"
            ),
        },
        Some(OutputPosition::LeftOf { left_of }) => match find(left_of) {
            Some(rect) => return (rect.loc.x - size.w, rect.loc.y).into(),
            None => warn!(
                output = name,
                left_of, "Reference output not connected yet, using auto placement"
            ),
        },
        Some(OutputPosition::Above { above }) => match find(above) {
            Some(rect) => return (rect.loc.x, rect.loc.y - size.h).into(),
            None => warn!(
                output = name,
                above, "Reference output not connected yet, using auto placement"
            ),
        },
        Some(OutputPosition::Below { below }) => match find(below) {
            Some(rect) => return (rect.loc.x, rect.loc.y + rect.size.h).into(),
            None => warn!(
                output = name,
                below, "Reference output not connected yet, using auto placement"
            ),
        },
        None => {}
    }

    let x = placed
        .iter()
        .map(|(_, rect)| rect.loc.x + rect.size.w)
        .max()
        .unwrap_or(0);
    (x, 0).into()
}

/// Whether workspaces are independent per output ("split") or shared across
/// every connected output ("combined", i.e. switching workspaces moves every
/// monitor to the same slot at once, GNOME-style).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMode {
    #[default]
    PerMonitor,
    Combined,
}

/// Workspace settings: how many virtual desktops exist, whether outputs
/// share them or each gets their own, and whether the on-screen dot
/// indicator (shown briefly on switch) is enabled.
#[derive(Debug, Clone, Deserialize, PartialEq)]
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
}

/// Mouse cursor theme settings, passed to the `xcursor` crate the same way
/// the `XCURSOR_THEME`/`XCURSOR_SIZE` environment variables are: `None`
/// means "fall back to that environment variable, or its own built-in
/// default if that isn't set either".
#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct CursorSettings {
    pub theme: Option<String>,
    pub size: Option<u32>,
}

/// Background blur shown through translucent application surfaces.
#[derive(Debug, Clone, Deserialize, PartialEq)]
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

/// Pointer/keyboard-focus interaction. Both default off, matching the
/// click-to-focus behavior this compositor had before either existed.
#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
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

impl Default for WorkspaceSettings {
    fn default() -> Self {
        WorkspaceSettings {
            mode: WorkspaceMode::default(),
            count: 4,
            dynamic: false,
            overlay: true,
        }
    }
}

/// A single parsed keybinding: the modifiers/key it fires on, and the name
/// of the action to run (looked up against the compositor's own action
/// table, since the set of possible actions is compositor-specific and this
/// module only knows about parsing).
#[derive(Debug, Clone)]
pub struct Keybinding {
    pub modifiers: KeyModifiers,
    pub keysym: Keysym,
    pub action: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct RawConfig {
    keyboard: KeyboardSettings,
    terminal: Option<String>,
    browser: Option<String>,
    file_manager: Option<String>,
    top_bar: bool,
    wallpaper: Option<String>,
    blur: BlurSettings,
    cursor: CursorSettings,
    focus: FocusSettings,
    shortcuts: HashMap<String, Vec<String>>,
    outputs: HashMap<String, OutputSettings>,
    workspaces: WorkspaceSettings,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub keyboard: KeyboardSettings,
    pub terminal: String,
    /// Command spawned by the `open_browser` action.
    pub browser: String,
    /// Command spawned by the `open_file_manager` action.
    pub file_manager: String,
    /// Whether windows may get a compositor-drawn header bar ("top bar") for
    /// server-side decoration. Off by default: a client's request for
    /// server-side decoration is overridden back to client-side (see
    /// [`crate::state::AnvilState`]'s `XdgDecorationHandler` impl), so no
    /// header bar is drawn regardless of what individual clients ask for.
    pub top_bar: bool,
    /// Path to an image file (PNG/JPEG/WebP) to use as the desktop
    /// background, scaled and center-cropped to cover each output. `None`
    /// (the default, and also the fallback if the path fails to load) uses
    /// the compositor's built-in default wallpaper.
    pub wallpaper: Option<String>,
    /// Gaussian backdrop blur settings for translucent application windows.
    pub blur: BlurSettings,
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
            cursor: CursorSettings::default(),
            focus: FocusSettings::default(),
            shortcuts: default_shortcuts(),
            outputs: HashMap::new(),
            workspaces: WorkspaceSettings::default(),
        }
    }
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
fn default_shortcuts() -> HashMap<String, Vec<String>> {
    [
        ("quit", vec!["super+alt+backspace", "super+q"]),
        ("run_terminal", vec!["super+c"]),
        ("toggle_launcher", vec!["super"]),
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

/// All action names the compositor understands, for validation and for the
/// settings GUI. Kept next to `default_shortcuts` so the two can't drift.
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

fn config_search_path() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Ok(explicit) = env::var("IRONLAND_COMPOSITOR_CONFIG") {
        paths.push(PathBuf::from(explicit));
    }

    let config_home = env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|_| env::var("HOME").map(|home| PathBuf::from(home).join(".config")));
    if let Ok(config_home) = config_home {
        paths.push(config_home.join("ironland-copositor/config.toml"));
    }

    paths.push(PathBuf::from("/etc/ironland-copositor/config.toml"));

    paths
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
                warn!(%err, "Failed to load compositor config, using defaults");
                Config::default()
            }
        }
    }

    /// Reads the current effective config without silently replacing a
    /// malformed file with defaults. Live reload uses this so a temporary
    /// typo never wipes the running configuration; it simply retries after
    /// the next file change.
    pub(crate) fn try_load() -> Result<(Config, Option<PathBuf>), String> {
        for path in config_search_path() {
            let contents = match fs::read_to_string(&path) {
                Ok(contents) => contents,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => {
                    warn!(path = %path.display(), %err, "Failed to read compositor config, skipping it");
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

    /// Parses every configured binding into `(modifiers, keysym, action
    /// name)` triples, skipping (with a warning) any binding that doesn't
    /// parse or whose action name isn't recognized.
    pub fn parsed_keybindings(&self) -> Vec<Keybinding> {
        let known = known_actions();
        let mut bindings = Vec::new();

        for (action, specs) in &self.shortcuts {
            if !known.contains(&action.as_str()) {
                warn!(action, "Unknown action in [shortcuts] config, ignoring");
                continue;
            }

            for spec in specs {
                // A bare modifier name (no `+`, e.g. `"super"`) isn't a
                // modifiers+key combo at all - it's handled separately by
                // `super_tap_action`, so skip it here rather than reporting
                // it as an unparseable combo.
                if is_bare_modifier_tap(spec) {
                    continue;
                }

                match parse_binding(spec) {
                    Some((modifiers, keysym)) => bindings.push(Keybinding {
                        modifiers,
                        keysym,
                        action: action.clone(),
                    }),
                    None => warn!(action, spec, "Failed to parse keybinding, ignoring"),
                }
            }
        }

        bindings
    }

    /// The action bound to a bare Super key tap (pressed and released with no
    /// other key in between - see `input_handler`'s tap tracking), if any is
    /// configured. Only one action can meaningfully fire on a Super tap, so
    /// if more than one action lists a bare `"super"` spec, the first found
    /// (in arbitrary map order) wins and the rest are ignored with a warning.
    pub fn super_tap_action(&self) -> Option<&str> {
        let known = known_actions();
        let mut found: Option<&str> = None;

        for (action, specs) in &self.shortcuts {
            if !specs.iter().any(|spec| is_bare_modifier_tap(spec)) {
                continue;
            }
            if !known.contains(&action.as_str()) {
                warn!(action, "Unknown action bound to a bare Super tap, ignoring");
                continue;
            }
            if let Some(existing) = found {
                warn!(
                    action,
                    existing, "Multiple actions bound to a bare Super tap, ignoring this one"
                );
                continue;
            }
            found = Some(action.as_str());
        }

        found
    }
}

/// Whether `spec` names a modifier on its own (no `+`), meaning "trigger on
/// a tap of this modifier alone" rather than a modifiers+key combo. Only the
/// Super/logo modifier is meaningful here today.
fn is_bare_modifier_tap(spec: &str) -> bool {
    matches!(
        spec.trim().to_ascii_lowercase().as_str(),
        "super" | "logo" | "meta" | "win"
    )
}

/// Parses a binding spec like `"ctrl+shift+left"` into its modifiers and
/// keysym. The last `+`-separated token is the key; everything before it is
/// a modifier name (`ctrl`/`control`, `alt`, `shift`, `super`/`logo`/`meta`).
fn parse_binding(spec: &str) -> Option<(KeyModifiers, Keysym)> {
    let parts: Vec<&str> = spec
        .split('+')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let (mod_parts, key_part) = parts.split_last()?;

    let mut modifiers = KeyModifiers::default();
    for part in key_part {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers.ctrl = true,
            "alt" => modifiers.alt = true,
            "shift" => modifiers.shift = true,
            "super" | "logo" | "meta" | "win" => modifiers.logo = true,
            other => {
                warn!(modifier = other, spec, "Unknown modifier in keybinding");
                return None;
            }
        }
    }

    let keysym = parse_key_name(mod_parts, modifiers.shift)?;
    Some((modifiers, keysym))
}

/// Resolves a key name to a keysym. Single ASCII letters are special-cased:
/// xkb has distinct keysyms for the lower- and upper-case forms of a letter
/// (`a` vs `A`), and it's the upper-case one that a physical key reports
/// once `modified_sym()` has applied an active Shift — so a binding that
/// asks for Shift always resolves the letter to its upper-case keysym,
/// regardless of how the user cased it in the config.
fn parse_key_name(name: &str, shift: bool) -> Option<Keysym> {
    let mut chars = name.chars();
    let keysym = match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphabetic() => {
            let letter = if shift {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            };
            xkb::keysym_from_name(&letter.to_string(), xkb::KEYSYM_NO_FLAGS)
        }
        _ => xkb::keysym_from_name(name, xkb::KEYSYM_CASE_INSENSITIVE),
    };

    if keysym.raw() == 0 {
        None
    } else {
        Some(keysym)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_binding() {
        let (mods, sym) = parse_binding("ctrl+q").unwrap();
        assert_eq!(
            mods,
            KeyModifiers {
                ctrl: true,
                ..Default::default()
            }
        );
        assert_eq!(sym, Keysym::q);
    }

    #[test]
    fn shift_uppercases_letter_bindings() {
        let (mods, sym) = parse_binding("ctrl+shift+m").unwrap();
        assert!(mods.shift);
        assert_eq!(sym, Keysym::M);
    }

    #[test]
    fn parses_named_keys_case_insensitively() {
        let (_, sym) = parse_binding("ctrl+RETURN").unwrap();
        assert_eq!(sym, Keysym::Return);

        let (_, sym) = parse_binding("ctrl+alt+backspace").unwrap();
        assert_eq!(sym, Keysym::BackSpace);
    }

    #[test]
    fn rejects_unknown_modifier() {
        assert!(parse_binding("hyper+q").is_none());
    }

    #[test]
    fn rejects_unknown_key() {
        assert!(parse_binding("ctrl+notarealkey").is_none());
    }

    #[test]
    fn every_default_shortcut_parses() {
        for (action, specs) in default_shortcuts() {
            for spec in specs {
                if is_bare_modifier_tap(&spec) {
                    continue;
                }
                assert!(
                    parse_binding(&spec).is_some(),
                    "default binding {action}={spec} failed to parse"
                );
            }
        }
    }

    #[test]
    fn default_toggle_launcher_is_a_bare_super_tap() {
        assert_eq!(
            Config::default().super_tap_action(),
            Some("toggle_launcher")
        );
    }

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
            cursor: CursorSettings::default(),
            focus: FocusSettings::default(),
            shortcuts,
            outputs: HashMap::new(),
            workspaces: WorkspaceSettings::default(),
        };

        let mut merged = default_shortcuts();
        merged.extend(raw.shortcuts);

        assert_eq!(merged["quit"], vec!["ctrl+alt+q"]);
        assert_eq!(merged["toggle_launcher"], vec!["super"]);
    }

    fn size(w: i32, h: i32) -> Size<i32, Logical> {
        (w, h).into()
    }

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    #[test]
    fn unknown_tables_are_ignored() {
        // `[appearance]` is written by the settings GUI for its own
        // dark-mode toggle, which the compositor itself has no use for.
        // Serde's default behavior (no `deny_unknown_fields`) is what makes
        // this safe to add there without needing a matching field here.
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
    fn auto_placement_stacks_to_the_right() {
        let settings = OutputSettings::default();
        let placed = [("eDP-1".to_string(), rect(0, 0, 1920, 1080))];
        let pos = resolve_output_position(&settings, "HDMI-A-1", size(1920, 1080), &placed);
        assert_eq!(pos, (1920, 0).into());
    }

    #[test]
    fn relative_positions_place_next_to_target() {
        let placed = [("eDP-1".to_string(), rect(0, 0, 1920, 1080))];

        let right = OutputSettings {
            position: Some(OutputPosition::RightOf {
                right_of: "eDP-1".to_string(),
            }),
            ..Default::default()
        };
        assert_eq!(
            resolve_output_position(&right, "b", size(800, 600), &placed),
            (1920, 0).into()
        );

        let left = OutputSettings {
            position: Some(OutputPosition::LeftOf {
                left_of: "eDP-1".to_string(),
            }),
            ..Default::default()
        };
        assert_eq!(
            resolve_output_position(&left, "b", size(800, 600), &placed),
            (-800, 0).into()
        );

        let above = OutputSettings {
            position: Some(OutputPosition::Above {
                above: "eDP-1".to_string(),
            }),
            ..Default::default()
        };
        assert_eq!(
            resolve_output_position(&above, "b", size(800, 600), &placed),
            (0, -600).into()
        );

        let below = OutputSettings {
            position: Some(OutputPosition::Below {
                below: "eDP-1".to_string(),
            }),
            ..Default::default()
        };
        assert_eq!(
            resolve_output_position(&below, "b", size(800, 600), &placed),
            (0, 1080).into()
        );
    }

    #[test]
    fn mirror_of_takes_priority_over_position() {
        let placed = [("eDP-1".to_string(), rect(0, 0, 1920, 1080))];
        let settings = OutputSettings {
            mirror_of: Some("eDP-1".to_string()),
            position: Some(OutputPosition::RightOf {
                right_of: "eDP-1".to_string(),
            }),
            ..Default::default()
        };
        assert_eq!(
            resolve_output_position(&settings, "HDMI-A-1", size(1920, 1080), &placed),
            (0, 0).into()
        );
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
    fn cursor_settings_default_to_none_but_can_be_set() {
        let raw: RawConfig = toml::from_str("").unwrap();
        assert_eq!(raw.cursor, CursorSettings::default());
        assert_eq!(raw.cursor.theme, None);
        assert_eq!(raw.cursor.size, None);

        let raw: RawConfig =
            toml::from_str("[cursor]\ntheme = \"Adwaita\"\nsize = 32\n").unwrap();
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
    fn missing_reference_output_falls_back_to_auto_placement() {
        let placed = [("eDP-1".to_string(), rect(0, 0, 1920, 1080))];
        let settings = OutputSettings {
            position: Some(OutputPosition::RightOf {
                right_of: "not-connected".to_string(),
            }),
            ..Default::default()
        };
        assert_eq!(
            resolve_output_position(&settings, "b", size(800, 600), &placed),
            (1920, 0).into()
        );
    }
}
