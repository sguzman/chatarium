//! Small, explicit keyboard-focus rules for the native conversation workspace.
//!
//! The transcript reader is never allowed to consume editing keys intended for
//! a focused text input. Sending remains an intentional composer-only action.

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

#[cfg(test)]
mod tests {
    use super::*;

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
