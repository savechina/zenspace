use rig_tap::{TelemetryHook, TelemetryHookConfig};

use crate::completion_model::ZenCompletionModel;

pub fn create_telemetry_hook(
    model: &str,
    conversation_id: &str,
) -> TelemetryHook<ZenCompletionModel> {
    TelemetryHook::new(TelemetryHookConfig::new(model, conversation_id))
}
