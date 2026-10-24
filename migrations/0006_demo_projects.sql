-- Track which project a demo clone was copied from (null for normal projects).
alter table projects
  add column demo_of uuid references projects (id) on delete set null;

create index projects_owner_demo_of
  on projects (owner_user_id, demo_of)
  where demo_of is not null;
