//! The `shortcuts` subcommand: the `[shortcuts]` table, which maps either a
//! compositor action name or a `shortcut:<name>` client-advertised event to
//! a list of key combos.

use anyhow::{Result, bail};
use clap::Subcommand;
use ironland_config::{Config, is_shortcut_action, known_actions};
use toml_edit::{Array, DocumentMut, Item, Table, value};

/// Client-advertised `shortcut:<name>` events curated from caelestia-shell's
/// `modules/Shortcuts.qml`, for `events list`'s suggestions - a name not
/// listed here can still be bound with `events add <name>`, it just won't
/// be suggested.
pub const ADVERTISED_EVENTS: &[(&str, &str)] = &[
    ("launcher", "Shell: toggle launcher"),
    ("showall", "Shell: toggle launcher/dashboard/OSD"),
    ("dashboard", "Shell: toggle dashboard"),
    ("session", "Shell: toggle session menu"),
    ("sidebar", "Shell: toggle sidebar"),
    ("utilities", "Shell: toggle utilities"),
    ("nexus", "Shell: open nexus"),
    ("launcherInterrupt", "Shell: interrupt launcher keybind"),
];

#[derive(Debug, Subcommand)]
pub enum ShortcutsCommand {
    /// List every known action and its bound combos.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Print the combos bound to one action, comma-separated.
    Get { action: String },
    /// Bind an action to one or more combos (comma-separated, e.g.
    /// "super+shift+q" or "super+left,super+kp_left").
    Set { action: String, combos: String },
    /// Revert an action to its built-in default (or, for a `shortcut:<name>`
    /// event, unbind it entirely).
    Unset { action: String },
    /// The `shortcut:<name>` events a client can request over
    /// ironland-shortcuts-v1, distinct from the compositor's own actions.
    #[command(subcommand)]
    Events(EventsCommand),
}

#[derive(Debug, Subcommand)]
pub enum EventsCommand {
    /// List configured events plus suggested, not-yet-configured ones.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Bind a shortcut event to one or more combos (comma-separated); omit
    /// combos to register the event with no keys bound yet.
    Add {
        name: String,
        combos: Option<String>,
    },
    /// Unbind a shortcut event (stored as an explicit empty list, which
    /// shadows the compositor's own built-in default for that name, rather
    /// than deleted outright).
    Remove { name: String },
}

pub fn split_combos(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn shortcuts_table(doc: &mut DocumentMut) -> &mut Table {
    if !doc.contains_key("shortcuts") {
        doc.insert("shortcuts", Item::Table(Table::new()));
    }
    doc["shortcuts"]
        .as_table_mut()
        .expect("config.toml has a non-table [shortcuts]")
}

fn combos_array(combos: &[String]) -> Array {
    combos.iter().map(String::as_str).collect()
}

fn validate_action(action: &str) -> Result<()> {
    if known_actions().contains(&action) || is_shortcut_action(action) {
        Ok(())
    } else {
        bail!(
            "{action:?} isn't a known compositor action; use `shortcuts events add` for a \
             client-advertised shortcut:<name> event, or check `shortcuts list` for valid names"
        )
    }
}

pub fn list(config: &Config, json: bool) {
    let mut actions: Vec<(&str, Vec<String>)> = known_actions()
        .into_iter()
        .map(|action| {
            (
                action,
                config.shortcuts.get(action).cloned().unwrap_or_default(),
            )
        })
        .collect();
    let mut events: Vec<(String, Vec<String>)> = config
        .shortcuts
        .iter()
        .filter_map(|(action, combos)| {
            action
                .strip_prefix("shortcut:")
                .map(|name| (name.to_string(), combos.clone()))
        })
        .collect();
    events.sort_by(|a, b| a.0.cmp(&b.0));

    if json {
        let actions_json: serde_json::Map<_, _> = actions
            .iter()
            .map(|(a, c)| ((*a).to_string(), serde_json::json!(c)))
            .collect();
        let events_json: serde_json::Map<_, _> = events
            .iter()
            .map(|(n, c)| (n.clone(), serde_json::json!(c)))
            .collect();
        println!(
            "{}",
            serde_json::json!({ "actions": actions_json, "events": events_json })
        );
        return;
    }

    actions.sort_by(|a, b| a.0.cmp(b.0));
    println!("Actions:");
    for (action, combos) in &actions {
        println!("  {action:<24} {}", combos.join(", "));
    }
    if !events.is_empty() {
        println!("\nShell events (shortcut:<name>):");
        for (name, combos) in &events {
            println!("  {name:<24} {}", combos.join(", "));
        }
    }
}

pub fn get(config: &Config, action: &str) -> Result<String> {
    validate_action(action)?;
    Ok(config
        .shortcuts
        .get(action)
        .cloned()
        .unwrap_or_default()
        .join(", "))
}

pub fn set(doc: &mut DocumentMut, action: &str, combos_raw: &str) -> Result<()> {
    validate_action(action)?;
    let combos = split_combos(combos_raw);
    shortcuts_table(doc)[action] = value(combos_array(&combos));
    Ok(())
}

pub fn unset(doc: &mut DocumentMut, action: &str) -> Result<()> {
    validate_action(action)?;
    shortcuts_table(doc).remove(action);
    Ok(())
}

pub fn events_list(config: &Config, json: bool) {
    let mut configured: Vec<(String, Vec<String>)> = config
        .shortcuts
        .iter()
        .filter_map(|(action, combos)| {
            action
                .strip_prefix("shortcut:")
                .map(|name| (name.to_string(), combos.clone()))
        })
        .collect();
    configured.sort_by(|a, b| a.0.cmp(&b.0));

    let suggested: Vec<&str> = ADVERTISED_EVENTS
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !configured.iter().any(|(n, _)| n == name))
        .collect();

    if json {
        let configured_json: serde_json::Map<_, _> = configured
            .iter()
            .map(|(n, c)| (n.clone(), serde_json::json!(c)))
            .collect();
        println!(
            "{}",
            serde_json::json!({ "configured": configured_json, "suggested": suggested })
        );
        return;
    }

    println!("Configured events:");
    for (name, combos) in &configured {
        let label = ADVERTISED_EVENTS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, l)| *l)
            .unwrap_or(name.as_str());
        println!("  {name:<20} {:<32} {}", label, combos.join(", "));
    }
    if !suggested.is_empty() {
        println!("\nSuggested (not yet configured):");
        for name in suggested {
            let label = ADVERTISED_EVENTS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, l)| *l)
                .unwrap_or(name);
            println!("  {name:<20} {label}");
        }
    }
}

pub fn events_add(doc: &mut DocumentMut, name: &str, combos_raw: Option<&str>) {
    let combos = combos_raw.map(split_combos).unwrap_or_default();
    shortcuts_table(doc)[&format!("shortcut:{name}")] = value(combos_array(&combos));
}

pub fn events_remove(doc: &mut DocumentMut, name: &str) {
    // An explicit empty list, not a deleted key: deleting would let the
    // compositor's own built-in default for this name (if any - only
    // "launcher" has one) show back through on next load.
    shortcuts_table(doc)[&format!("shortcut:{name}")] = value(Array::new());
}
