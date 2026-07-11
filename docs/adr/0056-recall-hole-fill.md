# Caption recall: fill the vote's token holes — decode variance measured first, then a count-deficit admission rule (ADR 0052 follow-through)

ADR 0055 flipped forced-alignment timing to the ensemble default and closed
the mis-onset lane. What remains on clip 3 is TEXT: the 2026-07-11 fresh
votes DROP `pinguin`@52 and `jalanannya`@54 and keep a garbled `gemoy` for
the real ~51.2 s `gemes` — a 5 s token hole over speechful audio, and the
room the ADR 0054 §garble-float floats into (the aligner faithfully spells
the orphan token at the best lookalike near 56). Fill the hole and the float
has no room; then the store can fix the spelling in place.

This ADR is **pre-registered** (same discipline as ADR 0049/0051/0052: the
instrument extension, the pre-committed fix branches, and the gate bars are
pinned BEFORE the measurement table is read). The **Verdict** section is
filled once the instrument runs.

## The open question the instrument answers first

The 2026-07-08 instrument run (ADR 0052's verdict) measured the SAME words
surviving the vote — `pinguin` heard 6/6, in merged; a vote-admission rule
was a measured NO-OP then. The 2026-07-11 fresh decodes dropped them. The
decode was deterministic on 07-08 (two runs byte-identical). So either:

1. **Decode variance across days** — the qwen sidecar decodes drifted
   (llama.cpp runs `--temp 0` greedy, so any drift is GPU/build/env-level,
   not sampling), or
2. **The vote is knife-edge** on these tokens — coverage hovers at the
   strict-majority bar and small decode wobble flips them, or
3. **Both.**

MEASURE FIRST: `caption_recall_diag` on the same production range, twice,
`YC_RECALL_DUMP` writing every decoder's exact word list so the two runs
diff byte-precisely. Byte-identical dumps = deterministic TODAY (the 07-08
delta is then cross-day drift — environment, not code, since the vote pool
code is unchanged through ADR 0055: `suppress_nst` fed only DTW timing,
never the vote). Differing dumps = live nondeterminism, measured per
variant and per anchor word.

## The instrument extension (pre-registered)

- New DROPS rows beside `pinguin`@52 (spoken count corrected 1 → 2: the
  shipped clip's vote carried two pinguins, today's keeps only the ~50.5 s
  one — ADR 0054): `jalanannya`@54 (10 chars; `similar_word` edit-2 also
  reads whisper's `jalannya` spelling) and the garble-family row
  `gemes`@51.2 (edit-≤2 groups gemes/gemoy/gemot, so per-decoder hits count
  the FAMILY; spoken=2 = real `gemoy`@49.7 + garbled `gemes`@51.2).
- A fused-units 48–58 s timeline print (the hole as a picture, not
  inferred) on both the measure pass (DTW repro — presence is
  fusion-independent) and the `YC_RECALL_EMIT=1` real-`apply` arm
  (production aligned timing — where the float actually lands).
- `YC_RECALL_DUMP=<path>`: per-decoder word lists + merged, for the
  two-run byte diff.

## Pre-committed fix branches (the lever is picked by the table)

**VOTE-loss (≥1 decoder hears a missing word) → count-deficit admission
rule** — ADR 0052's pre-registered text-only rule, with two deltas forced
by evidence gathered since:

1. **Count semantics, not presence** (delta from 0052's "absent from
   merged"): `pinguin` is spoken twice and today's merged carries it once —
   presence-based absence would refuse the second occurrence. The rule
   admits a candidate when ≥K denoised variants each carry MORE fuzzy
   occurrences of it than merged does (deficit ≥1), admitting AT MOST ONE
   occurrence per word per vote (the minimal fill).
2. **The pad-bleed guard** (new, structural): the agreeing denoised
   variants must include **≥1 unpadded** one. Without it the two
   head-padded variants (V0 backbone + V3) agreeing on pre-clip words —
   which the vote correctly kills 4-vs-2 — would resurrect them: pad bleed
   is exactly "a distinctive word 2 denoised variants heard that is absent
   from merged". Unpadded variants physically contain no pre-clip audio,
   so one agreeing unpadded witness kills the class.

Rest as pre-registered in ADR 0052: **distinctive** = ≥5 chars and not in
a static language-level common-word list (function-word runs are the
hallucination class the strict majority exists to kill; the list is
generic-Indonesian, never per-clip — the bias_context contract);
**K = 2 denoised** (cross-denoise agreement is the anticorrelation signal
the ensemble rests on; raw V4 and whisper corroborate but do not count);
**time-blind** (text only at admission — ADR 0050's lesson); insert
position from the first agreeing unpadded variant's token alignment
against merged, positions computed on the pre-admission merged and
inserted back-to-front. Runs between `vote_merge` and `apply_store_fuzzy`
so admitted words still get store corrections. Structurally inert on clean
speech: where every decoder agrees, merged already carries the word at
full count — no deficit, no fire.

**DECODE-loss (0/6 hear a word) → escalation ladder, no admission rule**:
extra decode configs on the masked span (stronger denoise a3/a0, longer
leading context, whisper param sweep) before any heavy per-speaker
machinery; a word no config hears anywhere = transcript override (ADR
0039) with the operator's ear confirming it is not in the mix. We do not
ship a speech-regressing blanket denoise to chase one word (ADR 0029).

**The gemes garble is NOT an admission case**: the vote KEEPS a family
token for it (gemoy). Its fix after the hole fills is positional store
curation (`at_s` gemoy→gemes @51.2), which becomes safe once the token
stops floating — with pinguin@52 back, the nearest-gemoy-to-51.2 is the
garbled one, not the real @49.7 (the twin-token trap ADR 0054 measured
dies with the float). Confidence gates and pre-fusion fuzzy rewrites for
the float stay MEASURED DEAD ENDS (ADR 0054) — not rebuilt here.

## The gate bars (pre-declared)

1. **The hole fills on the operator's clips**: pinguin@~52 and
   jalanannya@~54 appear near their heard moments in the real-`apply`
   output (and on the re-burn); the GEMOY float lands ~51.2 (or is renamed
   in place by the store once stable).
2. **Zero words added on turn-taking controls** (the ADR 0052 bar,
   turn-taking regime by its own pre-registration): guru gembul
   368.1–427.9 (solo) + ANTITESA 790.0–859.0 (2p) — run on their
   production decodes. The Deddy Tretan/Coki 1559.6–1629.7 clip is the
   OVERLAP regime (ADR 0055's own table): admissions there are the rule's
   target class and are reported as observations, not bar violations.
3. **No clean caption regresses** on clip 3: cues outside 48–58 s
   unchanged between pre-fix and post-fix real-`apply` runs, modulo decode
   variance the two-run diff already measured.
4. **Suite green, both engines.**
5. **Gate = the operator's eye on the re-burn** (segment_seek +
   `camera.fg` with the swapped .ass — the ADR 0050/0051 cheap path).
   Render is notified to the operator before it runs (standing
   instruction).

## Verdict (measured 2026-07-11; run 1 + dump — the hole has a STRUCTURE the
## per-anchor rows couldn't show)

**The hole is one six-word overlap phrase, and denoising is what kills it.**
Run 1 (fresh production decode, whisper 101 units / 103 words, variants
129/113/117/128/126 words, merged 117) localized every 51–56 s loss at once
via the `YC_RECALL_DUMP` word streams:

```
whisper:  ... pinguin gemot  jalanannya lucu aku ngefans banget mereka kamu ...
V4 (raw): ... pinguin gemer  njalannya  lucu aku ngefans banget mereka kamu ...
V0–V3 (denoised, all four): ... pinguin gemoy kamu ...
merged:   ... pinguin gemoy  kamu ...            <- the 5 s hole
```

The dropped phrase (`jalanannya lucu aku ngefans banget mereka` — the
co-speaker's line) is heard by EXACTLY the two un-denoised decoders and by
none of the four deep-filter variants: **the denoiser suppresses the
overlapping co-speaker as noise**, so for the overlap-drop class,
cross-DENOISE agreement (ADR 0052's clause b) is anti-correlated with
hearing the word — backwards at its root, not merely no-op. Two witnesses
(whisper + raw V4) lose 2-vs-3 to the strict-majority insert rule. This is
the third shape the measurement forced on this lane (0052: no loss; 0055:
version drift; now: witness structure).

Per-anchor classification (run 1):

| anchor | heard_by | in merged | placed (DTW repro) | class |
|---|---|---|---|---|
| pinguin @52 | 6/6, **1× each** | 1× | 50.56 | **not dropped** — matches the operator-approved surgical baseline (PINGUIN@50.58); the "second pinguin" exists in NO decoder today |
| jalanannya @54 | 1/6 (whisper timed @53.5; V4's `njalannya` is 3 edits out of the fuzzy family) | 0× | NONE | **VOTE-loss, whisper-only witness** |
| gemes @51.2 (family gemoy/gemot/gemer) | 6/6, 2× each | 2× | 51.20 (+0.00) | fully voted; the float is purely the aligner's spelling of the orphan next to the hole |
| siapa tau @20 | 6/6 | 2× | 3.46 | unchanged — the ADR 0052 transcript-override lane, admission correctly silent |

**The shipped branch: whisper-witness count-deficit admission**
(`ensemble::admit_recall`, called in `apply` between `vote_merge` and
`apply_store_fuzzy`; both engines' stores still apply). Deltas from the
pre-registration, forced by the table: the witness clause is **whisper
alone** (the ≥K-denoised clause was pre-registered twice and measured
useless twice — 07-08 no-op, today anti-correlated; it is NOT shipped);
count-deficit and the distinctiveness bars as pre-registered. Position
comes from [`align_weighted`] (the fusion's similarity-weighted DP), not
the vote's exact-token `align` — the exact-cost DP pairs a candidate
crosswise onto a foreign token at equal cost (jalanannya↔gemoy while
gemot becomes the Ins), which a unit test caught before it shipped.

On this clip the rule admits `jalanannya` + `ngefans` (both whisper-deficit
1, distinctive) in whisper order into the gemoy→kamu gap; `lucu`/`aku`
(short) and `banget`/`mereka` (common-listed) stay dropped by design — the
named complaint is the hole and the float's room, and the fills fence the
float on both sides. Admitting the full 2-witness RUN (whisper + raw V4
agree on the whole phrase) is a recorded NON-GOAL for this slice: it is a
new rule shape outside the pre-registration; revisit only if the operator's
eye wants the connective words back.

`admit_recall` is unit-tested nine ways (fills-in-place, count-parity
inertness, common/short refusal, one-per-family cap, agreement inertness,
same-gap ordering, deddy/duduk disagreement zones, range-edge partials);
workspace suite **372 green** (363 + the 9 new).

**Decode variance: NONE — byte-deterministic same-day.** Two full
production decodes (run 1 / run 2, `YC_RECALL_DUMP` word streams): all six
decoder word lists AND the merged vote byte-identical (`--temp 0` greedy
holds on this GPU). The vote is not knife-edge day-to-day. Cross-day: the
07-08 fresh decode ALSO voted one pinguin@50.6 (ADR 0052's real-apply
table) — the "two pinguins" only ever existed in the 07-07 shipped-era
vote, so the "version drift between votes" the nextprompt asked about
narrows to shipped(07-07)-vs-everything-since; jalanannya had no 07-08 row
to compare. Three watchdog aborts during the operator's contended gaming
window (variants 1/0/2 — a different variant each time, whisper always
passing, a 16 s healthy probe decode between aborts) were the 129 s budget
doing its job on a bursty machine, not decode instability.

**The admission's first live firing caught a false-positive class the
pre-registration missed.** On clip 3 the unguarded rule admitted FOUR
words: `jalanannya` + `ngefans` (the real hole, placed 53.48 / 55.64 on
the whisper skeleton — the hole fills) but also `deddy` and `duduk`:

- whisper `susu deddy corp` vs merged `susu dedikornya` — the word is
  PRESENT in the vote, agglutinated >2 edits outside the fuzzy family;
- whisper `duduk duduk` vs merged `ada dodo` — the gt's own wrong-row
  garble (`duduk-duduk` should be `dodo`), re-admitted beside the
  correctly-voted word.

Both share one structural signature the true hole lacks: their Ins runs
sit **adjacent to a Del** — merged already carries its own unpairable
token there, so whisper is RE-SPELLING a kept word (the curation lane),
not filling a gap. The shipped rule's **disagreement-zone guard** skips a
candidate whose Ins run borders a Del on either side (the candidate stays
live for a cleaner later occurrence); a true hole inserts between two
cleanly matched neighbors (`jalanannya`/`ngefans` between Ok(gemoy) and
Ok(kamu)). Known conservative trade: a phrase dropped IMMEDIATELY beside a
dissimilar garble would be skipped too — recorded, acceptable; the
operator's named class is the clean hole. Regression-tested both ways
(deddy-shaped, duduk-shaped).

**The turn-taking control caught a second class: range-edge truncation.**
The guru gembul control's first pass admitted `dirinya` — whisper
transcribes the word the range boundary cuts mid-utterance (a trailing
`diri`, prefix-matching the family and inflating its count), while the
one-shot qwen decodes emit no boundary partials at all. The **edge
guard** refuses admissions at the extreme ends of the merged stream
(interior gaps only; a truncated boundary word is not a hole). Regression
test covers head and tail shapes.

**The float is dead on the production path** (`YC_RECALL_EMIT` real
`apply`, aligner on, guards in): admitted exactly `jalanannya` + `ngefans`;
the garbled-gemes token that floated to 56.06 under ADR 0054 now aligns
at **51.20**, fenced by `jalanannya`@52.76 and `ngefans`@55.98. Cue-level
diff vs the ADR 0055 aligned artifact: 114/117 shared cues time-identical;
the only deltas are the GUE pin (not layered in this instrument's base
lexicon — real renders load the per-clip store) and the fix itself
(float@56.06 → gemoy@51.20 + the two admitted words). The 0054/0055
headline numbers reproduce with admission in (siapa@5.08, sdc unchanged,
pinguin@50.60 = the surgical baseline). Aligner residual for the
operator's eye: `jalanannya` at 52.76 vs their by-ear ~54 (whisper's own
skeleton said 53.5) — the `at_s` pin lane remains the override if the
burn reads early.

**Overlap-regime observation (Deddy Tretan/Coki 3p, 1559.6–1629.7,
pre-edge-guard binary):** the rule admitted `maksudnya` (heard by whisper
AND three denoised variants — 4/6 decoders — yet dropped by fragmented
gap-voting on dense overlap) plus second occurrences of `bingung`/`parah`
(repeated banter words whisper alone counted twice). The rule generalizes
to its target class beyond the authoring clip; the operator's eye judges
that clip if it ever re-renders.

**Gates, final binary (all pre-declared):** guru gembul (solo) and
ANTITESA 790.0–859.0 (2p) turn-taking controls: **NOTHING admitted**
(zero-added holds with all three guards in). Clip 3 emit with the final
binary: admitted exactly `jalanannya` + `ngefans`, float at 51.20 —
byte-stable across guard changes (decode determinism). Suite 372 green.

**The store pin landed** (`gemoy → gemes` @3643.2, clip_only, beside the
gue pin): with the hole filled, the nearest-gemoy-to-51.2 is the garbled
twin, so the ADR 0054 twin-token trap is structurally gone. On the burn
artifact (`clip_alignburn.ass`, 119 cues, store layered) the window reads
`GEMOY@49.72 · PINGUIN@50.60 · GEMES@51.48 · JALANANNYA@52.76 ·
NGEFANS@55.98` — both of the operator's named residuals from the ADR 0054
viewing are addressed on glass, and the pinned GUE sits at 28.14.

**Burned**: `_recall-hole-fill clip3 (ADR 0056 - production).mp4`
(segment_seek recipe, camera_align.fg, NVENC). **The operator's eye on
this burn is the one gate still open** — named check: `jalanannya` at
52.76 vs their by-ear ~54 (the `at_s` pin lane is the override if it
reads early).

**A/B addendum (2026-07-11 evening, eye-followthrough session):** the pin
lane was pre-staged so one sitting decides — `_recall-hole-fill clip3
(ADR 0056 - AB jalanannya at 54).mp4` burns the identical clip with the
`jalanannya -> jalanannya @3646.0` clip_only pin applied: JALANANNYA
52.76 -> **53.60** (the positional pass onset-snapped the operator's
~54.0 pin; whisper's own skeleton had said 53.5), emit cue-diff exactly
that ONE cue, decode determinism holding elsewhere. The pin is **not**
in the store — production state is untouched; the verdict picks A
(aligner) or B (pin). Instrument note for future readers: the diag's
stderr shows two `at_s pin ... no such word` WARNs per pinned-emit run —
those are the diag's own dtw/align diagnostic passes, whose word lists
are pre-admission (no jalanannya exists there); the burn artifact's pass
runs inside the real `apply` post-admission and its success line is
info-level (suppressed at the diag's default `warn` filter).
