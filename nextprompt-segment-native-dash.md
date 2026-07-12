# Session prompt — native DASH section fetcher (the ADR 0059 quality fix)

> **RESOLVED — SHIPPED 2026-07-12 (ADR 0060).** The spike passed all four
> pre-registered bars on both fixture VODs (ranged reads at MB/s, sidx dur
> dev ≤0.01%, A/V delta ≤1 ms via the copyts+output_ts_offset recipe) and
> the native fetcher is live as tier 2 of `fetch_segment` (HLS auto-heal →
> native DASH 1080p → progressive 360p floor). Operator-eye gate on a real
> Promote: see ADR 0060's gate section. Queue returns to
> nextprompt-caption-general.md (lane-2 / engine-default fork).

> **Why this is the queue head (2026-07-12):** YouTube killed the
> `web_safari` HLS client (ADR 0059) and every Promote now runs on the
> 360p progressive stopgap — test-grade, not publish-grade. Restoring
> 1080p Segments outranks the caption arc (which stays queued:
> nextprompt-caption-general.md, lane-2/engine-default fork). The operator
> picked this path over whole-VOD caching and PO-token providers, with
> whole-VOD cache as the recorded fallback if the spike refutes.

You are working in F:\yt-clipper (pure-Rust egui app). Fresh session: read
ADR 0006 + ADR 0059 + this file, then /grill-with-docs BEFORE any code.

## The idea (finally implement what ADR 0006's title promised)

YouTube's DASH https formats (avc1 137/136/135 + m4a 140 via the default
client mix / web_embedded) are single files whose **sidx index maps
time → byte ranges**. A native fetcher can: fetch the head (ftyp/moov/sidx,
a few hundred KB), parse sidx, compute the byte window covering the padded
Segment range, issue ranged reads (HTTP `Range:` and/or the googlevideo
`&range=a-b` URL param), then mux video+audio locally with the bundled
ffmpeg (concat of the init segment + the in-range media, `-c copy`).
Self-contained (no PO service, no cookies), full 1080p quality, and the
Segment stays a little longer than the Clip exactly as CONTEXT.md defines.

## Spike FIRST (measure before building — the ADR 0006/0059 discipline)

Pre-register in the session grill, then measure with curl/PowerShell before
any Rust:

1. **Do googlevideo DASH URLs honor ranged reads at full speed, without PO
   tokens, today?** Fetch ~10 MB windows from an itag 137 URL (get the URL
   via `yt-dlp -g -f 137`): HTTP `Range:` header vs `&range=` param; measure
   speed + completion on BOTH a fresh VOD and an older one. 403s /
   throttling-to-KB/s = refuted → fall back to the whole-VOD cache decision
   (operator already ranked it next).
2. **Is sidx present at the head** of those files (it should be for
   mp4_dash)? Parse offsets by hand once (a hex dump is fine for the spike).
3. **URL lifetime**: the signed URLs expire (~6 h) — fine per-promote, but
   confirm a promote's 3 fetches (video head, video window, audio window)
   inside one resolution survive.

## The slice (only after the spike passes its bars)

- `yc-ingest`: a `dash_section` module — resolve formats (yt-dlp `-j`),
  pick avc1<=1080p + m4a, head-fetch + sidx parse (pure, unit-tested on a
  dumped sidx), ranged reads with the existing cancel/timeout discipline
  (the M1 scar: hard wall-clock, killable tree), local ffmpeg mux to
  `segment.mp4`.
- `fetch_segment` tiers: HLS (auto-heal) → **native DASH** → progressive
  360p (the stopgap becomes the last-resort floor). The ADR 0059 selector
  test extends to pin the new tier order.
- The measured-alignment pass needs zero changes (protocol-agnostic by
  design) — but re-verify on one real promote (segment_seek numbers).
- Gate: a real Promote on the JADI/ECA podcast VOD producing a 1080p
  segment.mp4 in seconds, editor opens, export plays — the operator's eye
  on the burned Short before the stopgap floor is considered replaced.

## Hard rules (unchanged)

- Measure before building; pre-register bars; the operator's eye gates.
- Offline constraint: user-initiated network only (ingest + pinned
  downloads) — the fetcher runs only inside Promote.
- GPU rules, PS 5.1 rules, commit -F file: per memory / prior handoffs.

## Ritual

/grill-with-docs first; finish with /handoff + whatwedone.md entry; commit
as Evoyn with the model trailer; `git push origin main` has standing
permission; ALWAYS end with the next `read nextprompt-<slug>.md and follow
it.` line (this file until the slice ships; then back to
nextprompt-caption-general.md).
