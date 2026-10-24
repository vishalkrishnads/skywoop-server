# Skywoop API

HTTP JSON API for the Skywoop server. Machine-readable copy: `GET /openapi.json`.

Base URL locally: `http://localhost:8080`. Production: `https://api.skywoop.com`.

## Conventions

**Auth.** Every route except the public ones requires a signed-in user.

- Local / CI: `Authorization: Bearer <DEV_TOKEN>` (maps to synthetic `dev-user`).
- Production: `Authorization: Bearer <JWT>` issued by Better Auth on the frontend (`AUTH_MODE=jwt`). The API verifies the token against `AUTH_JWKS_URL` (issuer / audience optional via `AUTH_ISSUER` / `AUTH_AUDIENCE`).

**Tenancy.** Every project has an `owner_user_id`. Cross-tenant access returns 404.

**Errors.** `{ "error": "…" }` with status 401 / 404 / 409 / 422 / 502 / 500.

**IDs.** UUIDv7. Platforms: `instagram` | `facebook`. Dates: `YYYY-MM-DD` in the project timezone unless the field is `timestamptz`.

**Jobs.** Async work returns `job_id`. Poll `GET /jobs/{id}` until `succeeded` or `failed`.

**CORS.** `APP_ORIGIN` takes a comma-separated list of frontend origins (e.g. `https://app.skywoop.com,http://localhost:3000`). Each is allowed for CORS; the first is the default base for `OAUTH_DONE_URL`.

See also [PLAN_FORMAT.md](PLAN_FORMAT.md) and [TIMELINE.md](TIMELINE.md).

---

## Public routes

`/healthz`, `/readyz`, `/openapi.json`, `/r/{token}`, `/l/{slug}`, `/oauth/meta/callback`, local `/dev/assets/{*path}` and `/dev/upload/{*path}`.

---

## Current user

### `GET /me`

Returns `{ id, email, created_at, projects: [{ id, slug, name, onboarding_step, created_at, icon_url }] }`. `created_at` is the user's first sign-in; `icon_url` is a short-lived signed URL of the brand logo (null when unset).

The first authenticated request upserts the user. When Better Auth issues a new user id for an email that an older user row still holds (the old auth account was deleted and the address signed up again), the new id takes over that row's projects and the old row is removed.

---

## Onboarding

Order: App Store URL → Meta connect → confirm details → optional context uploads → complete.

### `POST /onboarding/projects`

Body: `{ "app_store_url": "https://apps.apple.com/…/id…" }`.

Creates a project owned by the caller (`onboarding_step=meta`), seeds asset directories (including `Inbox/` and `Context/`), links a discovery row, and enqueues `discover_product`. Returns `{ project_id, discovery_id, onboarding_step }` with 201.

Idempotent per user: when the caller already has a project still in onboarding, that project is returned with 200 (its current `onboarding_step` and latest `discovery_id`). If the App Store id in the URL differs, the project's app id, market country, timezone, placeholder name and `app-…` slug are updated and discovery is re-queued (new `discovery_id`). Slugs are `app-{id}`; when another project already holds it, a short suffix is added (`app-{id}-{6 chars}`).

### Meta

Existing routes under `/projects/{pid}/connections/meta/*`. After a successful `select`, the project advances from `meta` to `details` (never backward).

`GET /oauth/meta/callback` always redirects to `OAUTH_DONE_URL`:

