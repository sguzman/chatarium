//! Small, explicit keyboard-focus rules for the native conversation workspace.
//!
//! The transcript reader is never allowed to consume editing keys intended for
//! a focused text input. Sending remains an intentional composer-only action.

use eframe::egui;

/// Ctrl+L may focus the composer only in a native conversation. It is a
/// navigation action, not authorization to send, alter or persist any draft.
pub const fn may_focus_native_composer(native_conversation_active: bool) -> bool {
    native_conversation_active
}

/// Ctrl+Enter is a submit shortcut only while the message editor is focused.
/// Buttons remain separately actionable even when the editor is not focused.
pub const fn may_submit_composer_shortcut(can_commit: bool, composer_has_focus: bool) -> bool {
    can_commit && composer_has_focus
}

/// Home/End, PageUp/PageDown and reader hit cycling must not steal key events
/// from the editor, search fields or other text inputs.
pub const fn may_consume_reader_navigation(text_input_focused: bool) -> bool {
    !text_input_focused
}

/// Read the modifiers *on the pressed key event*, not the modifier snapshot
/// left over at the end of the frame. A short Ctrl+Tab may be released before
/// egui renders; the pressed event still correctly remembers Ctrl and Shift.
fn pressed_chord_modifiers(
    events: &[egui::Event],
    key: egui::Key,
    require_shift: Option<bool>,
) -> Option<egui::Modifiers> {
    events.iter().find_map(|event| match event {
        egui::Event::Key {
            key: event_key,
            pressed: true,
            repeat: false,
            modifiers,
            ..
        } if *event_key == key
            && modifiers.ctrl
            && !modifiers.alt
            && require_shift.map_or(true, |shift| modifiers.shift == shift) =>
        {
            Some(*modifiers)
        }
        _ => None,
    })
}

/// Consume only explicitly recognized global navigation chords. In particular,
/// ordinary Tab stays available to text fields and keyboard focus traversal.
pub fn consume_native_conversation_shortcuts(
    input: &mut egui::InputState,
) -> (bool, Option<super::conversation_navigation::CycleDirection>) {
    use super::conversation_navigation::CycleDirection;

    // Structural QA evidence only: key names/modifiers, never editor content.
    // This opt-in is used solely inside the isolated Xvfb production fixture.
    let trace = std::env::var("CHATARIUM_QA_SHORTCUT_TRACE").as_deref() == Ok("1");
    if trace {
        for event in &input.events {
            if let egui::Event::Key {
                key,
                pressed,
                repeat,
                modifiers,
                ..
            } = event
            {
                if modifiers.ctrl || *key == egui::Key::Tab {
                    eprintln!(
                        "CHATARIUM_QA_SHORTCUT_EVENT key={key:?} pressed={pressed} repeat={repeat} ctrl={} shift={} alt={} snapshot_ctrl={} snapshot_shift={}",
                        modifiers.ctrl,
                        modifiers.shift,
                        modifiers.alt,
                        input.modifiers.ctrl,
                        input.modifiers.shift
                    );
                }
            }
        }
    }

    let new_chat = pressed_chord_modifiers(&input.events, egui::Key::N, Some(true))
        .is_some_and(|modifiers| input.consume_key(modifiers, egui::Key::N));
    let cycle =
        pressed_chord_modifiers(&input.events, egui::Key::Tab, None).and_then(|modifiers| {
            input
                .consume_key(modifiers, egui::Key::Tab)
                .then_some(if modifiers.shift {
                    CycleDirection::Backward
                } else {
                    CycleDirection::Forward
                })
        });
    if trace && (new_chat || cycle.is_some()) {
        eprintln!("CHATARIUM_QA_SHORTCUT_CONSUMED new_chat={new_chat} cycle={cycle:?}");
    }
    (new_chat, cycle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_pressed(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn released_ctrl_shift_still_cycles_backward_using_event_modifiers() {
        let mut input = egui::InputState::default();
        let mut pressed_modifiers = egui::Modifiers::CTRL;
        pressed_modifiers.shift = true;
        input
            .events
            .push(key_pressed(egui::Key::Tab, pressed_modifiers));
        // Simulate a fast X11 chord released before the frame is rendered.
        assert!(!input.modifiers.ctrl && !input.modifiers.shift);
        assert_eq!(
            consume_native_conversation_shortcuts(&mut input),
            (
                false,
                Some(super::super::conversation_navigation::CycleDirection::Backward)
            )
        );
        assert!(input.events.is_empty());
    }

    #[test]
    fn released_ctrl_still_cycles_forward_using_event_modifiers() {
        let mut input = egui::InputState::default();
        input
            .events
            .push(key_pressed(egui::Key::Tab, egui::Modifiers::CTRL));
        assert_eq!(
            consume_native_conversation_shortcuts(&mut input),
            (
                false,
                Some(super::super::conversation_navigation::CycleDirection::Forward)
            )
        );
    }

    #[test]
    fn released_ctrl_shift_still_creates_new_local_conversation() {
        let mut input = egui::InputState::default();
        let mut mods = egui::Modifiers::CTRL;
        mods.shift = true;
        input.events.push(key_pressed(egui::Key::N, mods));
        assert_eq!(
            consume_native_conversation_shortcuts(&mut input),
            (true, None)
        );
    }

    #[test]
    fn non_shortcut_tab_and_alt_modified_chords_remain_unconsumed() {
        let mut input = egui::InputState::default();
        input
            .events
            .push(key_pressed(egui::Key::Tab, egui::Modifiers::NONE));
        assert_eq!(
            consume_native_conversation_shortcuts(&mut input),
            (false, None)
        );
        assert_eq!(input.events.len(), 1);

        let mut alt_ctrl = egui::Modifiers::CTRL;
        alt_ctrl.alt = true;
        input.events.push(key_pressed(egui::Key::Tab, alt_ctrl));
        assert_eq!(
            consume_native_conversation_shortcuts(&mut input),
            (false, None)
        );
        assert_eq!(input.events.len(), 2);
    }

    #[test]
    fn focus_composer_is_native_only_and_does_not_depend_on_editor_focus() {
        assert!(may_focus_native_composer(true));
        assert!(!may_focus_native_composer(false));
    }

    #[test]
    fn ctrl_enter_is_scoped_to_an_enabled_focused_composer() {
        assert!(may_submit_composer_shortcut(true, true));
        assert!(!may_submit_composer_shortcut(true, false));
        assert!(!may_submit_composer_shortcut(false, true));
        assert!(!may_submit_composer_shortcut(false, false));
    }

    #[test]
    fn text_inputs_own_home_end_page_and_navigation_key_events() {
        assert!(!may_consume_reader_navigation(true));
        assert!(may_consume_reader_navigation(false));
    }
}
