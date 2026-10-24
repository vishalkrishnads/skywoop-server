-- Plan format v2: day title + summary, no-op days, render metadata, smaller budget defaults.
alter table daily_plans drop constraint daily_plans_status_check;
alter table daily_plans add constraint daily_plans_status_check
  check (status in (
    'waiting_for_data', 'drafting', 'review', 'generating_assets',
    'building', 'built', 'scheduled', 'posted', 'failed', 'no_op'
  ));

alter table daily_plans
  add column title text not null default '',
  add column summary text not null default '',
  add column no_op_reason text,
  add column no_op_explanation text;

alter table video_projects
  add column rendered_at timestamptz;

update video_projects set rendered_at = updated_at where render_blob_path is not null;

alter table project_budgets
  alter column audience_videos_per_platform set default 1,
  alter column trend_videos_per_platform set default 0;
