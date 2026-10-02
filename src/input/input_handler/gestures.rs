use super::*;

#[cfg(feature = "udev")]
impl AnvilState<UdevData> {
    pub(super) fn on_gesture_swipe_begin<B: InputBackend>(&mut self, evt: B::GestureSwipeBeginEvent) {
        let fingers = evt.fingers();
        // A gesture some binding could match is claimed for the compositor
        // from the start (and never forwarded), since which direction it
        // ends up going isn't known until it's over.
        if self.gesture_bindings.iter().any(|(gesture, _)| {
            matches!(gesture, Gesture::Swipe { fingers: f, .. } if *f == fingers)
        }) {
            self.gesture_tracker = Some(GestureTracker::Swipe { fingers, dx: 0.0, dy: 0.0 });
            return;
        }

        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_swipe_begin(
            self,
            &GestureSwipeBeginEvent {
                serial,
                time: evt.time(),
                fingers,
            },
        );
    }

    pub(super) fn on_gesture_swipe_update<B: InputBackend>(&mut self, evt: B::GestureSwipeUpdateEvent) {
        if let Some(GestureTracker::Swipe { dx, dy, .. }) = &mut self.gesture_tracker {
            let delta = evt.delta();
            *dx += delta.x;
            *dy += delta.y;
            return;
        }

        let pointer = self.pointer.clone();
        pointer.gesture_swipe_update(
            self,
            &GestureSwipeUpdateEvent {
                time: evt.time(),
                delta: evt.delta(),
            },
        );
    }

    pub(super) fn on_gesture_swipe_end<B: InputBackend>(&mut self, evt: B::GestureSwipeEndEvent) {
        if let Some(GestureTracker::Swipe { fingers, dx, dy }) = self.gesture_tracker.take() {
            if !evt.cancelled()
                && let Some(direction) = classify_swipe(dx, dy)
            {
                self.fire_gesture(Gesture::Swipe { fingers, direction });
            }
            return;
        }

        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_swipe_end(
            self,
            &GestureSwipeEndEvent {
                serial,
                time: evt.time(),
                cancelled: evt.cancelled(),
            },
        );
    }

    pub(super) fn on_gesture_pinch_begin<B: InputBackend>(&mut self, evt: B::GesturePinchBeginEvent) {
        let fingers = evt.fingers();
        if self.gesture_bindings.iter().any(|(gesture, _)| {
            matches!(gesture, Gesture::Pinch { fingers: f, .. } if *f == fingers)
        }) {
            self.gesture_tracker = Some(GestureTracker::Pinch { fingers, scale: 1.0 });
            return;
        }

        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_pinch_begin(
            self,
            &GesturePinchBeginEvent {
                serial,
                time: evt.time(),
                fingers,
            },
        );
    }

    pub(super) fn on_gesture_pinch_update<B: InputBackend>(&mut self, evt: B::GesturePinchUpdateEvent) {
        if let Some(GestureTracker::Pinch { scale, .. }) = &mut self.gesture_tracker {
            // libinput reports the scale relative to the gesture's start, so
            // the latest value is the whole gesture's scale so far.
            *scale = evt.scale();
            return;
        }

        let pointer = self.pointer.clone();
        pointer.gesture_pinch_update(
            self,
            &GesturePinchUpdateEvent {
                time: evt.time(),
                delta: evt.delta(),
                scale: evt.scale(),
                rotation: evt.rotation(),
            },
        );
    }

    pub(super) fn on_gesture_pinch_end<B: InputBackend>(&mut self, evt: B::GesturePinchEndEvent) {
        if let Some(GestureTracker::Pinch { fingers, scale }) = self.gesture_tracker.take() {
            if !evt.cancelled()
                && let Some(zoom_in) = classify_pinch(scale)
            {
                self.fire_gesture(Gesture::Pinch { fingers, zoom_in });
            }
            return;
        }

        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_pinch_end(
            self,
            &GesturePinchEndEvent {
                serial,
                time: evt.time(),
                cancelled: evt.cancelled(),
            },
        );
    }

    /// Runs the action bound to a recognized touchpad gesture, if any. Only
    /// actions that don't depend on the backend can run here; the rest
    /// (output scale/rotation, VT switching, ...) are logged and skipped.
    pub(super) fn fire_gesture(&mut self, gesture: Gesture) {
        let Some(action) = self
            .gesture_bindings
            .iter()
            .find(|(bound, _)| *bound == gesture)
            .map(|(_, action)| action.clone())
        else {
            return;
        };

        match action {
            KeyAction::VtSwitch(_)
            | KeyAction::Screen(_)
            | KeyAction::ScaleUp
            | KeyAction::ScaleDown
            | KeyAction::RotateOutput
            | KeyAction::ToggleTint => {
                tracing::warn!(?gesture, ?action, "Action can't be triggered by a gesture, ignoring");
            }
            action => self.process_common_key_action(action),
        }
    }

