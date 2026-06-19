use std::fs::read_to_string;

use anyhow::Result;
use futures::stream::StreamExt;
use rig_compose::agent::{Agent, GenericAgent};
use rig_compose::context::{Evidence, InvestigationContext, Signal};
use rig_compose::ContextPackConfig;
use rig_core::completion::CompletionModel;
use rig_core::streaming::StreamedAssistantContent;
use rig_memvid::{CardSelection, MemoryCardContext};
use serde_json::json;
use tracing::instrument;
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
    let identity_dir = zen_paths.identity();

    let soul_path = identity_dir.join("SOUL.md");
    let soul_content = match read_to_string(&soul_path) {
        Ok(content) => content,
        Err(e) => {
            tracing::warn!(path = ?soul_path, error = %e, "SOUL.md not found or unreadable");
            String::new()
        }
    };

    let agents_content = {
        let agents_path = identity_dir.join("AGENTS.md");
        match read_to_string(&agents_path) {
            Ok(content) => content,
            Err(_) => {
                if let Some(ws) = zen_paths.workspace_root() {
                    let ws_agents = ws.join("AGENTS.md");
                    match read_to_string(&ws_agents) {
                        Ok(content) => content,
                        Err(e) => {
                            tracing::warn!(path = ?ws_agents, error = %e, "AGENTS.md not found in workspace root either");
                            String::new()
                        }
                    }
                } else {
                    tracing::warn!(path = ?agents_path, "AGENTS.md not found (no workspace root detected)");
                    String::new()
                }
            }
        }
    };

    let memory_path = identity_dir.join("MEMORY.md");
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

    /// Retrieve memories from the memvid store for this session.
    ///
    /// Uses per-session scoping (D7): the session_id from SessionContext
    /// isolates each conversation's memory namespace.
    #[instrument(skip(self), fields(session_id))]
    fn retrieve_memories(&self, session_id: &str) -> Option<Vec<String>> {
        self.memvid_store.as_ref().and_then(|store| {
            let mut all_cards = Vec::new();

            if let Ok(session_cards) = store.entity_memories(session_id) {
                all_cards.extend(session_cards);
            }

            if let Ok(user_cards) = store.entity_memories("user") {
                all_cards.extend(user_cards);
            }

            if all_cards.is_empty() {
                tracing::debug!(session_id, "No memories retrieved");
                return None;
            }

            tracing::info!(session_id, count = all_cards.len(), "Memories retrieved (session + user)");
            Some(all_cards.into_iter()
                .filter(|c| c.confidence.unwrap_or(1.0) >= zen_memory::memvid::TRIPLET_MIN_CONFIDENCE)
                .map(|c| {
                    format!("[{}] {}={}: {}", c.kind, c.entity, c.slot, c.value)
                }).collect())
        })
    }

    #[instrument(skip(self), fields(session_id, query_len = query.len()))]
    fn retrieve_memories_structured(&self, session_id: &str, query: &str) -> Option<Vec<String>> {
        self.memvid_store.as_ref().and_then(|store| {
            let ctx = MemoryCardContext::new(
                store.clone(),
                CardSelection::ForPrincipal(session_id.to_string()),
            );

            match ctx.select(query) {
                Ok(cards) if !cards.is_empty() => {
                    tracing::info!(session_id, count = cards.len(), "Structured memory cards retrieved");
                    Some(cards.into_iter()
                        .filter(|c| c.confidence.unwrap_or(1.0) >= zen_memory::memvid::TRIPLET_MIN_CONFIDENCE)
                        .map(|c| {
                            format!("[{}] {}={}: {}", c.kind, c.entity, c.slot, c.value)
                        }).collect())
                }
                Ok(_) => {
                    tracing::debug!(session_id, "No structured cards found");
                    None
                }
                Err(e) => {
                    tracing::warn!(session_id, error = %e, "Failed structured memory retrieval");
                    None
                }
            }
        })
    }

    /// Persist a conversation turn to the memvid store with per-session scoping.
    ///
    /// Replaces the former inline `put_text()` calls. The orchestrator calls
    /// this after execution completes, keeping the write concern at the
    /// orchestrator level (D2). Uses `uri = session_id` for scope isolation
    /// (D7) and `extract_triplets(false)` for Phase 1 (D9).
    #[instrument(skip(self), fields(session_id, response_len = assistant_response.len()))]
    pub fn persist_turn(&self, session_id: &str, user_query: &str, assistant_response: &str) {
        if let Some(ref store) = self.memvid_store {
            let zen_store = zen_memory::memvid::ZenMemvidStore::from_store(store.clone());
            let content = format!("User: {user_query}\nAssistant: {assistant_response}");

            if let Err(e) = zen_store.persist_structured_turn(session_id, "user", &content) {
                tracing::warn!(session_id, error = %e, "Failed to persist turn to memvid");
            }
        }
    }

    /// Execute a user query. Async-native, no nested runtime creation.
    #[instrument(skip(self, session), fields(session_id = %session.session_id, query_len = query.len()))]
    pub async fn execute(&self, query: &str, session: &mut SessionContext) -> Result<String> {
        let session_id = session.session_id.to_string();
        let mut ctx = InvestigationContext::new(&session_id, "query");

        ctx.evidence
            .push(Evidence::new("user-input", "query").with_detail(json!({ "summary": query })));

        if let Some(ref identity) = self.identity {
            ctx.evidence.push(
                Evidence::new("identity", "soul")
                    .with_detail(json!({ "summary": identity.soul_content })),
            );
            ctx.evidence.push(
                Evidence::new("identity", "agents")
                    .with_detail(json!({ "summary": identity.agents_content })),
            );
            ctx.evidence.push(
                Evidence::new("identity", "memory")
                    .with_detail(json!({ "summary": identity.memory_content })),
            );
        }

        for note in &session.knowledge {
            ctx.evidence.push(
                Evidence::new("knowledge", "wiki")
                    .with_detail(json!({ "summary": note.content, "path": note.path })),
            );
        }

        let memories = self
            .retrieve_memories_structured(&session_id, query)
            .or_else(|| self.retrieve_memories(&session_id));

        if let Some(memories) = memories {
            let memory_text = memories.join("\n");
            ctx.evidence.push(
                Evidence::new("retrieved-memory", "memvid")
                    .with_detail(json!({ "summary": memory_text })),
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

        let response = self.call_llm(query, &ctx, session).await?;

        tracing::info!(
            response_len = response.len(),
            "ZenAgent::execute: LLM response received"
        );

        session.add_turn("user", query);
        session.add_turn("assistant", &response);

        Ok(response)
    }

    fn tier_score(source_skill: &str, label: &str) -> f64 {
        match (source_skill, label) {
            ("identity", _) => 0.95,
            ("retrieved-memory", _) => 0.80,
            ("knowledge", _) => 0.70,
            ("user-input", _) => 1.00,
            _ => 0.50,
        }
    }

    fn tier_score_from_source_id(source_id: &str) -> f64 {
        let (skill, label) = source_id.split_once('/').unwrap_or(("_", "_"));
        Self::tier_score(skill, label)
    }

    fn build_chat_history(prompt: &str, session: &SessionContext) -> rig_core::OneOrMany<rig_core::message::Message> {
        use rig_core::message::Message;

        if session.conversation.is_empty() {
            return rig_core::OneOrMany::one(Message::user(prompt));
        }

        let mut messages: Vec<Message> = session
            .conversation
            .iter()
            .filter_map(|turn| match turn.role.as_str() {
                "user" => Some(Message::user(&turn.content)),
                "assistant" => Some(Message::assistant(&turn.content)),
                _ => None,
            })
            .collect();

        messages.push(Message::user(prompt));
        rig_core::OneOrMany::many(messages).unwrap_or_else(|_| rig_core::OneOrMany::one(Message::user(prompt)))
    }

    fn build_prompt(&self, query: &str, ctx: &InvestigationContext) -> String {
        let mut items = rig_resources::projection::evidence_to_context_items(ctx);

        items.sort_by(|a, b| {
            let ta = Self::tier_score_from_source_id(&a.source_id);
            let tb = Self::tier_score_from_source_id(&b.source_id);
            tb.partial_cmp(&ta).unwrap_or(std::cmp::Ordering::Equal)
        });
        for (rank, item) in items.iter_mut().enumerate() {
            item.rank = rank;
        }

        let config = ContextPackConfig::new(4096)
            .with_max_items(20)
            .with_reserve_chars(query.chars().count());

        let pack = rig_resources::projection::pack_resource_context(items, config);

        pack.selected
            .iter()
            .map(|i| i.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    #[instrument(skip(self, ctx, session), fields(session_id = %session.session_id, query_len = query.len()))]
    async fn call_llm(
        &self,
        query: &str,
        ctx: &InvestigationContext,
        session: &SessionContext,
    ) -> Result<String> {
        use rig_core::completion::CompletionRequest;

        let prompt = self.build_prompt(query, ctx);

        let request = CompletionRequest {
            model: None,
            preamble: Some("You are a helpful Zen assistant. Answer concisely. Use proper markdown formatting with blank lines between headings, paragraphs, code blocks, and lists.".to_string()),
            chat_history: Self::build_chat_history(&prompt, session),
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

    #[instrument(skip(self, session, on_token), fields(session_id = %session.session_id, query_len = query.len()))]
    pub async fn execute_stream(
        &self,
        query: &str,
        session: &mut SessionContext,
        on_token: impl FnMut(&str),
    ) -> Result<String> {
        let session_id = session.session_id.to_string();
        let mut ctx = InvestigationContext::new(&session_id, "query");

        ctx.evidence
            .push(Evidence::new("user-input", "query").with_detail(json!({ "summary": query })));

        if let Some(ref identity) = self.identity {
            ctx.evidence.push(
                Evidence::new("identity", "soul")
                    .with_detail(json!({ "summary": identity.soul_content })),
            );
            ctx.evidence.push(
                Evidence::new("identity", "agents")
                    .with_detail(json!({ "summary": identity.agents_content })),
            );
            ctx.evidence.push(
                Evidence::new("identity", "memory")
                    .with_detail(json!({ "summary": identity.memory_content })),
            );
        }

        for note in &session.knowledge {
            ctx.evidence.push(
                Evidence::new("knowledge", "wiki")
                    .with_detail(json!({ "summary": note.content, "path": note.path })),
            );
        }

        let memories = self
            .retrieve_memories_structured(&session_id, query)
            .or_else(|| self.retrieve_memories(&session_id));

        if let Some(memories) = memories {
            let memory_text = memories.join("\n");
            ctx.evidence.push(
                Evidence::new("retrieved-memory", "memvid")
                    .with_detail(json!({ "summary": memory_text })),
            );
        }

        ctx.signals.push(Signal::new("knowledge-query"));

        let _step_result = self.generic.step(&mut ctx).await?;

        let response = self.call_llm_stream(query, &ctx, session, on_token).await?;

        session.add_turn("user", query);
        session.add_turn("assistant", &response);

        Ok(response)
    }

    #[instrument(skip(self, ctx, session, on_token), fields(session_id = %session.session_id, query_len = query.len()))]
    async fn call_llm_stream(
        &self,
        query: &str,
        ctx: &InvestigationContext,
        session: &SessionContext,
        mut on_token: impl FnMut(&str),
    ) -> Result<String> {
        use rig_core::completion::{CompletionModel, CompletionRequest};

        let prompt = self.build_prompt(query, ctx);

        let request = CompletionRequest {
            model: None,
            preamble: Some("You are a helpful Zen assistant. Answer concisely. Use proper markdown formatting with blank lines between headings, paragraphs, code blocks, and lists.".to_string()),
            chat_history: Self::build_chat_history(&prompt, session),
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

#[cfg(test)]
mod chain_tests {
    use super::*;
    use zen_core::types::{ConversationTurn, RetrievedNote, Sensitivity, SessionContext};

    fn dummy_session_with_history() -> SessionContext {
        let mut session = SessionContext::new("test-agent".to_string(), String::new());
        session.add_turn("user", "I prefer Rust");
        session.add_turn("assistant", "Got it, Rust is great");
        session
    }

    #[test]
    fn build_chat_history_includes_prior_turns() {
        let session = dummy_session_with_history();
        let history = ZenAgent::build_chat_history("what did I say?", &session);
        let msgs: Vec<_> = history.iter().collect();
        assert_eq!(msgs.len(), 3, "Should have 2 prior turns + 1 current");
    }

    #[test]
    fn build_chat_history_empty_session_single_message() {
        let session = SessionContext::new("test".to_string(), String::new());
        let history = ZenAgent::build_chat_history("hello", &session);
        let msgs: Vec<_> = history.iter().collect();
        assert_eq!(msgs.len(), 1, "Empty session should produce single message");
    }

    #[test]
    fn tier_score_includes_knowledge() {
        let score = ZenAgent::tier_score("knowledge", "wiki");
        assert_eq!(score, 0.70);
        assert!(score < ZenAgent::tier_score("identity", "soul"));
        assert!(score > ZenAgent::tier_score("skills", "other"));
    }
}
