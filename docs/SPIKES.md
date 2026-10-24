# Phase 0 spikes

Checkpoint: plan-based execution is on `dev` (`b8ad716`); PRODUCT.md captures the video pivot.

## Spike A — Browser Azure OpenAI via MSAL

**Goal:** Call Azure OpenAI from the browser with an Entra token in AI SDK `createAzure({ tokenProvider })`.

**Requirements**
- Entra SPA app registration with delegated *Azure Cognitive Services* / *Cognitive Services OpenAI User*
- Operator assigned *Cognitive Services OpenAI User* on the AOAI resource
- Custom subdomain endpoint (`https://{resource}.openai.azure.com`)
- AOAI CORS allowed origins include the skywoop-app origin

**Fallback:** Next.js route that forwards the browser's bearer token; agent loop stays in the browser (`DirectChatTransport`).

**Status:** Documented for implementation in Phase 7 (`lib/ai/azure.ts` + AuthProvider). Live CORS verification happens when Entra + AOAI roles are wired in Phase 11.

## Spike B — Remotion `renderMediaOnWeb` 1080×1920

**Goal:** Export a ~15s reel in Chrome from blob-hosted media, including:
1. A JIT-compiled `motion` item (`@babel/standalone` + allowlisted remotion scope)
2. An alpha WebM foreground from `@remotion/video-matting` `separateVideoLayers`

**Constraints**
- Azure Blob must send CORS for GET/HEAD (and PUT for uploads)
- Web renderer: no `perspective`; layer order = DOM/track order (not `z-index`)
- Web renders always send Remotion telemetry
- Chrome is the target browser

**Status:** Packages and composition land in Phases 7–9. Measure render time during first editor export smoke test.
