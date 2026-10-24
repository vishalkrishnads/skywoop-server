use async_trait::async_trait;
use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;

use crate::agent::{AgentCtx, AgentTool};
use crate::ai::schema::strict_schema;
use crate::domain::Platform;

fn parse_uuid(s: &str, field: &str) -> Result<Uuid, String> {
    Uuid::parse_str(s).map_err(|_| format!("invalid {field}"))
}

fn parse_date_opt(s: &str, field: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| format!("invalid {field}, use YYYY-MM-DD"))
}

fn args_from<T: for<'de> Deserialize<'de>>(v: Value) -> Result<T, String> {
    serde_json::from_value(v).map_err(|e| e.to_string())
}

struct ListVideos;

#[derive(Deserialize, JsonSchema)]
struct ListVideosArgs {
    #[serde(default)]
    from_date: Option<String>,
    #[serde(default)]
    to_date: Option<String>,
    #[serde(default)]
    audience_id: Option<String>,
    #[serde(default)]
    plan_id: Option<String>,
    #[serde(default)]
    track: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

#[async_trait]
impl AgentTool for ListVideos {
    fn name(&self) -> &'static str {
        "list_videos"
    }

    fn description(&self) -> &'static str {
        "List planned/published videos for the project with post status and experiment metrics \
         (views, watch time, skip rate, CTA rate, installs)."
    }

    fn parameters_schema(&self) -> Value {
        strict_schema::<ListVideosArgs>()
    }

    async fn call(&self, ctx: &AgentCtx, args: Value) -> Result<Value, String> {
        let a: ListVideosArgs = args_from(args)?;
        let from = a
            .from_date
            .as_deref()
            .map(|s| parse_date_opt(s, "from_date"))
            .transpose()?;
        let to = a
            .to_date
            .as_deref()
            .map(|s| parse_date_opt(s, "to_date"))
            .transpose()?;
        let audience_id = a
            .audience_id
            .as_deref()
            .map(|s| parse_uuid(s, "audience_id"))
            .transpose()?;
        let plan_id = a
            .plan_id
            .as_deref()
            .map(|s| parse_uuid(s, "plan_id"))
            .transpose()?;
        let limit = a.limit.unwrap_or(50).clamp(1, 200);

        let rows: Vec<Value> = sqlx::query_scalar(
            r#"
            select jsonb_build_object(
                     'plan_video_id', pv.id,
                     'plan_id', pv.plan_id,
                     'for_date', dp.for_date,
                     'platform', dp.platform,
                     'title', pv.title,
                     'track', pv.track,
                     'audience_id', pv.audience_id,
                     'trend', pv.trend,
                     'daypart', pv.daypart,
                     'caption', pv.caption,
                     'post_status', po.status,
                     'permalink', po.permalink,
                     'experiment_id', e.id,
                     'views', e.views,
                     'avg_watch_time_ms', e.avg_watch_time_ms,
                     'skip_rate', e.skip_rate,
                     'cta_rate', e.cta_rate,
                     'installs', e.installs
                   )
              from plan_videos pv
              join daily_plans dp on dp.id = pv.plan_id
              left join posts po on po.plan_video_id = pv.id
              left join experiments e on e.plan_video_id = pv.id
             where dp.project_id = $1
               and dp.platform = $2
               and ($3::date is null or dp.for_date >= $3)
               and ($4::date is null or dp.for_date <= $4)
               and ($5::uuid is null or pv.audience_id = $5)
               and ($6::uuid is null or pv.plan_id = $6)
               and ($7::text is null or pv.track = $7)
             order by dp.for_date desc, pv.position
             limit $8
            "#,
        )
        .bind(ctx.project_id)
        .bind(ctx.platform.as_str())
        .bind(from)
        .bind(to)
        .bind(audience_id)
        .bind(plan_id)
        .bind(a.track.as_deref())
        .bind(limit)
        .fetch_all(&ctx.db)
        .await
        .map_err(|e| e.to_string())?;
        Ok(json!({ "videos": rows }))
    }
}

