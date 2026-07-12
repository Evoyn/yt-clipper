# Native DASH section fetcher: 1080p Segments back without tokens — sidx-planned ranged reads, tiered under the ADR 0059 selector

ADR 0059's stopgap left every Segment at 360p (itag 18) after YouTube cut
off the `web_safari` HLS client. The operator picked this slice as the
quality fix: implement what ADR 0006's title always promised — fetch the
**sidx index** of the 1080p avc1 + m4a DASH representations, map the padded
Segment's time range to **byte ranges**, read exactly those bytes, and mux
locally. Self-contained (no PO-token service, no cookies), full quality,
and the section never degenerates into the whole-file pull (the M1 hang)
because we never ask ffmpeg to seek over HTTP at all.

Discipline: spike-first. The bars below were committed to this file BEFORE
the spike ran (the ADR 0049/0058 pattern); the in-tree instrument is
`crates/ingest/examples/dash_spike.rs`, which exercises the same pure
functions production ships (`yc_ingest::dash`).

## PRE-REGISTERED spike bars (any failure on either VOD = STOP, no wiring)

Fixtures: the fresh ECA podcast VOD (`LeR59VmXiSc`) and the older VIOR VOD
(`o1SBOz5UK2Q`) — one new, one aged, both real workspace Creators.

- **S1 (permission)**: a mid-file ~10 MB ranged read of the itag-137 video
  AND itag-140 audio URLs returns exactly the requested window (HTTP 206 /
  correct byte count), no 403, on BOTH VODs. Both mechanisms probed —
  `Range:` header and the googlevideo `&range=a-b` URL param — and the
  working one recorded here.
- **S2 (speed)**: the 10 MB window sustains ≥ 5 MB/s end-to-end (the
  nsig/PO throttle class measures ~0.05–0.08 MB/s — two orders below the
  bar; ADR 0059's 4 s / 3.6 MB stopgap fetch is the reference class).
- **S3 (index)**: the stream head contains parseable `ftyp`+`moov`+`sidx`
  before any media box; ONE top-level sidx per stream; the sidx entries'
  summed duration ≈ the VOD duration (±2 %).
- **S4 (A/V alignment through the mux)**: video and audio snap to their own
  segment boundaries, so the two part-files start at different absolute
  times; the muxed `segment.mp4` must preserve their RELATIVE offset —
  measured (video_t0 − audio_t0 from sidx) vs the probed first-pts delta,
  within 50 ms — with the exact ffmpeg mux recipe pinned by measurement.
  Absolute placement on the VOD timeline stays the measured-alignment
  pass's job (protocol-agnostic by design, ADR 0059).

## Measured (2026-07-12, `dash_spike`, both fixture VODs — ALL BARS PASS)

```
                         ECA (fresh, LeR59VmXiSc)      VIOR (older, o1SBOz5UK2Q)
S3 video sidx            894 entries, ts 12800,        1006 entries, ts 12800,
                         dur dev 0.00%                 dur dev 0.01%
S3 audio sidx            481 entries, ts 44100,        527 entries, ts 44100,
                         dur dev 0.00%                 dur dev 0.01%
S1/S2 video 10MB mid     1.3 s = 7.9 MB/s              1.4 s = 7.3 MB/s
S1/S2 audio  2MB mid     0.5 s = 4.5 MB/s              0.3 s = 6.8 MB/s
S4 A/V delta err         0.001 s (delta −2.925)        0.000 s (delta +6.376)
section fetch (60-70s)   ~19 s                         ~35 s
```

- **S1 mechanism**: the plain `Range:` header is honored (HTTP 206, exact
  window) — the `&range=` param fallback exists in `read_range` but never
  fired. No 403, no throttle class anywhere (reads at MB/s, 100× the bar).
- **S4 recipe pinned**: `-copyts` on both inputs + `-c copy` preserves the
  sidx-computed A/V offset to ≤1 ms — the fMP4 `tfdt` values ARE the sidx
  times (probed first-pts equalled the plan's `t0_s` exactly on both VODs).
  Production adds `-output_ts_offset -min_t0`, verified on the same
  artifacts: container start_time 0.000, audio at 0, video at +6.376 — the
  exact yt-dlp-sections shape downstream already handles, and the ~10 s
  audio-subsegment snap sits well inside the anchor search's ±30 s.
- The VIOR range was clip 3's own 3592–3653 — the native fetch covers
  [3584.5–3655.8] where the old HLS fetch covered a similar snap; the
  measured-alignment pass owns the rest, unchanged.

## The slice (ships only if all bars pass)

- `yc-ingest/src/dash.rs`: pure box walk + sidx parse (+ unit tests on a
  real dumped head), pure time→byte window planning, ureq ranged reads
  (chunked, `CancelToken`-polled, hard wall budget — the M1 scar's
  discipline without a child process), and the two-stream fetch + local
  ffmpeg `-c copy` mux to `segment.mp4`.
- `fetch_segment` tiers (ADR 0059's order, now fully realized): resolve
  formats once per attempt (`yt-dlp -j`); **tier 1** muxed HLS ≤1080 avc1
  present → the existing yt-dlp section download (auto-heal, unchanged);
  **tier 2** DASH avc1 ≤1080 + m4a present → the native fetcher; **tier 3**
  the ADR 0059 progressive selector (360p floor). Any tier-2 error warns
  and falls to tier 3 — a Promote never fails because the fast path did.
- The ADR 0059 selector unit test stays as the tier-1/3 guard; new pure
  tests pin the sidx parse and the window planning.

## Gate

Suites green; a real Promote on the ECA VOD produces a **1920×1080 h264 +
aac** `segment.mp4` in seconds, the Studio opens, the export plays with
A/V in sync — the operator's eye on the burned Short before the 360p floor
is considered replaced. PENDING until their eye rules.

## Consequences (written at decision time; confirmed at the gate)

- Promote quality returns to the ADR 0006 cap (1080p H.264) with no
  external service; the Offline constraint holds (network only inside the
  user-initiated Promote).
- The signed DASH URLs expire in hours — they are resolved per-promote and
  never persisted.
- If YouTube later gates ranged reads (403s/throttle), tier 2 dies soft
  into tier 3 (360p) — visible as a quality drop, never a broken Promote;
  the whole-VOD cache remains the operator-ranked fallback decision.
- A future YouTube HLS revival silently outranks all of this (tier 1).
