// ============================================================================
// 4D Test Suite: zen-core errors.rs
//
// Dimensions:
//   NORMAL   — Every error variant returns correct category and display string
//   REVERSE  — From impls, category propagation through ZenError
//   ADVERSARIAL — Empty strings, max values, special chars in error fields
//   LOGIC TREE  — Every match arm in category() for all error types
// ============================================================================

use zen_core::errors::*;

// ============================================================================
// NORMAL PATH — Every variant returns the correct ErrorCategory
// ============================================================================

// ── AgenticError category mapping (20 variants) ──

#[test]
fn agentic_llm_provider_unavailable_is_retryable() {
    let e = AgenticError::LlmProviderUnavailable {
        provider: "openai".into(),
        reason: "connection refused".into(),
    };
    assert_eq!(e.category(), ErrorCategory::Retryable, "LlmProviderUnavailable should be Retryable");
}

#[test]
fn agentic_llm_routing_failed_is_retryable() {
    let e = AgenticError::LlmRoutingFailed {
        provider: "ollama".into(),
        reason: "no available model".into(),
    };
    assert_eq!(e.category(), ErrorCategory::Retryable, "LlmRoutingFailed should be Retryable");
}

#[test]
fn agentic_llm_rate_limited_is_retryable() {
    let e = AgenticError::LlmRateLimited {
        provider: "anthropic".into(),
        retry_after_secs: 30,
    };
    assert_eq!(e.category(), ErrorCategory::Retryable, "LlmRateLimited should be Retryable");
}

#[test]
fn agentic_llm_response_invalid_is_user_action() {
    let e = AgenticError::LlmResponseInvalid {
        expected: "json".into(),
        actual: "text/plain".into(),
    };
    assert_eq!(e.category(), ErrorCategory::UserAction, "LlmResponseInvalid should be UserAction");
}

#[test]
fn agentic_llm_context_overflow_is_user_action() {
    let e = AgenticError::LlmContextOverflow {
        tokens_used: 200_000,
        limit: 128_000,
    };
    assert_eq!(e.category(), ErrorCategory::UserAction, "LlmContextOverflow should be UserAction");
}

#[test]
fn agentic_knowledge_base_empty_is_system_error() {
    let e = AgenticError::KnowledgeBaseEmpty {
        path: "/data/knowledge".into(),
    };
    assert_eq!(e.category(), ErrorCategory::SystemError, "KnowledgeBaseEmpty should be SystemError");
}

#[test]
fn agentic_knowledge_search_failed_is_system_error() {
    let e = AgenticError::KnowledgeSearchFailed {
        reason: "index corrupted".into(),
    };
    assert_eq!(e.category(), ErrorCategory::SystemError, "KnowledgeSearchFailed should be SystemError");
}

#[test]
fn agentic_knowledge_consolidate_failed_is_system_error() {
    let e = AgenticError::KnowledgeConsolidateFailed {
        error: "deadlock detected".into(),
    };
    assert_eq!(e.category(), ErrorCategory::SystemError, "KnowledgeConsolidateFailed should be SystemError");
}

#[test]
fn agentic_knowledge_note_failed_is_system_error() {
    let e = AgenticError::KnowledgeNoteFailed {
        file: "note.md".into(),
        error: "parse error".into(),
    };
    assert_eq!(e.category(), ErrorCategory::SystemError, "KnowledgeNoteFailed should be SystemError");
}

#[test]
fn agentic_qqbot_connection_failed_is_user_action() {
    let e = AgenticError::QqBotConnectionFailed {
        reason: "network unreachable".into(),
    };
    assert_eq!(e.category(), ErrorCategory::UserAction, "QqBotConnectionFailed should be UserAction");
}

#[test]
fn agentic_qqbot_auth_failed_is_user_action() {
    let e = AgenticError::QqBotAuthFailed {
        reason: "invalid token".into(),
    };
    assert_eq!(e.category(), ErrorCategory::UserAction, "QqBotAuthFailed should be UserAction");
}

#[test]
fn agentic_qqbot_rate_limited_is_retryable() {
    let e = AgenticError::QqBotRateLimited { retry_after_secs: 60 };
    assert_eq!(e.category(), ErrorCategory::Retryable, "QqBotRateLimited should be Retryable");
}