struct GetVideoData;

#[derive(Deserialize, JsonSchema)]
struct GetVideoDataArgs {
    plan_video_ids: Vec<String>,
    /// Any of: experiment, snapshots, metric_values, clicks, asc.
    include: Vec<String>,
}

#[async_trait]
impl AgentTool for GetVideoData {
    fn name(&self) -> &'static str {
        "get_video_data"
    }

    fn description(&self) -> &'static str {
        "Fetch experiment, metric snapshots, metric values, link clicks and ASC campaign rows \
         for plan video ids."
    }

    fn parameters_schema(&self) -> Value {
        strict_schema::<GetVideoDataArgs>()
    }

    async fn call(&self, ctx: &AgentCtx, args: Value) -> Result<Value, String> {
        let a: GetVideoDataArgs = args_from(args)?;
        if a.plan_video_ids.is_empty() {
            return Err("plan_video_ids required".into());
        }
        let mut out = Vec::new();
        for id_s in &a.plan_video_ids {
            let vid = parse_uuid(id_s, "plan_video_id")?;
            let owner: Option<(Uuid, String, Option<String>)> = sqlx::query_as(
                r#"
                select dp.project_id, dp.platform, tl.token
                  from plan_videos pv
                  join daily_plans dp on dp.id = pv.plan_id
                  left join tracked_links tl on tl.plan_video_id = pv.id
                 where pv.id = $1
                "#,
            )
            .bind(vid)
            .fetch_optional(&ctx.db)
            .await
            .map_err(|e| e.to_string())?;
            let Some((project_id, platform, campaign_token)) = owner else {
                return Err(format!("plan video {vid} not found"));
            };
            if project_id != ctx.project_id {
                return Err(format!("plan video {vid} not in project"));
            }

            let mut row = json!({ "plan_video_id": vid, "platform": platform });

            for inc in &a.include {
                match inc.as_str() {
                    "experiment" => {
                        let exp: Option<Value> = sqlx::query_scalar(
                            "select row_to_json(e)::jsonb from experiments e where e.plan_video_id = $1",
                        )
                        .bind(vid)
                        .fetch_optional(&ctx.db)
                        .await
                        .map_err(|e| e.to_string())?;
                        row["experiment"] = exp.unwrap_or(Value::Null);
                    }
                    "snapshots" => {
                        let snaps: Vec<Value> = sqlx::query_scalar(
                            r#"
                            select row_to_json(s)::jsonb
                              from post_metric_snapshots s
                              join experiments e on e.id = s.experiment_id
                             where e.plan_video_id = $1
                             order by s.snapshot_no
                            "#,
                        )
                        .bind(vid)
                        .fetch_all(&ctx.db)
                        .await
                        .map_err(|e| e.to_string())?;
                        row["snapshots"] = json!(snaps);
                    }
                    "metric_values" => {
                        let vals: Vec<Value> = sqlx::query_scalar(
                            r#"
                            select row_to_json(v)::jsonb
                              from post_metric_values v
                              join experiments e on e.id = v.experiment_id
                             where e.plan_video_id = $1
                             order by v.snapshot_no, v.metric
                            "#,
                        )
                        .bind(vid)
                        .fetch_all(&ctx.db)
                        .await
                        .map_err(|e| e.to_string())?;
                        row["metric_values"] = json!(vals);
                    }
                    "clicks" => {
                        let clicks: (i64,) = sqlx::query_as(
                            r#"
                            select count(*)::bigint
                              from link_clicks lc
                              join tracked_links tl on tl.token = lc.token
                             where tl.plan_video_id = $1 and not lc.is_bot
                            "#,
                        )
                        .bind(vid)
                        .fetch_one(&ctx.db)
                        .await
                        .map_err(|e| e.to_string())?;
                        row["clicks"] = json!({ "total": clicks.0 });
                    }
                    "asc" => {
                        let mut asc_rows = Vec::new();
                        if let Some(ref token) = campaign_token {
                            let app_id: Option<String> = sqlx::query_scalar(
                                "select app_store_app_id from projects where id = $1",
                            )
                            .bind(ctx.project_id)
                            .fetch_optional(&ctx.db)
                            .await
                            .map_err(|e| e.to_string())?;
                            if let Some(app_id) = app_id {
                                let rows: Vec<Value> = sqlx::query_scalar(
                                    r#"
                                    select row_to_json(d)::jsonb
                                      from asc_campaign_daily d
                                     where d.app_id = $1 and d.campaign = $2
                                     order by d.date desc
                                     limit 90
                                    "#,
                                )
                                .bind(app_id)
                                .bind(token)
                                .fetch_all(&ctx.db)
                                .await
                                .map_err(|e| e.to_string())?;
                                asc_rows = rows;
                            }
                        }
                        row["asc"] = json!(asc_rows);
                    }
                    other => return Err(format!("unknown include section: {other}")),
                }
            }
            out.push(row);
        }
        Ok(json!({ "videos": out }))
    }
}

