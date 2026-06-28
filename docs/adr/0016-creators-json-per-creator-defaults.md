# creators.json: per-Creator remembered defaults

ADR 0015 introduced per-stream output folders `workspace/<creator>/<stream-title>/`,
realizing Creator *scoping* on disk but explicitly deferring the **defaults store**
— a global `creators.json` so a Creator remembers settings across its VODs. This
ADR adds that store and its first applied default: the **Caption Style**.

The `core::Creator` type (name, language, seam, caption style, crops) has existed
unused since the M5/M6 era; CONTEXT.md blesses "a Creator carries saved defaults …
that apply to every VOD of theirs." This wires the first of those defaults through.

## Decision

A global **`CreatorStore`** persisted at `workspace/creators.json` (a map keyed by
the Creator's name as it comes from VOD metadata — the same name the stream folder
is sanitized from). It is loaded/saved with the same no-database, fall-back-to-empty
discipline as `project.json`: a missing or corrupt store never blocks import or
render.

**Applied today: the Caption Style genre**, with the standard *remembered-setting*
lifecycle:

- **Seed on import.** When a known Creator's VOD is imported, the store's remembered
  `default_caption_genre` is sent to the UI (on `Progress::Imported`) and pre-selects
  the caption-style picker — the operator's usual choice for that streamer.
- **Operator override wins.** The seed is only a default; the operator can change the
  picker before rendering. Whatever they render with is authoritative for that clip.
- **Save back on render.** After a successful render, the Creator record is upserted
  with the genre just used (creating the record if new, keeping its language current).

So the store learns the operator's habit per streamer without ever overriding an
explicit choice. The precedence question — Creator default vs current GUI selection —
resolves to **seed, override-wins, save-back**, the conventional pattern for
remembered per-project settings.

## The pinned decisions (resolved)

- **Where it lives:** `workspace/creators.json` (workspace root, beside the
  per-stream folders), keyed by the raw metadata Creator name. Keying by raw name
  (not the sanitized folder segment) keeps `core` independent of `yc-ingest`'s
  sanitizer and matches what import has in hand; the rare two-names-one-folder clash
  is immaterial to a remembered-defaults map.
- **Precedence:** seed → operator-overrides-win → save-back (above). No surprise: a
  known Creator pre-fills the picker, the operator stays in control, the last choice
  sticks.
- **What's applied now vs deferred:** only **caption genre** is applied (it is chosen
  *after* import, so the Creator can cleanly seed it). **Language is recorded** on the
  Creator (for the future) but **not yet applied** — language is an *input* to import
  (chosen before the Creator is known from metadata), so applying it needs a
  Creator-aware import UI, a later slice. **Seam and crop defaults are reserved**
  `Option` fields, unapplied — they tangle with M6 per-Segment facecam detection
  (ADR 0011) and deserve their own slice. The reserved fields `skip_serializing_if`
  None, so `creators.json` stays minimal until a default is actually set.
- **`core::Creator` shape:** kept as the domain record but tightened to reality —
  `default_caption_style: String` (an unbuilt named-preset reference) became
  `default_caption_genre: Option<CaptionGenre>` (we select by genre today; ADR 0004's
  "genre selects the builder, the rest is data"), and the unapplied defaults became
  `Option`. A `Creator::new(name, language)` constructor makes a defaults-empty record.

## Considered options

- **Override the operator's selection with the Creator default.** Rejected — a known
  Creator forcing its saved style/language would block the operator from doing a
  one-off differently. Seeding + override-wins keeps them in control.
- **Apply per-Creator language now.** Rejected for this slice — language is selected
  before import (the Creator is unknown until metadata returns), so there is no clean
  seed point without a Creator-aware import UI. Recorded now, applied later.
- **Apply per-Creator seam/crops now.** Rejected — they interact with M6 auto-framing
  (the facecam is detected per Segment, ADR 0011), so a per-Creator crop default needs
  to compose with detection. Its own slice.
- **A separate lean persisted struct instead of `core::Creator`.** Rejected — the
  domain already has `Creator` (CONTEXT.md); reuse and tighten it rather than fork a
  parallel type.

## Consequences

- New `core::CreatorStore` (load/`try_load`/save/get/upsert) + the reshaped `Creator`;
  unit-tested (round-trip, upsert-in-place, minimal-JSON skipping, partial-store load).
- `Progress::Imported` carries `caption_genre: Option<CaptionGenre>`; the GUI seeds its
  picker from it. The app's two helpers — `remembered_caption_genre` (read on import)
  and `remember_creator_genre` (best-effort upsert on render) — own the store I/O; a
  store write failure logs and never fails a render.
- Headless rendering uses the CLI's caption-genre arg (it doesn't seed from the store),
  but **does** save back, so a headless render still populates `creators.json`.
- Touches `core` (the store) and the app (`pipeline.rs` seed/save, `main.rs` apply).
  `yc-ingest`/`yc-render` untouched.

## Outcome (2026-06-28)

Implemented and **verified live end-to-end**. Unit: `yc-core` 5 (2 new store tests).
A real headless render of a 30 s Indonesian gaming clip (`CWN_qbRZBSo` 170–200 s,
`--headless … id karaoke`) drove the whole pipeline — import → `data/analysis.wav`,
M6 face-detect (persistence 1.0), whisper (dialect store, 442k-word filter, harvested
4 words), karaoke ASS, NVENC — and produced `workspace/local/clip/clip-0-00.mp4`
(timestamp-fallback name at the stream root, intermediates under `data/`). The burned
captions show the karaoke fill sweeping white→gold across real Indonesian speech
("BOLEH," gold while "CEPAT KAYAK" white; mid-sweep through "MASAM"). And
`workspace/creators.json` was written exactly as designed:
`{"creators":{"local":{"name":"local","language":"id","default_caption_genre":"karaoke_fill"}}}`
— the reserved seam/crop fields cleanly omitted. One real run validated ADR 0015
(output organization), the M7 karaoke genre, and this store together.
