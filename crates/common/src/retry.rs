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