struct AggregateMetrics;

#[derive(Deserialize, JsonSchema)]
struct AggregateMetricsArgs {
    group_by: String,
    metrics: Vec<String>,
    #[serde(default)]
    from_date: Option<String>,
    #[serde(default)]
    to_date: Option<String>,
}

#[async_trait]
impl AgentTool for AggregateMetrics {
    fn name(&self) -> &'static str {
        "aggregate_metrics"
    }

    fn description(&self) -> &'static str {
        "Aggregate experiment metrics (incl. watch time, skip rate, plays) grouped by audience, track, daypart, video, date, or hour."
    }

    fn parameters_schema(&self) -> Value {
        strict_schema::<AggregateMetricsArgs>()
    }

    async fn call(&self, ctx: &AgentCtx, args: Value) -> Result<Value, String> {
        let a: AggregateMetricsArgs = args_from(args)?;
        if a.metrics.is_empty() {
            return Err("metrics required".into());
        }
        for m in &a.metrics {
            match m.as_str() {
                "views" | "cta_rate" | "installs" | "install_rate" | "reach" | "interactions"
                | "cta_clicks" | "growth" | "avg_watch_time_ms" | "skip_rate"
                | "reels_play_count" => {}
                other => return Err(format!("unsupported metric: {other}")),
            }
        }
        let from = a
            .from_date
            .as_deref()
            .map(|s| parse_date_opt(s, "from_date"))
            .transpose()?;
        let to = a
            .to_date
            .as_deref()
            .map(|s| parse_date_opt(s, "to_date"))
            .transpose()?;

        // (group key expression, join clause)
        let (key, join) = match a.group_by.as_str() {
            "audience" => ("e.audience_id::text", ""),
            "track" => ("e.track", ""),
            "daypart" => (
                "pv.daypart",
                "left join plan_videos pv on pv.id = e.plan_video_id",
            ),
            "video" => ("e.plan_video_id::text", ""),
            "date" => ("e.publish_local_date::text", ""),
            "hour" => (
                "extract(hour from e.published_at at time zone 'UTC')::text",
                "",
            ),
            other => return Err(format!("unsupported group_by: {other}")),
        };
        let sql = format!(
            r#"
            select {key} as group_key,
                   avg(e.views)::float8 as views,
                   avg(e.cta_rate)::float8 as cta_rate,
                   sum(e.installs)::float8 as installs,
                   avg(e.install_rate)::float8 as install_rate,
                   avg(e.reach)::float8 as reach,
                   avg(e.interactions)::float8 as interactions,
                   avg(e.cta_clicks)::float8 as cta_clicks,
                   avg(e.growth)::float8 as growth,
                   avg(e.avg_watch_time_ms)::float8 as avg_watch_time_ms,
                   avg(e.skip_rate)::float8 as skip_rate,
                   avg(e.reels_play_count)::float8 as reels_play_count
              from experiments e
              {join}
             where e.project_id = $1
               and e.platform = $2
               and ($3::date is null or e.publish_local_date >= $3)
               and ($4::date is null or e.publish_local_date <= $4)
             group by 1
             order by 1
            "#
        );

        let rows: Vec<sqlx::postgres::PgRow> = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(ctx.project_id)
            .bind(ctx.platform.as_str())
            .bind(from)
            .bind(to)
            .fetch_all(&ctx.db)
            .await
            .map_err(|e| e.to_string())?;

        let mut groups = Vec::new();
        for row in rows {
            let key: Option<String> = row.try_get("group_key").map_err(|e| e.to_string())?;
            let key = key.unwrap_or_default();
            let mut obj = json!({ "group_key": key });
            for m in &a.metrics {
                let v: Option<f64> = row.try_get(m.as_str()).map_err(|e| e.to_string())?;
                obj[m] = json!(v);
            }
            groups.push(obj);
        }
        Ok(json!({ "groups": groups }))
    }
}

