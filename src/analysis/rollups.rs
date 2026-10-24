//! Experiment metric rollups, including Instagram bio-install apportionment.

use anyhow::Context;
use chrono::NaiveDate;
use sqlx::PgPool;
use uuid::Uuid;

/// Recompute denormalized experiment metrics from snapshots, link clicks, and ASC rows.
pub async fn recompute(pool: &PgPool, experiment_id: Uuid) -> anyhow::Result<()> {
    let row = sqlx::query_as::<_, ExperimentCtx>(
        r#"
        select e.id, e.project_id, e.platform, e.asc_complete,
               tl.token as campaign_token,
               p.slug as project_slug
          from experiments e
          join projects p on p.id = e.project_id
          left join tracked_links tl on tl.plan_video_id = e.plan_video_id
         where e.id = $1
        "#,
    )
    .bind(experiment_id)
    .fetch_one(pool)
    .await
    .context("load experiment for rollup")?;

    let snap = sqlx::query_as::<_, SnapshotMetrics>(
        r#"
        select views, reach, interactions, platform_link_clicks,
               profile_visits, bio_link_taps, follows, likes, comments, saves, shares, reposts,
               avg_watch_time_ms, total_watch_time_ms, skip_rate
          from post_metric_snapshots
         where experiment_id = $1
         order by snapshot_no desc
         limit 1
        "#,
    )
    .bind(experiment_id)
    .fetch_optional(pool)
    .await?;

    let s = snap.unwrap_or_default();
    let (views, reach, interactions, platform_link_clicks) =
        (s.views, s.reach, s.interactions, s.platform_link_clicks);
    let (profile_visits, bio_link_taps, follows) = (s.profile_visits, s.bio_link_taps, s.follows);
    let (likes, comments, saves, shares, reposts) =
        (s.likes, s.comments, s.saves, s.shares, s.reposts);
    let (avg_watch_time_ms, total_watch_time_ms, skip_rate) =
        (s.avg_watch_time_ms, s.total_watch_time_ms, s.skip_rate);

    // Plays come from the flat metric values of the latest snapshot (platform-specific name).
    let reels_play_count: Option<i64> = sqlx::query_scalar(
        r#"
        select round(v.value)::bigint
          from post_metric_values v
         where v.experiment_id = $1
           and v.metric in ('ig_reels_aggregated_all_plays_count', 'fb_reels_total_plays', 'plays')
           and v.dim_key = ''
         order by v.snapshot_no desc,
                  case v.metric when 'ig_reels_aggregated_all_plays_count' then 0
                                when 'fb_reels_total_plays' then 1 else 2 end
         limit 1
        "#,
    )
    .bind(experiment_id)
    .fetch_optional(pool)
    .await?;

    let (cta_clicks, cta_clicks_unique) = if let Some(ref token) = row.campaign_token {
        let clicks: (i64, i64) = sqlx::query_as(
            r#"
            select count(*)::bigint,
                   count(distinct visitor_hash)::bigint
              from link_clicks
             where token = $1 and not is_bot
            "#,
        )
        .bind(token)
        .fetch_one(pool)
        .await?;
        (Some(clicks.0), Some(clicks.1))
    } else {
        (None, None)
    };

    // Facebook keeps per-post ASC campaign attribution. Instagram uses a shared bio campaign.
    let (asc_impressions, asc_page_views, installs, has_asc_rows) = if row.platform == "facebook" {
        if let Some(ref token) = row.campaign_token {
            load_campaign_asc(pool, token).await?
        } else {
            (None, None, None, false)
        }
    } else {
        // Instagram: no per-post ASC installs; est_installs filled by apportionment.
        (None, None, None, false)
    };

    let (cta_rate, cta_source) = if row.platform == "instagram" {
        let rate = match (profile_visits, views) {
            (Some(c), Some(v)) if v > 0 => Some(c as f64 / v as f64),
            _ => None,
        };
        (rate, Some("profile_visits".to_owned()))
    } else {
        let rate = match (cta_clicks_unique, views) {
            (Some(c), Some(v)) if v > 0 => Some(c as f64 / v as f64),
            _ => None,
        };
        (rate, Some("tracked_link".to_owned()))
    };

    let install_rate = if row.platform == "facebook" {
        match (installs, cta_clicks_unique) {
            (Some(i), Some(c)) if c > 0 => Some(i as f64 / c as f64),
            _ => None,
        }
    } else {
        None // filled after est_installs
    };

    // Temporary growth using known installs (FB); IG filled after apportionment.
    let (growth, growth_baseline) = compute_growth(pool, &row, installs).await?;

    let asc_no_data = row.asc_complete && !has_asc_rows && row.platform == "facebook";

    sqlx::query(
        r#"
        update experiments set
            views = $2,
            reach = $3,
            interactions = $4,
            platform_link_clicks = $5,
            cta_clicks = $6,
            cta_clicks_unique = $7,
            asc_impressions = $8,
            asc_page_views = $9,
            installs = $10,
            cta_rate = $11,
            install_rate = $12,
            growth = $13,
            growth_baseline = $14,
            asc_no_data = $15,
            profile_visits = $16,
            bio_link_taps = $17,
            follows = $18,
            likes = $19,
            comments = $20,
            saves = $21,
            shares = $22,
            reposts = $23,
            cta_source = $24,
            avg_watch_time_ms = $25,
            total_watch_time_ms = $26,
            skip_rate = $27,
            reels_play_count = $28,
            updated_at = now()
         where id = $1
        "#,
    )
    .bind(experiment_id)
    .bind(views)
    .bind(reach)
    .bind(interactions)
    .bind(platform_link_clicks)
    .bind(cta_clicks)
    .bind(cta_clicks_unique)
    .bind(asc_impressions)
    .bind(asc_page_views)
    .bind(installs)
    .bind(cta_rate)
    .bind(install_rate)
    .bind(growth)
    .bind(growth_baseline)
    .bind(asc_no_data)
    .bind(profile_visits)
    .bind(bio_link_taps)
    .bind(follows)
    .bind(likes)
    .bind(comments)
    .bind(saves)
    .bind(shares)
    .bind(reposts)
    .bind(cta_source)
    .bind(avg_watch_time_ms)
    .bind(total_watch_time_ms)
    .bind(skip_rate)
    .bind(reels_play_count)
    .execute(pool)
    .await?;

    if row.platform == "instagram" {
        recompute_ig_est_installs(pool, row.project_id, &row.project_slug).await?;
    }

    Ok(())
}

