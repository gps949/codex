use super::*;
use pretty_assertions::assert_eq;

#[test]
fn later_concurrent_failures_cannot_shorten_a_retry_after_deadline() {
    let mut cooldowns = ServiceCooldowns::default();
    let key = ServiceCooldowns::key("https://example.test/advisor", Some("test-key"));
    cooldowns.record(key, Duration::from_secs(600));
    let deadline = cooldowns.0[0].1;
    cooldowns.record(key, Duration::from_secs(5));
    assert_eq!(cooldowns.0, VecDeque::from([(key, deadline)]));
    assert!(cooldowns.blocked(key));
}

#[test]
fn retry_after_dates_and_seconds_bound_service_recovery() {
    let mut headers = HeaderMap::new();
    headers.insert(http::header::RETRY_AFTER, "120".parse().unwrap());
    assert_eq!(
        ServiceCooldowns::response_delay(StatusCode::TOO_MANY_REQUESTS, &headers),
        Some(Duration::from_secs(120))
    );
    headers.insert(http::header::RETRY_AFTER, "999999999".parse().unwrap());
    assert_eq!(
        ServiceCooldowns::response_delay(StatusCode::SERVICE_UNAVAILABLE, &headers),
        Some(MAX_DELAY)
    );
    let future = (chrono::Utc::now() + chrono::Duration::seconds(120))
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string();
    headers.insert(http::header::RETRY_AFTER, future.parse().unwrap());
    let delay = ServiceCooldowns::response_delay(StatusCode::TOO_MANY_REQUESTS, &headers).unwrap();
    assert!((Duration::from_secs(118)..=Duration::from_secs(120)).contains(&delay));
}
