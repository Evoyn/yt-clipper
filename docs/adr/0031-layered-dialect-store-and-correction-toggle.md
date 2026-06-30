# Layered dialect store (base < per-Creator < per-clip) + a GUI correction toggle

The dialect store was a single per-language file (`assets/dialect/<lang>.json`,
ADR 0014). After the LLM correction pass shipped (ADR 0030), the operator asked for
two things to make curation practical at scale:

1. **Per-Creator memory** — a streamer's confirmed slang/names should carry across
   *all* their VODs, not live in one global per-language pile (a fix for Creator A
   shouldn't apply to Creator B).
2. **Per-export curation** — "an id.json per exported clip, so it's easy to curate":
   each Short should get its own small review queue of *that clip's* garbles, instead
   of the operator hunting through one ever-growing global list.

Plus a **GUI toggle** for the (off-by-default) correction pass.

## Decision

### Three layered stores
Same `DialectLexicon` schema at every layer; a render loads and merges them:

| layer | path | role |
|-------|------|------|
| **base** | `assets/dialect/<lang>.json` (bundled) | shared dictionary wordlists + config (`prime`/`harvest`/`dictionaries`) + generic corrections |
| **per-Creator** | `workspace/<creator>/<lang>.json` | the streamer's confirmed slang/names — applies to **all** their clips |
| **per-clip** | `<stream>/<ClipStem>.<lang>.json` (beside the exported `.mp4`) | **this clip's** auto-harvest — a small, easy-to-curate per-export queue |

- **Merge** (`load_layered`): corrections union, **most-specific wins** on a duplicate
  `wrong` — clip > creator > base. The dictionary/config come from base; overlays
  contribute only `corrections`. A missing/unparseable overlay is skipped.
- **Harvest** writes to the **per-clip** store (`harvest_to_file`), so each export's
  unknown words land in their own file, with the title + VOD timestamp note (ADR 0022).
- **Auto-promote** (the operator's choice): on each render, every *confirmed* (filled
  `right`) per-clip correction rises to the per-Creator store (`promote_confirmed`),
  skipping ones it already knows. So the operator curates once, in the small per-clip
  file, and it **sticks for the Creator** — applying to every future clip of theirs.
  (Manual-only was the alternative; auto was chosen for least friction.)

The flow: harvest → per-clip queue → operator fills `right` → next render applies it
(via the merge) *and* promotes it to the Creator store → it now corrects all their VODs.

### GUI correction toggle
A "Correct captions (LLM)" checkbox (default **off** — opt-in, matching the
unsigned-off status). It sets a `correct` bool on `Job::Render`, gating the
`#[cfg(feature = "correct")]` pass in `do_render`. Headless/batch read `YC_CORRECT=1`
(off otherwise) for the same bool. Still requires a `correct` build + the sidecar.

## Considered / rejected
- **One global file with clip tags.** Rejected: the operator wanted *separate* small
  files per export to curate, and per-Creator scoping to stop cross-Creator bleed.
- **Manual promotion only.** Rejected (operator chose auto): re-typing confirmed fixes
  into the Creator file is friction; auto-promote keeps the per-clip file as the single
  place to curate.
- **Migrate the existing base corrections to per-Creator now.** Deferred: the bundled
  base keeps its current clip-7 confirmations (they work for the operator's main
  Creator); new corrections flow base-untouched into per-clip → per-Creator. The base
  can be slimmed later.

## Consequences

- `DialectLexicon`: new `load_layered`, `harvest_to_file` (path-based; `harvest_to_store`
  now delegates), `promote_confirmed`, and a `merge_corrections` helper — all
  unit-tested without a model. `do_render` computes the per-Creator + per-clip paths,
  loads layered, harvests to the clip store, and promotes.
- `Job::Render` gains `correct: bool`; `do_render` gates the pass on it; a GUI checkbox
  + `correct_from_env()` (headless) drive it.
- A re-render of the same clip reuses its per-clip store (keyed to the clip stem, not
  the dedup'd export filename), so curation accumulates rather than duplicating.
- CONTEXT.md's **Dialect store** definition updated to describe the layering.

## Outcome

**Shipped (2026-06-30).** Layered base/per-Creator/per-clip stores with auto-promotion,
and a default-off GUI correction toggle. Tests green; default + all-feature builds
clean. The correction pass itself stays off until the operator signs off (ADR 0030).
