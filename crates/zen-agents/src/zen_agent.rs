use std::fs::read_to_string;

use anyhow::Result;
use futures::stream::StreamExt;
use rig_compose::agent::{Agent, GenericAgent};
use rig_compose::context::{Evidence, InvestigationContext, Signal};
use rig_core::completion::CompletionModel;
use rig_core::streaming::StreamedAssistantContent;
use serde_json::json;
use zen_core::paths::ZenPaths;
use zen_core::types::SessionContext;
use zen_provider::DefaultRouter;

use crate::completion_model::ZenCompletionModel;
pub use crate::wiring::ZenWiring;

/// Context loaded from identity files: SOUL.md, AGENTS.md, MEMORY.md.
#[derive(Debug, Clone, Default)]
pub struct IdentityContext {
    pub soul_content: String,
    pub agents_content: String,
    pub memory_content: String,
}

/// Load identity files from the Zen home directory (~/.zen/).
///
/// Each file is optional — missing or unreadable files yield empty strings
/// with a warning logged.
pub fn load_identity_files(zen_paths: &ZenPaths) -> IdentityContext {
    let root = zen_paths.global_root();

    let soul_path = root.join("SOUL.md");
    let soul_content = match read_to_string(&soul_path) {
        Ok(content) => content,
        Err(e) => {
            tracing::warn!(path = ?soul_path, error = %e, "SOUL.md not found or unreadable");
            String::new()
        }
    };

    let agents_path = root.join("AGENTS.md");
    let agents_content = match read_to_string(&agents_path) {
        Ok(content) => content,
        Err(e) => {
            tracing::warn!(path = ?agents_path, error = %e, "AGENTS.md not found or unreadable");
            String::new()
        }
    };

    let memory_path = root.join("MEMORY.md");
    let memory_content = match read_to_string(&memory_path) {
        Ok(content) => content,
        Err(e) => {
            tracing::warn!(path = ?memory_path, error = %e, "MEMORY.md not found or unreadable");
            String::new()
        }
    };

    IdentityContext {
        soul_content,
        agents_content,
        memory_content,
    }
}

/// A Zen-tailored agent combining rig_compose's skill-driver [`GenericAgent`]
/// with a [`ZenCompletionModel`] for direct LLM routing.
pub struct ZenAgent {
    pub generic: GenericAgent,
    pub completion_model: ZenCompletionModel,
    identity: Option<IdentityContext>,
    memvid_store: Option<rig_memvid::MemvidStore>,
}

impl ZenAgent {
    /// Create a [`ZenAgentBuilder`] for this agent name.
    pub fn builder(name: &str) -> ZenAgentBuilder {
        ZenAgentBuilder::new(name)
    }

    /// Access the agent's identity context (SOUL.md/MEMORY.md/AGENTS.md).
    pub fn identity(&self) -> &Option<IdentityContext> {
        &self.identity
    }

    /// Execute a user query. Async-native, no nested runtime creation.
    pub async fn execute(&self, query: &str, session: &mut SessionContext) -> Result<String> {
        let mut ctx = InvestigationContext::new("zen-session", "query");

        ctx.evidence
            .push(Evidence::new("user-input", "query").with_detail(json!({ "text": query })));

        if let Some(ref identity) = self.identity {
            ctx.evidence.push(
                Evidence::new("identity", "soul")
                    .with_detail(json!({ "content": identity.soul_content })),
            );
            ctx.evidence.push(
                Evidence::new("identity", "agents")
                    .with_detail(json!({ "content": identity.agents_content })),
            );
            ctx.evidence.push(
                Evidence::new("identity", "memory")
                    .with_detail(json!({ "content": identity.memory_content })),
            );
        }

        ctx.signals.push(Signal::new("knowledge-query"));

        let step_result = self.generic.step(&mut ctx).await?;

        tracing::info!(
            skills_run = ?step_result.skills_run,
            confidence = step_result.confidence,
            concluded = step_result.concluded,
            "ZenAgent::execute: skills completed"
        );

        let response = self.call_llm(query, &ctx).await?;

        tracing::info!(
            response_len = response.len(),
            "ZenAgent::execute: LLM response received"
        );

        session.add_turn("user", query);
        session.add_turn("assistant", &response);

        // Record to memvid store if available
        if let Some(ref store) = self.memvid_store {
            let _ = store.put_text(
                &format!("User: {}\nAssistant: {}", query, response),
                rig_memvid::memvid_core::PutOptions::default(),
            );
        }

        Ok(response)
    }

    async fn call_llm(&self, query: &str, ctx: &InvestigationContext) -> Result<String> {
        use rig_core::OneOrMany;
        use rig_core::completion::CompletionRequest;
        use rig_core::message::Message;

        let mut prompt = query.to_string();

        for ev in &ctx.evidence {
            if let Some(text) = ev.detail.get("text").and_then(|v| v.as_str())
                && !text.is_empty()
            {
                prompt.push_str(&format!("\n\n{}: {text}", ev.label));
            }
            if let Some(content) = ev.detail.get("content").and_then(|v| v.as_str())
                && !content.is_empty()
            {
                prompt.push_str(&format!("\n\n[{}] {content}", ev.source_skill));
            }
        }

        let request = CompletionRequest {
            model: None,
            preamble: Some("You are a helpful Zen assistant. Answer concisely.".to_string()),
            chat_history: OneOrMany::one(Message::user(&prompt)),
            documents: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            max_tokens: Some(2048),
            tool_choice: None,
            additional_params: None,
            output_schema: None,
        };

        let response = self.completion_model.completion(request).await?;

        match response.choice.first() {
            rig_core::completion::AssistantContent::Text(t) => Ok(t.text.clone()),
            other => Ok(format!("{other:?}")),
        }
    }

