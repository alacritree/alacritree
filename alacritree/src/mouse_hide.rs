//! Whether the mouse pointer is hidden because the user is typing, behind
//! alacritty's `[mouse] hide_when_typing`. Alacritty hides the pointer on
//! every key it processes and shows it again on motion, a click or a wheel
//! tick; the same two rules live here, read off one frame of egui events
//! rather than off winit callbacks.

use egui::Event;

/// Hidden state, carried across frames. `Default` is visible, which is where
/// a window starts and where the option being off leaves it.
#[derive(Debug, Default)]
pub(crate) struct MouseHide {
    hidden: bool,
}

impl MouseHide {
    /// Fold one frame's events in. They are read in order, so a frame
    /// carrying both a keystroke and a pointer move ends wherever the later of
    /// the two put it.
    ///
    /// `enabled` is passed per call rather than stored, so turning the option
    /// off reveals the pointer on the next frame with nothing else to reset.
    pub(crate) fn observe(&mut self, enabled: bool, events: &[Event]) {
        if !enabled {
            self.hidden = false;
            return;
        }
        for event in events {
            match event {
                // Releases are left alone: holding a key down and letting go
                // of it is still typing, and alacritty only acts on presses.
                Event::Key { pressed: true, .. } | Event::Text(_) => self.hidden = true,
                Event::PointerMoved(_)
                | Event::MouseMoved(_)
                | Event::PointerButton { .. }
                | Event::MouseWheel { .. }
                | Event::Touch { .. } => self.hidden = false,
                _ => {},
            }
        }
    }

    pub(crate) fn hidden(&self) -> bool {
        self.hidden
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(pressed: bool) -> Event {
        Event::Key {
            key: egui::Key::A,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        }
    }

    fn click() -> Event {
        Event::PointerButton {
            pos: egui::Pos2::ZERO,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        }
    }

    #[test]
    fn typing_hides_the_pointer() {
        let mut hide = MouseHide::default();
        hide.observe(true, &[key(true)]);
        assert!(hide.hidden());
    }

    #[test]
    fn a_quiet_frame_leaves_the_pointer_where_it_was() {
        let mut hide = MouseHide::default();
        hide.observe(true, &[key(true)]);
        hide.observe(true, &[]);
        assert!(hide.hidden());
    }

    #[test]
    fn moving_the_pointer_brings_it_back() {
        let mut hide = MouseHide::default();
        hide.observe(true, &[key(true)]);
        hide.observe(true, &[Event::PointerMoved(egui::Pos2::ZERO)]);
        assert!(!hide.hidden());
    }

    #[test]
    fn a_click_brings_it_back() {
        let mut hide = MouseHide::default();
        hide.observe(true, &[key(true)]);
        hide.observe(true, &[click()]);
        assert!(!hide.hidden());
    }

    #[test]
    fn a_wheel_tick_brings_it_back() {
        let mut hide = MouseHide::default();
        hide.observe(true, &[key(true)]);
        hide.observe(true, &[Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::Vec2::new(0.0, 1.0),
            modifiers: egui::Modifiers::default(),
        }]);
        assert!(!hide.hidden());
    }

    #[test]
    fn the_last_event_of_a_mixed_frame_decides() {
        let mut hide = MouseHide::default();
        hide.observe(true, &[key(true), Event::PointerMoved(egui::Pos2::ZERO)]);
        assert!(!hide.hidden());

        let mut hide = MouseHide::default();
        hide.observe(true, &[Event::PointerMoved(egui::Pos2::ZERO), key(true)]);
        assert!(hide.hidden());
    }

    #[test]
    fn a_key_release_on_its_own_does_not_hide() {
        let mut hide = MouseHide::default();
        hide.observe(true, &[key(false)]);
        assert!(!hide.hidden());
    }

    #[test]
    fn the_option_being_off_keeps_the_pointer_visible() {
        let mut hide = MouseHide::default();
        hide.observe(false, &[key(true)]);
        assert!(!hide.hidden());
    }

    #[test]
    fn turning_the_option_off_reveals_a_hidden_pointer() {
        let mut hide = MouseHide::default();
        hide.observe(true, &[key(true)]);
        hide.observe(false, &[]);
        assert!(!hide.hidden());
    }
}
