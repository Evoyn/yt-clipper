# Too-fast grouping: cramped huge-word runs fall back to a compact 2–3 word line (ADR 0049 fix #2)

ADR 0049 measured the shipped 4-person VIOR captions and pre-declared the
**too-fast** class: a cue on screen `< MIN_READ_S` (0.40 s) is sub-readable.
The approved fix roadmap queued **grouping** as fix #2 — "when onsets are
denser than the floor allows, fall back from one-word-per-cue to a compact
2–3 word line (shared reading budget)" — with the huge-word look-bend
**flagged and accepted by the operator as a tradeoff to weigh at that gate**
(2026-07-08). Fix #1 was reversed (ADR 0050: real fast speech, not phantoms),
fix #3 became the operator time-pin + forced alignment (ADR 0051/0053–0055),
fix #4 (recall) closed on the eye (ADR 0052→0056). This is the last roadmap
slice, taken under delegated authority (operator: "do this automatically",
2026-07-11 evening; the pick is the only menu item with a standing operator
approval and no interactive dependency).

## The mechanism (why cues flash, measured)

`refine_caption_timing` floors each word's end at `start + MIN_READ_S` *where
there is room* and clamps to the next onset LAST — correct for sync (one word
on screen, never overlap), but when onsets pack tighter than 0.40 s the clamp
defeats the floor by design and the cue is sub-readable. Huge-word is the only
genre that makes one CUE per WORD, so it alone inherits the raw onset density:

- Approved burn artifact (`clip_alignburn.ass`, the eye-passed ADR 0056 state,
  119 cues): **74/119 = 62% sub-0.40**, longest cramped run **12 consecutive
  cues** (13.90–18.71 s, the VIOR banter).
- ADR 0055 cross-clip (word-unit basis): VIOR 4p 63%, Deddy Tretan/Coki 3p
  80%, guru gembul solo 73%, ANTITESA 2p 74%.
- ADR 0049's "turn-taking 0–2% sub-floor" rows were **line-genre clips**
  (karaoke/rolling cues are whole lines) — NOT evidence that turn-taking
  speech is sparse at word level. Solo lecture speech runs ≈2.7 words/s;
  sub-0.40 word spans are speech physics, not an overlap exclusive. The
  overlap clips are still the defect surface (the operator ships them
  huge-word and named them); the genre, not the regime, decides who is
  exposed. Recorded so the 0049 control row is not misread as "grouping must
  never fire on turn-taking material".

## Decision

Group **cramped runs** at the caption line model (`preview_lines`, HugeWord
arm) — the single home both the ASS emitter and the editor preview consume
(ADR 0036), applied after all timing (refine, pins), text untouched:

- A unit is **cramped** when `next.start_s - start_s < MIN_READ_S` (strict,
  f64; the next-onset clamp will defeat its floor).
- A cramped unit **starts a group**; the group grows while the group window
  (`next-after-last.start − group.start`) stays `< MIN_READ_S`, capped at
  **3 words** and **`MAX_LINE_CHARS` (22) chars** (the line genres' proven
  fit). By construction every absorbed word's onset lies `< MIN_READ_S` after
  the group start — grouping can never bridge a real pause.
- Group cue: `start = first.start_s`,
  `end = last.end_s.max(start + MIN_READ_S).min(next_after_group.start_s)` —
  the established floor-then-clamp-LAST discipline lifted to the group.
- **Singletons keep the verbatim old expression** (`end_s.max(start+WORD_MIN_S)
  .min(next.start)`) — a transcript with no cramped unit renders
  byte-identical ASS.
- Grouped lines render **compact**: `PreviewLine.font_scale` (new field, 1.0
  everywhere else) = `GROUP_FS_FRAC` = **0.64** (= 96/150, the line genres'
  proven 22-char size at the huge-word default), emitted as an inline `\fs`
  on the resolved size and multiplied into the editor overlay's font — one
  shared number, the two surfaces cannot drift (ADR 0036). Singleton
  Dialogue text is byte-identical (no `\fs` when scale is 1.0).
