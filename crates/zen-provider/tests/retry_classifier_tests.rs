//! Unit tests for LlmRetryClassifier.

use rig_compose::reliability::RetryClass;
use zen_provider::{DefaultLlmRetryClassifier, LlmError, LlmRetryClassifier};

#[test]
fn test_classify_provider_unavailable_as_transient() {
    let classifier = DefaultLlmRetryClassifier;
    let error = LlmError::ProviderUnavailable {
        provider: "ollama".into(),
        reason: "connection timeout".into(),
    };

    assert_eq!(classifier.classify(&error), RetryClass::Transient);
}

#[test]
fn test_classify_call_error_as_transient() {
    let classifier = DefaultLlmRetryClassifier;
    let error = LlmError::Call {
        reason: "500 Internal Server Error".into(),
    };

    assert_eq!(classifier.classify(&error), RetryClass::Transient);
}

#[test]
fn test_classify_routing_error_as_permanent() {
    let classifier = DefaultLlmRetryClassifier;
    let error = LlmError::Routing {
        reason: "No provider configured".into(),
    };

    assert_eq!(classifier.classify(&error), RetryClass::Permanent);
}