- success: `?state=…&project_id=…`
- cancel / error: `?state=…&project_id=…&error=…&error_description=…` (`error` is Meta's code such as `access_denied`, or `missing_code` / `callback_failed`; `state` and `project_id` are included when known).

### `GET /projects/{pid}/onboarding/details`

Discovery status + draft (name, description, icon/screenshots with short-lived `preview_url`).

### `PUT /projects/{pid}/onboarding/details`

Body: `{ name, description, icon?, screenshots?, primary_color?, secondary_color?, tertiary_color? }` where media refs are either `{ "discovery": { "blob_path" } }` or `{ "asset": { "asset_id" } }` and colors are `#rrggbb`.

Allowed while `onboarding_step` is `meta`, `details`, or `context` (409 once complete). The first save materializes App Store assets from discovery; later saves keep the brand chosen before (omitted colors / media stay as they were) and reuse identical App Store files instead of copying them again. Seeds audiences from discovery suggestions when none exist and enqueues `classify_industry`. The step only moves forward: `meta` / `details` → `context`; `context` stays `context`. Returns `{ project_id, onboarding_step, slug }`.

### `POST /projects/{pid}/onboarding/context`

Body: `{ notes, asset_ids, posters?: [{ asset_id, poster_asset_id }] }`.

Saves notes and enqueues `categorize_asset` per upload (moves into `Context/<Category>/`).

### `POST /projects/{pid}/onboarding/complete`

Sets `onboarding_step=complete` and enqueues the first `draft_plan` jobs.

Most plan / experiment / diary routes require `onboarding_step=complete` (409 otherwise).

---

## Projects

- `GET /projects` — caller's projects only
- `GET/PATCH /projects/{id}`, `PUT /projects/{id}/brand`
- `PUT /projects/{id}/app-store`, schedule get/put, bio link
- Lists: countries, industries, timezones, font families
- Trends: `GET /projects/{pid}/{platform}/trends/today`

---

## Asset manager

- `GET/POST /projects/{pid}/asset-dirs`
- `PATCH/DELETE /asset-dirs/{id}`
- `GET /projects/{pid}/assets?dir=`
- `GET /projects/{pid}/assets/search?q=&kind=&dir=`
- `GET|POST /projects/{pid}/assets/resolve`
- `POST /projects/{pid}/assets/uploads` → `{ upload_id, upload_url }`
- `POST /projects/{pid}/assets` — finalize upload
- `GET/PATCH/DELETE /assets/{id}`
- Audio library search / attach / ingest

Seeded top-level dirs: `App Store/`, `Screen recordings/`, `Screenshots/`, `Characters/`, `Generated/`, `Inbox/`, `Context/`.

---

## Home

### `GET /projects/{pid}/overview`

Home page summary:

- `stats`: `{ posts_7d, views_7d, views_prev_7d, shares_7d, installs_7d, install_rate_7d }` over experiments published in the last 7 days (and the 7 before for `views_prev_7d`). Installs are measured installs, else estimated.
- `plans`: the next plan (today or later) per platform: `{ platform, plan_id, for_date, status, title, videos }`.
- `recent_videos`: latest 8 published videos `{ plan_video_id, plan_id, platform, title, published_at, views, shares, retention, permalink }`.
- `activity`: latest 20 events `{ at, kind, platform, title, detail, plan_id, plan_video_id }` where `kind` is `video_published` | `plan_drafted` | `plan_scheduled` | `diary`.

---

## Plans (markdown)

Statuses: `waiting_for_data` → `drafting` → `review` ⇄ `generating_assets` ⇄ `building` ⇄ `built` → `scheduled` → `posted` (or `failed`). Any status before `scheduled` can become `no_op` (nothing is posted that day).

- `GET /projects/{pid}/platforms/{p}/plans?from=&to=` — `[{ id, for_date, status, title }]`, oldest first. Defaults: `from` = today − 30 days, `to` = today + 7 days; at most 366 days.
- `GET …/plans/latest`, `GET …/plans/{date}`, `GET /plans/{id}` — `PlanView`
- `POST …/plans/draft` — enqueue PlanDay (never chooses no-op)
- `PUT /plans/{id}` — body `{ version, markdown }`; stores the `#` title and summary on the plan
- `PUT /plans/{id}/checklist`
- `POST /plans/{id}/status`
- `POST /plans/{id}/resume`
- `POST /plans/{id}/schedule`
- Comments, chat, audiences, versions as before

### `PlanView`

- `plan`: the plan row, including `title`, `summary`, `no_op_reason`, `no_op_explanation`.
- `videos[]`: `{ video, audience_name, hypothesis, flow, flow_steps, checklist, video_project, post }`.
  - `flow_steps[]`: `{ label, description, start_ms, end_ms }` (timings null for legacy `A --> B` flows).
  - `checklist[]`: parsed items with `key`, `state`, `kind`, `text`, `refs`, `name`, `model`, `status` (`open` | `queued` | `generated` | `regenerate_queued` | `confirmed`), `regenerate_note`, `preview_url` (signed URL of the first `@path`, null when it is not in the asset manager), plus generation readiness fields.
  - `video_project`: `{ id, rev, render_rev, status, render_current, render_url, render_bytes, duration_ms, width, height, fps, rendered_at }`.
- `no_op_metrics`: `{ views_last_48h, install_rate, prior_posts }` for this platform (views gained in the last 48 hours across posts, installs per view over the last 7 days, posts so far).
- `previous_day[]`: videos published on `for_date − 1`: `{ plan_video_id, title, views, shares, retention, note, permalink }` where `retention = 1 − skip_rate` and `note` is the latest daily diary observation.
- `comments`, `annotated_markdown`, `warnings`, `read_only` as before.

### `PUT /plans/{id}/checklist`

Body: `{ key, state?, asset_path?, model?, regenerate_note? }`.

- `state`: `open` | `pending` | `done` (also `queued`, `confirmed`). Omit to keep the marker.
- `asset_path`: attach as `@"path"` (replaces the previous path and drops any REGENERATE note).
- `model`: string sets `  - model: …` (`[a-z0-9._/-]{1,80}`); `null` removes it; omit to leave it.
- `regenerate_note`: string sets `<!-- REGENERATE: … -->` on the line; `null` (or empty) removes it; omit to leave it.

### `POST /plans/{id}/status`

Body: `{ status, reason?, explanation? }`.

- `review` | `generating_assets` | `building` | `built` as before.
- `no_op`: allowed from any status before `scheduled` on today or a future date. `reason` defaults to `user`; `explanation` is optional.
- `review` on a `no_op` plan resumes it (see below).

### `POST /plans/{id}/resume`

Leaves a `no_op` day. When the plan already has videos it returns to `review`; otherwise it moves to `waiting_for_data` and enqueues a fresh PlanDay draft that may not choose no-op. 409 when the plan is not `no_op` or is in the past.

### No-op days from PlanDay

The daily PlanDay run may return a no-op day only when earlier posts exist on the platform and they gained views in the last 48 hours. The plan lands in `no_op` with the agent's `no_op_reason` / `no_op_explanation` and a markdown of just the title and summary.

---

## Budget

- `GET /projects/{pid}/budget` — defaults when never set: 1 audience video and 0 trend videos per platform per day, $25 daily cap.
- `PUT /projects/{pid}/budget` — `{ daily_usd_cap, audience_videos_per_platform, trend_videos_per_platform }`; audience 1–5, trend 0–5.

---

## Videos, experiments, diary, playbook

Unchanged path shapes from the upstream video-creative API; all scoped to the project owner. `POST /video-projects/{id}/render/finalize` records `rendered_at`.
