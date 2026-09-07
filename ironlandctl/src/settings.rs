//! The `get`/`set`/`unset <key>` commands: every scalar leaf setting,
//! addressed by its dotted TOML path, covering every field the settings GUI
//! exposes outside of `[shortcuts]` and `[outputs]` (see [`crate::shortcuts`]
//! and [`crate::outputs`] for those - they're maps, not scalars, so don't
//! fit this one key/value shape).

use anyhow::{Result, bail};
use clap::ValueEnum;
use ironland_config::FullConfig;
use toml_edit::{DocumentMut, Item, Table, value};

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum SettingKey {
    #[value(name = "terminal")]
    Terminal,
    #[value(name = "browser")]
    Browser,
    #[value(name = "file_manager")]
    FileManager,
    #[value(name = "top_bar")]
    TopBar,
    #[value(name = "wallpaper")]
    Wallpaper,
    #[value(name = "keyboard.rules")]
    KeyboardRules,
    #[value(name = "keyboard.model")]
    KeyboardModel,
    #[value(name = "keyboard.layout")]
    KeyboardLayout,
    #[value(name = "keyboard.variant")]
    KeyboardVariant,
    #[value(name = "keyboard.options")]
    KeyboardOptions,
    #[value(name = "blur.enabled")]
    BlurEnabled,
    #[value(name = "blur.radius")]
    BlurRadius,
    #[value(name = "corners.enabled")]
    CornersEnabled,
    #[value(name = "corners.radius")]
    CornersRadius,
    #[value(name = "cursor.theme")]
    CursorTheme,
    #[value(name = "cursor.size")]
    CursorSize,
    #[value(name = "focus.follows_mouse")]
    FocusFollowsMouse,
    #[value(name = "focus.mouse_follows_focus")]
    FocusMouseFollowsFocus,
    #[value(name = "appearance.dark_mode")]
    AppearanceDarkMode,
    #[value(name = "workspaces.mode")]
    WorkspacesMode,
    #[value(name = "workspaces.count")]
    WorkspacesCount,
    #[value(name = "workspaces.dynamic")]
    WorkspacesDynamic,
    #[value(name = "workspaces.overlay")]
    WorkspacesOverlay,
}

impl SettingKey {
    /// All keys, in declaration order - used to print `show`/`defaults` in a
    /// stable, human-friendly order.
    pub fn all() -> &'static [SettingKey] {
        use SettingKey::*;
        &[
            Terminal,
            Browser,
            FileManager,
            TopBar,
            Wallpaper,
            KeyboardRules,
            KeyboardModel,
            KeyboardLayout,
            KeyboardVariant,
            KeyboardOptions,
            BlurEnabled,
            BlurRadius,
            CornersEnabled,
            CornersRadius,
            CursorTheme,
            CursorSize,
            FocusFollowsMouse,
            FocusMouseFollowsFocus,
            AppearanceDarkMode,
            WorkspacesMode,
            WorkspacesCount,
            WorkspacesDynamic,
            WorkspacesOverlay,
        ]
    }

    pub fn dotted(self) -> &'static str {
        use SettingKey::*;
        match self {
            Terminal => "terminal",
            Browser => "browser",
            FileManager => "file_manager",
            TopBar => "top_bar",
            Wallpaper => "wallpaper",
            KeyboardRules => "keyboard.rules",
            KeyboardModel => "keyboard.model",
            KeyboardLayout => "keyboard.layout",
            KeyboardVariant => "keyboard.variant",
            KeyboardOptions => "keyboard.options",
            BlurEnabled => "blur.enabled",
            BlurRadius => "blur.radius",
            CornersEnabled => "corners.enabled",
            CornersRadius => "corners.radius",
            CursorTheme => "cursor.theme",
            CursorSize => "cursor.size",
            FocusFollowsMouse => "focus.follows_mouse",
            FocusMouseFollowsFocus => "focus.mouse_follows_focus",
            AppearanceDarkMode => "appearance.dark_mode",
            WorkspacesMode => "workspaces.mode",
            WorkspacesCount => "workspaces.count",
            WorkspacesDynamic => "workspaces.dynamic",
            WorkspacesOverlay => "workspaces.overlay",
        }
    }
}

/// Reads `key`'s current value out of the effective config, formatted the
/// same way `set` expects it back (so `ironlandctl set $(ironlandctl get k)`
/// round-trips).
pub fn get(full: &FullConfig, key: SettingKey) -> String {
    let cfg = &full.config;
    use SettingKey::*;
    match key {
        Terminal => cfg.terminal.clone(),
        Browser => cfg.browser.clone(),
        FileManager => cfg.file_manager.clone(),
        TopBar => cfg.top_bar.to_string(),
        Wallpaper => cfg.wallpaper.clone().unwrap_or_default(),
        KeyboardRules => cfg.keyboard.rules.clone(),
        KeyboardModel => cfg.keyboard.model.clone(),
        KeyboardLayout => cfg.keyboard.layout.clone(),
        KeyboardVariant => cfg.keyboard.variant.clone(),
        KeyboardOptions => cfg.keyboard.options.clone(),
        BlurEnabled => cfg.blur.enabled.to_string(),
        BlurRadius => cfg.blur.radius.to_string(),
        CornersEnabled => cfg.corners.enabled.to_string(),
        CornersRadius => cfg.corners.radius.to_string(),
        CursorTheme => cfg.cursor.theme.clone().unwrap_or_default(),
        CursorSize => cfg.cursor.size.map(|s| s.to_string()).unwrap_or_default(),
        FocusFollowsMouse => cfg.focus.follows_mouse.to_string(),
        FocusMouseFollowsFocus => cfg.focus.mouse_follows_focus.to_string(),
        AppearanceDarkMode => full.appearance.dark_mode.to_string(),
        WorkspacesMode => workspace_mode_str(cfg.workspaces.mode).to_string(),
        WorkspacesCount => cfg.workspaces.count.to_string(),
        WorkspacesDynamic => cfg.workspaces.dynamic.to_string(),
        WorkspacesOverlay => cfg.workspaces.overlay.to_string(),
    }
}

