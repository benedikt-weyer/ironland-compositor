//! Keybinding parsing and output-placement math built on top of the plain
//! data types in [`ironland_config`] (re-exported as [`crate::config`]).
//! Split out from that crate because both need a smithay type (a keysym, or
//! logical-space geometry) that `ironland-config` deliberately doesn't
//! depend on, so it can stay a lightweight build for `ironlandctl` and the
//! settings GUI.

use ironland_config::{Config, KeyboardSettings, OutputPosition, OutputSettings, is_shortcut_action, known_actions};
use smithay::{
    input::keyboard::{Keysym, ModifiersState, XkbConfig, xkb},
    utils::{Logical, Point, Rectangle, Size},
};
use tracing::warn;

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

/// Builds the [`XkbConfig`] xkbcommon expects from a [`KeyboardSettings`].
pub fn to_xkb_config(settings: &KeyboardSettings) -> XkbConfig<'_> {
    XkbConfig {
        rules: &settings.rules,
        model: &settings.model,
        layout: &settings.layout,
        variant: &settings.variant,
        options: if settings.options.is_empty() {
            None
        } else {
            Some(settings.options.clone())
        },
    }
}

/// Parses every configured binding into `(modifiers, keysym, action name)`
/// triples, skipping (with a warning) any binding that doesn't parse or
/// whose action name isn't recognized.
pub fn parsed_keybindings(config: &Config) -> Vec<Keybinding> {
    let known = known_actions();
    let mut bindings = Vec::new();

    for (action, specs) in &config.shortcuts {
        if !known.contains(&action.as_str()) && !is_shortcut_action(action) {
            warn!(action, "Unknown action in [shortcuts] config, ignoring");
            continue;
        }

        for spec in specs {
            // A bare modifier name (no `+`, e.g. `"super"`) isn't a
            // modifiers+key combo at all - it's handled separately by
            // `super_tap_action`, so skip it here rather than reporting it
            // as an unparseable combo.
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
/// configured. Only one action can meaningfully fire on a Super tap, so if
/// more than one action lists a bare `"super"` spec, the first found (in
/// arbitrary map order) wins and the rest are ignored with a warning.
pub fn super_tap_action(config: &Config) -> Option<&str> {
    let known = known_actions();
    let mut found: Option<&str> = None;

    for (action, specs) in &config.shortcuts {
        if !specs.iter().any(|spec| is_bare_modifier_tap(spec)) {
            continue;
        }
        if !known.contains(&action.as_str()) && !is_shortcut_action(action) {
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
pub fn parse_binding(spec: &str) -> Option<(KeyModifiers, Keysym)> {
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
    use ironland_config::default_shortcuts;

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
    fn default_shell_launcher_is_a_bare_super_tap() {
        assert_eq!(super_tap_action(&Config::default()), Some("shortcut:launcher"));
    }

    fn size(w: i32, h: i32) -> Size<i32, Logical> {
        (w, h).into()
    }

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
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
