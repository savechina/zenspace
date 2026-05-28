use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use rig_compose::context::InvestigationContext;
use rig_compose::registry::{KernelError, ToolRegistry};
use rig_compose::skill::{Skill, SkillOutcome};
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::Value;

type ToolHandler = dyn Fn(Value) -> Pin<Box<dyn Future<Output = Result<Value, KernelError>> + Send>> + Send + Sync;

#[derive(Debug, Clone)]
pub struct ZenSkill {
    id: String,
    description: String,
}

impl ZenSkill {
    pub fn new(id: &str, description: &str) -> Self {
        Self {
            id: id.to_string(),
            description: description.to_string(),
        }
    }
}

#[async_trait]
impl Skill for ZenSkill {
    fn id(&self) -> &str {
        &self.id
    }

    fn description(&self) -> &str {
        &self.description
    }

    async fn execute(
        &self,
        _ctx: &mut InvestigationContext,
        _tools: &ToolRegistry,
    ) -> Result<SkillOutcome, KernelError> {
        Ok(SkillOutcome::noop())
    }
}

#[derive(Clone)]
pub struct ZenTool {
    schema: ToolSchema,
    f: Arc<ToolHandler>,
}

impl std::fmt::Debug for ZenTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZenTool")
            .field("schema", &self.schema)
            .finish_non_exhaustive()
    }
}

impl ZenTool {
    pub fn new<F, Fut>(schema: ToolSchema, f: F) -> Self
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, KernelError>> + Send + 'static,
    {
        Self {
            schema,
            f: Arc::new(move |v| Box::pin(f(v))),
        }
    }
}

#[async_trait]
impl Tool for ZenTool {
    fn schema(&self) -> ToolSchema {
        self.schema.clone()
    }

    fn name(&self) -> String {
        self.schema.name.clone()
    }

    async fn invoke(&self, args: Value) -> Result<Value, KernelError> {
        (self.f)(args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn zen_skill_executes() {
        let skill = ZenSkill::new("test-skill", "A test skill");
        assert_eq!(skill.id(), "test-skill");
        assert_eq!(skill.description(), "A test skill");

        let mut ctx = InvestigationContext::new("test", "query");
        let tools = ToolRegistry::new();
        let outcome = skill.execute(&mut ctx, &tools).await.unwrap();
        assert_eq!(outcome.confidence_delta, 0.0);
    }

    #[tokio::test]
    async fn zen_tool_invokes_handler() {
        let schema = ToolSchema {
            name: "test.uppercase".to_string(),
            description: "Converts text to uppercase".to_string(),
            args_schema: json!({"type": "object"}),
            result_schema: json!({"type": "object"}),
        };

        let tool = ZenTool::new(schema, |v| async move {
            let input = v.get("text").and_then(|t| t.as_str()).unwrap_or("");
            Ok(json!({ "result": input.to_uppercase() }))
        });

        assert_eq!(tool.name(), "test.uppercase");
        let result = tool.invoke(json!({ "text": "hello" })).await.unwrap();
        assert_eq!(result["result"], "HELLO");
    }
}
