pub mod handlers;
pub mod queue;
pub mod scheduler;
pub mod worker;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::Platform;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum UserJob {
    /// Publish a scheduled video (IG Reels / FB Reels). `attempt` disambiguates retries.
    PublishVideo {
        post_id: Uuid,
        attempt: i32,
    },
    PollIgContainer {
        post_id: Uuid,
        poll_no: i32,
    },
    /// Poll a Facebook Reel's processing/publishing state.
    PollFbVideo {
        post_id: Uuid,
        poll_no: i32,
    },
    /// Delete the rendered MP4 from blob storage after a successful publish.
    CleanupPostMedia {
        post_id: Uuid,
    },
    DiscoverProduct {
        discovery_id: Uuid,
    },
    /// Classify project industry (Luna) and seed posting schedules.
    ClassifyIndustry {
        project_id: Uuid,
    },
    /// Move an Inbox upload into Context/<Category>/ via vision.
    CategorizeAsset {
        asset_id: Uuid,
        /// Optional poster frame asset (required for good video categorization).
        poster_asset_id: Option<Uuid>,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum DataJob {
    FetchPostInsights {
        experiment_id: Uuid,
        snapshot_no: i32,
    },
    FetchAccountInsights {
        project_id: Uuid,
        platform: Platform,
        date: NaiveDate,
    },
    AscDailyIngest {
        app_id: String,
        date: NaiveDate,
    },
    EvaluateMaturity {
        hour: DateTime<Utc>,
    },
    AnalyzeExperiment {
        experiment_id: Uuid,
    },
    DailyCycle {
        project_id: Uuid,
        platform: Platform,
        local_date: NaiveDate,
    },
    /// Prefetch Google + web trends for a project/platform local day.
    RefreshTrends {
        project_id: Uuid,
        platform: Platform,
        local_date: NaiveDate,
        country: String,
    },
    /// Draft the day's markdown plan with the PlanDay agent.
    DraftPlan {
        project_id: Uuid,
        platform: Platform,
        for_date: NaiveDate,
        /// Whether PlanDay may decide to post nothing today. False when the user asked for videos.
        #[serde(default = "default_true")]
        allow_no_op: bool,
    },
    /// Weekly agent review of posting schedule against audience activity.
    ReviewPostingSchedule {
        project_id: Uuid,
        platform: Platform,
    },
}

fn default_true() -> bool {
    true
}

#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[error(transparent)]
    Retry(#[from] anyhow::Error),
    /// Retry after a Meta (or similar) rate-limit window.
    #[error("{source}")]
    RetryAfter {
        minutes: i64,
        #[source]
        source: anyhow::Error,
    },
    #[error("{0}")]
    Permanent(#[source] anyhow::Error),
}

impl JobError {
    pub fn permanent(e: impl Into<anyhow::Error>) -> Self {
        Self::Permanent(e.into())
    }

    pub fn retry_after(minutes: i64, e: impl Into<anyhow::Error>) -> Self {
        Self::RetryAfter {
            minutes,
            source: e.into(),
        }
    }

    pub fn is_permanent(&self) -> bool {
        matches!(self, Self::Permanent(_))
    }

    /// Optional explicit delay for retryable errors (e.g. Meta rate limits).
    pub fn retry_delay_minutes(&self) -> Option<i64> {
        match self {
            Self::RetryAfter { minutes, .. } => Some(*minutes),
            _ => None,
        }
    }

    pub fn from_ai(e: crate::ai::AiError) -> Self {
        use crate::ai::AiError;
        match e {
            AiError::ContentFiltered => Self::permanent(anyhow::anyhow!(e)),
            AiError::NoCredentials => Self::permanent(anyhow::anyhow!(e)),
            AiError::Http { status, .. } if status == 429 || (500..600).contains(&status) => {
                Self::Retry(anyhow::anyhow!(e))
            }
            AiError::Incomplete(_) | AiError::Transport(_) => Self::Retry(anyhow::anyhow!(e)),
            other => Self::permanent(anyhow::anyhow!(other)),
        }
    }
}

pub use queue::{DataJobQueue, UserJobQueue};

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn user_and_data_job_kind_tags_are_disjoint() {
        let user = UserJob::CleanupPostMedia {
            post_id: Uuid::nil(),
        };
        let data = DataJob::DailyCycle {
            project_id: Uuid::nil(),
            platform: Platform::Instagram,
            local_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        };
        let u = serde_json::to_value(&user).unwrap();
        let d = serde_json::to_value(&data).unwrap();

        let user_kinds = [
            "publish_video",
            "poll_ig_container",
            "poll_fb_video",
            "cleanup_post_media",
            "discover_product",
            "classify_industry",
            "categorize_asset",
        ];
        let data_kinds = [
            "fetch_post_insights",
            "fetch_account_insights",
            "asc_daily_ingest",
            "evaluate_maturity",
            "analyze_experiment",
            "daily_cycle",
            "refresh_trends",
            "draft_plan",
            "review_posting_schedule",
        ];
        for k in user_kinds {
            assert!(
                !data_kinds.contains(&k),
                "overlap on kind tag {k} — scheduler must never enqueue UserJob kinds"
            );
        }
        assert_eq!(u["kind"], "cleanup_post_media");
        assert_eq!(d["kind"], "daily_cycle");
    }

    #[test]
    fn draft_plan_payload_defaults_allow_no_op() {
        let old = serde_json::json!({
            "kind": "draft_plan",
            "payload": {
                "project_id": Uuid::nil(),
                "platform": "instagram",
                "for_date": "2026-10-24",
            }
        });
        match serde_json::from_value::<DataJob>(old).unwrap() {
            DataJob::DraftPlan { allow_no_op, .. } => assert!(allow_no_op),
            other => panic!("unexpected {other:?}"),
        }
    }
}
