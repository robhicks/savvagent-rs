//! `internal:user-hooks` — discovers and dispatches Claude-Code-compatible
//! user shell hooks from `settings.json`. See
//! `docs/superpowers/specs/2026-05-22-user-hooks-design.md`.

mod config;
mod decision;
pub mod discovery;
mod matcher;
mod payload;
pub mod pre_tool_gate;
mod runner;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, SlashSpec,
    StyledLine,
};
use serde_json::json;
use tokio::sync::RwLock;

use crate::plugin::builtin::provider_common::BuiltinHookPlugin;
use crate::plugin::builtin::user_hooks::decision::HookDecision;
use crate::plugin::builtin::user_hooks::discovery::{HookEvent, HooksIndex};
use crate::plugin::builtin::user_hooks::payload::HookContext;
use crate::plugin::builtin::user_hooks::pre_tool_gate::UserHooksPreToolGate;

/// Built-in plugin that exposes user-authored shell hooks.
///
/// # v1 limitations on `PostToolUse`
///
/// `HostEvent::ToolCallEnd` currently carries only `{ call_id, success }`
/// — there is no `tool_name`, `tool_input`, or `tool_response` payload.
/// As a result:
///
/// * `PostToolUse` hooks only fire when their `matcher` matches `"*"`
///   (tool-specific matchers are skipped — they have nothing to match
///   against).
/// * The stdin payload uses sentinel values: `tool_name = "<unknown>"`,
///   `tool_input = {}`, and `tool_response = { "success": <bool> }`.
///
/// Lifting these requires enriching `HostEvent::ToolCallEnd` with the
/// tool's name and IO buffers; tracked as a follow-up.
pub struct UserHooksPlugin {
    pub hooks: Arc<RwLock<HooksIndex>>,
    pub session_id: String,
    pub project_root: PathBuf,
    pub transcript_path: Arc<RwLock<PathBuf>>,
    cached_gate: Option<Arc<UserHooksPreToolGate>>,
}

impl UserHooksPlugin {
    /// Construct a new [`UserHooksPlugin`].
    pub fn new(
        hooks: Arc<RwLock<HooksIndex>>,
        session_id: String,
        project_root: PathBuf,
        transcript_path: Arc<RwLock<PathBuf>>,
    ) -> Self {
        Self {
            hooks,
            session_id,
            project_root,
            transcript_path,
            cached_gate: None,
        }
    }

    fn gate_arc(&mut self) -> Arc<UserHooksPreToolGate> {
        if let Some(g) = self.cached_gate.as_ref() {
            return g.clone();
        }
        let g = Arc::new(UserHooksPreToolGate {
            hooks: self.hooks.clone(),
            session_id: self.session_id.clone(),
            project_root: self.project_root.clone(),
            transcript_path: self.transcript_path.clone(),
        });
        self.cached_gate = Some(g.clone());
        g
    }

