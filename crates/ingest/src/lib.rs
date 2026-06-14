//! Ingestion: gets a VOD's audio (and later, padded video segments) into the
//! per-VOD workspace folder. Audio-first, two-phase (ADR 0001):
//! audio + chat replay at import; video segments only when a Moment is
//! promoted to a Clip. YouTube fetches shell out to the pinned yt-dlp
//! sidecar; local files go through ffmpeg audio extraction.
//!
//! M1 scope: local-file audio extraction. M2 scope: YouTube audio + chat +
//! segment downloads.
