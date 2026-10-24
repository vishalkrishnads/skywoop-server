# Plan markdown format (v2)

Daily plans are a single markdown document per platform and date. The server parses the title, summary, and video sections; video sections become `plan_videos` rows, and the plan desk renders `flow` and `assets` fenced blocks as UI cards. PlanDay writes this format from prompt `plan_day` version 6.

## Full example

````markdown
# Tactile ergonomics vs digital clutter
Contrast timer fatigue with the rotary dial; test whether tactile beats yesterday's widget reel.

## Video 1: Why 99% of focus timers ruin your dopamine {#video-1}
- track: audience
- audience: 2
- daypart: prime
- caption: Your timer is shouting at you
- hypothesis: Tactile contrast beats yesterday's widget reel on retention.

Concept prose: hook, story beats, on-screen text, CTA.

```flow
- 0:00-0:03 | Hook: Digital noise | Loud beeps and blinking counters
- 0:03-0:07 | Friction | Sensory fatigue of a normal timer
- 0:07-0:12 | Reveal | The rotary dial turning, calm daylight
```

```assets
- [ ] video: close-up of a hand turning a rotary dial on an iPhone, soft daylight
  - name: Rotary dial close-up
  - reuse: 40
  - model: kling-2.5-turbo-pro
- [x] image: app icon @"App Store/app-icon.png"
  - name: App icon
```
````

## Title and summary

- The first `#` heading is the day's title (stored as `daily_plans.title`).
- The paragraph between the title and the next heading is the summary (stored as `daily_plans.summary`), joined onto one line. It must be at most 280 characters.
- PlanDay output must have both. Hand edits may omit them; older plans titled `# Plan for <date>` still parse with an empty summary.

## Headings and anchors

Every `##` section must carry a stable anchor:

```markdown
## Video 1: Hook title {#video-1}
```

Anchors match `[A-Za-z0-9][A-Za-z0-9_-]{0,63}` and are unique. Video anchors start with `video-`. Other sections such as `## Strategy {#strategy}` or `## Findings {#findings}` are optional; comments attach to any anchored section.

## Video metadata (bullets)

```markdown
- track: audience
- audience: 2
- daypart: prime
- caption: On-screen / post caption text
- hypothesis: What we expect to learn
```

`track` is `audience` or `trend`. `audience` is a 1-based index, id, or exact audience name (audience track). `trend` is a trend id or title (trend track). `daypart` is `prime` | `morning` | `afternoon` | `evening` | `night`.

## Flow block (timed steps)

````markdown
```flow
- 0:00-0:03 | Hook: Digital noise | Loud beeps and blinking counters
- 0:03-0:07 | Friction | Sensory fatigue of a normal timer
```
````

Each line is `- m:ss-m:ss | Step | description` (the description is optional). The API returns `flow_steps: [{ label, description, start_ms, end_ms }]`.

Validation for timed flows:

- steps start at `0:00` and are contiguous (each step starts where the previous ended);
- each step ends after it starts;
- 2-8 steps;
- the last step ends between 8 and 25 seconds.

When any line in the block is timed, every line must be. Legacy chains such as `Hook[Girl scrolling] --> Problem[12h] --> Reveal[App]` still parse: each node becomes a step with `label` before the brackets, `description` inside them, and null timings. Legacy flows skip the timing checks.

## Assets checklist

````markdown
```assets
- [ ] image: front-facing character portrait on a plain wall
  - name: Character portrait
  - reuse: 80
- [~] video: the same character turns toward the doorway, one continuous shot, opaque
  - name: Doorway turn
  - reuse: 35
- [~] image: screen-time still reading about 12 hours @"Generated images/screen-time.png" <!-- REGENERATE: show 11 hours -->
  - name: Screen-time still
- [x] image: app icon @"App Store/app-icon.png"
  - name: App icon
```
````

Markers: `[ ]` open, `[~]` pending generation / review, `[x]` done.
Kinds: `image`, `video`, `audio`.
Reference assets with `@"path"` or `@path` only when `search_assets` returned that exact file.

Checklist keys are `{anchor}#{n}` (1-based line index inside the block). Updates go through `PUT /plans/{id}/checklist`.