    /// Dispatch `PostToolUse` hooks. See struct-level docs for the v1
    /// limitations (only `"*"`-matching groups run; sentinel tool
    /// payload).
    async fn dispatch_post_tool_use(
        &mut self,
        success: bool,
    ) -> Result<Vec<Effect>, PluginError> {
        let idx = self.hooks.read().await;
        let Some(groups) = idx.by_event.get(&HookEvent::PostToolUse) else {
            return Ok(vec![]);
        };
        let groups = groups.clone();
        drop(idx);

        let transcript = self.transcript_path.read().await.clone();
        let ctx = HookContext {
            session_id: &self.session_id,
            transcript_path: &transcript,
            cwd: &self.project_root,
        };
        let payload =
            payload::post_tool_use(&ctx, "<unknown>", &json!({}), &json!({ "success": success }));

        let mut effects: Vec<Effect> = Vec::new();
        for group in &groups {
            // v1: tool name/IO aren't in `ToolCallEnd`, so we can only
            // dispatch hooks whose matcher catches `"*"`. Tool-specific
            // matchers are skipped pending a richer payload.
            if !group.matcher.is_match("*") {
                continue;
            }
            for cmd in &group.commands {
                let (decision, warnings, stdout, stderr) = runner::run_one(
                    HookEvent::PostToolUse,
                    &cmd.command,
                    cmd.timeout,
                    &payload,
                    &self.project_root,
                )
                .await;
                for w in &warnings {
                    effects.push(Effect::PushNote {
                        line: StyledLine::plain(format!("[warn] {w}")),
                    });
                }
                match decision {
                    HookDecision::Continue {
                        suppress_output, ..
                    } => {
                        if !suppress_output {
                            let so = stdout.trim_end();
                            if !so.is_empty() {
                                effects.push(Effect::PushNote {
                                    line: StyledLine::plain(so.to_string()),
                                });
                            }
                            let se = stderr.trim_end();
                            if !se.is_empty() {
                                effects.push(Effect::PushNote {
                                    line: StyledLine::plain(se.to_string()),
                                });
                            }
                        }
                    }
                    HookDecision::Block { reason, .. } => {
                        // PostToolUse cannot block per spec. Demote
                        // to a warning note and continue the chain.
                        effects.push(Effect::PushNote {
                            line: StyledLine::plain(format!(
                                "[warn] PostToolUse hooks cannot block; ignoring Block from `{}`: {reason}",
                                cmd.command
                            )),
                        });
                    }
                }
            }
        }
        Ok(effects)
    }

    /// Dispatch `SessionStart` hooks. Source is hardcoded to `"startup"`
    /// in v1 (we don't distinguish resume/clear). All groups run — the
    /// matcher field is ignored for non-tool events.
    async fn dispatch_session_start(&mut self) -> Result<Vec<Effect>, PluginError> {
        let idx = self.hooks.read().await;
        let Some(groups) = idx.by_event.get(&HookEvent::SessionStart) else {
            return Ok(vec![]);
        };
        let groups = groups.clone();
        drop(idx);

        let transcript = self.transcript_path.read().await.clone();
        let ctx = HookContext {
            session_id: &self.session_id,
            transcript_path: &transcript,
            cwd: &self.project_root,
        };
        let payload = payload::session_start(&ctx, "startup");

        let mut effects: Vec<Effect> = Vec::new();
        for group in &groups {
            // SessionStart is not a tool event; matcher is ignored.
            for cmd in &group.commands {
                let (decision, warnings, stdout, stderr) = runner::run_one(
                    HookEvent::SessionStart,
                    &cmd.command,
                    cmd.timeout,
                    &payload,
                    &self.project_root,
                )
                .await;
                for w in &warnings {
                    effects.push(Effect::PushNote {
                        line: StyledLine::plain(format!("[warn] {w}")),
                    });
                }
                match decision {
                    HookDecision::Continue {
                        suppress_output, ..
                    } => {
                        if !suppress_output {
                            let so = stdout.trim_end();
                            if !so.is_empty() {
                                effects.push(Effect::PushNote {
                                    line: StyledLine::plain(so.to_string()),
                                });
                            }
                            let se = stderr.trim_end();
                            if !se.is_empty() {
                                effects.push(Effect::PushNote {
                                    line: StyledLine::plain(se.to_string()),
                                });
                            }
                        }
                    }
                    HookDecision::Block { reason, .. } => {
                        // SessionStart cannot block startup. Demote to
                        // a warning note and continue.
                        effects.push(Effect::PushNote {
                            line: StyledLine::plain(format!(
                                "[warn] SessionStart hooks cannot block; ignoring Block from `{}`: {reason}",
                                cmd.command
                            )),
                        });
                    }
                }
            }
        }
        Ok(effects)
    }

    /// Placeholder for Task 18 — `UserPromptSubmit` dispatch lands there.
    async fn dispatch_user_prompt_submit(
        &mut self,
        _prompt: &str,
    ) -> Result<Vec<Effect>, PluginError> {
        unimplemented!("filled in by Task 18")
    }