async fn load_campaign_asc(
    pool: &PgPool,
    token: &str,
) -> anyhow::Result<(Option<i64>, Option<i64>, Option<i64>, bool)> {
    let metrics: Vec<(String, i64)> = sqlx::query_as(
        r#"
        select metric, coalesce(sum(value), 0)::bigint
          from asc_campaign_daily
         where campaign = $1
         group by metric
        "#,
    )
    .bind(token)
    .fetch_all(pool)
    .await?;

    let has_rows: bool =
        sqlx::query_scalar("select exists(select 1 from asc_campaign_daily where campaign = $1)")
            .bind(token)
            .fetch_one(pool)
            .await?;

    let mut impressions = 0i64;
    let mut page_views = 0i64;
    let mut installs = 0i64;
    for (metric, value) in metrics {
        match metric.as_str() {
            "impressions" => impressions = value,
            "page_views" => page_views = value,
            "first_time_downloads" | "redownloads" => installs += value,
            _ => {}
        }
    }
    if has_rows {
        Ok((Some(impressions), Some(page_views), Some(installs), true))
    } else {
        Ok((None, None, None, false))
    }
}

/// Bio campaign token for Instagram link-in-bio App Store attribution.
pub fn ig_bio_campaign_token(slug: &str) -> String {
    format!("{slug}-ig-bio")
}

