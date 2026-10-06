//! Bounds repeated failures for a service and credential without retaining either in diagnostics.

use http::HeaderMap;
use http::StatusCode;
use sha2::Digest;
use sha2::Sha256;
use std::collections::VecDeque;
use std::time::Duration;
use std::time::Instant;

const MAX_SERVICES: usize = 32;
const MAX_DELAY: Duration = Duration::from_secs(600);

pub(super) type ServiceKey = [u8; 32];

#[derive(Default)]
pub(super) struct ServiceCooldowns(VecDeque<(ServiceKey, Instant)>);

impl ServiceCooldowns {
    pub(super) fn key(endpoint: &str, credential: Option<&str>) -> ServiceKey {
        let mut digest = Sha256::new();
        digest.update(b"codex-advisor-service-v1\0");
        digest.update(endpoint.len().to_be_bytes());
        digest.update(endpoint.as_bytes());
        digest.update(Sha256::digest(credential.unwrap_or_default().as_bytes()));
        digest.finalize().into()
    }

    pub(super) fn blocked(&mut self, key: ServiceKey) -> bool {
        let now = Instant::now();
        self.0.retain(|(_, until)| *until > now);
        self.0.iter().any(|(service, _)| *service == key)
    }

    pub(super) fn record(&mut self, key: ServiceKey, delay: Duration) {
        let deadline = Instant::now() + delay.min(MAX_DELAY);
        let deadline = self
            .0
            .iter()
            .find(|(service, _)| *service == key)
            .map_or(deadline, |(_, previous)| deadline.max(*previous));
        self.0.retain(|(service, _)| *service != key);
        if self.0.len() >= MAX_SERVICES {
            self.0.pop_front();
        }
        self.0.push_back((key, deadline));
    }

    pub(super) fn response_delay(status: StatusCode, headers: &HeaderMap) -> Option<Duration> {
        let default = match status.as_u16() {
            401 | 403 => Duration::from_secs(300),
            429 => Duration::from_secs(60),
            500..=599 => Duration::from_secs(30),
            _ => return None,
        };
        let retry_after = headers
            .get(http::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| {
                value
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .map(Duration::from_secs)
                    .or_else(|| {
                        chrono::DateTime::parse_from_rfc2822(value)
                            .ok()
                            .and_then(|until| {
                                (until.with_timezone(&chrono::Utc) - chrono::Utc::now())
                                    .to_std()
                                    .ok()
                            })
                    })
            });
        Some(
            retry_after
                .unwrap_or(default)
                .clamp(Duration::from_secs(1), MAX_DELAY),
        )
    }
}

#[cfg(test)]
#[path = "decision_advisor_cooldown_tests.rs"]
mod tests;
