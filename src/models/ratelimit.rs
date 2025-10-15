use chrono::{DateTime, Utc};

#[derive(Clone, Debug)]
pub struct RateLimitInfo {
    pub status: Option<String>,
    pub resets_at: Option<DateTime<Utc>>,
    pub fallback_available: Option<bool>,
    pub fallback_percentage: Option<f64>,
    pub rate_limit_type: Option<String>,
    pub overage_status: Option<String>,
    pub overage_resets_at: Option<DateTime<Utc>>,
    pub is_using_overage: Option<bool>,
}

impl RateLimitInfo {
    pub fn is_empty(&self) -> bool {
        self.status.is_none()
            && self.resets_at.is_none()
            && self.fallback_available.is_none()
            && self.fallback_percentage.is_none()
            && self.rate_limit_type.is_none()
            && self.overage_status.is_none()
            && self.overage_resets_at.is_none()
            && self.is_using_overage.is_none()
    }
}
