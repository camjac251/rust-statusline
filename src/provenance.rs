use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionCostSource {
    TranscriptResult,
    HookCost,
    TranscriptScan,
}

impl SessionCostSource {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionCostSource::TranscriptResult => "transcript_result",
            SessionCostSource::HookCost => "hook_cost",
            SessionCostSource::TranscriptScan => "transcript_scan",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TodayCostSource {
    DbGlobalUsage,
    ScanFallback,
}

impl TodayCostSource {
    pub fn as_str(self) -> &'static str {
        match self {
            TodayCostSource::DbGlobalUsage => "db_global_usage",
            TodayCostSource::ScanFallback => "scan_fallback",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingSource {
    EnvOverride,
    Embedded,
    StaticFallback,
    FamilyHeuristic,
    Unavailable,
}

impl PricingSource {
    pub fn as_str(self) -> &'static str {
        match self {
            PricingSource::EnvOverride => "env_override",
            PricingSource::Embedded => "embedded",
            PricingSource::StaticFallback => "static_fallback",
            PricingSource::FamilyHeuristic => "family_heuristic",
            PricingSource::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CostProvenance {
    pub session_cost: SessionCostSource,
    pub today_cost: TodayCostSource,
    pub pricing: PricingSource,
}
