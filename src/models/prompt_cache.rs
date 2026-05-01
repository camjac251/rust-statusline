use chrono::{DateTime, Utc};

pub const PROMPT_CACHE_5M_TTL_SECONDS: u64 = 300;
pub const PROMPT_CACHE_1H_TTL_SECONDS: u64 = 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptCacheBucketKind {
    FiveMinute,
    OneHour,
    Unknown,
}

impl PromptCacheBucketKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FiveMinute => "5m",
            Self::OneHour => "1h",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PromptCacheBucketInfo {
    pub kind: PromptCacheBucketKind,
    pub created_at: DateTime<Utc>,
    pub ttl_seconds: u64,
    pub input_tokens: u64,
}

impl PromptCacheBucketInfo {
    pub fn expires_at(&self) -> DateTime<Utc> {
        self.created_at + chrono::TimeDelta::seconds(self.ttl_seconds as i64)
    }

    pub fn remaining_seconds_at(&self, now: DateTime<Utc>) -> i64 {
        (self.expires_at() - now).num_seconds().max(0)
    }

    pub fn age_seconds_at(&self, now: DateTime<Utc>) -> i64 {
        (now - self.created_at).num_seconds().max(0)
    }

    pub fn percent_remaining_at(&self, now: DateTime<Utc>) -> f64 {
        if self.ttl_seconds == 0 {
            return 0.0;
        }
        (self.remaining_seconds_at(now) as f64 / self.ttl_seconds as f64 * 100.0).clamp(0.0, 100.0)
    }
}

#[derive(Debug, Clone)]
pub struct PromptCacheInfo {
    pub buckets: Vec<PromptCacheBucketInfo>,
    pub last_activity_at: DateTime<Utc>,
    pub cache_read_input_tokens: u64,
    pub now: DateTime<Utc>,
}

impl PromptCacheInfo {
    pub fn set_unknown_ttl_seconds(&mut self, ttl_seconds: u64) {
        for bucket in &mut self.buckets {
            if bucket.kind == PromptCacheBucketKind::Unknown {
                bucket.ttl_seconds = ttl_seconds;
            }
        }
    }

    pub fn primary_bucket(&self) -> Option<&PromptCacheBucketInfo> {
        self.buckets
            .iter()
            .filter(|bucket| bucket.remaining_seconds_at(self.now) > 0)
            .min_by_key(|bucket| bucket.remaining_seconds_at(self.now))
            .or_else(|| self.buckets.first())
    }

    pub fn age_seconds(&self) -> i64 {
        (self.now - self.last_activity_at).num_seconds().max(0)
    }

    pub fn remaining_seconds(&self) -> i64 {
        self.primary_bucket()
            .map(|bucket| bucket.remaining_seconds_at(self.now))
            .unwrap_or(0)
    }

    pub fn ttl_seconds(&self) -> Option<u64> {
        self.primary_bucket().map(|bucket| bucket.ttl_seconds)
    }

    pub fn expires_at(&self) -> Option<DateTime<Utc>> {
        self.primary_bucket().map(PromptCacheBucketInfo::expires_at)
    }

    pub fn percent_remaining(&self) -> f64 {
        self.primary_bucket()
            .map(|bucket| bucket.percent_remaining_at(self.now))
            .unwrap_or(0.0)
    }
}
