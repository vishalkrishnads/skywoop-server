//! Video project timelines: JSON-Patch operations with inverse generation, revisions, undo.
//!
//! The timeline is an opaque JSON document owned by the browser editor (the server only
//! requires an object with a `tracks` array). Edits arrive as RFC 6902 style operations
//! (`add`, `remove`, `replace`, `move`, `copy`, `test`); applying them yields inverse
//! operations stored with the revision so undo is a plain `apply`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::domain::new_id;
use crate::error::{AppError, AppResult};

pub const MAX_TIMELINE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_OPS_PER_APPLY: usize = 500;

/// Timeline for a fresh video project (vertical 9:16, 30 fps).
pub fn empty_timeline() -> Value {
    json!({
        "version": 1,
        "fps": 30,
        "width": 1080,
        "height": 1920,
        "duration_ms": 0,
        "tracks": [
            { "id": "video-1", "kind": "video", "clips": [] },
            { "id": "overlay-1", "kind": "overlay", "clips": [] },
            { "id": "text-1", "kind": "text", "clips": [] },
            { "id": "audio-1", "kind": "audio", "clips": [] }
        ]
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct PatchOp {
    /// add | remove | replace | move | copy | test
    pub op: String,
    /// RFC 6901 JSON pointer. The empty string addresses the whole document.
    pub path: String,
    /// Source pointer for `move` / `copy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

fn parse_pointer(p: &str) -> Result<Vec<String>, String> {
    if p.is_empty() {
        return Ok(Vec::new());
    }
    if !p.starts_with('/') {
        return Err(format!("invalid JSON pointer \"{p}\""));
    }
    Ok(p[1..]
        .split('/')
        .map(|t| t.replace("~1", "/").replace("~0", "~"))
        .collect())
}

fn pointer_string(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|t| format!("/{}", t.replace('~', "~0").replace('/', "~1")))
        .collect()
}

fn array_index(tok: &str, len: usize, allow_end: bool) -> Result<usize, String> {
    if allow_end && tok == "-" {
        return Ok(len);
    }
    let i: usize = tok
        .parse()
        .map_err(|_| format!("invalid array index \"{tok}\""))?;
    let max = if allow_end {
        len
    } else {
        len.saturating_sub(1)
    };
    if (allow_end && i > max) || (!allow_end && i >= len) {
        return Err(format!("array index {i} out of range (len {len})"));
    }
    Ok(i)
}

fn navigate<'a>(doc: &'a Value, tokens: &[String]) -> Result<&'a Value, String> {
    let mut cur = doc;
    for t in tokens {
        cur = match cur {
            Value::Object(m) => m.get(t).ok_or_else(|| format!("path not found: {t}"))?,
            Value::Array(a) => {
                let i = array_index(t, a.len(), false)?;
                &a[i]
            }
            _ => return Err(format!("cannot descend into scalar at \"{t}\"")),
        };
    }
    Ok(cur)
}

fn navigate_mut<'a>(doc: &'a mut Value, tokens: &[String]) -> Result<&'a mut Value, String> {
    let mut cur = doc;
    for t in tokens {
        cur = match cur {
            Value::Object(m) => m.get_mut(t).ok_or_else(|| format!("path not found: {t}"))?,
            Value::Array(a) => {
                let i = array_index(t, a.len(), false)?;
                &mut a[i]
            }
            _ => return Err(format!("cannot descend into scalar at \"{t}\"")),
        };
    }
    Ok(cur)
}