#[test]
fn agentic_search_fts5_failed_is_system_error() {
    let e = AgenticError::SearchFts5Failed { reason: "syntax error".into() };
    assert_eq!(e.category(), ErrorCategory::SystemError, "SearchFts5Failed should be SystemError");
}

#[test]
fn agentic_search_vector_failed_is_system_error() {
    let e = AgenticError::SearchVectorFailed { reason: "dimension mismatch".into() };
    assert_eq!(e.category(), ErrorCategory::SystemError, "SearchVectorFailed should be SystemError");
}

#[test]
fn agentic_search_graph_failed_is_system_error() {
    let e = AgenticError::SearchGraphFailed { reason: "node not found".into() };
    assert_eq!(e.category(), ErrorCategory::SystemError, "SearchGraphFailed should be SystemError");
}

#[test]
fn agentic_macos_keychain_denied_is_user_action() {
    let e = AgenticError::MacosKeychainDenied {
        service: "zen-openai".into(),
    };
    assert_eq!(e.category(), ErrorCategory::UserAction, "MacosKeychainDenied should be UserAction");
}

#[test]
fn agentic_macos_accessibility_denied_is_user_action() {
    let e = AgenticError::MacosAccessibilityDenied {
        feature: "screen recording".into(),
    };
    assert_eq!(e.category(), ErrorCategory::UserAction, "MacosAccessibilityDenied should be UserAction");
}

#[test]
fn agentic_plugin_load_failed_is_user_action() {
    let e = AgenticError::PluginLoadFailed {
        plugin_id: "my-plugin".into(),
        error: "binary not found".into(),
    };
    assert_eq!(e.category(), ErrorCategory::UserAction, "PluginLoadFailed should be UserAction");
}

#[test]
fn agentic_plugin_sandbox_violation_is_safety_block() {
    let e = AgenticError::PluginSandboxViolation {
        plugin_id: "rogue".into(),
        operation: "read /etc/passwd".into(),
    };
    assert_eq!(e.category(), ErrorCategory::SafetyBlock, "PluginSandboxViolation should be SafetyBlock");
}

#[test]
fn agentic_plugin_permission_denied_is_user_action() {
    let e = AgenticError::PluginPermissionDenied {
        plugin_id: "my-plugin".into(),
        permission: "network".into(),
    };
    assert_eq!(e.category(), ErrorCategory::UserAction, "PluginPermissionDenied should be UserAction");
}

// ── ConfigError Display (4 variants) ──

#[test]
fn config_error_missing_file_display() {
    let e = ConfigError::MissingFile { path: "/tmp/zen/config.toml".into() };
    let msg = e.to_string();
    assert!(msg.contains("/tmp/zen/config.toml"), "Display should contain path: {msg}");
}

#[test]
fn config_error_parse_error_display() {
    let e = ConfigError::ParseError {
        path: "config.toml".into(),
        reason: "invalid TOML syntax".into(),
    };
    let msg = e.to_string();
    assert!(msg.contains("config.toml"), "Display should contain path: {msg}");
    assert!(msg.contains("invalid TOML syntax"), "Display should contain reason: {msg}");
}

#[test]
fn config_error_validation_error_display() {
    let e = ConfigError::ValidationError {
        key: "model".into(),
        reason: "must be non-empty".into(),
    };
    let msg = e.to_string();
    assert!(msg.contains("model"), "Display should contain key: {msg}");
    assert!(msg.contains("must be non-empty"), "Display should contain reason: {msg}");
}

#[test]
fn config_error_missing_env_var_display() {
    let e = ConfigError::MissingEnvVar { variable: "ZEN_API_KEY".into() };
    let msg = e.to_string();
    assert!(msg.contains("ZEN_API_KEY"), "Display should contain variable name: {msg}");
}

// ── PathError Display (5 variants) ──

#[test]
fn path_error_not_found_display() {
    let e = PathError::NotFound { path: "/nonexistent".into() };
    assert!(e.to_string().contains("/nonexistent"));
}

#[test]
fn path_error_not_a_directory_display() {
    let e = PathError::NotADirectory { path: "/tmp/file.txt".into() };
    assert!(e.to_string().contains("/tmp/file.txt"));
}

#[test]
fn path_error_not_a_file_display() {
    let e = PathError::NotAFile { path: "/tmp/dir".into() };
    assert!(e.to_string().contains("/tmp/dir"));
}

