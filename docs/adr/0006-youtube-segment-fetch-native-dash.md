# YouTube segment fetch: HLS section download via the web_safari client

ADR 0001 said the promoted-Clip video Segment is fetched with yt-dlp `--download-sections`, but the 2026 YouTube reality (SABR + PO tokens) plus M1's scar make the *mechanism* non-obvious. What we learned by spiking it (2026-06-17):

- The **`web`** client now returns **SABR-only** formats yt-dlp can't download directly (no PO token) — its format list comes back empty.
- The **`android_vr`** client returns direct **DASH** (`https`) formats, but their moov atom isn't at the front, so **ffmpeg cannot range-seek them over HTTP** (`could not seek to position 600 / partial file`). Seeking means downloading the whole multi-GB file first — *that* is what M1 experienced as the "10-minute hang," not a missing timeout.

We decided to fetch the Segment from the **`web_safari`** player client, which serves **HLS (m3u8) formats without a PO token**. HLS is fragmented, so `--download-sections` selects only the in-range segments via yt-dlp's native HLS downloader — **no ffmpeg seek at all**. deno stays on the child PATH to solve nsig (anti-throttle); ffmpeg only muxes/cuts.

**Spike-validated 2026-06-17:** a 60 s 1080p60 H.264 section (itag 301) downloaded in ~5 s / 37 MB — clean `h264 1920x1080 + aac`, no hang, no whole-file pull.

## The command shape

    yt-dlp --download-sections "*<start>-<end>" \
      -f "best[height<=1080][vcodec^=avc1]/best[height<=1080]" \
      --extractor-args "youtube:player_client=web_safari" \
      --socket-timeout 30 --ffmpeg-location <ffmpeg> \
      -o "<workspace>/segment.%(ext)s" <url>

## Considered Options

- **ffmpeg `--download-sections` on android_vr DASH (ADR 0001's literal mechanism)** — rejected: moov-not-at-front formats can't be range-seeked; seeking pulls the whole file (the M1 hang).
- **`web` client DASH** — rejected: SABR-only in 2026, not downloadable without a PO-token provider.
- **PO-token + web/mweb DASH** — rejected for now: needs a PO-token provider plugin running, contradicting the self-contained exe+folders shape. Revisit only if HLS quality proves insufficient.
- **HLS via `web_safari` (native fragment section download)** — chosen: token-free, fragmented (no seek), delivers 1080p60 H.264 muxed.

## Consequences

- **deno remains a required, pinned sidecar** (nsig). Confirmed in the spike (`[jsc:deno] Solving JS challenges using deno`).
- web_safari's HLS formats are **muxed** (video+audio in one itag, e.g. 301), so the Segment needs no separate audio-merge. The whole-VOD analysis **audio** at import is still its own small download.
- The **1080p H.264 cap** decision lands exactly on itag 301 (1080p60). Higher than 1080p would require DASH (android_vr, unseekable) or a PO token — deferred.
- `--download-sections` on HLS cuts close to the request (spike: 60.016 s for 60 s), but the start still snaps to a segment boundary — so we **pad the request and read the true in-segment offset via ffprobe** before the frame-accurate export cut (unchanged from the original plan).
- Every yt-dlp/ffmpeg child still gets `--socket-timeout` and a hard wall-clock/cancel guard (the M1 scar) even though the HLS path is fast.
- This tracks YouTube's anti-bot moves: if web_safari HLS dries up, the fallbacks are a PO-token provider or cookies (tv client). Pinned yt-dlp + deno versions in `VERSIONS.txt` keep breakage diagnosable.