    pub async fn execute_stream(
        &self,
        query: &str,
        session: &mut SessionContext,
        on_token: impl FnMut(&str),
    ) -> Result<String> {
        let mut ctx = InvestigationContext::new("zen-session", "query");

        ctx.evidence
            .push(Evidence::new("user-input", "query").with_detail(json!({ "text": query })));

        if let Some(ref identity) = self.identity {
            ctx.evidence.push(
                Evidence::new("identity", "soul")
                    .with_detail(json!({ "content": identity.soul_content })),
            );
            ctx.evidence.push(
                Evidence::new("identity", "agents")
                    .with_detail(json!({ "content": identity.agents_content })),
            );
            ctx.evidence.push(
                Evidence::new("identity", "memory")
                    .with_detail(json!({ "content": identity.memory_content })),
            );
        }

        ctx.signals.push(Signal::new("knowledge-query"));

        let _step_result = self.generic.step(&mut ctx).await?;

        let response = self.call_llm_stream(query, &ctx, on_token).await?;

        session.add_turn("user", query);
        session.add_turn("assistant", &response);

        if let Some(ref store) = self.memvid_store {
            let _ = store.put_text(
                &format!("User: {}\nAssistant: {}", query, response),
                rig_memvid::memvid_core::PutOptions::default(),
            );
        }

        Ok(response)
    }

    async fn call_llm_stream(
        &self,
        query: &str,
        ctx: &InvestigationContext,
        mut on_token: impl FnMut(&str),
    ) -> Result<String> {
        use rig_core::OneOrMany;
        use rig_core::completion::{CompletionModel, CompletionRequest};
        use rig_core::message::Message;

        let mut prompt = query.to_string();

        for ev in &ctx.evidence {
            if let Some(text) = ev.detail.get("text").and_then(|v| v.as_str())
                && !text.is_empty()
            {
                prompt.push_str(&format!("\n\n{}: {text}", ev.label));
            }
            if let Some(content) = ev.detail.get("content").and_then(|v| v.as_str())
                && !content.is_empty()
            {
                prompt.push_str(&format!("\n\n[{}] {content}", ev.source_skill));
            }
        }

        let request = CompletionRequest {
            model: None,
            preamble: Some("You are a helpful Zen assistant. Answer concisely.".to_string()),
            chat_history: OneOrMany::one(Message::user(&prompt)),
            documents: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            max_tokens: Some(2048),
            tool_choice: None,
            additional_params: None,
            output_schema: None,
        };

        let mut stream = self.completion_model.stream(request).await?;
        let mut full_response = String::new();

        while let Some(item) = stream.next().await {
            match item {
                Ok(StreamedAssistantContent::Text(text)) => {
                    let token = text.text.clone();
                    full_response.push_str(&token);
                    on_token(&token);
                }
                Ok(StreamedAssistantContent::Final(_)) => break,
                Ok(_) => {}
                Err(e) => {
                    return Err(anyhow::anyhow!("streaming error: {}", e));
                }
            }
        }

        Ok(full_response)
    }
}

/// Builder for [`ZenAgent`].
pub struct ZenAgentBuilder {
    name: String,
    skill_ids: Vec<String>,
    tool_ids: Vec<String>,
    zen_paths: Option<ZenPaths>,
    memvid_store: Option<rig_memvid::MemvidStore>,
}

impl ZenAgentBuilder {
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            skill_ids: Vec::new(),
            tool_ids: Vec::new(),
            zen_paths: None,
            memvid_store: None,
        }
    }

    pub fn with_skill(mut self, id: impl Into<String>) -> Self {
        self.skill_ids.push(id.into());
        self
    }

    pub fn with_tool(mut self, id: impl Into<String>) -> Self {
        self.tool_ids.push(id.into());
        self
    }

    pub fn with_paths(mut self, paths: ZenPaths) -> Self {
        self.zen_paths = Some(paths);
        self
    }

    pub fn with_memvid_store(mut self, store: rig_memvid::MemvidStore) -> Self {
        self.memvid_store = Some(store);
        self
    }

    pub fn build(self, wiring: &ZenWiring, router: &DefaultRouter) -> Result<ZenAgent> {
        let completion_model =
            ZenCompletionModel::new(router.clone(), router.default_provider_name());

        let generic = GenericAgent::builder(&self.name)
            .with_skills(self.skill_ids.clone())
            .with_tools(self.tool_ids.clone())
            .build(&wiring.skills, &wiring.tools)?;

        let identity = self.zen_paths.as_ref().map(load_identity_files);

        Ok(ZenAgent {
            generic,
            completion_model,
            identity,
            memvid_store: self.memvid_store,
        })
    }
}
