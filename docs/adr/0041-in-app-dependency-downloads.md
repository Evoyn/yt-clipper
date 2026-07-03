# In-app dependency downloads: pinned official sources, SHA-256-verified

The Diagnostics registry (2026-07-03) shows every tool and model the pipeline
resolves — but a missing row only pointed at a PowerShell script. A fresh
machine (or a deliberately slim install) should heal from inside the app: each
missing row now carries a **Download** button, plus one **Download all
missing**, sized before it starts. This is a deliberate widening of the
network boundary; CONTEXT.md's **Offline** term was re-worded for it in
advance: the only permitted network uses are *user-initiated* — ingesting a
VOD, and fetching a missing tool or model from its pinned official source.

## Considered options

- **Pinned URL + pinned SHA-256 per artifact, in-app (chosen).** A static
  table (`App::download_specs`) holds, per artifact: the version-stable
  official URL, the expected SHA-256, the pinned byte size (drives a real
  progress bar), and the install shape (plain file, or zip picks + a DLL
  sweep for the llama runtime). Downloads run on the existing serial worker
  (`Job::Download`) — a download never races a GPU job, the existing Cancel
  aborts mid-stream, and progress rides the existing channel
  (`Progress::Download`). The stream stages to a `.part` **beside the
  destination** (same volume), hashes while streaming, and only a verified
  artifact is renamed into place; a mismatch deletes the partial and fails
  loudly. New deps: `ureq` (rustls — no OS TLS stack assumed), `sha2`,
  `zip` (deflate only). ~40 LB of glue; no async runtime.
- **Rejected: shell out to the fetch scripts.** `fetch-*.ps1` are
  PowerShell-5.1-quirk-laden, checksum-free, and partly unpinned
  (`releases/latest`); driving them from the app would inherit all three and
  add a console-window problem ADR 0025 just finished killing.
- **Rejected: latest-version URLs.** `releases/latest` can't be checksummed
  (the content moves), and an unverified multi-GB model write into `models/`
  is exactly the kind of silent drift the registry exists to prevent.
  Version-pinning trades that for **pin rot** — accepted, see below.
- **Rejected: a package manager (winget/scoop).** Covers the exes at best;
  the models (the bulk) still need custom fetching, and winget's deno is
  already only a fallback path.

## The pins (verified 2026-07-03)

Every hash was taken from upstream release metadata (GitHub asset digests,
HF LFS OIDs, zenodo checksums) and **cross-verified byte-identical against
the operator's working set** — the pins install exactly what today's renders
run. Sources: gyan.dev (ffmpeg 8.1.2 essentials), yt-dlp 2026.06.09 (tagged),
deno v2.9.1 (tagged), llama.cpp b9859 CUDA pair (tagged), DeepFilterNet
v0.5.6 (tagged), HF at pinned *revisions* (whisper large-v3, Silero VAD,
Qwen3-ASR Q8_0 pair, bartowski's Qwen2.5-7B Q5_K_M — the judge GGUF's true
origin, found by hash), google/fonts at a pinned commit (Anton), Linzaer at a
pinned commit (Ultraface), zenodo record 6221127 (audeering w2v2 SER zip).
`yc-llm-judge.exe` has no pin: it is built with the app and ships beside it.

## Consequences

- **Pin rot is a maintenance duty, not a runtime risk.** HF revision URLs and
  GitHub tags are immutable; gyan.dev rotates old versioned packages out
  (404) and a moved artifact fails the SHA check — either way nothing
  half-installs, and the fix is bumping URL+hash together in
  `download_specs`, nowhere else. The fetch scripts stay for dev bootstrap;
  the app table is the operator-facing path.
- Presence stays live (`path.exists()` per frame), so rows turn green the
  moment an install lands; a fresh deno sidecar re-resolves in both the UI
  and the worker without a restart.
- The worker is busy while downloading (serial by design): the model fetch
  (~3 GB) blocks a detect exactly as long as it would have blocked on the
  missing model. Cancel deletes the partial.
- Zip extraction trusts the pinned archive but still refuses a layout drift:
  a pick that matches no entry aborts the install with the missing name.
- Local files are never re-hashed against the pins at runtime (presence is
  the check) — an operator's hand-replaced model stays their business
  (credible-source rule).