#[test]
fn path_error_permission_denied_display() {
    let e = PathError::PermissionDenied { path: "/etc/shadow".into() };
    assert!(e.to_string().contains("/etc/shadow"));
}

#[test]
fn path_error_home_dir_not_found_display() {
    let e = PathError::HomeDirNotFound;
    assert_eq!(e.to_string(), "could not resolve home directory");
}

// ── ParseError Display (3 variants) ──

#[test]
fn parse_error_json_error_display() {
    let e = ParseError::JsonError { reason: "unexpected token".into() };
    assert!(e.to_string().contains("JSON"));
    assert!(e.to_string().contains("unexpected token"));
}

#[test]
fn parse_error_toml_error_display() {
    let e = ParseError::TomlError {
        location: "config.toml:42".into(),
        reason: "duplicate key".into(),
    };
    assert!(e.to_string().contains("config.toml:42"));
    assert!(e.to_string().contains("duplicate key"));
}

#[test]
fn parse_error_int_error_display() {
    let e = ParseError::IntError {
        input: "1_000_000".into(),
        reason: "invalid digit".into(),
    };
    assert!(e.to_string().contains("1_000_000"));
    assert!(e.to_string().contains("invalid digit"));
}

// ── ServiceError Display (2 variants) ──

#[test]
fn service_error_message_display() {
    let e = ServiceError::Message("something broke".into());
    assert_eq!(e.to_string(), "message: something broke");
}

// ── ZenError Display (8 variants) ──

#[test]
fn zen_error_config_display() {
    let err = ConfigError::MissingFile { path: "test.toml".into() };
    let e = ZenError::Config(err);
    assert!(e.to_string().contains("test.toml"));
}

#[test]
fn zen_error_path_display() {
    let err = PathError::HomeDirNotFound;
    let e = ZenError::Path(err);
    assert!(e.to_string().contains("home directory"));
}

#[test]
fn zen_error_agentic_display() {
    let inner = AgenticError::LlmRateLimited { provider: "openai".into(), retry_after_secs: 10 };
    let e = ZenError::Agentic(inner, ErrorCategory::Retryable);
    let msg = e.to_string();
    assert!(msg.contains("openai"), "Display should contain provider: {msg}");
    assert!(msg.contains("retryable"), "Display should contain category: {msg}");
}

#[test]
fn zen_error_io_display() {
    let e = ZenError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "file missing"));
    assert!(e.to_string().contains("file missing"));
}

#[test]
fn zen_error_parse_display() {
    let e = ZenError::Parse(ParseError::JsonError { reason: "bad json".into() });
    assert!(e.to_string().contains("bad json"));
}

#[test]
fn zen_error_serialization_display() {
    let e = ZenError::Serialization(serde_json::from_str::<serde_json::Value>("invalid").unwrap_err());
    assert!(e.to_string().contains("error"));
}

#[test]
fn zen_error_service_display() {
    let e = ZenError::Service("service unavailable".into());
    assert_eq!(e.to_string(), "service error: service unavailable");
}

#[test]
fn zen_error_message_display() {
    let e = ZenError::Message("generic message".into());
    assert_eq!(e.to_string(), "message: generic message");
}

// ── ErrorCategory Display ──

#[test]
fn error_category_display_retryable() {
    assert_eq!(ErrorCategory::Retryable.to_string(), "retryable");
}

#[test]
fn error_category_display_user_action() {
    assert_eq!(ErrorCategory::UserAction.to_string(), "user-action");
}

#[test]
fn error_category_display_system_error() {
    assert_eq!(ErrorCategory::SystemError.to_string(), "system-error");
}

#[test]
fn error_category_display_safety_block() {
    assert_eq!(ErrorCategory::SafetyBlock.to_string(), "safety-block");
}

// ============================================================================
// REVERSE PATH — From impls and category propagation
// ============================================================================

#[test]
fn agentic_error_into_zen_error_preserves_category() {
    let original = AgenticError::LlmRateLimited { provider: "test".into(), retry_after_secs: 5 };
    let cat = original.category();
    let zen: ZenError = original.into();
    match &zen {
        ZenError::Agentic(_, c) => assert_eq!(*c, cat, "Agentic→ZenError should preserve category"),
        _ => panic!("Expected ZenError::Agentic, got {zen:?}"),
    }
}