struct MetricCatalog;

#[derive(Deserialize, JsonSchema)]
struct MetricCatalogArgs {}

#[async_trait]
impl AgentTool for MetricCatalog {
    fn name(&self) -> &'static str {
        "metric_catalog"
    }

    fn description(&self) -> &'static str {
        "Distinct metric names and dimension keys from post_metric_values for this project."
    }

    fn parameters_schema(&self) -> Value {
        strict_schema::<MetricCatalogArgs>()
    }

    async fn call(&self, ctx: &AgentCtx, _args: Value) -> Result<Value, String> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            r#"
            select distinct v.metric, v.dim_key
              from post_metric_values v
              join experiments e on e.id = v.experiment_id
             where e.project_id = $1
             order by v.metric, v.dim_key
            "#,
        )
        .bind(ctx.project_id)
        .fetch_all(&ctx.db)
        .await
        .map_err(|e| e.to_string())?;
        Ok(json!({
            "metrics": rows.iter().map(|(m, d)| json!({ "metric": m, "dim_key": d })).collect::<Vec<_>>()
        }))
    }
}

struct AccountMetrics;

#[derive(Deserialize, JsonSchema)]
struct AccountMetricsArgs {
    #[serde(default)]
    metrics: Option<Vec<String>>,
    #[serde(default)]
    from_date: Option<String>,
    #[serde(default)]
    to_date: Option<String>,
}

#[async_trait]
impl AgentTool for AccountMetrics {
    fn name(&self) -> &'static str {
        "account_metrics"
    }

    fn description(&self) -> &'static str {
        "Daily account-level metrics for the project platform."
    }

    fn parameters_schema(&self) -> Value {
        strict_schema::<AccountMetricsArgs>()
    }

    async fn call(&self, ctx: &AgentCtx, args: Value) -> Result<Value, String> {
        let a: AccountMetricsArgs = args_from(args)?;
        let from = a
            .from_date
            .as_deref()
            .map(|s| parse_date_opt(s, "from_date"))
            .transpose()?;
        let to = a
            .to_date
            .as_deref()
            .map(|s| parse_date_opt(s, "to_date"))
            .transpose()?;

        let rows: Vec<(NaiveDate, String, String, String, Option<f64>)> = sqlx::query_as(
            r#"
            select local_date, metric, dim_key, dim_value, value
              from account_metric_daily
             where project_id = $1 and platform = $2
               and ($3::date is null or local_date >= $3)
               and ($4::date is null or local_date <= $4)
               and (
                 $5::text[] is null
                 or metric = any($5)
               )
             order by local_date desc, metric
            "#,
        )
        .bind(ctx.project_id)
        .bind(ctx.platform.as_str())
        .bind(from)
        .bind(to)
        .bind(a.metrics.as_ref())
        .fetch_all(&ctx.db)
        .await
        .map_err(|e| e.to_string())?;

        Ok(json!({
            "rows": rows.iter().map(|r| json!({
                "date": r.0,
                "metric": r.1,
                "dim_key": r.2,
                "dim_value": r.3,
                "value": r.4,
            })).collect::<Vec<_>>()
        }))
    }
}