    pub(super) fn on_gesture_hold_begin<B: InputBackend>(&mut self, evt: B::GestureHoldBeginEvent) {
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_hold_begin(
            self,
            &GestureHoldBeginEvent {
                serial,
                time: evt.time(),
                fingers: evt.fingers(),
            },
        );
    }

    pub(super) fn on_gesture_hold_end<B: InputBackend>(&mut self, evt: B::GestureHoldEndEvent) {
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_hold_end(
            self,
            &GestureHoldEndEvent {
                serial,
                time: evt.time(),
                cancelled: evt.cancelled(),
            },
        );
    }
}

/// A touchpad gesture the compositor has claimed because a binding exists
/// for its finger count, accumulating what's needed to classify it once it
/// ends.
#[derive(Debug, Clone, Copy)]
pub(crate) enum GestureTracker {
    /// Total finger travel so far.
    Swipe { fingers: u32, dx: f64, dy: f64 },
    /// Latest scale relative to the start of the pinch (1.0 = unchanged).
    Pinch { fingers: u32, scale: f64 },
}

/// Minimum total travel (libinput's unaccelerated units) for a swipe to count.
const SWIPE_MIN_DISTANCE: f64 = 80.0;
/// A pinch must shrink to at most this scale, or grow to at least its
/// inverse, to count.
const PINCH_MIN_CHANGE: f64 = 0.8;

pub(super) fn classify_swipe(dx: f64, dy: f64) -> Option<SwipeDirection> {
    let (ax, ay) = (dx.abs(), dy.abs());
    if ax.max(ay) < SWIPE_MIN_DISTANCE {
        return None;
    }
    Some(if ax >= ay {
        if dx < 0.0 { SwipeDirection::Left } else { SwipeDirection::Right }
    } else if dy < 0.0 {
        SwipeDirection::Up
    } else {
        SwipeDirection::Down
    })
}

/// `Some(true)` for pinch-in, `Some(false)` for pinch-out.
pub(super) fn classify_pinch(scale: f64) -> Option<bool> {
    if scale <= PINCH_MIN_CHANGE {
        Some(true)
    } else if scale >= 1.0 / PINCH_MIN_CHANGE {
        Some(false)
    } else {
        None
    }
}

/// Resolves every `[gestures]` binding from `config` into the table the
/// gesture handlers consult. Unknown actions and unparseable specs are
/// skipped with a warning; if two actions claim the same gesture the
/// alphabetically-first action wins, so the result is stable across reloads.
pub(crate) fn compile_gesture_bindings(
    config: &crate::config::Config,
) -> Vec<(Gesture, KeyAction)> {
    let known = crate::config::known_actions();
    let mut actions: Vec<&String> = config.gestures.keys().collect();
    actions.sort();

    let mut bindings: Vec<(Gesture, KeyAction)> = Vec::new();
    for name in actions {
        if !known.contains(&name.as_str()) && !crate::config::is_shortcut_action(name) {
            tracing::warn!(action = name.as_str(), "Unknown action in [gestures] config, ignoring");
            continue;
        }
        let Some(action) = action_for_name(
            name,
            &config.terminal,
            &config.browser,
            &config.file_manager,
        ) else {
            continue;
        };

        for spec in &config.gestures[name] {
            match crate::config::parse_gesture(spec) {
                Some(gesture) if bindings.iter().any(|(g, _)| *g == gesture) => {
                    tracing::warn!(spec = spec.as_str(), action = name.as_str(), "Gesture already bound to another action, ignoring");
                }
                Some(gesture) => bindings.push((gesture, action.clone())),
                None => tracing::warn!(spec = spec.as_str(), action = name.as_str(), "Failed to parse gesture, ignoring"),
            }
        }
    }
    bindings
}

#[cfg(test)]
mod gesture_tests {
    use super::*;

    #[test]
    fn swipe_needs_enough_travel_and_picks_dominant_axis() {
        assert_eq!(classify_swipe(10.0, 5.0), None);
        assert_eq!(classify_swipe(-120.0, 30.0), Some(SwipeDirection::Left));
        assert_eq!(classify_swipe(120.0, -30.0), Some(SwipeDirection::Right));
        assert_eq!(classify_swipe(20.0, -150.0), Some(SwipeDirection::Up));
        assert_eq!(classify_swipe(20.0, 150.0), Some(SwipeDirection::Down));
    }

    #[test]
    fn pinch_needs_enough_scale_change() {
        assert_eq!(classify_pinch(0.95), None);
        assert_eq!(classify_pinch(0.7), Some(true));
        assert_eq!(classify_pinch(1.4), Some(false));
    }

    #[test]
    fn default_gestures_compile() {
        let bindings = compile_gesture_bindings(&crate::config::Config::default());
        assert!(bindings.iter().any(|(g, a)| {
            *g == Gesture::Swipe { fingers: 3, direction: SwipeDirection::Left }
                && matches!(a, KeyAction::SwitchWorkspace(1))
        }));
    }
}
