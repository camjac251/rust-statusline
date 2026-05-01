use chrono::{DateTime, Utc};

#[derive(Debug, Clone)]
pub struct PromptCacheInfo {
    pub last_response_at: DateTime<Utc>,
    pub ttl_seconds: u64,
    pub now: DateTime<Utc>,
}

impl PromptCacheInfo {
    pub fn expires_at(&self) -> DateTime<Utc> {
        self.last_response_at + chrono::TimeDelta::seconds(self.ttl_seconds as i64)
    }

    pub fn remaining_seconds(&self) -> i64 {
        (self.expires_at() - self.now).num_seconds().max(0)
    }

    pub fn age_seconds(&self) -> i64 {
        (self.now - self.last_response_at).num_seconds().max(0)
    }

    pub fn percent_remaining(&self) -> f64 {
        if self.ttl_seconds == 0 {
            return 0.0;
        }
        (self.remaining_seconds() as f64 / self.ttl_seconds as f64 * 100.0).clamp(0.0, 100.0)
    }
}
