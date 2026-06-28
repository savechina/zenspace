use rig_tap::{EventKind, emit_kind};

pub fn emit_prompt_started(model: &str, conversation_id: &str, messages_in: usize) {
    emit_kind(
        conversation_id,
        EventKind::PromptStarted {
            model: model.to_string(),
            messages_in,
        },
    );
}

pub fn emit_prompt_completed(
    model: &str,
    conversation_id: &str,
    tokens_in: Option<u64>,
    tokens_out: Option<u64>,
    duration_ms: Option<u64>,
) {
    emit_kind(
        conversation_id,
        EventKind::PromptCompleted {
            model: model.to_string(),
            tokens_in,
            tokens_out,
            cached_tokens_in: None,
            reasoning_tokens: None,
            cost_usd: None,
            finish_reason: None,
            response_id: None,
            previous_response_id: None,
            time_to_first_token_ms: None,
            duration_ms,
        },
    );
}

pub fn emit_prompt_failed(model: &str, conversation_id: &str, error: &str) {
    emit_kind(
        conversation_id,
        EventKind::PromptFailed {
            model: model.to_string(),
            error_class: rig_tap::ErrorClass::Unknown,
            message: error.to_string(),
            retriable: false,
            provider_error_code: None,
            http_status: None,
        },
    );
}
