use std::future::Future;
use std::time::Duration;
use tracing::warn;

/// Retry an async operation with exponential backoff.
/// Useful for connecting to services that may not be ready yet at container startup.
pub async fn retry<F, Fut, T, E>(label: &str, max_attempts: u32, mut f: F) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    let mut delay = Duration::from_secs(1);
    for attempt in 1..=max_attempts {
        match f().await {
            Ok(val) => return Ok(val),
            Err(e) => {
                if attempt == max_attempts {
                    return Err(e);
                }
                warn!(
                    %attempt,
                    max_attempts,
                    label,
                    error = %e,
                    "Connection failed, retrying in {:?}",
                    delay
                );
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(10));
            }
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn test_retry_succeeds_first_attempt() {
        let attempts = AtomicU32::new(0);
        let result: Result<&str, String> = retry("test", 3, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async { Ok("ok") }
        })
        .await;
        assert_eq!(result.unwrap(), "ok");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_retry_succeeds_after_failures() {
        let attempts = Arc::new(AtomicU32::new(0));
        let a = attempts.clone();
        let result: Result<&str, String> = retry("test", 3, move || {
            let count = a.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                if count < 3 {
                    Err("not yet".to_string())
                } else {
                    Ok("finally")
                }
            }
        })
        .await;
        assert_eq!(result.unwrap(), "finally");
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_retry_fails_after_max_attempts() {
        let attempts = AtomicU32::new(0);
        let result: Result<&str, String> = retry("test", 2, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async { Err("fail".to_string()) }
        })
        .await;
        assert_eq!(result.unwrap_err(), "fail");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_retry_single_attempt_returns_error() {
        let result: Result<(), String> = retry("test", 1, || async { Err("once".to_string()) }).await;
        assert_eq!(result.unwrap_err(), "once");
    }

    use std::sync::Arc;
}
