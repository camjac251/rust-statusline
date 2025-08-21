use chrono::{DateTime, Utc};

#[derive(Clone, Debug)]
pub struct Entry {
    pub ts: DateTime<Utc>,
    pub input: u64,
    pub output: u64,
    pub cache_create: u64,
    pub cache_read: u64,
    pub web_search_requests: u64,
    pub service_tier: Option<String>,
    pub cost: f64,
    #[allow(dead_code)]
    pub model: Option<String>,
    pub session_id: Option<String>,
    #[allow(dead_code)]
    pub msg_id: Option<String>,
    #[allow(dead_code)]
    pub req_id: Option<String>,
    #[allow(dead_code)]
    pub project: Option<String>,
}