#[test]
fn agentic_error_into_zen_error_all_categories() {
    let variants: Vec<AgenticError> = vec![
        AgenticError::LlmProviderUnavailable { provider: "p".into(), reason: "r".into() },
        AgenticError::LlmResponseInvalid { expected: "e".into(), actual: "a".into() },
        AgenticError::KnowledgeBaseEmpty { path: "p".into() },
        AgenticError::QqBotConnectionFailed { reason: "r".into() },
        AgenticError::QqBotRateLimited { retry_after_secs: 1 },
        AgenticError::SearchFts5Failed { reason: "r".into() },
        AgenticError::MacosKeychainDenied { service: "s".into() },
        AgenticError::PluginSandboxViolation { plugin_id: "p".into(), operation: "o".into() },
    ];
    for v in variants {
        let cat = v.category();
        let zen: ZenError = v.into();
        match &zen {
            ZenError::Agentic(_, c) => assert_eq!(*c, cat, "category mismatched for {zen}"),
            _ => panic!("Expected ZenError::Agentic"),
        }
    }
}

#[test]
fn service_error_io_into_zen_error() {
    let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
    let svc_err = ServiceError::Io(io_err);
    let zen: ZenError = svc_err.into();
    assert!(matches!(zen, ZenError::Io(_)), "ServiceError::Io should become ZenError::Io");
}

#[test]
fn service_error_message_into_zen_error() {
    let svc_err = ServiceError::Message("custom message".into());
    let zen: ZenError = svc_err.into();
    match &zen {
        ZenError::Service(m) => assert_eq!(m, "custom message"),
        _ => panic!("Expected ZenError::Service, got {zen:?}"),
    }
}

#[test]
fn config_error_into_zen_error() {
    let cerr = ConfigError::MissingFile { path: "test.toml".into() };
    let zen: ZenError = cerr.into();
    assert!(matches!(zen, ZenError::Config(_)), "ConfigError should become ZenError::Config");
}

#[test]
fn path_error_into_zen_error() {
    let perr = PathError::NotFound { path: "/x".into() };
    let zen: ZenError = perr.into();
    assert!(matches!(zen, ZenError::Path(_)), "PathError should become ZenError::Path");
}

#[test]
fn parse_error_into_zen_error() {
    let perr = ParseError::JsonError { reason: "bad".into() };
    let zen: ZenError = perr.into();
    assert!(matches!(zen, ZenError::Parse(_)), "ParseError should become ZenError::Parse");
}

// ============================================================================
// ADVERSARIAL PATH — Edge cases in error fields
// ============================================================================

#[test]
fn agentic_error_empty_strings() {
    let e = AgenticError::LlmProviderUnavailable {
        provider: "".into(),
        reason: "".into(),
    };
    assert_eq!(e.category(), ErrorCategory::Retryable, "Empty strings should not affect category");
    let msg = e.to_string();
    // Should not panic or produce garbled output
    assert!(!msg.is_empty(), "Display should not be empty");
}

#[test]
fn agentic_error_max_u64_values() {
    let e = AgenticError::LlmContextOverflow {
        tokens_used: u64::MAX,
        limit: u64::MAX,
    };
    assert_eq!(e.category(), ErrorCategory::UserAction);
    let msg = e.to_string();
    assert!(msg.contains(&u64::MAX.to_string()), "Display should contain max u64: {msg}");
}

#[test]
fn agentic_error_zero_retry_after() {
    let e = AgenticError::LlmRateLimited { provider: "test".into(), retry_after_secs: 0 };
    assert_eq!(e.category(), ErrorCategory::Retryable);
    assert!(e.to_string().contains("0s"), "Display should include 0s");
}

#[test]
fn agentic_error_unicode_in_fields() {
    let e = AgenticError::KnowledgeNoteFailed {
        file: "文書.md".into(),
        error: "解析エラー: 無効な文字".into(),
    };
    assert_eq!(e.category(), ErrorCategory::SystemError);
    let msg = e.to_string();
    assert!(msg.contains("文書.md"), "Display should contain unicode: {msg}");
    assert!(msg.contains("解析エラー"), "Display should contain unicode: {msg}");
}

#[test]
fn agentic_error_special_chars() {
    let e = AgenticError::PluginSandboxViolation {
        plugin_id: "test<script>".into(),
        operation: "rm -rf /".into(),
    };
    assert_eq!(e.category(), ErrorCategory::SafetyBlock);
}

