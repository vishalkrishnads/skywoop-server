-- Skywoop video-creative baseline (squashed; database reset expected).
-- Extensions for asset search.
create extension if not exists pg_trgm;

-- ---------------------------------------------------------------------------
-- Projects & brand
-- ---------------------------------------------------------------------------
create table projects (
  id uuid primary key,
  slug text not null unique,
  name text not null,
  description text not null default '',
  website text,
  timezone text not null default 'UTC',
  market_country text not null default 'US',
  industry text,
  app_store_app_id text,
  asc_provider_token text,
  brand jsonb not null default '{}',
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now()
);

create table project_audiences (
  id uuid primary key,
  project_id uuid not null references projects on delete cascade,
  position int not null check (position between 1 and 5),
  name text not null,
  description text not null default '',
  archived_at timestamptz,
  created_at timestamptz not null default now()
);
create unique index project_audiences_active_pos
  on project_audiences (project_id, position)
  where archived_at is null;
create index project_audiences_project
  on project_audiences (project_id) where archived_at is null;

create table project_budgets (
  project_id uuid primary key references projects on delete cascade,
  daily_usd_cap numeric(10,2) not null default 25.00,
  audience_videos_per_platform int not null default 5 check (audience_videos_per_platform >= 0),
  trend_videos_per_platform int not null default 5 check (trend_videos_per_platform >= 0),
  updated_at timestamptz not null default now()
);

create table platform_workspaces (
  project_id uuid not null references projects on delete cascade,
  platform text not null check (platform in ('instagram','facebook')),
  max_daily_posts int not null default 10 check (max_daily_posts >= 1),
  min_evidence_views int not null default 100,
  primary key (project_id, platform)
);

create table platform_connections (
  id uuid primary key,
  project_id uuid not null references projects on delete cascade,
  platform text not null,
  external_account_id text not null,
  display_name text not null,
  token_secret_name text not null,
  status text not null check (status in ('connected','needs_reauth')),
  connected_at timestamptz not null default now(),
  unique (project_id, platform)
);

create table oauth_states (
  state text primary key,
  project_id uuid not null references projects on delete cascade,
  candidates jsonb,
  user_token_secret_name text,
  created_at timestamptz not null default now(),
  consumed_at timestamptz
);

create table project_discoveries (
  id uuid primary key,
  url text not null,
  status text not null default 'pending'
    check (status in ('pending','running','ready','failed')),
  draft jsonb,
  error text,
  job_id uuid,
  created_at timestamptz not null default now()
);

-- ---------------------------------------------------------------------------
-- Asset manager
-- ---------------------------------------------------------------------------
create table asset_dirs (
  id uuid primary key,
  project_id uuid not null references projects on delete cascade,
  parent_id uuid references asset_dirs on delete cascade,
  name text not null,
  path text not null,
  description text not null default '',
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now(),
  unique (project_id, path)
);
create index asset_dirs_parent on asset_dirs (project_id, parent_id);

create table assets (
  id uuid primary key,
  project_id uuid not null references projects on delete cascade,
  dir_id uuid references asset_dirs on delete set null,
  name text not null,
  path text not null,
  kind text not null check (kind in ('image','video','audio','other')),
  blob_path text not null,
  mime text not null,
  width int,
  height int,
  duration_ms int,
  sha256 text not null,
  description text not null default '',
  tags text[] not null default '{}',
  transcript jsonb,
  bytes bigint,
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now(),
  unique (project_id, path)
);
create index assets_dir on assets (dir_id);
create index assets_kind on assets (project_id, kind);
create index assets_name_trgm on assets using gin (name gin_trgm_ops);
create index assets_path_trgm on assets using gin (path gin_trgm_ops);
create index assets_description_trgm on assets using gin (description gin_trgm_ops);
create index assets_fts on assets using gin (
  to_tsvector('english', coalesce(name,'') || ' ' || coalesce(path,'') || ' ' || coalesce(description,''))
);

create table asset_generations (
  id uuid primary key,
  project_id uuid not null references projects on delete cascade,
  plan_id uuid,
  checklist_key text not null,
  provider text not null default 'fal',
  model_id text not null,
  external_request_id text,
  input jsonb not null default '{}',
  status text not null default 'queued'
    check (status in ('queued','running','succeeded','failed','cancelled')),
  asset_id uuid references assets on delete set null,
  cost_usd numeric(10,4),
  error text,
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now(),
  finished_at timestamptz
);
create index asset_generations_lookup
  on asset_generations (project_id, checklist_key, created_at desc);
create index asset_generations_external
  on asset_generations (external_request_id)
  where external_request_id is not null;

