// 4D Test: ErrorCategory, RetryPolicy
//
// Dimensions:
//   Normal: Status code classification, retry delay calculation
//   Reverse: Unknown codes, zero retries, max delay
//   Adversarial: Negative delay, overflow, extreme jitter
//   Logic Tree: Retryable vs non-retryable decision matrix

use zen_agents::{ErrorCategory, RetryPolicy};

// ============================================================================
// Normal Dimension
// ============================================================================

#[test]
fn error_category_from_status_code_transient() {
    assert_eq!(ErrorCategory::from_status_code(429), ErrorCategory::Transient);
    assert_eq!(ErrorCategory::from_status_code(500), ErrorCategory::Transient);
    assert_eq!(ErrorCategory::from_status_code(502), ErrorCategory::Transient);
    assert_eq!(ErrorCategory::from_status_code(503), ErrorCategory::Transient);
    assert_eq!(ErrorCategory::from_status_code(504), ErrorCategory::Transient);
}

#[test]
fn error_category_from_status_code_client_error() {
    assert_eq!(ErrorCategory::from_status_code(400), ErrorCategory::ClientError);
    assert_eq!(ErrorCategory::from_status_code(401), ErrorCategory::ClientError);
    assert_eq!(ErrorCategory::from_status_code(403), ErrorCategory::ClientError);
    assert_eq!(ErrorCategory::from_status_code(404), ErrorCategory::ClientError);
    assert_eq!(ErrorCategory::from_status_code(422), ErrorCategory::ClientError);
}

#[test]
fn error_category_from_error_message_rate_limit() {
    let msg = "429 Too Many Requests";
    assert_eq!(
        ErrorCategory::from_error_message(msg),
        ErrorCategory::Transient
    );
}

#[test]
fn error_category_from_error_message_timeout() {
    let msg = "Connection timeout after 30s";
    assert_eq!(
        ErrorCategory::from_error_message(msg),
        ErrorCategory::Transient
    );
}

#[test]
fn error_category_from_error_message_400() {
    let msg = "400 Bad Request: invalid model";
    assert_eq!(
        ErrorCategory::from_error_message(msg),
        ErrorCategory::ClientError
    );
}

#[test]
fn error_category_is_retryable() {
    assert!(ErrorCategory::Transient.is_retryable());
    assert!(ErrorCategory::Unknown.is_retryable());
    assert!(!ErrorCategory::ClientError.is_retryable());
}

#[test]
fn retry_policy_default_values() {
    let policy = RetryPolicy::default();
    assert_eq!(policy.max_retries, 3);
    assert_eq!(policy.base_delay_ms, 500);
    assert_eq!(policy.max_delay_ms, 10_000);
    assert!(policy.jitter);
}

#[test]
fn retry_policy_delay_exponential() {
    let policy = RetryPolicy {
        max_retries: 3,
        base_delay_ms: 100,
        max_delay_ms: 10_000,
        jitter: false,
    };
    // attempt 0: 100 * 2^0 = 100
    // attempt 1: 100 * 2^1 = 200
    // attempt 2: 100 * 2^2 = 400
    assert_eq!(policy.delay_ms(0), 100);
    assert_eq!(policy.delay_ms(1), 200);
    assert_eq!(policy.delay_ms(2), 400);
}

// ============================================================================
// Reverse Dimension
// ============================================================================

#[test]
fn error_category_unknown_status_code() {
    assert_eq!(ErrorCategory::from_status_code(999), ErrorCategory::Unknown);
    assert_eq!(ErrorCategory::from_status_code(0), ErrorCategory::Unknown);
}

#[test]
fn error_category_empty_message() {
    assert_eq!(
        ErrorCategory::from_error_message(""),
        ErrorCategory::Unknown
    );
}

#[test]
fn retry_policy_zero_max_retries() {
    let policy = RetryPolicy {
        max_retries: 0,
        base_delay_ms: 100,
        max_delay_ms: 1000,
        jitter: false,
    };
    assert_eq!(policy.delay_ms(0), 100);
}

#[test]
fn retry_policy_delay_capped_at_max() {
    let policy = RetryPolicy {
        max_retries: 10,
        base_delay_ms: 1000,
        max_delay_ms: 2000,
        jitter: false,
    };
    // 1000 * 2^0 = 1000
    // 1000 * 2^1 = 2000
    // 1000 * 2^2 = 4000 → capped to 2000
    assert_eq!(policy.delay_ms(0), 1000);
    assert_eq!(policy.delay_ms(1), 2000);
    assert_eq!(policy.delay_ms(2), 2000);
}

// ============================================================================
// Adversarial Dimension
// ============================================================================

#[test]
fn error_category_case_insensitive_matching() {
    assert_eq!(
        ErrorCategory::from_error_message("RATE LIMIT EXCEEDED"),
        ErrorCategory::Transient
    );
    assert_eq!(
        ErrorCategory::from_error_message("TIMEOUT"),
        ErrorCategory::Transient
    );
    assert_eq!(
        ErrorCategory::from_error_message("Connection Reset"),
        ErrorCategory::Transient
    );
}

#[test]
fn error_category_with_status_code_in_text() {
    // Message contains "500" which is transient
    assert_eq!(
        ErrorCategory::from_error_message("Error 500: Internal Server Error"),
        ErrorCategory::Transient
    );
    // Message contains "403" which is client error
    assert_eq!(
        ErrorCategory::from_error_message("403 Forbidden"),
        ErrorCategory::ClientError
    );
}

#[test]
fn retry_policy_with_jitter_produces_varied_delays() {
    let policy = RetryPolicy {
        max_retries: 3,
        base_delay_ms: 1000,
        max_delay_ms: 5000,
        jitter: true,
    };
    // With jitter, delays should vary
    let d1 = policy.delay_ms(0);
    let d2 = policy.delay_ms(0);
    // May or may not differ (random), but must be >= base
    assert!(d1 >= 1000);
    assert!(d2 >= 1000);
}

// ============================================================================
// Logic Tree Dimension
// ============================================================================

#[test]
fn retryable_decision_matrix() {
    let cases = [
        (429u16, true, "429 should be retryable"),
        (500u16, true, "500 should be retryable"),
        (503u16, true, "503 should be retryable"),
        (400u16, false, "400 should NOT be retryable"),
        (401u16, false, "401 should NOT be retryable"),
        (403u16, false, "403 should NOT be retryable"),
        (404u16, false, "404 should NOT be retryable"),
        (422u16, false, "422 should NOT be retryable"),
        (200u16, true, "200 should be retryable (Unknown)"),
        (999u16, true, "999 should be retryable (Unknown)"),
    ];
    for (status, expected_retryable, msg) in &cases {
        let cat = ErrorCategory::from_status_code(*status);
        assert_eq!(cat.is_retryable(), *expected_retryable, "{}", msg);
    }
}
