-- Shared text layouts. PlanDay and the editor read the same catalog.
create table text_templates (
  id text primary key,
  name text not null,
  description text not null default '',
  elements jsonb not null check (jsonb_typeof(elements) = 'array'),
  seeded boolean not null default false,
  position int not null,
  created_at timestamptz not null default now()
);

insert into text_templates (id, name, description, elements, seeded, position) values
(
  'hook',
  'Hook',
  'One large line near the top of the frame.',
  '[{"name":"Headline","fontFamily":"Inter","fontSize":72,"fontWeight":700,"box":{"x":60,"y":290,"width":900,"height":220}}]'::jsonb,
  true,
  1
),
(
  'hook-subline',
  'Hook and subline',
  'A headline with a smaller supporting line under it.',
  '[{"name":"Headline","fontFamily":"Inter","fontSize":68,"fontWeight":700,"box":{"x":60,"y":290,"width":900,"height":180}},{"name":"Subline","fontFamily":"Inter","fontSize":40,"fontWeight":500,"box":{"x":60,"y":490,"width":900,"height":140}}]'::jsonb,
  true,
  2
),
(
  'lower-third',
  'Lower third',
  'Name and role near the bottom of the safe zone.',
  '[{"name":"Name","fontFamily":"Inter","fontSize":48,"fontWeight":700,"box":{"x":60,"y":1360,"width":900,"height":100}},{"name":"Role","fontFamily":"Inter","fontSize":32,"fontWeight":500,"box":{"x":60,"y":1460,"width":900,"height":80}}]'::jsonb,
  true,
  3
),
(
  'three-stack',
  'Three stack',
  'Eyebrow, headline, and body stacked in the upper half.',
  '[{"name":"Eyebrow","fontFamily":"Inter","fontSize":28,"fontWeight":600,"box":{"x":60,"y":270,"width":900,"height":70}},{"name":"Headline","fontFamily":"Inter","fontSize":64,"fontWeight":700,"box":{"x":60,"y":350,"width":900,"height":180}},{"name":"Body","fontFamily":"Inter","fontSize":36,"fontWeight":500,"box":{"x":60,"y":550,"width":900,"height":200}}]'::jsonb,
  true,
  4
),
(
  'quote',
  'Quote',
  'A centered quote with attribution underneath.',
  '[{"name":"Quote","fontFamily":"Playfair Display","fontSize":56,"fontWeight":700,"box":{"x":60,"y":720,"width":900,"height":280}},{"name":"Attribution","fontFamily":"Inter","fontSize":32,"fontWeight":500,"box":{"x":60,"y":1020,"width":900,"height":80}}]'::jsonb,
  true,
  5
),
(
  'call-to-action',
  'Call to action',
  'Headline plus a short call to action at the bottom.',
  '[{"name":"Headline","fontFamily":"Inter","fontSize":64,"fontWeight":700,"box":{"x":60,"y":330,"width":900,"height":200}},{"name":"CTA","fontFamily":"Inter","fontSize":40,"fontWeight":700,"box":{"x":60,"y":1420,"width":900,"height":100}}]'::jsonb,
  true,
  6
);