struct AppStoreData;

#[derive(Deserialize, JsonSchema)]
struct AppStoreDataArgs {
    #[serde(default)]
    from_date: Option<String>,
    #[serde(default)]
    to_date: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

#[async_trait]
impl AgentTool for AppStoreData {
    fn name(&self) -> &'static str {
        "app_store_data"
    }

    fn description(&self) -> &'static str {
        "ASC campaign and source daily metrics for the project's App Store app."
    }

    fn parameters_schema(&self) -> Value {
        strict_schema::<AppStoreDataArgs>()
    }

    async fn call(&self, ctx: &AgentCtx, args: Value) -> Result<Value, String> {
        let a: AppStoreDataArgs = args_from(args)?;
        let app_id: Option<String> =
            sqlx::query_scalar("select app_store_app_id from projects where id = $1")
                .bind(ctx.project_id)
                .fetch_optional(&ctx.db)
                .await
                .map_err(|e| e.to_string())?;
        let Some(app_id) = app_id else {
            return Ok(json!({ "app_store_app_id": null, "rows": [] }));
        };
        let from = a
            .from_date
            .as_deref()
            .map(|s| parse_date_opt(s, "from_date"))
            .transpose()?;
        let to = a
            .to_date
            .as_deref()
            .map(|s| parse_date_opt(s, "to_date"))
            .transpose()?;
        let use_source = a.source.as_deref() == Some("source");

        let rows: Vec<Value> = if use_source {
            sqlx::query_scalar(
                r#"
                select row_to_json(d)::jsonb
                  from asc_source_daily d
                 where d.app_id = $1
                   and ($2::date is null or d.date >= $2)
                   and ($3::date is null or d.date <= $3)
                 order by d.date desc
                 limit 500
                "#,
            )
            .bind(&app_id)
            .bind(from)
            .bind(to)
            .fetch_all(&ctx.db)
            .await
            .map_err(|e| e.to_string())?
        } else {
            sqlx::query_scalar(
                r#"
                select row_to_json(d)::jsonb
                  from asc_campaign_daily d
                 where d.app_id = $1
                   and ($2::date is null or d.date >= $2)
                   and ($3::date is null or d.date <= $3)
                 order by d.date desc
                 limit 500
                "#,
            )
            .bind(&app_id)
            .bind(from)
            .bind(to)
            .fetch_all(&ctx.db)
            .await
            .map_err(|e| e.to_string())?
        };

        Ok(json!({ "app_store_app_id": app_id, "rows": rows }))
    }
}

struct QueryDiary;

#[derive(Deserialize, JsonSchema)]
struct QueryDiaryArgs {
    #[serde(default)]
    platform: Option<String>,
    #[serde(default)]
    plan_video_id: Option<String>,
    #[serde(default)]
    from_date: Option<String>,
    #[serde(default)]
    to_date: Option<String>,
    /// experiment | platform_trend | manual | video_daily | day_summary | plan_retro
    #[serde(default)]
    kind: Option<String>,
}