/// Insert `value` at `tokens` (RFC 6902 `add`). Returns the inverse operation.
fn do_add(doc: &mut Value, tokens: &[String], value: Value) -> Result<PatchOp, String> {
    let Some((last, parent_tokens)) = tokens.split_last() else {
        let old = std::mem::replace(doc, value);
        return Ok(PatchOp {
            op: "replace".into(),
            path: String::new(),
            from: None,
            value: Some(old),
        });
    };
    let parent = navigate_mut(doc, parent_tokens)?;
    match parent {
        Value::Object(m) => {
            let old = m.insert(last.clone(), value);
            Ok(match old {
                Some(o) => PatchOp {
                    op: "replace".into(),
                    path: pointer_string(tokens),
                    from: None,
                    value: Some(o),
                },
                None => PatchOp {
                    op: "remove".into(),
                    path: pointer_string(tokens),
                    from: None,
                    value: None,
                },
            })
        }
        Value::Array(a) => {
            let i = array_index(last, a.len(), true)?;
            a.insert(i, value);
            let mut inv = parent_tokens.to_vec();
            inv.push(i.to_string());
            Ok(PatchOp {
                op: "remove".into(),
                path: pointer_string(&inv),
                from: None,
                value: None,
            })
        }
        _ => Err("cannot add into a scalar".into()),
    }
}

/// Remove the value at `tokens`; returns (removed value, inverse op).
fn do_remove(doc: &mut Value, tokens: &[String]) -> Result<(Value, PatchOp), String> {
    let Some((last, parent_tokens)) = tokens.split_last() else {
        return Err("cannot remove the document root".into());
    };
    let parent = navigate_mut(doc, parent_tokens)?;
    let removed = match parent {
        Value::Object(m) => m
            .remove(last)
            .ok_or_else(|| format!("path not found: {last}"))?,
        Value::Array(a) => {
            let i = array_index(last, a.len(), false)?;
            a.remove(i)
        }
        _ => return Err("cannot remove from a scalar".into()),
    };
    let inv = PatchOp {
        op: "add".into(),
        path: pointer_string(tokens),
        from: None,
        value: Some(removed.clone()),
    };
    Ok((removed, inv))
}

/// Apply one operation, returning its inverse (`None` for `test`).
pub fn apply_op(doc: &mut Value, op: &PatchOp) -> Result<Option<PatchOp>, String> {
    let path = parse_pointer(&op.path)?;
    match op.op.as_str() {
        "add" => {
            let v = op.value.clone().ok_or("add requires value")?;
            do_add(doc, &path, v).map(Some)
        }
        "remove" => do_remove(doc, &path).map(|(_, inv)| Some(inv)),
        "replace" => {
            let v = op.value.clone().ok_or("replace requires value")?;
            let target = navigate_mut(doc, &path)?;
            let old = std::mem::replace(target, v);
            Ok(Some(PatchOp {
                op: "replace".into(),
                path: op.path.clone(),
                from: None,
                value: Some(old),
            }))
        }
        "move" => {
            let from = parse_pointer(op.from.as_deref().ok_or("move requires from")?)?;
            if from.is_empty() {
                return Err("cannot move the document root".into());
            }
            if path.len() > from.len() && path[..from.len()] == from[..] {
                return Err("cannot move a value into itself".into());
            }
            if from == path {
                return Ok(Some(PatchOp {
                    op: "test".into(),
                    path: op.path.clone(),
                    from: None,
                    value: Some(navigate(doc, &path)?.clone()),
                }));
            }
            let (val, _) = do_remove(doc, &from)?;
            // For same-array moves RFC 6902 evaluates `path` after the removal.
            do_add(doc, &path, val)?;
            // Inverse: move back. Compute final location (for `-` appends resolve to index).
            let final_path = match path.last().map(String::as_str) {
                Some("-") => {
                    let parent = navigate(doc, &path[..path.len() - 1])?;
                    let len = parent.as_array().map(Vec::len).unwrap_or(1);
                    let mut p = path[..path.len() - 1].to_vec();
                    p.push((len - 1).to_string());
                    pointer_string(&p)
                }
                _ => op.path.clone(),
            };
            Ok(Some(PatchOp {
                op: "move".into(),
                path: op.from.clone().unwrap_or_default(),
                from: Some(final_path),
                value: None,
            }))
        }
        "copy" => {
            let from = parse_pointer(op.from.as_deref().ok_or("copy requires from")?)?;
            let val = navigate(doc, &from)?.clone();
            do_add(doc, &path, val).map(Some)
        }
        "test" => {
            let want = op.value.clone().ok_or("test requires value")?;
            let got = navigate(doc, &path)?;
            if *got != want {
                return Err(format!("test failed at \"{}\"", op.path));
            }
            Ok(None)
        }
        other => Err(format!("unknown op \"{other}\"")),
    }
}

