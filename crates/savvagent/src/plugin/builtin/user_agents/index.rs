//! `AgentIndex` — async-friendly shared map from agent name to spec.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::plugin::builtin::user_agents::spec::AgentSpec;

#[derive(Clone, Default)]
pub struct AgentIndex {
    inner: Arc<RwLock<HashMap<String, Arc<AgentSpec>>>>,
}

impl AgentIndex {
    pub fn empty() -> Self {
        Self::default()
    }

    pub async fn replace(&self, agents: Vec<AgentSpec>) {
        let map: HashMap<String, Arc<AgentSpec>> = agents
            .into_iter()
            .map(|spec| (spec.name.clone(), Arc::new(spec)))
            .collect();
        *self.inner.write().await = map;
    }

    #[allow(dead_code)] // Consumed by TaskToolHandler in Task 20.
    pub async fn get(&self, name: &str) -> Option<Arc<AgentSpec>> {
        self.inner.read().await.get(name).cloned()
    }

    pub async fn len(&self) -> usize {
        self.inner.read().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.inner.read().await.is_empty()
    }

    pub async fn names_snapshot(&self) -> Vec<String> {
        let mut names: Vec<String> = self.inner.read().await.keys().cloned().collect();
        names.sort();
        names
    }
}
