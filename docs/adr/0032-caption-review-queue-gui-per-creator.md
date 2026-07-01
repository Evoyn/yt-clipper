# Caption review-queue panel: curate the per-Creator layer in-app, grouped by clip

The dialect store's auto-harvest (ADR 0014/0022) drops every unsure, non-dictionary
word into a review queue as an `unverified` to-do (`right: ""`), so the operator's
curation list self-populates. ADR 0031 layered the stores (base ‹ per-Creator ‹
per-clip) and framed the queue as **per-clip** — "an id.json per exported clip, so
it's easy to curate." But curation still meant **opening JSON files by hand** — the
operator: "id.json is hard to curate." This ADR adds the GUI panel that closes the loop.

## The reality that shapes the decision

Two facts, surfaced grilling this against the code + data:

1. **The actionable backlog lives in the per-Creator store, not per-clip files.** On
   disk today: **zero** `<ClipStem>.<lang>.json` per-clip files (the tested VOD's audio
   is clean — harvest surfaced 0 candidates, ADR 0022), while **17 real `unverified`
   to-dos** sit in `workspace/Ino Gemink Live Streaming/id.json` (migrated there in the
   ADR 0031 split, and where every confirmed per-clip fix auto-promotes anyway). A
   strictly per-clip panel would show an **empty queue** and hide the real backlog.
2. **The GUI has no render/Clip state.** `selected` is a Moment id; there is no
   Clip/Short record, and the UI doesn't even hold the creator or `stream_dir`.
   Anchoring a panel to a rendered Short would need new render-tracking plumbing.

## Decision

A **Caption review queue** panel in the detail pane that curates the **per-Creator**
store (`workspace/<creator>/<lang>.json`):

- **Reads the per-Creator layer**, lists every `unverified` (blank-`right`) correction,
  **grouped by the source clip Title** parsed from each entry's provenance note (ADR
  0022) — preserving the "per-export" curation feel without needing separate per-clip
  files. Entries with no title group under "Other".
- Each row shows the garbled `wrong`, the whisper **confidence**, and — parsed from the
  note — a **click-to-open YouTube jump** at the word's absolute VOD timestamp (YouTube
  VODs), so the operator can *hear it in context* (the exact pain ADR 0022 named).
  Local-file VODs show the timestamp as text.
- The operator types `right`, optionally ticks **context** (a real word meant as
  slang/a name — routes through the LLM pass per ADR 0030, not the global dict), and
  **Save** writes the store back (`serde_json::to_string_pretty`), setting
  `status: "confirmed"`. Because the panel edits the **promote-target layer directly**,
  the fix is durable immediately — no re-render needed to promote (unlike a per-clip edit).
- **Provenance is parsed from the note, not read from new fields.** ADR 0022 deferred
  structured `source`/`at_s` fields "until a curation UI wants machine-readable
  provenance"; parsing wins anyway because it works on the 17 existing entries with
  **zero migration** (structured fields would need a legacy-row parser regardless).
- **I/O lives on the UI thread, gated on idle.** The worker sends the per-Creator store
  path + `video_id` on `Progress::Imported`; the panel loads/saves synchronously
  (precedent: `play_range`), and **Save is disabled while a job runs**, so it can never
  race the worker's `promote_confirmed` (the only other writer, mid-render).

## Considered / rejected

- **Per-clip panel (ADR 0031 literal).** Rejected: empty today (no per-clip files);
  hides the real backlog; needs render-tracking the GUI lacks. The per-clip *files*
  remain (harvest still writes them, they still auto-promote) — the panel just reads the
  layer where curation lands.
- **Merged per-clip + per-Creator view.** Deferred: doubles the write-back paths and
  still needs render-tracking, for no benefit while per-clip files are empty. The
  per-Creator layer already receives everything via promotion.
- **Structured provenance fields on `Correction`.** Deferred again (ADR 0022): parsing
  the note is migration-free and sufficient; revisit if the note format proves fragile.
- **Worker-round-trip I/O (`Job::LoadReview`/`SaveReview`).** Rejected as over-plumbing:
  the store is a tiny file and the idle-gate removes the race; a synchronous UI
  read/write matches the existing `play_range` precedent.

## Consequences

- New pure helpers, unit-tested: `yc_transcribe::parse_harvest_note` (conf/title/at_s
  from the note), and app-side jump-URL + group-by-title.
- `Progress::Imported` gains the per-Creator store path + `video_id`; `App` retains
  them; a new `ui_review` renders the panel.
- The panel writes the same layer `promote_confirmed` targets — safe because writes are
  idle-gated.
- CONTEXT.md gains a **Review queue** term.

## Outcome

**Built + data-path-verified (2026-07-01).** The panel ships: `ui_review` in the detail
pane reads the per-Creator store, groups the `unverified` to-dos by source clip, and
edits `right`/`context` in place with an idle-gated Save. Pure logic (`parse_harvest_note`
in `yc-transcribe`; `group_unverified` / `youtube_jump_url` / `ReviewState` round-trip in
the app) is unit-tested — 6 + 4 new tests green — including the exact note strings from
the real store. **Production-path check:** loading the real
`workspace/Ino Gemink Live Streaming/id.json` through the shipped `ReviewState::load` +
`group_unverified` surfaced all **17 to-dos in 8 clip groups** ("Other" — the 3 untitled
harvests — last), confidences parsed, and correct YouTube jump URLs (`nyoli-nyoli` →
t=4271s = 1:11:11; `dulu-diam` → t=2209s = 36:49). Default + `--features face` builds
clean, no new warnings. The interactive click (fill → Save → next render applies) is the
operator's verify-on-use, per the codebase's GUI-wiring convention.