### Directives

Indented bullets under a checklist line:

| Directive | Applies to | Rule |
| --- | --- | --- |
| `name:` | every line | Short label shown on the asset card, at most 60 characters. Required on PlanDay output. Lines without it fall back to the first 6 words of the description (or the file name of its `@path`). |
| `model:` | every line | Optional generation model id matching `[a-z0-9._/-]{1,80}`. |
| `reuse:` | open lines | Integer 0-100: how reusable the asset is for other ideas (not confidence). High means a generic plate; low means the shot only fits this video. Confirmed lines omit it. |
| `mode:` / `duration:` / `input:` | `video:` lines | Legacy generation controls (see below). |

### Derived status

The API returns a `status` for each line:

| Line | `status` |
| --- | --- |
| `[ ]` | `open` |
| `[~]` without `@path` | `queued` |
| `[~]` with `@path` | `generated` |
| `[~]` with `@path` and `<!-- REGENERATE: note -->` | `regenerate_queued` |
| `[x]` | `confirmed` |

`regenerate_note` carries the text of the REGENERATE mark. Attaching a new file drops the mark. `preview_url` is a signed read URL for the line's first `@path`.

### Generation directives (legacy / optional on video lines)

Older plans may still declare fal controls as indented bullets under a `video:` line. New drafts leave mode, duration, and inputs to the video subagent at generate time. When present:

- `mode:` `text` (default) | `start_frame` | `first_last_frame` | `reference` | `extend`
- `duration:` integer seconds (1–60)
- `input: <role> <target>` where role is `start_frame`, `end_frame`, `reference_image`, `reference_video`, `reference_audio`, or `source_video`, and target is either a checklist key (`video-1#1`) or an exact `@"asset path"` from the asset manager

When a plan still declares inputs, the operator must Confirm those prerequisite lines (`[x]` with an attached `@path`) before Generate activates for the dependent video.

### One line = one generated shot

The operator predominantly generates assets. Each checklist line must be a prompt a model can run in one pass.

- A `video:` line is one continuous opaque fal clip (~5s, 9:16) described as a visual scene, not human instructions ("inspect", "record if absent"). One camera setup. A second angle, cut, "then", or "plus" is a new line. Multi-shot prompts hallucinate.
- Video models cannot produce alpha / transparent / green-screen plates. Ask for an opaque frame; if the subject must sit over another plate, describe a plain background so the editor can matte it.
- Character-over-background is two `video:` lines (plate + subject). The editor overlays them; the generator never composites.
- An `image:` line is one still (aspect ratio free; the editor fits it into 9:16). An `audio:` line is one track: start with `voiceover`, `sound effects`, or `music`. Voiceover includes a gpt-audio-1.5 voice and the exact words to speak. Sound effects and music describe the sound. Leave audio lines open until the operator generates them; generation may reuse a shared royalty-free library or Fal.
- Captions and on-screen text belong in the `caption:` bullet / flow / editor text items, not burned into a video prompt.
- Real product UI comes from existing screenshots or recordings. If none exist, checklist an `image:` still, not a video of the app.
- The `flow` block is edit order. The video model never sees the whole video.

Validation flags `video:` descriptions that contain ` plus `, ` then `, ` cut to `, or the words `transparent`, `alpha`, or `green screen`. Saving a plan splits those joiners into one line per shot, and splits a comma list of shots (`hook, post, and return`). A semicolon stays on the same line: a qualifier is kept, and a "record the missing…" clause is dropped. Do not use em dashes or en dashes in plan text.

## No-op days

PlanDay may decide to post nothing (`no_op`) when earlier posts exist and are still gaining views. A no-op plan's markdown is just the title and summary (no video sections), and the video-count checks are skipped. The plan's `no_op_reason` / `no_op_explanation` carry the decision. Operators can also mark a day as no-op and resume it; see [API.md](API.md).

## Status workflow

Typical operator flow: `review` → `generating_assets` → `building` → `built` → `scheduled` → `posted`. Any status before `scheduled` can become `no_op`.
Past dates and locked plans are read-only.

See `src/plan_format.rs` for validation rules used by PlanDay and `PUT /plans/{id}`.
