# Segment fetch: retry transient yt-dlp format-resolution failures

A 10-clip batch export (the "guntur69" horror VOD, `BUDS9qx2jw0`) rendered 3
clips, then **aborted** on the 4th: yt-dlp exited 1 with

```
ERROR: [youtube] BUDS9qx2jw0: Requested format is not available.
```

The batch driver treats any `Progress::Failed` as fatal (`exit(1)`), so one
failed fetch threw away the 7 unrendered clips.

## Finding (diagnosis)

The log makes the failure mode unambiguous. A **healthy** Segment fetch
(`web_safari` HLS, ADR 0006) walks: `webpage → player → m3u8 → format 301`. The
failed 4th fetch stopped one step in — `webpage → ERROR` — never reaching the
player/m3u8 stage, and erroring **before any bytes downloaded**. The three fetches
before it and the audio import all succeeded.

So this is not a systematic format loss (the avc1/itag-301 HLS path still works);
it is **transient**: an unlucky `web_safari` webpage response that yields no
player JS, so format selection fails immediately. This is exactly the SABR-era
extraction flakiness ADR 0006 already documents — just on an unlucky draw. A fresh
yt-dlp invocation re-resolves the format almost every time.

## Decision

Retry the Segment fetch up to **`SEGMENT_FETCH_ATTEMPTS = 4`** times
(`SEGMENT_RETRY_BACKOFF_S = 3`s between tries), in `yc_ingest::youtube::fetch_segment`:

- Each attempt re-runs the same `segment_args` yt-dlp invocation, **clearing any
  partial `segment.*` first** (a failed try may leave a `.part`).
- A **cancel is terminal** — checked before each attempt and after each `run`
  error, so a user cancel surfaces immediately and never burns retries.
- After the final attempt the original error is returned, wrapped with the attempt
  count. A genuinely unfetchable range (e.g. a dead spot, or a request YouTube
  refuses repeatedly) still fails — it just no longer fails *spuriously*.

The retry lives at the fetch (not the batch driver) so it hardens **every** caller
— GUI Promote, `--headless`, and `--batch` alike — against the same transient.

## Considered options

- **Retry inside the batch driver only.** Rejected: GUI/headless Promote hit the
  same flaky `web_safari` extraction; the fix belongs where the flake is.
- **Skip-and-continue + over-fetch in `--batch`** (on a per-clip failure, skip and
  backfill the k-th slot from the remaining ranked candidates). Deferred, not
  rejected — it is the *completeness* fix (guarantees k clips even when a moment is
  truly unfetchable), orthogonal to this *transient* fix. Worth doing next; needs a
  small batch state-machine change (keep all candidates, track a cursor, advance on
  `Failed`). Left out of this slice to keep the change surgical.
- **A different extractor client / PO token for the retry.** Rejected for now: the
  same `web_safari` path works on retry (it worked 3× in the same run), so changing
  clients is unwarranted complexity. Reconsider only if retries stop sufficing.
- **Just re-run the whole batch by hand.** Rejected as the fix: re-renders the
  good clips, and a fresh run is just as exposed to the next unlucky draw.

## Consequences

- A flaky fetch now costs up to ~9s of backoff + re-extraction instead of killing
  the run. On the happy path (no failure) nothing changes — the loop runs once.
- `fetch_segment` gains a retry loop + two tuning consts (tune-from-use, like the
  caption-timing and harvest constants). No signature change; all 14 `yc-ingest`
  tests still pass (the `segment_args` contract is untouched).
- The batch is now resilient to *transient* fetch failures but **not yet** to a
  persistently-unfetchable moment — that is the deferred skip-and-continue work.

## Outcome

**Shipped (2026-06-30).** Retry added to `fetch_segment`; `yc-ingest` compiles and
14/14 tests green. Re-running the 10-clip export of `BUDS9qx2jw0` is the live A/B —
the same moment that aborted the run should now fetch on a retry.