fn workspace_mode_str(mode: ironland_config::WorkspaceMode) -> &'static str {
    match mode {
        ironland_config::WorkspaceMode::PerMonitor => "per_monitor",
        ironland_config::WorkspaceMode::Combined => "combined",
    }
}

fn parse_bool(raw: &str) -> Result<bool> {
    match raw.to_ascii_lowercase().as_str() {
        "true" | "on" | "yes" | "1" => Ok(true),
        "false" | "off" | "no" | "0" => Ok(false),
        other => bail!("expected true/false, got {other:?}"),
    }
}

fn parse_u32(raw: &str) -> Result<u32> {
    raw.parse::<u32>()
        .map_err(|_| anyhow::anyhow!("expected a non-negative number, got {raw:?}"))
}

/// A key's path into the TOML document, as `[table_path, leaf_key]`; the
/// table path is created if it doesn't exist yet.
fn table_and_leaf(key: SettingKey) -> (&'static [&'static str], &'static str) {
    use SettingKey::*;
    match key {
        Terminal => (&[], "terminal"),
        Browser => (&[], "browser"),
        FileManager => (&[], "file_manager"),
        TopBar => (&[], "top_bar"),
        Wallpaper => (&[], "wallpaper"),
        KeyboardRules => (&["keyboard"], "rules"),
        KeyboardModel => (&["keyboard"], "model"),
        KeyboardLayout => (&["keyboard"], "layout"),
        KeyboardVariant => (&["keyboard"], "variant"),
        KeyboardOptions => (&["keyboard"], "options"),
        BlurEnabled => (&["blur"], "enabled"),
        BlurRadius => (&["blur"], "radius"),
        CornersEnabled => (&["corners"], "enabled"),
        CornersRadius => (&["corners"], "radius"),
        CursorTheme => (&["cursor"], "theme"),
        CursorSize => (&["cursor"], "size"),
        FocusFollowsMouse => (&["focus"], "follows_mouse"),
        FocusMouseFollowsFocus => (&["focus"], "mouse_follows_focus"),
        AppearanceDarkMode => (&["appearance"], "dark_mode"),
        WorkspacesMode => (&["workspaces"], "mode"),
        WorkspacesCount => (&["workspaces"], "count"),
        WorkspacesDynamic => (&["workspaces"], "dynamic"),
        WorkspacesOverlay => (&["workspaces"], "overlay"),
    }
}

fn table_mut<'a>(doc: &'a mut DocumentMut, path: &[&str]) -> &'a mut Table {
    let mut table = doc.as_table_mut();
    for segment in path {
        if !table.contains_key(segment) {
            table.insert(segment, Item::Table(Table::new()));
        }
        table = table[segment]
            .as_table_mut()
            .expect("config.toml has a non-table value where a settings table is expected");
    }
    table
}

/// Validates and writes `raw` into `doc` at `key`'s path.
pub fn set(doc: &mut DocumentMut, key: SettingKey, raw: &str) -> Result<()> {
    let (path, leaf) = table_and_leaf(key);
    let table = table_mut(doc, path);

    use SettingKey::*;
    match key {
        Terminal | Browser | FileManager | Wallpaper | KeyboardRules | KeyboardModel
        | KeyboardLayout | KeyboardVariant | KeyboardOptions | CursorTheme => {
            table[leaf] = value(raw);
        }
        TopBar | BlurEnabled | CornersEnabled | FocusFollowsMouse | FocusMouseFollowsFocus
        | WorkspacesDynamic | WorkspacesOverlay | AppearanceDarkMode => {
            table[leaf] = value(parse_bool(raw)?);
        }
        BlurRadius | CornersRadius => {
            let radius = parse_u32(raw)?;
            if !(1..=50).contains(&radius) {
                bail!("radius must be between 1 and 50, got {radius}");
            }
            table[leaf] = value(i64::from(radius));
        }
        CursorSize => {
            table[leaf] = value(i64::from(parse_u32(raw)?));
        }
        WorkspacesMode => {
            let mode = match raw {
                "per_monitor" => "per_monitor",
                "combined" => "combined",
                other => bail!("expected per_monitor or combined, got {other:?}"),
            };
            table[leaf] = value(mode);
        }
        WorkspacesCount => {
            let count = parse_u32(raw)?;
            if count == 0 {
                bail!("workspace count must be at least 1");
            }
            table[leaf] = value(i64::from(count));
        }
    }
    Ok(())
}

/// Removes `key`'s override, so the effective value reverts to its default
/// on next load.
pub fn unset(doc: &mut DocumentMut, key: SettingKey) {
    let (path, leaf) = table_and_leaf(key);
    let table = table_mut(doc, path);
    table.remove(leaf);
}
