# Bundled common-name dictionary: stop flagging correctly-read names as garbles

Viewer names the streamer reads aloud were a big source of auto-harvest noise: a
correctly-read common name (Budi, Siti, Wahyuni, John, Kayla, …) is a real word,
not a garble, but it isn't in the bundled `id`/`en` *word* lists, so the harvest
flagged it. The operator: names get skipped, `names.json` is empty despite the
streamer naming viewers, "maybe a name asset like id.words/en.words — common
Indonesian + world names?"

## Decision

Bundle `assets/dialect/names.words.txt` — common given names (Indonesian +
international) — and **always load it** into the harvest dictionary (every
`DialectLexicon::load`, regardless of the `dictionaries` field), exactly like the
per-Creator `names.json` roster. A correctly-read **common** name is then a known
word and is not harvested; an **unusual** handle is absent from the bundle and
still harvests (the one worth curating — now with the Title + VOD timestamp of ADR
0022 to identify it).

- **Filter only, never primed.** Names go into the harvest `dictionary`, which is
  *never* fed to whisper's `initial_prompt` (priming drifts/hallucinates — ADR
  0014; `prime` is off and only reads vocabulary + confirmed corrections). So the
  bundle changes only what the harvest *flags*, never what whisper *hears*.
- **Always-load, not a `dictionaries` code.** Common given names are
  language-agnostic (a streamer reads viewer names from anywhere), so they belong
  loaded unconditionally — like `names.json` — not gated behind every store opting
  `"names"` into `dictionaries` (forgetting it would wrongly flag names).

### `names.words.txt` vs `names.json`

Complementary, not redundant: `names.words.txt` is the **bundled, universal**
common-name filter (so common names never clutter the queue); `names.json` is the
**per-Creator roster** the operator fills with that streamer's actual viewer
handles (so a streamer-specific handle is known, and a *garbled* handle maps to the
right name via a `corrections` entry). The bundle covers the common case; the
roster handles the long tail per community.

## Sourcing

- **International given names:** `dominictarr/random-name` `first-names.txt` (MIT),
  4,945 curated common first names — downloaded directly (operator-authorised
  credible-source download).
- **Indonesian given names:** a curated set of ~236 common Indonesian given names
  (male, female, frequent name-components). The clean open Indonesian *given-name*
  sources found were either XLSX (`irfnrdh/Dataset-Nama`) or massive scraped dumps
  (`philipperemy/name-dataset`, the Facebook 533M-user dump) — and a *focused*
  common-names set is actually better here than a giant dump: an obscure name is
  garble-prone and *should* still harvest, so over-broad coverage would hurt recall.

Merged, lowercased, kept to pure-ASCII-alpha (≥2 chars), deduped, sorted → 5,135
words. A `#`-comment header records the provenance in-file (the loader skips `#`
lines).

## Considered options

- **Always-load the bundle (chosen)** — universal, low-config, mirrors `names.json`.
- **A `dictionaries` code `"names"`** — rejected: per-store opt-in, easy to forget,
  no upside for a universal list.
- **Prime whisper with the names** — rejected: priming drifts (ADR 0014); the fix
  belongs at the harvest filter, not in transcription.
- **A giant scraped name dump** — rejected: over-filters, hurting harvest recall on
  the unusual handles that are the whole point of the review queue.

## Consequences

- New asset `assets/dialect/names.words.txt` (~36 KB, 5,135 names). The wordlist
  loading is refactored into one `load_wordlist` helper (trim, lowercase, skip
  blank + `#`-comment lines, dedupe) shared by the `dictionaries` codes and the
  always-loaded names — so the bundle can self-document its source in-file.
- The harvest *decision* is otherwise unchanged; only the dictionary it filters
  against grows. Quiet-name *drops* are item 5 (ADR 0021); this is the
  correctly-read-name *false-positive*.

## Outcome

**Shipped + verified (2026-06-28).** `names.words.txt` bundled and always-loaded;
`load_wordlist` helper (+ `#`-comment skipping). 19 transcribe tests green (+1: a
bundled name is loaded and filtered from the harvest, a comment line is skipped,
and an unusual handle still harvests).

**Live (real `assets/dialect` store via `caption_diag`):** the harvest dictionary
grew to 445,852 words (id 79,898 + en 370,105 + names 5,138 − overlaps). Sampled
common names "wahyuni", "salsabila", "raihan" (Indonesian) and "kayla"
(international) are present in **none** of `id`/`en.words` — only `names.words` —
so each would have been flagged before and is now correctly a known word, while
"dominic" was already in `en.words` (harmless overlap).
