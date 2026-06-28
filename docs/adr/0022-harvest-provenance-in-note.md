# Auto-harvest provenance: record each flagged word's source in its note

The dialect store's auto-harvest (ADR 0014) appends every word whisper was unsure
of — and that the bundled dictionary doesn't know — to `assets/dialect/<lang>.json`
as an `unverified` to-do, so the operator's review queue self-populates. But each
entry was just `{wrong, right:"", note:"auto-harvested (conf X) - operator verify",
status:"unverified"}` — with **no way to find where the word came from**. The
operator: "id.json is hard to curate." To decide what a garble should map to, they
need to hear it in context.

## Decision

Record each newly-harvested word's **source** in its note: the generated Short
**Title** (if the clip has one) and the **absolute VOD timestamp** of the word, as
`auto-harvested (conf 0.42) from "He LOST it on the boss" at 1:23:45 - operator
verify`. The timestamp is the clip's VOD start plus the word's clip-relative onset,
so it points into the source VOD (e.g. a YouTube `?t=` jump).

Threading:
- `harvest_candidates` now returns `HarvestCandidate { word, confidence, start_s }`
  — carrying the unit's clip-relative onset (it previously dropped it).
- `harvest_to_store` takes `clip_start_s` and `title`, computes the absolute time
  (`clip_start_s + candidate.start_s`, formatted `h:mm:ss`), and writes the note.
- `do_render` passes the clip's `range.start_s` and the promoted Moment's generated
  title (`prepared.title`) — both already in hand.

## Considered options

- **Human-readable note (chosen).** The operator curates `id.json` by hand (reads
  the JSON); the note is what they see, and `from "Title" at h:mm:ss` is directly
  actionable. No schema change, no migration of existing stores, and a manual clip
  (no title) degrades cleanly to `at h:mm:ss`.
- **Structured fields (`source`, `at_s`).** Cleaner for tooling, but there is no
  curation UI to surface them today, and they would change the `Correction` schema.
  Deferred until a curation UI wants machine-readable provenance — the note already
  carries the same facts for now.
- **VOD-absolute vs clip-relative timestamp.** Chose **absolute** (operator's
  preference): it locates the word in the *source VOD*, which is what they open to
  check it; a clip-relative time would need the clip on hand.
- **Per-entry title vs first-seen.** A word recurs across clips, but `harvest_to_store`
  already dedupes (a word is recorded once, the first clip it is flagged in). One
  example title+timestamp is enough to locate the word; appending every occurrence
  would bloat the note for no curation benefit.

## Consequences

- `HarvestCandidate` is a small public struct (replaces the `(String, f32)` tuple)
  — clearer at the call sites and room to carry more provenance later. `vod_clock`
  formats the timestamp.
- No change to the harvest *decision* (which words, the dedup, the dictionary
  filter) — only what gets written into a new entry's note.
- `caption_diag`'s harvest line now shows `word(conf@onset)`.
- Underpins item 7c: a *garbled* viewer name now harvests with the Title + time, so
  the operator can identify which garbles are names and map them in `names.json`.

## Outcome

**Shipped + verified (2026-06-28).** `HarvestCandidate` threaded through
`harvest_candidates` → `transcribe_with_harvest`/`_range_harvesting` →
`harvest_to_store`, and `do_render` passes `range.start_s` + `prepared.title`.
18 transcribe tests green (+3): a candidate carries its onset; `vod_clock` formats
m:ss / h:mm:ss; and `harvest_to_store` **writes a real `id.json`** whose note
contains the title "He LOST it on the boss", the absolute time "1:00:12" (clip
start 1:00:00 + word +12 s), and the confidence — the production note-writer
exercised end-to-end with real file I/O.

Live: a real render's harvest pipeline was exercised on the clean Ino VOD
(`caption_diag`), but it surfaced **0 candidates** — the audio is clear and the
always-loaded 79,889-word `id` dictionary filters real words, so this clip has no
garble to harvest (the harvest is correctly selective). The note format itself is
covered by the unit test's real file write; the `do_render` wiring is compile- and
inspection-verified. (Finding for item 7: `id.words` loads regardless of the
`dictionaries` field.)
