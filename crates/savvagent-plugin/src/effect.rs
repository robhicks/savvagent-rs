//! Effect enum — the closed vocabulary plugins use to request host actions.
//! See spec section "Contribution kinds" and section "Runtime / Effects applier".

use crate::styled::StyledLine;
use crate::types::{ProviderId, ScreenArgs};

/// Closed vocabulary of host operations a plugin can request. Returned from
/// `Plugin::handle_slash`, `Plugin::on_event`, and `Screen::on_key`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Effect {
    /// Append a styled-line note to the conversation log.
    PushNote {
        /// The styled text line to append.
        line: StyledLine,
    },
    /// Push a new screen onto the runtime's screen stack.
    OpenScreen {
        /// Unique identifier for the screen to open.
        id: String,
        /// Arguments forwarded to the screen's constructor.
        args: ScreenArgs,
    },
    /// Pop the current screen from the runtime's screen stack.
    CloseScreen,
    /// Switch the active UI theme.
    SetActiveTheme {
        /// Slug of the theme to activate (e.g. `"dark"` or `"solarized"`).
        slug: String,
        /// Whether to persist the selection across sessions.
        persist: bool,
    },
    /// Switch the active LLM provider.
    SetActiveProvider {
        /// Stable identifier of the provider to activate.
        id: ProviderId,
        /// Whether to persist the selection across sessions.
        persist: bool,
    },
    /// Announce that this plugin has a connected `ProviderClient` ready for
    /// use. The runtime fetches the client via a savvagent-internal seam (not
    /// part of the WIT-portable surface).
    RegisterProvider {
        /// Stable identifier for the provider being registered.
        id: ProviderId,
        /// Human-readable name shown in UI pickers.
        display_name: String,
    },
    /// Serialize the current transcript to disk at the given path.
    SaveTranscript {
        /// Absolute or repo-relative file path for the output.
        path: String,
    },
    /// Submit a message to the active provider as if the user typed it.
    PromptSend {
        /// The text to send.
        text: String,
    },
    /// Invoke a registered slash command by name.
    RunSlash {
        /// Name of the slash command, without the leading `/`.
        name: String,
        /// Positional arguments forwarded to the command handler.
        args: Vec<String>,
    },
    /// Erase all entries from the conversation log display.
    ClearLog,
    /// Shut down the application cleanly.
    Quit,
    /// Compound: apply children in order as a single transaction. Allows
    /// returns like `Stack(vec![SetActiveTheme{..}, CloseScreen])`.
    Stack(Vec<Effect>),
}

/// The right-hand side of a [`crate::types::KeybindingSpec`]: either invoke a
/// slash command or emit a typed effect directly.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum BoundAction {
    /// Invoke a registered slash command when the keybinding fires.
    RunSlash {
        /// Name of the slash command, without the leading `/`.
        name: String,
        /// Positional arguments forwarded to the command handler.
        args: Vec<String>,
    },
    /// Emit the contained [`Effect`] directly when the keybinding fires.
    EmitEffect(
        /// The effect to emit.
        Effect,
    ),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_is_recursive() {
        let outer = Effect::Stack(vec![
            Effect::SetActiveTheme {
                slug: "dark".into(),
                persist: true,
            },
            Effect::CloseScreen,
        ]);
        match outer {
            Effect::Stack(children) => assert_eq!(children.len(), 2),
            _ => panic!(),
        }
    }

    #[test]
    fn bound_action_holds_an_effect() {
        let _ = BoundAction::EmitEffect(Effect::Quit);
        let _ = BoundAction::RunSlash {
            name: "theme".into(),
            args: vec![],
        };
    }
}
