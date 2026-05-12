//! Slash command routing.

use savvagent_plugin::{Effect, PluginError, PluginId};

use crate::plugin::manifests::Indexes;
use crate::plugin::registry::PluginRegistry;

/// Maximum number of nested slash dispatches before the router hard-errors.
/// Prevents infinite loops when an `Effect::RunSlash` emitted by a plugin
/// handler would re-enter the same (or another) slash handler.
const MAX_REENTRANCY_DEPTH: u8 = 4;

/// Routes bare slash command names to the plugin that owns them and dispatches
/// the call, threading a re-entrancy chain to enforce [`MAX_REENTRANCY_DEPTH`].
pub struct SlashRouter<'a> {
    indexes: &'a Indexes,
    registry: &'a PluginRegistry,
}

/// Errors that can occur during slash command dispatch.
#[derive(Debug)]
pub enum SlashError {
    /// No enabled plugin has registered a slash command with this name.
    Unknown(String),
    /// A chain of `Effect::RunSlash` re-entries exceeded [`MAX_REENTRANCY_DEPTH`].
    ReentrancyLimitExceeded {
        /// The ordered sequence of slash names that formed the cycle.
        chain: Vec<String>,
    },
    /// The plugin's own `handle_slash` returned an error.
    Plugin(PluginError),
}

impl std::fmt::Display for SlashError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SlashError::Unknown(name) => write!(f, "unknown slash command: /{name}"),
            SlashError::ReentrancyLimitExceeded { chain } => {
                write!(
                    f,
                    "slash re-entrancy depth exceeded: {}",
                    chain.join(" -> ")
                )
            }
            SlashError::Plugin(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SlashError {}

impl<'a> SlashRouter<'a> {
    /// Construct a router backed by the given index and registry snapshots.
    pub fn new(indexes: &'a Indexes, registry: &'a PluginRegistry) -> Self {
        Self { indexes, registry }
    }

    /// Look up which plugin owns `name` (without the leading `/`).
    /// Returns `None` if no enabled plugin has registered that command.
    pub fn resolve(&self, name: &str) -> Option<&PluginId> {
        self.indexes.slash.get(name)
    }

    /// Dispatch a slash command by name. Locks the owning plugin, calls
    /// `handle_slash`, and returns the emitted effects.
    ///
    /// Returns [`SlashError::Unknown`] if no plugin owns `name`, or
    /// [`SlashError::ReentrancyLimitExceeded`] if the re-entrancy depth
    /// cap is hit before dispatch.
    pub async fn dispatch(&self, name: &str, args: Vec<String>) -> Result<Vec<Effect>, SlashError> {
        self.dispatch_inner(name, args, &mut Vec::new()).await
    }

    async fn dispatch_inner(
        &self,
        name: &str,
        args: Vec<String>,
        chain: &mut Vec<String>,
    ) -> Result<Vec<Effect>, SlashError> {
        if chain.len() >= MAX_REENTRANCY_DEPTH as usize {
            chain.push(name.to_string());
            return Err(SlashError::ReentrancyLimitExceeded {
                chain: chain.clone(),
            });
        }
        chain.push(name.to_string());

        let pid = self
            .resolve(name)
            .ok_or_else(|| SlashError::Unknown(name.to_string()))?
            .clone();
        let handle = self
            .registry
            .get(&pid)
            .ok_or_else(|| SlashError::Unknown(name.to_string()))?;

        let result = {
            let mut plugin = handle.lock().await;
            plugin
                .handle_slash(name, args)
                .await
                .map_err(SlashError::Plugin)?
        };

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use savvagent_plugin::{Contributions, Manifest, Plugin, PluginKind, SlashSpec, StyledLine};

    struct Echo(String);

    #[async_trait]
    impl Plugin for Echo {
        fn manifest(&self) -> Manifest {
            let mut contributions = Contributions::default();
            contributions.slash_commands = vec![SlashSpec {
                name: "echo".into(),
                summary: "".into(),
                args_hint: None,
            }];
            Manifest {
                id: PluginId::new(&self.0).expect("valid test id"),
                name: self.0.clone(),
                version: "0".into(),
                description: "".into(),
                kind: PluginKind::Optional,
                contributions,
            }
        }

        async fn handle_slash(
            &mut self,
            _: &str,
            args: Vec<String>,
        ) -> Result<Vec<Effect>, PluginError> {
            Ok(vec![Effect::PushNote {
                line: StyledLine::plain(args.join(" ")),
            }])
        }
    }

    #[tokio::test]
    async fn dispatch_routes_to_the_plugin() {
        let reg = PluginRegistry::new(vec![Box::new(Echo("test:p".into()))]);
        let idx = Indexes::build(&reg).await.unwrap();
        let r = SlashRouter::new(&idx, &reg);
        let out = r.dispatch("echo", vec!["hi".into()]).await.unwrap();
        assert_eq!(out.len(), 1);
    }

    #[tokio::test]
    async fn unknown_slash_yields_unknown_error() {
        let reg = PluginRegistry::new(vec![Box::new(Echo("test:p".into()))]);
        let idx = Indexes::build(&reg).await.unwrap();
        let r = SlashRouter::new(&idx, &reg);
        let err = r.dispatch("nope", vec![]).await.unwrap_err();
        assert!(matches!(err, SlashError::Unknown(ref n) if n == "nope"));
    }
}
