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
};
use tokio::sync::RwLock;

use crate::plugin::builtin::provider_common::BuiltinHookPlugin;
use crate::plugin::builtin::user_hooks::discovery::HooksIndex;
use crate::plugin::builtin::user_hooks::pre_tool_gate::UserHooksPreToolGate;

/// Built-in plugin that exposes user-authored shell hooks.
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
}

impl BuiltinHookPlugin for UserHooksPlugin {
    fn take_pre_tool_gate(&mut self) -> Option<Arc<dyn savvagent_host::PreToolUseGate>> {
        Some(self.gate_arc())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub_plugin() -> UserHooksPlugin {
        UserHooksPlugin::new(
            Arc::new(RwLock::new(HooksIndex::default())),
            String::new(),
            PathBuf::from("."),
            Arc::new(RwLock::new(PathBuf::new())),
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
}
