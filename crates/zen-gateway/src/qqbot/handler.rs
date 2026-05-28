use anyhow::{Context, Result};
use serde::Deserialize;
use tracing::{debug, info};

use super::api::QqBotApi;
use super::commands::{QqBotCommand, parse_command};
use super::integration::QqBotIntegration;

#[derive(Debug, Deserialize)]
struct MessageCreateEvent {
    _msg_id: String,
    channel_id: String,
    _user_id: String,
    content: String,
    msg_type: u32,
}

pub struct QqBotHandler {
    integration: QqBotIntegration,
    api: QqBotApi,
}

impl QqBotHandler {
    pub fn new(integration: QqBotIntegration, api: QqBotApi) -> Self {
        Self { integration, api }
    }

    pub async fn handle_event(&self, raw: &str) -> Result<()> {
        let event: MessageCreateEvent =
            serde_json::from_str(raw).with_context(|| "parse MESSAGE_CREATE event".to_string())?;

        if event.msg_type != 0 && event.msg_type != 1 && event.msg_type != 2 {
            return Ok(());
        }

        let cmd = parse_command(&event.content);

        match cmd {
            Some(QqBotCommand::Note { content }) => {
                info!("note command received");
                let note = self
                    .integration
                    .create_note(&content, vec!["qq".to_string()])
                    .await?;
                self.send_reply(&event.channel_id, &format!("note created: id={}", note.id))
                    .await?;
            },
            Some(QqBotCommand::Search { query }) => {
                info!("search command: {query}");
                self.send_reply(&event.channel_id, &format!("search: {query}"))
                    .await?;
            },
            Some(QqBotCommand::Status) => {
                self.send_reply(&event.channel_id, "agentic bot online")
                    .await?;
            },
            None => {
                debug!("no command matched");
            },
        }

        Ok(())
    }

    async fn send_reply(&self, channel_id: &str, content: &str) -> Result<()> {
        self.api.send_message(channel_id, content).await
    }
}