-- ---------------------------------------------------------------------------
-- Daily plans (markdown + greeting)
-- ---------------------------------------------------------------------------
create table daily_plans (
  id uuid primary key,
  project_id uuid not null references projects on delete cascade,
  platform text not null check (platform in ('instagram', 'facebook')),
  for_date date not null,
  status text not null default 'waiting_for_data'
    check (status in (
      'waiting_for_data', 'drafting', 'review', 'generating_assets',
      'building', 'built', 'scheduled', 'posted', 'failed'
    )),
  version int not null default 1,
  markdown text not null default '',
  greeting text not null default '',
  findings jsonb not null default '{}',
  prime_window_override jsonb,
  job_id uuid,
  scheduled_at timestamptz,
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now(),
  unique (project_id, platform, for_date)
);
create index daily_plans_lookup
  on daily_plans (project_id, platform, for_date desc);

alter table asset_generations
  add constraint asset_generations_plan_fk
  foreign key (plan_id) references daily_plans on delete set null;

create table plan_videos (
  id uuid primary key,
  plan_id uuid not null references daily_plans on delete cascade,
  anchor text not null,
  title text not null default '',
  track text not null check (track in ('audience', 'trend')),
  audience_id uuid references project_audiences,
  trend jsonb,
  caption text not null default '',
  daypart text
    check (daypart is null or daypart in ('prime', 'morning', 'afternoon', 'evening', 'night')),
  position int not null,
  scheduled_for timestamptz,
  created_at timestamptz not null default now(),
  unique (plan_id, anchor),
  unique (plan_id, position)
);
create index plan_videos_plan on plan_videos (plan_id);

create table plan_comments (
  id uuid primary key,
  plan_id uuid not null references daily_plans on delete cascade,
  anchor text not null,
  body text not null,
  author text not null default 'user' check (author in ('user', 'agent')),
  created_at timestamptz not null default now()
);
create index plan_comments_plan on plan_comments (plan_id, anchor, created_at);

create table plan_messages (
  id uuid primary key,
  plan_id uuid not null references daily_plans on delete cascade,
  role text not null check (role in ('user', 'agent', 'system')),
  parts jsonb not null default '[]',
  created_at timestamptz not null default now()
);
create index plan_messages_plan on plan_messages (plan_id, created_at);

create table plan_versions (
  plan_id uuid not null references daily_plans on delete cascade,
  version int not null,
  markdown text not null,
  greeting text not null default '',
  reason text not null default '',
  created_at timestamptz not null default now(),
  primary key (plan_id, version)
);

create table agent_runs (
  id uuid primary key,
  plan_id uuid references daily_plans on delete set null,
  project_id uuid references projects on delete cascade,
  purpose text not null,
  steps jsonb not null default '[]',
  status text not null default 'running'
    check (status in ('running', 'succeeded', 'failed')),
  input_tokens int,
  output_tokens int,
  error text,
  created_at timestamptz not null default now(),
  finished_at timestamptz
);

-- ---------------------------------------------------------------------------
-- Video projects (timeline editor)
-- ---------------------------------------------------------------------------
create table video_projects (
  id uuid primary key,
  plan_video_id uuid not null unique references plan_videos on delete cascade,
  project_id uuid not null references projects on delete cascade,
  timeline jsonb not null,
  rev int not null default 1,
  render_blob_path text,
  render_rev int,
  render_mime text,
  render_bytes bigint,
  status text not null default 'draft'
    check (status in ('draft', 'building', 'ready', 'failed')),
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now()
);

create table video_project_revisions (
  id uuid primary key,
  video_project_id uuid not null references video_projects on delete cascade,
  rev int not null,
  ops jsonb not null,
  inverse_ops jsonb not null,
  timeline_after jsonb not null,
  reason text not null default '',
  created_at timestamptz not null default now(),
  unique (video_project_id, rev)
);

-- ---------------------------------------------------------------------------
-- Publishing & experiments (point at plan_videos)
-- ---------------------------------------------------------------------------
create table tracked_links (
  token text primary key check (length(token) <= 30),
  plan_video_id uuid not null unique references plan_videos on delete cascade,
  destination_url text not null,
  created_at timestamptz not null default now()
);

create table link_clicks (
  id bigint generated always as identity primary key,
  token text not null references tracked_links,
  clicked_at timestamptz not null default now(),
  visitor_hash text not null,
  user_agent text,
  is_bot boolean not null
);
create index link_clicks_token on link_clicks (token) where not is_bot;

create table posts (
  id uuid primary key,
  plan_video_id uuid not null unique references plan_videos on delete cascade,
  project_id uuid not null references projects on delete cascade,
  platform text not null,
  status text not null
    check (status in ('scheduled','pending','container_created','publishing','published','failed')),
  attempt int not null default 1,
  container_id text,
  container_polls int not null default 0,
  platform_post_id text,
  platform_video_id text,
  permalink text,
  media_deleted boolean not null default false,
  scheduled_for timestamptz,
  publish_started_at timestamptz,
  published_at timestamptz,
  error text
);
create index posts_scheduled on posts (scheduled_for)
  where status = 'scheduled';