#[async_trait]
impl AgentTool for QueryDiary {
    fn name(&self) -> &'static str {
        "query_diary"
    }

    fn description(&self) -> &'static str {
        "Query diary entries for the project with optional filters."
    }

    fn parameters_schema(&self) -> Value {
        strict_schema::<QueryDiaryArgs>()
    }

    async fn call(&self, ctx: &AgentCtx, args: Value) -> Result<Value, String> {
        let a: QueryDiaryArgs = args_from(args)?;
        let platform = match a.platform.as_deref() {
            None => None,
            Some(p) => Some(
                Platform::parse(p)
                    .ok_or_else(|| format!("unknown platform: {p}"))?
                    .as_str()
                    .to_string(),
            ),
        };
        let plan_video_id = a
            .plan_video_id
            .as_deref()
            .map(|s| parse_uuid(s, "plan_video_id"))
            .transpose()?;
        let from = a
            .from_date
            .as_deref()
            .map(|s| parse_date_opt(s, "from_date"))
            .transpose()?;
        let to = a
            .to_date
            .as_deref()
            .map(|s| parse_date_opt(s, "to_date"))
            .transpose()?;

        let rows: Vec<Value> = sqlx::query_scalar(
            r#"
            select row_to_json(d)::jsonb
              from diary_entries d
             where d.project_id = $1
               and ($2::text is null or d.platform = $2)
               and ($3::uuid is null or d.plan_video_id = $3)
               and ($4::date is null or d.entry_date >= $4)
               and ($5::date is null or d.entry_date <= $5)
               and ($6::text is null or d.kind = $6)
             order by d.entry_date desc, d.created_at desc
             limit 200
            "#,
        )
        .bind(ctx.project_id)
        .bind(platform.as_deref())
        .bind(plan_video_id)
        .bind(from)
        .bind(to)
        .bind(a.kind.as_deref())
        .fetch_all(&ctx.db)
        .await
        .map_err(|e| e.to_string())?;

        Ok(json!({ "entries": rows }))
    }
}

struct ListPlans;

#[derive(Deserialize, JsonSchema)]
struct ListPlansArgs {
    #[serde(default)]
    limit: Option<i64>,
}

#[async_trait]
impl AgentTool for ListPlans {
    fn name(&self) -> &'static str {
        "list_plans"
    }

    fn description(&self) -> &'static str {
        "Recent daily plans with video counts, post outcomes and status."
    }

    fn parameters_schema(&self) -> Value {
        strict_schema::<ListPlansArgs>()
    }

    async fn call(&self, ctx: &AgentCtx, args: Value) -> Result<Value, String> {
        let a: ListPlansArgs = args_from(args)?;
        let limit = a.limit.unwrap_or(14).clamp(1, 60);

        let rows: Vec<(Uuid, NaiveDate, String, String, i64, i64)> = sqlx::query_as(
            r#"
            select dp.id, dp.for_date, dp.status, dp.greeting,
                   count(pv.id) as video_count,
                   count(po.id) filter (where po.status = 'published') as published_count
              from daily_plans dp
              left join plan_videos pv on pv.plan_id = dp.id
              left join posts po on po.plan_video_id = pv.id
             where dp.project_id = $1 and dp.platform = $2
             group by dp.id
             order by dp.for_date desc
             limit $3
            "#,
        )
        .bind(ctx.project_id)
        .bind(ctx.platform.as_str())
        .bind(limit)
        .fetch_all(&ctx.db)
        .await
        .map_err(|e| e.to_string())?;

        Ok(json!({
            "plans": rows.iter().map(|r| json!({
                "plan_id": r.0,
                "for_date": r.1,
                "status": r.2,
                "greeting": r.3,
                "video_count": r.4,
                "published_count": r.5,
            })).collect::<Vec<_>>()
        }))
    }
}

struct ReadPastPlan;

#[derive(Deserialize, JsonSchema)]
struct ReadPastPlanArgs {
    /// Local plan date YYYY-MM-DD.
    date: String,
    /// outline (default) | section | lines | all
    #[serde(default)]
    part: Option<String>,
    /// Section anchor for part=section (e.g. video-1).
    #[serde(default)]
    anchor: Option<String>,
    /// 1-based start line for part=lines.
    #[serde(default)]
    start_line: Option<i64>,
    /// 1-based end line for part=lines (inclusive, max 200 lines).
    #[serde(default)]
    end_line: Option<i64>,
}

