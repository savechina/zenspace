use std::collections::HashMap;

use zen_core::types::Sensitivity;
use zen_core::types::SessionContext;

#[derive(Clone)]
pub struct AgentContext {
    pub agent_profile: crate::AgentProfile,
    pub user_query: String,
    pub session: SessionContext,
    pub preferences: Vec<zen_core::config::LlmPreference>,
    pub max_tokens: usize,
    pub sensitivity: Sensitivity,
    pub metadata: HashMap<String, serde_json::Value>,
}

impl AgentContext {
    pub fn new(
        agent_profile: crate::AgentProfile,
        user_query: String,
        session: SessionContext,
    ) -> Self {
        let sensitivity = session.sensitivity_policy;
        let max_tokens = session.max_tokens;

        Self {
            agent_profile,
            user_query,
            session,
            preferences: Vec::new(),
            max_tokens,
            sensitivity,
            metadata: HashMap::new(),
        }
    }

    #[must_use]
    pub fn with_preferences(mut self, preferences: Vec<zen_core::config::LlmPreference>) -> Self {
        self.preferences = preferences;
        self
    }

    #[must_use]
    pub fn with_max_tokens(mut self, max_tokens: usize) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    #[must_use]
    pub fn with_sensitivity(mut self, sensitivity: Sensitivity) -> Self {
        self.sensitivity = sensitivity;
        self
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    #[must_use]
    pub fn get_metadata(&self, key: &str) -> Option<&serde_json::Value> {
        self.metadata.get(key)
    }
}