create table experiments (
  id uuid primary key,
  plan_video_id uuid not null unique references plan_videos on delete cascade,
  post_id uuid not null unique references posts,
  project_id uuid not null references projects on delete cascade,
  platform text not null,
  track text not null check (track in ('audience', 'trend')),
  audience_id uuid references project_audiences,
  published_at timestamptz not null,
  publish_local_date date not null,
  hypothesis text not null default '',
  maturity text not null default 'collecting'
    check (maturity in ('collecting','mature','inconclusive')),
  meta_complete boolean not null default false,
  asc_complete boolean not null default false,
  asc_no_data boolean not null default false,
  matured_at timestamptz,
  views bigint, reach bigint, interactions bigint, platform_link_clicks bigint,
  cta_clicks bigint, cta_clicks_unique bigint,
  profile_visits bigint, bio_link_taps bigint, follows bigint,
  likes bigint, comments bigint, saves bigint, shares bigint, reposts bigint,
  -- Video-specific
  avg_watch_time_ms bigint,
  total_watch_time_ms bigint,
  skip_rate double precision,
  reels_play_count bigint,
  asc_impressions bigint, asc_page_views bigint, installs bigint,
  est_installs bigint, est_install_share double precision,
  cta_source text check (cta_source is null or cta_source in ('profile_visits', 'tracked_link')),
  cta_rate double precision,
  install_rate double precision,
  growth bigint,
  growth_baseline text,
  observation_diary_id uuid,
  updated_at timestamptz not null default now()
);
create index experiments_by_day on experiments (project_id, platform, publish_local_date);

create table post_metric_snapshots (
  id uuid primary key,
  experiment_id uuid not null references experiments on delete cascade,
  snapshot_no int not null,
  fetched_at timestamptz not null default now(),
  raw jsonb not null,
  views bigint, reach bigint, interactions bigint, platform_link_clicks bigint,
  profile_visits bigint, bio_link_taps bigint, follows bigint,
  likes bigint, comments bigint, saves bigint, shares bigint, reposts bigint,
  avg_watch_time_ms bigint, total_watch_time_ms bigint, skip_rate double precision,
  unique (experiment_id, snapshot_no)
);

create table post_metric_values (
  snapshot_id uuid not null references post_metric_snapshots on delete cascade,
  experiment_id uuid not null references experiments on delete cascade,
  snapshot_no int not null,
  metric text not null,
  dim_key text not null default '',
  dim_value text not null default '',
  value double precision not null,
  primary key (snapshot_id, metric, dim_key, dim_value)
);
create index post_metric_values_experiment
  on post_metric_values (experiment_id, metric);

create table account_metric_daily (
  project_id uuid not null references projects on delete cascade,
  platform text not null check (platform in ('instagram', 'facebook')),
  local_date date not null,
  metric text not null,
  dim_key text not null default '',
  dim_value text not null default '',
  value double precision,
  raw jsonb not null default '{}',
  fetched_at timestamptz not null default now(),
  primary key (project_id, platform, local_date, metric, dim_key, dim_value)
);

-- ---------------------------------------------------------------------------
-- App Store Connect
-- ---------------------------------------------------------------------------
create table asc_report_requests (
  app_id text primary key,
  request_id text not null,
  created_at timestamptz not null default now(),
  last_seen_ok_at timestamptz
);
create table asc_ingested_instances (
  instance_id text primary key,
  app_id text not null,
  report_name text not null,
  processing_date date not null,
  ingested_at timestamptz not null default now()
);
create table asc_campaign_daily (
  app_id text not null, date date not null, campaign text not null,
  metric text not null check (metric in ('impressions','page_views','taps','first_time_downloads','redownloads')),
  value bigint not null,
  processing_date date not null,
  primary key (app_id, date, campaign, metric)
);
create table asc_source_daily (
  app_id text not null, date date not null, source_info text not null,
  metric text not null, value bigint not null, processing_date date not null,
  primary key (app_id, date, source_info, metric)
);

-- ---------------------------------------------------------------------------
-- Diary, trends, schedules, playbook, AI ledger, jobs
-- ---------------------------------------------------------------------------
create table diary_entries (
  id uuid primary key,
  project_id uuid not null references projects on delete cascade,
  entry_date date not null,
  platform text not null,
  plan_video_id uuid references plan_videos on delete set null,
  experiment_id uuid references experiments,
  plan_id uuid references daily_plans on delete set null,
  audience_id uuid references project_audiences on delete set null,
  kind text not null check (kind in (
    'experiment', 'platform_trend', 'manual',
    'video_daily', 'day_summary', 'plan_retro'
  )),
  views bigint, cta_clicks bigint, installs bigint, growth bigint,
  profile_visits bigint, bio_link_taps bigint, est_installs bigint,
  avg_watch_time_ms bigint, skip_rate double precision,
  observation text not null,
  author text not null check (author in ('agent','user')),
  model text,
  created_at timestamptz not null default now()
);
create index diary_by_project on diary_entries (project_id, platform, entry_date desc);
create unique index diary_video_daily_upsert
  on diary_entries (plan_video_id, entry_date, kind)
  where plan_video_id is not null and kind = 'video_daily';