#[test]
fn config_error_empty_path() {
    let e = ConfigError::MissingFile { path: "".into() };
    let msg = e.to_string();
    assert!(msg.contains("missing configuration file"), "Even empty path should produce valid display");
}

#[test]
fn path_error_empty_path() {
    let e = PathError::NotFound { path: "".into() };
    let msg = e.to_string();
    assert!(msg.contains("path does not exist"), "Even empty path should produce valid display");
}

#[test]
fn zen_error_agentic_with_empty_agentic_error() {
    let inner = AgenticError::LlmProviderUnavailable { provider: "".into(), reason: "".into() };
    let e = ZenError::Agentic(inner, ErrorCategory::Retryable);
    // Should not panic
    let _ = e.to_string();
    let _ = e.category();
}

// ============================================================================
// LOGIC TREE — Every branch of category() for all error types
// ============================================================================

// ── ZenError::category() — every variant ──

#[test]
fn zen_error_config_category_is_user_action() {
    let e = ZenError::Config(ConfigError::MissingFile { path: "x".into() });
    assert_eq!(e.category(), Some(ErrorCategory::UserAction));
}

#[test]
fn zen_error_path_category_is_user_action() {
    let e = ZenError::Path(PathError::HomeDirNotFound);
    assert_eq!(e.category(), Some(ErrorCategory::UserAction));
}

#[test]
fn zen_error_io_not_found_is_user_action() {
    let e = ZenError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "missing"));
    assert_eq!(e.category(), Some(ErrorCategory::UserAction));
}

#[test]
fn zen_error_io_permission_denied_is_user_action() {
    let e = ZenError::Io(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"));
    assert_eq!(e.category(), Some(ErrorCategory::UserAction));
}

#[test]
fn zen_error_io_timed_out_is_retryable() {
    let e = ZenError::Io(std::io::Error::new(std::io::ErrorKind::TimedOut, "timeout"));
    assert_eq!(e.category(), Some(ErrorCategory::Retryable));
}

#[test]
fn zen_error_io_interrupted_is_retryable() {
    let e = ZenError::Io(std::io::Error::new(std::io::ErrorKind::Interrupted, "interrupted"));
    assert_eq!(e.category(), Some(ErrorCategory::Retryable));
}

#[test]
fn zen_error_io_other_is_system_error() {
    let e = ZenError::Io(std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused"));
    assert_eq!(e.category(), Some(ErrorCategory::SystemError));
}

#[test]
fn zen_error_parse_category_is_user_action() {
    let e = ZenError::Parse(ParseError::JsonError { reason: "bad".into() });
    assert_eq!(e.category(), Some(ErrorCategory::UserAction));
}

#[test]
fn zen_error_serialization_category_is_system_error() {
    let e = ZenError::Serialization(
        serde_json::from_str::<serde_json::Value>("{invalid}").unwrap_err(),
    );
    assert_eq!(e.category(), Some(ErrorCategory::SystemError));
}

#[test]
fn zen_error_service_category_is_system_error() {
    let e = ZenError::Service("down".into());
    assert_eq!(e.category(), Some(ErrorCategory::SystemError));
}

#[test]
fn zen_error_message_category_is_user_action() {
    let e = ZenError::Message("info".into());
    assert_eq!(e.category(), Some(ErrorCategory::UserAction));
}

#[test]
fn zen_error_agentic_category_propagated() {
    let inner = AgenticError::LlmRateLimited { provider: "p".into(), retry_after_secs: 5 };
    let cat = inner.category();
    let e = ZenError::Agentic(inner, cat);
    assert_eq!(e.category(), Some(cat));
}

// ── IoErrorKind comprehensive coverage ──

#[test]
fn zen_error_io_broken_pipe_is_system_error() {
    let e = ZenError::Io(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "broken"));
    assert_eq!(e.category(), Some(ErrorCategory::SystemError));
}

#[test]
fn zen_error_io_connection_reset_is_system_error() {
    let e = ZenError::Io(std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset"));
    assert_eq!(e.category(), Some(ErrorCategory::SystemError));
}

#[test]
fn zen_error_io_connection_aborted_is_system_error() {
    let e = ZenError::Io(std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "aborted"));
    assert_eq!(e.category(), Some(ErrorCategory::SystemError));
}

#[test]
fn zen_error_io_addr_in_use_is_system_error() {
    let e = ZenError::Io(std::io::Error::new(std::io::ErrorKind::AddrInUse, "in use"));
    assert_eq!(e.category(), Some(ErrorCategory::SystemError));
}

#[test]
fn zen_error_io_write_zero_is_system_error() {
    let e = ZenError::Io(std::io::Error::new(std::io::ErrorKind::WriteZero, "write zero"));
    assert_eq!(e.category(), Some(ErrorCategory::SystemError));
}

#[test]
fn zen_error_io_unexpected_eof_is_system_error() {
    let e = ZenError::Io(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "eof"));
    assert_eq!(e.category(), Some(ErrorCategory::SystemError));
}

