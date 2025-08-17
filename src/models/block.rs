use crate::models::entry::Entry;
use chrono::{DateTime, Utc};

#[derive(Default, Clone)]
pub struct TokenCounts {
    pub input: u64,
    pub output: u64,
    pub cache_create: u64,
    pub cache_read: u64,
}

#[derive(Clone)]
pub struct Block {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub actual_end: DateTime<Utc>,
    pub is_active: bool,
    pub is_gap: bool,
    pub entries: Vec<Entry>,
    pub tokens: TokenCounts,
    pub cost: f64,
}