create unique index diary_day_summary_upsert
  on diary_entries (project_id, platform, entry_date, kind)
  where kind = 'day_summary';
create unique index diary_plan_retro_upsert
  on diary_entries (plan_id, kind)
  where plan_id is not null and kind = 'plan_retro';

alter table experiments add constraint experiments_observation_fk
  foreign key (observation_diary_id) references diary_entries;

create table trend_snapshots (
  id uuid primary key,
  project_id uuid not null references projects on delete cascade,
  platform text not null check (platform in ('instagram','facebook')),
  local_date date not null,
  country text not null,
  status text not null default 'ready'
    check (status in ('pending','ready','partial','failed')),
  items jsonb not null default '[]',
  error text,
  created_at timestamptz not null default now(),
  unique (project_id, platform, local_date)
);

create table posting_schedules (
  project_id uuid not null references projects on delete cascade,
  platform text not null check (platform in ('instagram', 'facebook')),
  slots jsonb not null default '[]',
  prime_windows jsonb not null default '[]',
  dayparts jsonb not null default '{
    "morning":   {"start_minute": 420, "end_minute": 600},
    "afternoon": {"start_minute": 720, "end_minute": 900},
    "evening":   {"start_minute": 1020, "end_minute": 1200},
    "night":     {"start_minute": 1230, "end_minute": 1380}
  }'::jsonb,
  source text not null default 'industry_default'
    check (source in ('industry_default', 'agent', 'manual')),
  rationale text not null default '',
  version int not null default 1,
  updated_at timestamptz not null default now(),
  primary key (project_id, platform)
);

create table posting_schedule_changes (
  id uuid primary key,
  project_id uuid not null references projects on delete cascade,
  platform text not null,
  previous_slots jsonb,
  new_slots jsonb not null,
  source text not null,
  rationale text not null default '',
  created_at timestamptz not null default now()
);
create index posting_schedule_changes_lookup
  on posting_schedule_changes (project_id, platform, created_at desc);

create table playbook_entries (
  id uuid primary key,
  title text not null,
  body_md text not null,
  app_categories text[] not null default '{}',
  tags text[] not null default '{}',
  evidence text not null default '',
  source_project_id uuid references projects on delete set null,
  status text not null default 'proposed'
    check (status in ('proposed', 'approved', 'rejected')),
  reviewed_at timestamptz,
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now()
);
create index playbook_entries_status on playbook_entries (status, updated_at desc);

create table ai_calls (
  id uuid primary key,
  project_id uuid references projects on delete set null,
  purpose text not null,
  prompt_version text not null default '',
  deployment text not null default '',
  source text not null default 'server'
    check (source in ('server', 'browser')),
  provider text not null default 'azure'
    check (provider in ('azure', 'fal', 'other')),
  input_tokens int, output_tokens int, images int,
  cost_usd numeric(10,4),
  latency_ms int not null default 0,
  status text not null,
  error text,
  meta jsonb not null default '{}',
  created_at timestamptz not null default now()
);
create index ai_calls_project_day
  on ai_calls (project_id, created_at desc);

create table jobs (
  id uuid primary key,
  kind text not null,
  payload jsonb not null,
  status text not null default 'queued' check (status in ('queued','running','succeeded','failed')),
  run_at timestamptz not null default now(),
  attempts int not null default 0,
  max_attempts int not null default 5,
  locked_at timestamptz, locked_by text,
  last_error text,
  result jsonb,
  dedupe_key text unique,
  created_at timestamptz not null default now(),
  finished_at timestamptz
);
create index jobs_ready on jobs (run_at) where status = 'queued';

-- Seed playbook: screen-time style creative pattern
insert into playbook_entries (id, title, body_md, app_categories, tags, evidence, status, reviewed_at)
values (
  '01900000-0000-7000-8000-000000000001',
  'Screen-time confession with see-through overlays',
  $md$
Use a single simple shot (no camera moves). Overlay cycling screenshots at ~40% opacity so the subject stays visible. Keep captions inside the Instagram safe zone (center ~70% vertically). Prefer two beats: problem (high screen time) → solution (low screen time after finding the app).
$md$,
  array['productivity','health','utilities'],
  array['ugc','overlay','safe-zone','simple-shot'],
  'Internal playbook seed for screen-time / habit apps.',
  'approved',
  now()
);
