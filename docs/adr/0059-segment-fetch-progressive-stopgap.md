# Segment fetch survives the web_safari HLS cutoff: section-seekable selector + 360p progressive stopgap; the native DASH section fetcher is the queued quality fix

On 2026-07-12 the operator promoted a Moment on a fresh podcast VOD and the
Studio editor refused to open. Diagnosis (measured, not guessed): **YouTube
switched the `web_safari` player client to SABR-only** (yt-dlp #12482 — the
wave ADR 0006 explicitly predicted with "if web_safari HLS dries up"), so
the client ADR 0006 pinned now returns **storyboard images only** — zero
media formats — for NEW and OLD VODs alike, on the pinned yt-dlp
(2026.06.09) and the newest (2026.07.04). The fetch's format selector
matched nothing, every retry failed identically ("Requested format is not
available"), and **every Promote on every VOD was dead**. The operator chose
the recovery path interactively: 360p stopgap now, native DASH sections as
the next slice.

## What was measured before deciding (2026-07-12, both yt-dlp versions)

- `web_safari`, `ios`: storyboards only. `tv`: hit by YouTube's DRM-on-all-
  videos experiment (yt-dlp #12563) — storyboards only. `android`, `web`:
  SABR-only, https formats skipped ("missing a URL"). `mweb`: https formats
  PO-token-gated. **No token-free client serves HLS anymore.**
- The DASH https ladder (avc1 up to 1080p) still lists via the default
  client mix / `web_embedded` — but a DASH **section** fetch re-creates the
  M1 hang ADR 0006 documented (moov-not-at-front ⇒ ffmpeg cannot range-seek
  ⇒ the "30 s section" pulls the whole 1.5 GiB file): measured twice, 7 min
  stall and a 150 s bounded kill with zero bytes.
- **itag 18 (progressive 360p, muxed avc1+aac, moov at front) still
  section-fetches perfectly**: a real 53 s moment range in **4 s / 3.6 MB**,
  clean h264 640x360 + aac probe, through the exact production args.

## Decision (operator-picked from the measured options)

1. **`segment_args` selects by SECTION-SEEKABILITY** — the property a
   section download actually needs — in tiers:
   muxed HLS (`protocol^=m3u8`, any client) first: matches nothing today
   but **auto-heals** the moment any client serves m3u8 again; then muxed
   progressive (`acodec!=none`): today itag 18, 360p — the accepted
   quality **stopgap**. Never `bv*+ba` merges, never bare `best` (both can
   land on unseekable DASH = the whole-file hang). The `web_safari` client
   pin is dropped entirely — the default client mix is yt-dlp's maintained
   pick and survives per-client cutoffs.
2. **yt-dlp sidecar + Diagnostics pin bumped 2026.06.09 → 2026.07.04**
   (sha256 + size re-pinned; old exe kept beside as `.bak`). In this arms
   race a month-old extractor is a liability; the fix was validated on the
   exact pinned build.
3. **The quality fix is its own queued slice** (`nextprompt-segment-native-
   dash.md`, now the queue head — a broken Promote outranks the caption
   arc): a native DASH section fetcher — fetch the sidx index of the 1080p
   avc1 + m4a DASH representations, map time→byte ranges, issue ranged
   requests, mux locally. Finally implements what ADR 0006's title
   promised, self-contained (no PO-token service), full quality. Needs its
   own spike + pre-registered gate (does googlevideo still honor ranged
   reads on those URLs at full speed? measure first).

## Considered and rejected (for now)

- **Whole-VOD video cache** (download 1080p once per VOD, cut locally):
  unblocks everything at full quality but overturns ADR 0001's
  never-download-whole rule (~1.1 GiB/hour disk) — kept as the fallback if
  the native DASH spike refutes ranged reads.
- **PO-token provider** (bgutil): restores token-gated ladders but adds an
  external service to the self-contained exe+folders shape (ADR 0006
  rejected once) — and sections on DASH still need the seek problem solved,
  so it is not even sufficient alone.
- **Cookies / tv client**: operator-account risk, fragile, and the tv
  client is currently inside a DRM experiment.

## Consequences

- Promotes work again everywhere, **at 360p**, until the native-DASH slice
  lands: acceptable for editor/framing/caption verification (captions
  come from analysis.wav and are untouched; the export burns 360p video
  and itag-18 audio). The operator accepted the tradeoff explicitly —
  Shorts exported in the interim are test-grade, not publish-grade.
- The measured-alignment pass (the segment-seek machinery) is protocol-
  agnostic — it measures where the download actually sits — so the
  progressive path needs no timing changes.
- If YouTube re-serves HLS on any client, tier 1 picks it up with no code
  change (and quality jumps back to 1080p muxed automatically).
- `fetch-sidecars.ps1` / VERSIONS pinning: the Diagnostics registry is the
  live pin (url+sha256); the in-repo fetch scripts follow it.
- Unit test `segment_selects_only_section_seekable_formats` pins the
  selector's shape: every tier m3u8-or-muxed, no `+` merges, no client pin
  — the M1-hang class cannot silently re-enter through this function.