/// Apply `ops` in order to a copy of `doc`. Returns (new doc, inverse ops in undo order).
pub fn apply_ops(doc: &Value, ops: &[PatchOp]) -> Result<(Value, Vec<PatchOp>), String> {
    let mut out = doc.clone();
    let mut inverses: Vec<PatchOp> = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        match apply_op(&mut out, op) {
            Ok(Some(inv)) => inverses.push(inv),
            Ok(None) => {}
            Err(e) => return Err(format!("op[{i}] {} {}: {e}", op.op, op.path)),
        }
    }
    inverses.reverse();
    Ok((out, inverses))
}

pub fn validate_timeline(doc: &Value) -> Result<(), String> {
    let obj = doc.as_object().ok_or("timeline must be a JSON object")?;
    if !obj.get("tracks").is_some_and(Value::is_array) {
        return Err("timeline.tracks must be an array".into());
    }
    let size = serde_json::to_vec(doc).map(|v| v.len()).unwrap_or(0);
    if size > MAX_TIMELINE_BYTES {
        return Err(format!("timeline exceeds {MAX_TIMELINE_BYTES} bytes"));
    }
    Ok(())
}

// ── DB ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct VideoProject {
    pub id: Uuid,
    pub plan_video_id: Uuid,
    pub project_id: Uuid,
    pub timeline: Value,
    pub rev: i32,
    pub render_blob_path: Option<String>,
    pub render_rev: Option<i32>,
    pub render_mime: Option<String>,
    pub render_bytes: Option<i64>,
    /// draft | building | ready | failed
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub const VP_COLUMNS: &str = "id, plan_video_id, project_id, timeline, rev, render_blob_path, \
     render_rev, render_mime, render_bytes, status, created_at, updated_at";

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct RevisionView {
    pub rev: i32,
    pub reason: String,
    #[schema(value_type = Vec<PatchOp>)]
    pub ops: Value,
    #[schema(value_type = Vec<PatchOp>)]
    pub inverse_ops: Value,
    /// Set when this revision was created by an undo of another revision.
    #[sqlx(skip)]
    #[serde(default)]
    pub reverts_rev: Option<i32>,
    pub created_at: DateTime<Utc>,
}

