-- Shared royalty-free music and sound effects, reusable across projects.
create table audio_library (
  id uuid primary key,
  kind text not null check (kind in ('sound_effect', 'music')),
  title text not null,
  description text not null default '',
  tags text[] not null default '{}',
  duration_ms int,
  blob_path text not null,
  mime text not null,
  bytes bigint,
  sha256 text not null,
  source text not null check (source in ('openverse', 'fal')),
  source_id text not null,
  license text not null default '',
  attribution text not null default '',
  created_at timestamptz not null default now(),
  unique (source, source_id)
);

create index audio_library_kind on audio_library (kind);
create index audio_library_title_trgm on audio_library using gin (title gin_trgm_ops);
create index audio_library_description_trgm on audio_library using gin (description gin_trgm_ops);
