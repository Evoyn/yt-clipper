# Audio-first two-phase ingest

Gaming VODs are long (multi-hour) and heavy (10–20 GB at 1080p), but the analysis that finds clip-worthy moments only needs audio (~400 MB for 6 h). We decided the app never downloads a full VOD's video: ingestion downloads the audio-only stream for transcription and moment detection, and full-quality video is fetched later as small padded segments (`--download-sections`), only for the moments the user promotes to clips.

## Consequences

- The moment-review UI is built on transcript, waveform, and audio playback — there is no full-VOD video scrubbing.
- Moment detection can only use audio/transcript signals (no kill-feed or other visual detection) unless this ADR is revisited.
- Section downloads cut at keyframes, so segments are downloaded with padding and the frame-accurate cut happens at export time.
- Local-file VODs skip the download but follow the same two-phase shape: audio is extracted first for analysis.