#[async_trait]
impl AgentTool for ReadPastPlan {
    fn name(&self) -> &'static str {
        "read_past_plan"
    }

    fn description(&self) -> &'static str {
        "Read a previous day's plan markdown. Prefer part=outline, then part=section with an \
         anchor, or part=lines with a range. Use part=all only when you need the whole file."
    }

    fn parameters_schema(&self) -> Value {
        strict_schema::<ReadPastPlanArgs>()
    }

    async fn call(&self, ctx: &AgentCtx, args: Value) -> Result<Value, String> {
        let a: ReadPastPlanArgs = args_from(args)?;
        let for_date = parse_date_opt(&a.date, "date")?;
        let part = match a.part.as_deref() {
            None | Some("") => crate::plan_format::PlanReadPart::Outline,
            Some(s) => crate::plan_format::PlanReadPart::parse(s)
                .ok_or_else(|| format!("part must be outline, section, lines, or all; got {s}"))?,
        };

        let row: Option<(Uuid, String, String)> = sqlx::query_as(
            r#"
            select id, status, markdown
              from daily_plans
             where project_id = $1 and platform = $2 and for_date = $3
            "#,
        )
        .bind(ctx.project_id)
        .bind(ctx.platform.as_str())
        .bind(for_date)
        .fetch_optional(&ctx.db)
        .await
        .map_err(|e| e.to_string())?;

        let Some((plan_id, status, markdown)) = row else {
            return Ok(json!({
                "date": for_date,
                "found": false,
                "error": format!("No plan for {for_date} on this platform."),
            }));
        };

        if markdown.trim().is_empty() {
            return Ok(json!({
                "date": for_date,
                "plan_id": plan_id,
                "status": status,
                "found": true,
                "error": "Plan exists but markdown is empty.",
            }));
        }

        let mut slice = crate::plan_format::read_plan_part(
            &markdown,
            part,
            a.anchor.as_deref(),
            a.start_line.map(|n| n.max(1) as usize),
            a.end_line.map(|n| n.max(1) as usize),
        );
        if let Some(obj) = slice.as_object_mut() {
            obj.insert("date".into(), json!(for_date));
            obj.insert("plan_id".into(), json!(plan_id));
            obj.insert("status".into(), json!(status));
            obj.insert("found".into(), json!(true));
        }
        Ok(slice)
    }
}

struct GetTrends;

#[derive(Deserialize, JsonSchema)]
struct GetTrendsArgs {
    #[serde(default)]
    date: Option<String>,
}

#[async_trait]
impl AgentTool for GetTrends {
    fn name(&self) -> &'static str {
        "get_trends"
    }

    fn description(&self) -> &'static str {
        "Load cached trend snapshot items for a local date."
    }

    fn parameters_schema(&self) -> Value {
        strict_schema::<GetTrendsArgs>()
    }

    async fn call(&self, ctx: &AgentCtx, args: Value) -> Result<Value, String> {
        let a: GetTrendsArgs = args_from(args)?;
        let local_date = match a.date.as_deref() {
            Some(s) => parse_date_opt(s, "date")?,
            None => chrono::Utc::now().date_naive(),
        };

        let row: Option<(String, Value)> = sqlx::query_as(
            r#"
            select status, items
              from trend_snapshots
             where project_id = $1 and platform = $2 and local_date = $3
            "#,
        )
        .bind(ctx.project_id)
        .bind(ctx.platform.as_str())
        .bind(local_date)
        .fetch_optional(&ctx.db)
        .await
        .map_err(|e| e.to_string())?;

        Ok(match row {
            Some((status, items)) => {
                json!({ "date": local_date, "status": status, "items": items })
            }
            None => json!({ "date": local_date, "status": "missing", "items": [] }),
        })
    }
}

pub fn analytics_tools() -> Vec<Box<dyn AgentTool>> {
    vec![
        Box::new(ListVideos),
        Box::new(GetVideoData),
        Box::new(AggregateMetrics),
        Box::new(MetricCatalog),
        Box::new(AccountMetrics),
        Box::new(AppStoreData),
        Box::new(QueryDiary),
        Box::new(ListPlans),
        Box::new(ReadPastPlan),
        Box::new(GetTrends),
    ]
}