/// Apportion daily bio-campaign ASC installs across IG posts by bio-link-tap (or profile-visit) share.
pub async fn recompute_ig_est_installs(
    pool: &PgPool,
    project_id: Uuid,
    slug: &str,
) -> anyhow::Result<()> {
    let bio = ig_bio_campaign_token(slug);

    // Daily bio installs.
    let daily: Vec<(NaiveDate, i64)> = sqlx::query_as(
        r#"
        select date, coalesce(sum(value), 0)::bigint
          from asc_campaign_daily
         where campaign = $1
           and metric in ('first_time_downloads', 'redownloads')
         group by date
         order by date
        "#,
    )
    .bind(&bio)
    .fetch_all(pool)
    .await?;

    // All IG experiments for this project with their latest funnel metrics.
    let experiments: Vec<(Uuid, NaiveDate, Option<i64>, Option<i64>, Option<i64>)> =
        sqlx::query_as(
            r#"
            select e.id, e.publish_local_date, e.bio_link_taps, e.profile_visits, e.views
              from experiments e
             where e.project_id = $1 and e.platform = 'instagram'
            "#,
        )
        .bind(project_id)
        .fetch_all(pool)
        .await?;

    // Snapshot series for interpolating daily increments: (experiment_id, snapshot_no, bio, visits)
    // SNAPSHOT_OFFSETS_H: 1,6,24,48,72,96,120,144,168,... → snapshot_no 0..=
    // Day N after publish ≈ snapshot at offset 24*(N+1) hours when available.
    let snaps: Vec<(Uuid, i32, Option<i64>, Option<i64>)> = sqlx::query_as(
        r#"
        select experiment_id, snapshot_no, bio_link_taps, profile_visits
          from post_metric_snapshots
         where experiment_id = any($1)
         order by experiment_id, snapshot_no
        "#,
    )
    .bind(experiments.iter().map(|e| e.0).collect::<Vec<_>>())
    .fetch_all(pool)
    .await?;

    // Build per-experiment cumulative series keyed by day_offset (0 = publish day).
    // Map snapshot_no → day_offset using known offsets: [1,6,24,48,72,96,120,144,168,336,504,672]
    let offsets_h: &[i64] = &[1, 6, 24, 48, 72, 96, 120, 144, 168, 336, 504, 672];

    use std::collections::HashMap;
    let mut series: HashMap<Uuid, Vec<(i64, i64, i64)>> = HashMap::new(); // day_offset → (taps, visits)
    for (eid, snap_no, taps, visits) in &snaps {
        let idx = *snap_no as usize;
        let hours = offsets_h.get(idx).copied().unwrap_or(24 * (idx as i64 + 1));
        let day_off = (hours / 24).max(0);
        let entry = series.entry(*eid).or_default();
        // Keep max cumulative at each day_off
        if let Some(existing) = entry.iter_mut().find(|(d, _, _)| *d == day_off) {
            existing.1 = existing.1.max(taps.unwrap_or(0));
            existing.2 = existing.2.max(visits.unwrap_or(0));
        } else {
            entry.push((day_off, taps.unwrap_or(0), visits.unwrap_or(0)));
        }
    }
    for v in series.values_mut() {
        v.sort_by_key(|(d, _, _)| *d);
    }

    // Accumulate estimated installs per experiment.
    let mut est: HashMap<Uuid, (f64, f64)> = HashMap::new(); // (est_installs, share_sum)
    for (date, day_installs) in &daily {
        if *day_installs <= 0 {
            continue;
        }
        // For each experiment, increment of taps/visits attributed to this calendar day.
        let mut weights: Vec<(Uuid, f64)> = Vec::new();
        for (eid, publish_date, latest_taps, latest_visits, _) in &experiments {
            if publish_date > date {
                continue;
            }
            let day_off = (*date - *publish_date).num_days();
            if day_off < 0 {
                continue;
            }
            let (taps_inc, visits_inc) =
                daily_increment(series.get(eid), day_off, *latest_taps, *latest_visits);
            weights.push((*eid, taps_inc));
            // stash visits as fallback via negative sentinel handled below
            let _ = visits_inc;
        }

        let taps_total: f64 = weights.iter().map(|(_, w)| w.max(0.0)).sum();
        let use_visits = taps_total <= 0.0;
        if use_visits {
            weights.clear();
            for (eid, publish_date, latest_taps, latest_visits, _) in &experiments {
                if publish_date > date {
                    continue;
                }
                let day_off = (*date - *publish_date).num_days();
                if day_off < 0 {
                    continue;
                }
                let (_, visits_inc) =
                    daily_increment(series.get(eid), day_off, *latest_taps, *latest_visits);
                weights.push((*eid, visits_inc.max(0.0)));
            }
        }

        let total_w: f64 = weights.iter().map(|(_, w)| *w).sum();
        if total_w <= 0.0 {
            // No attribution signal — skip this day (don't invent equal shares).
            continue;
        }
        for (eid, w) in weights {
            if w <= 0.0 {
                continue;
            }
            let share = w / total_w;
            let add = *day_installs as f64 * share;
            let entry = est.entry(eid).or_insert((0.0, 0.0));
            entry.0 += add;
            entry.1 += share;
        }
    }

    // Write est_installs / est_install_share / install_rate / growth for each IG experiment.
    for (eid, publish_date, _, _, views) in &experiments {
        let (est_i, share) = est.get(eid).copied().unwrap_or((0.0, 0.0));
        let est_installs = if est.contains_key(eid) {
            Some(est_i.round() as i64)
        } else {
            None
        };
        let est_share = if est.contains_key(eid) {
            Some(share)
        } else {
            None
        };

        // install_rate = est_installs / bio_link_taps (or profile_visits)
        let bio_taps: Option<i64> =
            sqlx::query_scalar("select bio_link_taps from experiments where id = $1")
                .bind(eid)
                .fetch_one(pool)
                .await?;
        let profile_visits: Option<i64> =
            sqlx::query_scalar("select profile_visits from experiments where id = $1")
                .bind(eid)
                .fetch_one(pool)
                .await?;

        let install_rate = match (est_installs, bio_taps.or(profile_visits)) {
            (Some(i), Some(c)) if c > 0 => Some(i as f64 / c as f64),
            _ => None,
        };

        let ctx = ExperimentCtx {
            id: *eid,
            project_id,
            platform: "instagram".into(),
            asc_complete: false,
            campaign_token: None,
            project_slug: slug.to_owned(),
        };
        let (growth, growth_baseline) = compute_growth(pool, &ctx, est_installs).await?;

        sqlx::query(
            r#"
            update experiments set
                est_installs = $2,
                est_install_share = $3,
                install_rate = $4,
                growth = $5,
                growth_baseline = $6,
                updated_at = now()
             where id = $1
            "#,
        )
        .bind(eid)
        .bind(est_installs)
        .bind(est_share)
        .bind(install_rate)
        .bind(growth)
        .bind(growth_baseline)
        .execute(pool)
        .await?;

        let _ = (publish_date, views);
    }

    Ok(())
}