- Presentation is a **static line** (all words visible from the group start,
  `WordState::Base`): maximal per-word read time, the karaoke precedent for
  words visible pre-onset. The rejected alternative — per-word reveal inside
  the group (rolling-pop tags) — preserves onset sync but gives the group's
  last word a sliver of solo visibility (the same sub-0.40 flash, one level
  down); if the eye dislikes early-visible words, that variant is the
  pre-committed iteration.

## Pins vs grouping (measured before building)

An `at_s` pin is the operator's ear asserting a word's *appearance* time;
absorption into a group would move that appearance earlier. Measured on the
approved artifact: all three pinned cues — GUE@28.14 (prev window 3.53 s, own
0.55 s), GEMES@51.48 (own 2.92 s), JALANANNYA@54.40 (own 1.58 s) — are
non-cramped with non-cramped predecessors, so their starts survive grouping
**byte-exact**. The collision remains possible for a future pin inside a
cramped run; the pre-committed fix (only if an eye-approved pin ever lands
there) is threading a `pinned` flag so a pinned unit may only *start* a
group — not built now (no live case; one implementation per session).

## Pre-registered bars (declared before the after-measurement)

- **A (fix):** clip 3 (VIOR 4p) sub-0.40 cue rate **62% → ≤ 15%**;
  Deddy Tretan/Coki 3p **≈80% → ≤ 25%**. Residual = cap-limited bursts
  (>3 words or >22 chars inside one 0.40 s window) and tail clamps.
- **B (text):** concatenated cue text identical before/after on every clip —
  zero words added / dropped / reordered / respelled. The vote and admission
  are untouched (presentation-only change); zero-added on turn-taking
  controls holds trivially.
- **C (genre + sparse control):** rolling-pop / karaoke outputs byte-identical
  (arm untouched, pinned by existing tests); huge-word output on transcripts
  with no cramped unit byte-identical (existing unit tests pass unchanged).
- **D (pins):** GUE@28.14, GEMES@51.48, JALANANNYA@54.40 cue starts byte-exact
  in the regrouped artifact.
- **E (invariants):** cues stay non-overlapping and monotonic; every grouped
  line ≤ 3 words and ≤ 22 chars; no word appears more than `MIN_READ_S`
  before its own onset; `font_scale` = 0.64 exactly on multi-word lines,
  1.0 on singletons.
- **F (named expected delta, for the eye):** KAYAK@50.32 (window 0.28 s)
  absorbs PINGUIN → one "KAYAK PINGUIN" line at 50.32 (PINGUIN's appearance
  −0.28 s, dwell 0.88 → 1.16 s). Inside the eye-approved 48–58 s stretch;
  called out in the verdict script rather than discovered.
- **G (production-path identity):** the CPU instrument's regrouped prediction
  and the fresh `YC_ALIGN_EMIT=1` production emit must agree byte-for-byte
  (known epsilon: a grouping decision could flip only within the .ass
  centisecond quantum; the emit is truth if they differ).

## Measured (caption_regroup_diag over the ADR 0055 align artifacts, 2026-07-11)

```
clip                          regime            sub-0.40 cues      median dwell   grouped lines
VIOR fans Fadhil 4p (the      overlap (defect)  62% -> 1% (1/72)   0.32 -> 0.65s  40 (87 words)
  eye-approved alignburn)
Deddy Tretan/Coki 3p          overlap (densest) 80% -> 5% (5/104)  0.23 -> 0.56s  80 (187 words)
guru gembul (solo lecture)    turn-taking       72% -> 3% (3/94)   0.30 -> 0.57s  61 (131 words)
ANTITESA 2p                   turn-taking       73% -> 1% (1/99)   0.26 -> 0.58s  68 (153 words)
```

