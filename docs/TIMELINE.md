# Timeline JSON

Video projects store a timeline document (JSON) edited in the browser and synced with `POST /video-projects/{id}/apply` (RFC 6902 JSON Patch).

## Canvas

- **Size:** 1080×1920 (Instagram Reels)
- **FPS:** 30
- **Time units:** milliseconds (`start`, `end`, `in`, `out`)
- **Layers:** `tracks[0]` is the **top** layer (drawn first in the Remotion stack)

## Tracks

| `kind`   | Accepts item types                          |
|----------|---------------------------------------------|
| `video`  | `clip`, `image`, `slideshow`, `motion`      |
| `overlay`| above + `captions`, `text`                  |
| `text`   | `text`, `captions`, `motion`                |
| `audio`  | `audio` only                                |

## Items

Shared fields: `id`, `start`, `end`, optional `pending` (placeholder copy while generating).

- **clip** — `asset` path, `in`/`out` trim, optional `volume`, `transform`, `matte` `{ base, foreground }` WebM paths
- **image** — `asset`, `transform`
- **slideshow** — `assets[]`, `interval` ms, `transform`
- **text** — `text`, `style`, `transform`, optional `behindSubject` clip id
- **captions** — `captions[]` (Remotion caption shape), `combineMs`, `style`, `transform`
- **motion** — `code` (default-export React component), optional `props`, `transform`
- **audio** — `asset`, `in`/`out`, `volume`, optional `fadeIn`/`fadeOut`

Asset paths are project-relative (e.g. `Screen recordings/demo.mp4`), resolved via `POST /projects/{id}/assets/resolve`.

## Client editing

Semantic **EditOps** (`lib/timeline/ops.ts` in skywoop-app) compile to patch ops. The server applies the same patch engine as the client (`src/domain/video.rs`).

## Revision

`video_projects.rev` is authoritative. Each apply returns `inverse_ops` for undo.
