-- Multi-user ownership and onboarding steps for Skywoop.
create table app_users (
  id text primary key,
  email text not null unique,
  created_at timestamptz not null default now(),
  last_seen_at timestamptz
);

alter table projects
  add column owner_user_id text not null references app_users (id),
  add column onboarding_step text not null default 'complete'
    check (onboarding_step in ('meta', 'details', 'context', 'complete')),
  add column context_notes text not null default '';

create index projects_owner on projects (owner_user_id);

alter table project_discoveries
  add column project_id uuid references projects on delete cascade;

create index project_discoveries_project on project_discoveries (project_id);