// ── AgenticError category() — exhaustive match ──

#[test]
fn agentic_error_all_variants_mapped() {
    // Every variant must return a valid ErrorCategory — no panic, no wildcard
    let variants: Vec<(AgenticError, ErrorCategory)> = vec![
        (AgenticError::LlmProviderUnavailable { provider: "p".into(), reason: "r".into() }, ErrorCategory::Retryable),
        (AgenticError::LlmRoutingFailed { provider: "p".into(), reason: "r".into() }, ErrorCategory::Retryable),
        (AgenticError::LlmRateLimited { provider: "p".into(), retry_after_secs: 1 }, ErrorCategory::Retryable),
        (AgenticError::LlmResponseInvalid { expected: "e".into(), actual: "a".into() }, ErrorCategory::UserAction),
        (AgenticError::LlmContextOverflow { tokens_used: 1, limit: 1 }, ErrorCategory::UserAction),
        (AgenticError::KnowledgeBaseEmpty { path: "p".into() }, ErrorCategory::SystemError),
        (AgenticError::KnowledgeSearchFailed { reason: "r".into() }, ErrorCategory::SystemError),
        (AgenticError::KnowledgeConsolidateFailed { error: "e".into() }, ErrorCategory::SystemError),
        (AgenticError::KnowledgeNoteFailed { file: "f".into(), error: "e".into() }, ErrorCategory::SystemError),
        (AgenticError::QqBotConnectionFailed { reason: "r".into() }, ErrorCategory::UserAction),
        (AgenticError::QqBotAuthFailed { reason: "r".into() }, ErrorCategory::UserAction),
        (AgenticError::QqBotRateLimited { retry_after_secs: 1 }, ErrorCategory::Retryable),
        (AgenticError::SearchFts5Failed { reason: "r".into() }, ErrorCategory::SystemError),
        (AgenticError::SearchVectorFailed { reason: "r".into() }, ErrorCategory::SystemError),
        (AgenticError::SearchGraphFailed { reason: "r".into() }, ErrorCategory::SystemError),
        (AgenticError::MacosKeychainDenied { service: "s".into() }, ErrorCategory::UserAction),
        (AgenticError::MacosAccessibilityDenied { feature: "f".into() }, ErrorCategory::UserAction),
        (AgenticError::PluginLoadFailed { plugin_id: "p".into(), error: "e".into() }, ErrorCategory::UserAction),
        (AgenticError::PluginSandboxViolation { plugin_id: "p".into(), operation: "o".into() }, ErrorCategory::SafetyBlock),
        (AgenticError::PluginPermissionDenied { plugin_id: "p".into(), permission: "p".into() }, ErrorCategory::UserAction),
    ];

    for (variant, expected_category) in variants {
        assert_eq!(variant.category(), expected_category, "Failed for variant: {variant}");
    }
}

// ── Cannot create un-categorized AgenticError: all 20 variants are covered ──

// Error: We cannot construct a ServiceError::Io easily without an io::Error,
// but we already tested it above.

// ============================================================================
// Edge: Ensure Debug + Send + Sync bounds
// ============================================================================

#[test]
fn error_types_are_send_sync() {
    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}

    assert_send::<AgenticError>();
    assert_sync::<AgenticError>();
    assert_send::<ZenError>();
    assert_sync::<ZenError>();
    assert_send::<ConfigError>();
    assert_sync::<ConfigError>();
    assert_send::<PathError>();
    assert_sync::<PathError>();
    assert_send::<ParseError>();
    assert_sync::<ParseError>();
    assert_send::<ServiceError>();
    assert_sync::<ServiceError>();
    assert_send::<ErrorCategory>();
    assert_sync::<ErrorCategory>();
}
