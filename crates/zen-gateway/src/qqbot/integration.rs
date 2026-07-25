use anyhow::Result;
use zen_vault::note::{Note, NoteService};

pub struct QqBotIntegration {
    note_service: NoteService,
}

impl QqBotIntegration {
    pub fn new() -> Self {
        Self {
            note_service: NoteService,
        }
    }

    pub async fn create_note(&self, content: &str, tags: Vec<String>) -> Result<Note> {
        self.note_service
            .create_note(content, tags, "qq_private")
            .await
    }

    pub async fn search_notes(&self, _query: &str) -> Result<Vec<String>> {
        Ok(vec![])
    }

    pub async fn get_status(&self) -> Result<String> {
        Ok("agentic bot online".to_string())
    }
}

impl Default for QqBotIntegration {
    fn default() -> Self {
        Self::new()
    }
}