/// Estimate the metric increase attributed to a given day_offset after publish,
/// using the cumulative snapshot series. Falls back to latest totals for day_off 0.
fn daily_increment(
    series: Option<&Vec<(i64, i64, i64)>>,
    day_off: i64,
    latest_taps: Option<i64>,
    latest_visits: Option<i64>,
) -> (f64, f64) {
    let Some(s) = series else {
        // No snapshots yet — attribute latest totals only to day 0.
        if day_off == 0 {
            return (
                latest_taps.unwrap_or(0) as f64,
                latest_visits.unwrap_or(0) as f64,
            );
        }
        return (0.0, 0.0);
    };
    if s.is_empty() {
        if day_off == 0 {
            return (
                latest_taps.unwrap_or(0) as f64,
                latest_visits.unwrap_or(0) as f64,
            );
        }
        return (0.0, 0.0);
    }

    let at = |off: i64| -> (i64, i64) {
        // Largest cumulative at or before off; if none, 0.
        let mut taps = 0i64;
        let mut visits = 0i64;
        for (d, t, v) in s {
            if *d <= off {
                taps = *t;
                visits = *v;
            }
        }
        // If off is beyond last snapshot, use last.
        if off >= s.last().map(|x| x.0).unwrap_or(0)
            && let Some((_, t, v)) = s.last()
        {
            return (*t, *v);
        }
        (taps, visits)
    };

    let (t_now, v_now) = at(day_off);
    let (t_prev, v_prev) = if day_off == 0 {
        (0, 0)
    } else {
        at(day_off - 1)
    };
    (
        (t_now - t_prev).max(0) as f64,
        (v_now - v_prev).max(0) as f64,
    )
}

async fn compute_growth(
    pool: &PgPool,
    row: &ExperimentCtx,
    installs: Option<i64>,
) -> anyhow::Result<(Option<i64>, Option<String>)> {
    let Some(installs) = installs else {
        return Ok((None, None));
    };

    let median: Option<f64> = sqlx::query_scalar(
        r#"
        select percentile_cont(0.5) within group (order by coalesce(installs, est_installs))
          from experiments
         where project_id = $1
           and platform = $2
           and maturity = 'mature'
           and coalesce(installs, est_installs) is not null
           and id <> $3
        "#,
    )
    .bind(row.project_id)
    .bind(&row.platform)
    .bind(row.id)
    .fetch_one(pool)
    .await?;

    match median {
        Some(m) => Ok((
            Some(installs - m.round() as i64),
            Some("platform_median".into()),
        )),
        None => Ok((None, None)),
    }
}

#[derive(Debug, sqlx::FromRow)]
struct ExperimentCtx {
    id: Uuid,
    project_id: Uuid,
    platform: String,
    asc_complete: bool,
    campaign_token: Option<String>,
    project_slug: String,
}

#[derive(Debug, Default, sqlx::FromRow)]
struct SnapshotMetrics {
    views: Option<i64>,
    reach: Option<i64>,
    interactions: Option<i64>,
    platform_link_clicks: Option<i64>,
    profile_visits: Option<i64>,
    bio_link_taps: Option<i64>,
    follows: Option<i64>,
    likes: Option<i64>,
    comments: Option<i64>,
    saves: Option<i64>,
    shares: Option<i64>,
    reposts: Option<i64>,
    avg_watch_time_ms: Option<i64>,
    total_watch_time_ms: Option<i64>,
    skip_rate: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bio_token_format() {
        assert_eq!(ig_bio_campaign_token("acme"), "acme-ig-bio");
    }

    #[test]
    fn daily_increment_from_series() {
        let s = vec![(0, 2, 10), (1, 5, 20), (2, 5, 22)];
        assert_eq!(daily_increment(Some(&s), 0, None, None), (2.0, 10.0));
        assert_eq!(daily_increment(Some(&s), 1, None, None), (3.0, 10.0));
        assert_eq!(daily_increment(Some(&s), 2, None, None), (0.0, 2.0));
    }

    #[test]
    fn daily_increment_no_series_day0() {
        assert_eq!(daily_increment(None, 0, Some(7), Some(30)), (7.0, 30.0));
        assert_eq!(daily_increment(None, 1, Some(7), Some(30)), (0.0, 0.0));
    }
}
