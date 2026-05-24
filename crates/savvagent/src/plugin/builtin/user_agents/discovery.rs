//! Four-path discovery. Implemented in Task 18.

use std::path::Path;

use crate::plugin::builtin::user_agents::spec::AgentSpec;

pub fn discover(_project_root: &Path, _home: &Path) -> Vec<AgentSpec> {
    Vec::new()
}
