//! Async retry loop over [`zen_core::retry::BACKOFF_SECS`].

use std::future::Future;
use std::time::Duration;

/// Retry `f` up to `max_attempts` times, sleeping the shared backoff
/// schedule between attempts, but only when `retryable` accepts the error.
/// Non-retryable errors fail immediately; the last retryable error is
/// returned on exhaustion.
pub async fn retry_with_backoff<T, E, F, Fut>(
    max_attempts: usize,
    retryable: impl Fn(&E) -> bool,
    mut f: F,
) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let schedule = zen_core::retry::BACKOFF_SECS;
    for attempt in 0..max_attempts {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                let can_retry = attempt + 1 < max_attempts && retryable(&e);
                if !can_retry {
                    return Err(e);
                }
                let delay = Duration::from_secs(schedule[attempt.min(schedule.len() - 1)]);
                tokio::time::sleep(delay).await;
            }
        }
    }
    unreachable!("the loop always returns on the final attempt")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn succeeds_first_try() {
        let calls = AtomicUsize::new(0);
        let out = retry_with_backoff(
            3,
            |_| true,
            || async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, String>(42)
            },
        )
        .await
        .unwrap();
        assert_eq!(out, 42);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retries_retryable_then_succeeds() {
        let calls = AtomicUsize::new(0);
        let out = retry_with_backoff(
            3,
            |e| e == "retry",
            || async {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err("retry".to_string())
                } else {
                    Ok::<_, String>("ok")
                }
            },
        )
        .await
        .unwrap();
        assert_eq!(out, "ok");
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn non_retryable_fails_immediately() {
        let calls = AtomicUsize::new(0);
        let err = retry_with_backoff(
            3,
            |e| e == "retry",
            || async {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>("fatal".to_string())
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err, "fatal");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