    /// Placeholder for Task 18 — `Stop` dispatch lands there.
    async fn dispatch_stop(&mut self, _success: bool) -> Result<Vec<Effect>, PluginError> {
        unimplemented!("filled in by Task 18")
    }
}

#[async_trait]
impl Plugin for UserHooksPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "reload-hooks".into(),
            summary: "Rescan user-defined hooks (settings.json)".into(),
            args_hint: None,
            requires_arg: false,
        }];
        contributions.hooks = vec![
            savvagent_plugin::HookKind::ToolCallEnd, // -> PostToolUse
            savvagent_plugin::HookKind::HostStarting, // -> SessionStart
            savvagent_plugin::HookKind::PromptSubmitted, // -> UserPromptSubmit (Task 18)
            savvagent_plugin::HookKind::TurnEnd,     // -> Stop (Task 18)
        ];
        Manifest {
            id: PluginId::new("internal:user-hooks").expect("valid built-in id"),
            name: "User hooks".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "Claude-Code-compatible settings.json hooks".into(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    async fn handle_slash(
        &mut self,
        _name: &str,
        _args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        Ok(vec![])
    }

    async fn on_event(
        &mut self,
        event: savvagent_plugin::HostEvent,
    ) -> Result<Vec<Effect>, PluginError> {
        use savvagent_plugin::HostEvent;
        match event {
            HostEvent::ToolCallEnd { success, .. } => self.dispatch_post_tool_use(success).await,
            HostEvent::HostStarting => self.dispatch_session_start().await,
            HostEvent::PromptSubmitted { text } => self.dispatch_user_prompt_submit(&text).await,
            HostEvent::TurnEnd { success, .. } => self.dispatch_stop(success).await,
            _ => Ok(vec![]),
        }
    }
}

impl BuiltinHookPlugin for UserHooksPlugin {
    fn take_pre_tool_gate(&mut self) -> Option<Arc<dyn savvagent_host::PreToolUseGate>> {
        Some(self.gate_arc())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::{HookKind, HostEvent};

    fn stub_plugin() -> UserHooksPlugin {
        UserHooksPlugin::new(
            Arc::new(RwLock::new(HooksIndex::default())),
            String::new(),
            PathBuf::from("."),
            Arc::new(RwLock::new(PathBuf::new())),
        )
    }

    fn mk_plugin(idx: HooksIndex) -> UserHooksPlugin {
        UserHooksPlugin::new(
            Arc::new(RwLock::new(idx)),
            "sid".into(),
            PathBuf::from("/tmp"),
            Arc::new(RwLock::new(PathBuf::from("/t.json"))),
        )
    }

    #[test]
    fn manifest_has_reload_hooks() {
        let p = stub_plugin();
        let m = p.manifest();
        assert_eq!(m.id.as_str(), "internal:user-hooks");
        let names: Vec<_> = m
            .contributions
            .slash_commands
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert!(names.contains(&"reload-hooks"));
    }

    #[tokio::test]
    async fn no_hooks_means_no_effects() {
        let mut p = mk_plugin(HooksIndex::default());
        let effs = p.on_event(HostEvent::HostStarting).await.unwrap();
        assert!(effs.is_empty());
    }

    #[tokio::test]
    async fn ignores_unrelated_events() {
        let mut p = mk_plugin(HooksIndex::default());
        let effs = p
            .on_event(HostEvent::TurnStart { turn_id: 1 })
            .await
            .unwrap();
        assert!(effs.is_empty());
    }

    #[test]
    fn manifest_subscribes_to_four_kinds() {
        let p = stub_plugin();
        let m = p.manifest();
        let mut kinds = m.contributions.hooks.clone();
        kinds.sort_by_key(|k| format!("{k:?}"));
        let mut expected = vec![
            HookKind::ToolCallEnd,
            HookKind::HostStarting,
            HookKind::PromptSubmitted,
            HookKind::TurnEnd,
        ];
        expected.sort_by_key(|k| format!("{k:?}"));
        assert_eq!(kinds, expected);
    }
}