pub fn parse_reverts(reason: &str) -> Option<i32> {
    reason
        .strip_prefix("undo rev ")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

pub async fn get_video_project(pool: &PgPool, id: Uuid) -> AppResult<VideoProject> {
    sqlx::query_as::<_, VideoProject>(sqlx::AssertSqlSafe(format!(
        "select {VP_COLUMNS} from video_projects where id = $1"
    )))
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(AppError::from)
}

pub async fn get_for_plan_video(
    pool: &PgPool,
    plan_video_id: Uuid,
) -> sqlx::Result<Option<VideoProject>> {
    sqlx::query_as::<_, VideoProject>(sqlx::AssertSqlSafe(format!(
        "select {VP_COLUMNS} from video_projects where plan_video_id = $1"
    )))
    .bind(plan_video_id)
    .fetch_optional(pool)
    .await
}

/// Create the video project for a plan video (idempotent).
pub async fn create_video_project(
    pool: &PgPool,
    plan_video_id: Uuid,
    timeline: Option<Value>,
) -> AppResult<VideoProject> {
    if let Some(vp) = get_for_plan_video(pool, plan_video_id).await? {
        return Ok(vp);
    }
    let project_id: Uuid = sqlx::query_scalar(
        "select dp.project_id from plan_videos pv join daily_plans dp on dp.id = pv.plan_id \
          where pv.id = $1",
    )
    .bind(plan_video_id)
    .fetch_one(pool)
    .await?;
    let timeline = timeline.unwrap_or_else(empty_timeline);
    validate_timeline(&timeline).map_err(AppError::Invalid)?;
    let id = new_id();
    let mut tx = pool.begin().await?;
    let inserted = sqlx::query_as::<_, VideoProject>(sqlx::AssertSqlSafe(format!(
        "insert into video_projects (id, plan_video_id, project_id, timeline, rev) \
         values ($1,$2,$3,$4,1) on conflict (plan_video_id) do nothing returning {VP_COLUMNS}"
    )))
    .bind(id)
    .bind(plan_video_id)
    .bind(project_id)
    .bind(&timeline)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(vp) = inserted else {
        tx.rollback().await?;
        return get_for_plan_video(pool, plan_video_id)
            .await?
            .ok_or(AppError::NotFound);
    };
    sqlx::query(
        "insert into video_project_revisions \
            (id, video_project_id, rev, ops, inverse_ops, timeline_after, reason) \
         values ($1,$2,1,'[]','[]',$3,'created')",
    )
    .bind(new_id())
    .bind(vp.id)
    .bind(&timeline)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(vp)
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApplyResult {
    pub rev: i32,
    pub timeline: Value,
    pub ops: Vec<PatchOp>,
    pub inverse_ops: Vec<PatchOp>,
}

/// Apply ops against `base_rev`, storing the new revision with its inverse ops.
pub async fn apply(
    pool: &PgPool,
    video_project_id: Uuid,
    base_rev: i32,
    ops: Vec<PatchOp>,
    reason: &str,
) -> AppResult<ApplyResult> {
    if ops.is_empty() {
        return Err(AppError::Invalid("ops must not be empty".into()));
    }
    if ops.len() > MAX_OPS_PER_APPLY {
        return Err(AppError::Invalid(format!(
            "at most {MAX_OPS_PER_APPLY} ops per apply"
        )));
    }
    let mut tx = pool.begin().await?;
    let vp = sqlx::query_as::<_, VideoProject>(sqlx::AssertSqlSafe(format!(
        "select {VP_COLUMNS} from video_projects where id = $1 for update"
    )))
    .bind(video_project_id)
    .fetch_one(&mut *tx)
    .await?;
    if vp.rev != base_rev {
        return Err(AppError::Conflict(format!(
            "stale revision: base_rev {base_rev}, current rev {}",
            vp.rev
        )));
    }
    let (timeline, inverse) = apply_ops(&vp.timeline, &ops).map_err(AppError::Invalid)?;
    validate_timeline(&timeline).map_err(AppError::Invalid)?;
    let new_rev = vp.rev + 1;
    sqlx::query(
        "insert into video_project_revisions \
            (id, video_project_id, rev, ops, inverse_ops, timeline_after, reason) \
         values ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(new_id())
    .bind(vp.id)
    .bind(new_rev)
    .bind(serde_json::to_value(&ops).map_err(|e| AppError::Other(e.into()))?)
    .bind(serde_json::to_value(&inverse).map_err(|e| AppError::Other(e.into()))?)
    .bind(&timeline)
    .bind(reason)
    .execute(&mut *tx)
    .await?;
    // A new revision invalidates "ready" (the render no longer matches the timeline).
    sqlx::query(
        "update video_projects set timeline = $2, rev = $3, updated_at = now(), \
                status = case when status = 'building' then status else 'draft' end \
          where id = $1",
    )
    .bind(vp.id)
    .bind(&timeline)
    .bind(new_rev)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(ApplyResult {
        rev: new_rev,
        timeline,
        ops,
        inverse_ops: inverse,
    })
}

pub async fn list_revisions(
    pool: &PgPool,
    video_project_id: Uuid,
    limit: i64,
) -> AppResult<Vec<RevisionView>> {
    let mut rows = sqlx::query_as::<_, RevisionView>(
        "select rev, reason, ops, inverse_ops, created_at from video_project_revisions \
          where video_project_id = $1 order by rev desc limit $2",
    )
    .bind(video_project_id)
    .bind(limit.clamp(1, 500))
    .fetch_all(pool)
    .await?;
    for r in &mut rows {
        r.reverts_rev = parse_reverts(&r.reason);
    }
    Ok(rows)
}

pub async fn get_revision_timeline(
    pool: &PgPool,
    video_project_id: Uuid,
    rev: i32,
) -> AppResult<Value> {
    sqlx::query_scalar(
        "select timeline_after from video_project_revisions \
          where video_project_id = $1 and rev = $2",
    )
    .bind(video_project_id)
    .bind(rev)
    .fetch_one(pool)
    .await
    .map_err(AppError::from)
}

/// Undo a revision by applying its inverse ops as a new revision. With `rev = None` the most
/// recent revision that is neither an undo nor already undone is reverted.
pub async fn undo(
    pool: &PgPool,
    video_project_id: Uuid,
    rev: Option<i32>,
) -> AppResult<ApplyResult> {
    let all: Vec<(i32, String, Value)> = sqlx::query_as(
        "select rev, reason, inverse_ops from video_project_revisions \
          where video_project_id = $1 and rev > 1 order by rev desc",
    )
    .bind(video_project_id)
    .fetch_all(pool)
    .await?;
    let undone: std::collections::HashSet<i32> = all
        .iter()
        .filter_map(|(_, r, _)| parse_reverts(r))
        .collect();
    let target = match rev {
        Some(r) => all.iter().find(|(n, _, _)| *n == r),
        None => all
            .iter()
            .find(|(n, reason, _)| parse_reverts(reason).is_none() && !undone.contains(n)),
    }
    .ok_or_else(|| AppError::Conflict("nothing to undo".into()))?;
    if parse_reverts(&target.1).is_some() {
        return Err(AppError::Invalid(
            "cannot undo an undo; apply the ops again".into(),
        ));
    }
    if undone.contains(&target.0) {
        return Err(AppError::Conflict(format!(
            "rev {} is already undone",
            target.0
        )));
    }
    let inverse: Vec<PatchOp> =
        serde_json::from_value(target.2.clone()).map_err(|e| AppError::Other(e.into()))?;
    if inverse.is_empty() {
        return Err(AppError::Conflict("revision has no inverse ops".into()));
    }
    let current: i32 = sqlx::query_scalar("select rev from video_projects where id = $1")
        .bind(video_project_id)
        .fetch_one(pool)
        .await?;
    apply(
        pool,
        video_project_id,
        current,
        inverse,
        &format!("undo rev {}", target.0),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(op: &str, path: &str, value: Option<Value>) -> PatchOp {
        PatchOp {
            op: op.into(),
            path: path.into(),
            from: None,
            value,
        }
    }

    fn mv(from: &str, path: &str) -> PatchOp {
        PatchOp {
            op: "move".into(),
            path: path.into(),
            from: Some(from.into()),
            value: None,
        }
    }

    /// Applying ops then their inverses must restore the original document.
    fn roundtrip(doc: Value, ops: Vec<PatchOp>) -> Value {
        let (after, inv) = apply_ops(&doc, &ops).expect("apply");
        let (back, _) = apply_ops(&after, &inv).expect("undo");
        assert_eq!(back, doc, "inverse must restore original");
        after
    }

    #[test]
    fn add_replace_remove_roundtrip() {
        let doc = empty_timeline();
        let after = roundtrip(
            doc,
            vec![
                op(
                    "add",
                    "/tracks/0/clips/-",
                    Some(json!({"id":"c1","start":0})),
                ),
                op(
                    "add",
                    "/tracks/0/clips/-",
                    Some(json!({"id":"c2","start":1000})),
                ),
                op("replace", "/tracks/0/clips/0/start", Some(json!(250))),
                op("add", "/tracks/0/clips/1/volume", Some(json!(0.5))),
                op("remove", "/tracks/0/clips/1/id", None),
            ],
        );
        assert_eq!(after["tracks"][0]["clips"][0]["start"], 250);
        assert_eq!(after["tracks"][0]["clips"][1]["volume"], 0.5);
        assert!(after["tracks"][0]["clips"][1].get("id").is_none());
    }

    #[test]
    fn insert_at_index_and_object_overwrite() {
        let doc = json!({"tracks":[{"clips":[1,2,3]}],"meta":{"a":1}});
        let after = roundtrip(
            doc,
            vec![
                op("add", "/tracks/0/clips/1", Some(json!(9))),
                op("add", "/meta/a", Some(json!(2))), // overwrite existing key
                op("add", "/meta/b", Some(json!(3))),
            ],
        );
        assert_eq!(after["tracks"][0]["clips"], json!([1, 9, 2, 3]));
        assert_eq!(after["meta"]["a"], 2);
    }

    #[test]
    fn move_within_array_roundtrip() {
        let doc = json!({"tracks":[{"clips":["a","b","c","d"]}]});
        let after = roundtrip(
            doc.clone(),
            vec![mv("/tracks/0/clips/0", "/tracks/0/clips/2")],
        );
        assert_eq!(after["tracks"][0]["clips"], json!(["b", "c", "a", "d"]));
        let after = roundtrip(
            doc.clone(),
            vec![mv("/tracks/0/clips/3", "/tracks/0/clips/0")],
        );
        assert_eq!(after["tracks"][0]["clips"], json!(["d", "a", "b", "c"]));
        let after = roundtrip(doc, vec![mv("/tracks/0/clips/0", "/tracks/0/clips/-")]);
        assert_eq!(after["tracks"][0]["clips"], json!(["b", "c", "d", "a"]));
    }

    #[test]
    fn move_between_tracks_roundtrip() {
        let doc = json!({"tracks":[{"clips":["a","b"]},{"clips":[]}]});
        let after = roundtrip(doc, vec![mv("/tracks/0/clips/1", "/tracks/1/clips/0")]);
        assert_eq!(after["tracks"][0]["clips"], json!(["a"]));
        assert_eq!(after["tracks"][1]["clips"], json!(["b"]));
    }

    #[test]
    fn copy_and_root_replace() {
        let doc = json!({"tracks":[{"clips":[{"id":"x"}]}]});
        let after = roundtrip(
            doc.clone(),
            vec![PatchOp {
                op: "copy".into(),
                path: "/tracks/0/clips/-".into(),
                from: Some("/tracks/0/clips/0".into()),
                value: None,
            }],
        );
        assert_eq!(after["tracks"][0]["clips"].as_array().unwrap().len(), 2);
        let after = roundtrip(doc, vec![op("replace", "", Some(json!({"tracks":[]})))]);
        assert_eq!(after, json!({"tracks":[]}));
    }

    #[test]
    fn test_op_guards() {
        let doc = json!({"tracks":[],"fps":30});
        assert!(apply_ops(&doc, &[op("test", "/fps", Some(json!(30)))]).is_ok());
        let err = apply_ops(&doc, &[op("test", "/fps", Some(json!(24)))]).unwrap_err();
        assert!(err.contains("test failed"));
    }

    #[test]
    fn errors_are_indexed() {
        let doc = json!({"tracks":[]});
        let err = apply_ops(
            &doc,
            &[
                op("add", "/fps", Some(json!(30))),
                op("remove", "/nope", None),
            ],
        )
        .unwrap_err();
        assert!(err.starts_with("op[1]"), "{err}");
        let err = apply_ops(&doc, &[op("add", "/tracks/5", Some(json!(1)))]).unwrap_err();
        assert!(err.contains("out of range"), "{err}");
    }

    #[test]
    fn pointer_escaping() {
        let doc = json!({"a/b":{"c~d":1}});
        let after = roundtrip(doc, vec![op("replace", "/a~1b/c~0d", Some(json!(2)))]);
        assert_eq!(after["a/b"]["c~d"], 2);
    }

    #[test]
    fn validates_timeline_shape() {
        assert!(validate_timeline(&empty_timeline()).is_ok());
        assert!(validate_timeline(&json!([])).is_err());
        assert!(validate_timeline(&json!({"tracks": 1})).is_err());
    }

    #[test]
    fn undo_reason_parse() {
        assert_eq!(parse_reverts("undo rev 7"), Some(7));
        assert_eq!(parse_reverts("trim clip"), None);
    }
}