Bars A, B, C, D, E all pass at instrument level on every clip: every walk OK
(text preserved word-for-word), every singleton Dialogue byte-identical to its
input line, headers identical, invariants clean, cap-limited residuals ≤ 1
per clip. The pinned cues GUE@28.14 / GEMES@51.48 / JALANANNYA@54.40 all stay
singletons with byte-exact lines (bar D).

**Bar F correction (recorded, not moved):** the predicted KAYAK+PINGUIN merge
was wrong — the greedy left-to-right walk reaches BANGET@50.02 (also cramped)
first, so the merges near the approved stretch are "TUH GEMOY"@49.58 and
"BANGET KAYAK"@50.02, and **PINGUIN keeps its approved 50.60 start untouched**
(as do GEMES, JALANANNYA, NGEFANS). Strictly smaller delta than pre-registered;
the eye's attention goes to the two 2-word lines instead.

**The §G honest-negative confirmed:** turn-taking clips group heavily too
(61–68 grouped lines) when rendered huge-word — word-level onset density is
speech physics, not an overlap exclusive. Those creators ship line genres;
nothing they ship changes.

**Bar G (production-emit identity), measured:** the fresh `YC_ALIGN_EMIT=1`
emit (5 new decodes, GPU idle-gated, probe 6 s) differs from the instrument's
prediction in exactly ONE decision — the pre-registered centisecond-quantum
class: UMKM@30.31 − GUE@29.91 is exactly 0.40 s; the cs-reconstructed window
computes 1.4e-15 below the floor (→ predicted 3-word group) while production's
raw f64 times read ≥ 0.40 (→ "GUE BUKA" at 0.40 s + UMKM singleton at 1.20 s —
both readable). Production is the artifact of record: **73 cues, 1 sub-0.40
(1%), all three pins byte-exact, everything else byte-identical to the
prediction.** (This GUE@29.91 is not the pinned GUE@28.14, which is identical
on both sides.)

## The measure loop

`caption_regroup_diag` (`crates/render/examples/`, CPU-only): reconstructs
units from an existing huge-word `.ass` (roundtrip proof: re-emitting through
the OLD path must reproduce the input byte-for-byte), then prints old-vs-new
cue stats (sub-0.40 %, grouped-cue count, word/char caps hit, text-preservation
verdict, pinned-cue starts) and writes `<input>.regrouped.ass`. Run over the
four ADR 0055 align artifacts. Then ONE production emit (5 fresh decodes,
GPU idle-gated) → bar G byte-compare → NVENC burn (the ADR 0056 recipe) →
**gate on the operator's eye**: approved
`_recall-hole-fill clip3 (ADR 0056 - AB2 jalanannya 54.4).mp4` (A) vs
`_toofast-regroup clip3 (ADR 0057 - grouped).mp4` (B). Both staged
2026-07-12 (burn exit 0, 50.47 MB, 61.000 s; gate 4×15 s at 1–2% util,
probe 6 s); the verdict script with pre-committed branches is
`nextprompt-toofast-verdict.md`.

## Consequences

- The huge-word invariant weakens from "never more than one word on screen"
  to "never more than one *cue* on screen; a cue is one word except in
  cramped runs, where it is a compact ≤3-word line at the line-genre scale".
  Sync semantics: a grouped word can appear up to 0.40 s before it is spoken
  (karaoke shows unsung words earlier than that; ADR 0019's never-early rule
  governed *mis-timed onsets*, not designed line context).
- Rolling-pop / karaoke are untouched: their cues are already lines.
- The editor preview shows grouped lines at the same compact scale the burn
  uses (`font_scale` in the shared model — ADR 0036 discipline).
- The instrument's reconstruction trick (units from a huge-word .ass) is
  exact up to centisecond quantization; production emits stay the artifact
  of record (bar G).
- **Gate: the operator's eye on the A/B burn.** Ships dark until then — the
  code lands but the eye rules on the look tradeoff it was promised
  (2026-07-08); the reversal path is small and recorded (drop the grouping
  walk, singletons are already verbatim).
