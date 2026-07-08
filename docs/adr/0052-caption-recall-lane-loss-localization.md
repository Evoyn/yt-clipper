# Caption recall lane: a loss-localization instrument first, then a text-only recall-admission rule (ADR 0049 fix #4)

ADR 0049 named **drop** (a word spoken but no cue carries it) as the fourth
overlap-specific caption failure and filed fix #4 as *the heavy one* —
"per-speaker decode or denoise for masked overlap (ADR 0014/0029 reopened)."
This ADR records the measure-first work on that slice. It is **pre-registered**
below (instrument design, pre-committed fix branches, gate bars — grilled and
operator-approved 2026-07-08 BEFORE any measurement); the **Verdict** section is
filled once the instrument runs. Same discipline as ADR 0049's drop-window bar
and ADR 0051's refuted auto-reanchor — the plan and its bars are pinned before
the table is read.

## The operator's drop list (clip 3, `qwen_ensemble`, the must-fix bar)

From `benchmarks/vior-fans-fadhil.captions.groundtruth.txt` (`drop` rows), all on
masked / overlapping stretches:

- **"siapa tau"** (~20 s) — another speaker, just before the real word "SDC";
  whisper's 20.78–23.28 "SDC" span swallowed it.
- **"yang itu isinya otot kayaknya"** (~6.5 s) — only "yang" captioned.
- **"yang keluar kreatin-kreatin"** (~8.5 s) — "yang" + "kreatin-kreatin" dropped
  (kreatin = creatine, the gym supplement — a real, distinctive Indonesian word).
- **"pinguin"** (~52 s) — spoken, never captioned.

## Why fix #4 is NOT assumed heavy — the vote is aggressive at dropping minority words

ADR 0049 assumed a **decode-recall** failure (no decoder hears the word → need new
audio processing). Reading `vote_merge` (ensemble.rs) shows that is unproven:

- The pool is backbone (variant 0 = a6 + head-pad) + 5 voters (4 qwen variants +
  whisper). An **inserted** word (not in the backbone) needs a **strict majority
  ≥3** to survive. Even a **backbone** word is voted out if the majority say it is
  absent (`{pinguin:1, DEL:5}` → DEL wins).

So a dropped word may well *be heard* — by one or two of the six decoders — and
**killed by the strict-majority insert rule**, not by acoustic masking. That is a
cheap, surgical fix (a vote-admission rule), not the heavy per-speaker decode. We
cannot tell which by reading — it needs the GPU. Hence: **localize the loss first.**

## The instrument (pre-registered): `caption_recall_diag`

A measure-first probe forked from `caption_reanchor_diag`, on the PRODUCTION decode
path. For each dropped phrase it decodes **all six views** (whisper + the five qwen
variants, via the same `VARIANTS` the ensemble uses — extracted to a shared
`decode_variants` so the instrument and `ensemble::apply` can never drift) and
classifies the loss:

- **DECODE-loss** — 0 of 6 decoders heard the word → genuinely masked in the mix.
- **VOTE-loss** — ≥1 heard it but it lost the strict-majority insert → reachable by
  a vote-rule change with **no** new audio processing.

**The "heard" test** (settled in the grill): the qwen variants are **untimed word
lists** — and that matches the vote's own semantics (`vote_merge` is text-only;
timing enters only later from whisper's skeleton). So "heard" is a **text-presence**
question, per decoder:

- Match unit = the **distinctive anchor** of each dropped phrase (a rare token:
  `pinguin`/`kreatin`/`otot`; a **bigram** `siapa tau` for the all-common-words one,
  disambiguated by position relative to whisper's `SDC` span). Never a bare common
  word (they appear all over every decode).
- Text tolerance = **fuzzy** (edit-1, edit-2 on longer anchors ≥6 chars), reusing
  the ensemble's own `edit1`/`similar_word` — parity with `apply_store_fuzzy`, the
  production cross-engine garble transfer. `penguin` ≈ `pinguin` counts.
- **Whisper alone** is additionally **time-gated** (it is the only timed decoder);
  the five qwen variants score on presence-in-list only.
- Shipped-`clip.ass` **"carried" confirmation** (the ADR 0049 tolerant alignment):
  a cue carries the word only if its text **fuzzy-matches the anchor** *and* sits in
  a generous time window (~±1.0 s). The fuzzy-*text* match — not a tight time window
  — is what stops a mis-transcribed neighbour reading as "carried" (ADR 0049's trap).

## Pre-committed fix branches (pick the lever from the measured split)

- **VOTE-loss dominant → a text-only recall-admission rule.** The vote is
  **time-blind**, so recall cannot be mask-gated at admission (and leaning on a
  post-placement mask gate is what ADR 0050 got burned on). The admission rule is
  therefore text-only and **structurally inert on clean speech**: re-admit a dropped
  candidate only when it is **(a) distinctive** (a rare content word — function-word
  runs are the hallucination class the strict majority exists to kill), **(b) heard
  in agreement by ≥K denoised variants** (K≈2 — cross-denoise agreement is the
  anticorrelation signal the ensemble rests on), and **(c) absent from the current
  merged output**. Clause (c) means the rule **cannot fire on clean captions** (every
  decoder already agrees there, so the word is already in the output); it fires only
  where decoders disagree — the overlap regime. Fusion then places it via the anchor/
  onset machinery. No mask, no across-the-board threshold drop.
- **DECODE-loss dominant → escalate, then honest-negative as a last resort.** First
  probe **extra decode configs** on the masked span (stronger denoise a3/a0, longer
  leading context, a whisper param sweep) to find *any* config that hears the word;
  if one does, it becomes a new voted variant carried *safely* by the same admission
  rule. Only a word **no decode config anywhere** surfaces — provably buried under
  the co-speaker — falls back to the **transcript override** (ADR 0039, the operator's
  verbatim words), and only with the operator's ear confirming it is not in the mix.
  We do **not** ship a speech-regressing blanket denoise to chase one word (ADR 0029:
  stronger denoise trades masked speech for *different* garbles and breaks curated
  corrections on the production path).

## The gate bars (pre-declared, ADR 0045 discipline)

1. **Recall lands (operator's eye):** every *recoverable* drop appears on the clip-3
   **re-burn**, judged by ear. A word measured truly absent from all audio is
   recorded as audio-limited, not counted as a miss.
2. **No clean speech regresses — proven three ways:** (a) *structural* — the
   admission rule's absent-from-merged clause cannot fire on clean captions;
   (b) *control clip* — run the rule on a **turn-taking** clip (the ADR 0049
   cross-clip set: guru gembul / Helmy / ANTITESA) and require **zero** words added;
   (c) *operator's eye* on the clip-3 re-burn — no clean caption changed text or time.
   This is the false-positive killer: a recall pass that re-garbles clean captions is
   a net loss.
3. **Turn-taking controls unchanged** — the cross-clip control stays quiet.
4. **Suite green, both engines** (fixture count reported at run time).
5. **Gate = the operator's eye on a re-burn** via `segment_seek` + swapping `clip.ass`
   in `camera.fg` — the ADR 0050/0051 cheap path, no GPU camera re-decode.
   **Precondition:** reproduce the *shipped ensemble* `clip.ass` (BEFORE) and confirm
   it matches what shipped (cue times vs the ground-truth `shown_s` column); if BEFORE
   cannot be reproduced, AFTER cannot be trusted.

**Scope: ensemble-engine only.** The fix is a vote-admission rule; whisper is
single-decode and cannot vote. Clip 3 is `qwen_ensemble`. Whisper-engine recall is a
separate lever/slice, out of scope here; whisper users keep the transcript override.

## Verdict (measured 2026-07-08 — a THIRD outcome, neither pre-committed branch)

The instrument decoded all six views (deterministic — two runs byte-identical coverage)
and reproduced production placement (`fuse_onto_timing` on whisper ∪ suppress_nst). The
loss is **neither DECODE nor a live VOTE drop**: the ensemble HEARS all four drops, and
the **current** fusion code already places three of them near the heard moment. A real
`ensemble::apply` render (117 cues, vs the shipped 101) confirmed it on the production
path:

| drop | heard_by | vote | **real `apply` placement** | class |
|---|---|---|---|---|
| **otot** @6.5 | 5/6 | in merged | **OTOT @6.64** (+0.14) | recovered, correct text |
| **kreatin** @8.5 | 4/6 | merged 1× | **KERATIN @8.30** (−0.20) | recovered as a garble → curate |
| **pinguin** @52 | 6/6 | in merged | **PINGUIN @50.58** (−1.42) | mis-onset (ADR 0051), not recall |
| **siapa tau** @20 | 6/6 | merged 2× | **SIAPA @3.46** (−16.5) | mis-placed to the opening |

So the shipped clip's missing `otot`/`kreatin` is **version drift** (the fusion/vote code
evolved since that render — the decode is deterministic, so it is not decode luck), and
`pinguin` was never a drop — it is carried 1.4 s early. **The heavy per-speaker decode /
denoise ADR 0049 feared is measured UNNECESSARY** for these drops, and a speculative
vote-admission rule would be a no-op on this clip (the words already survive the vote), so
shipping one would be the unmeasured-fix trap (ADR 0029 enh). The residual is:

- **otot** — recovered, correct. Nothing to do beyond re-rendering with current code.
- **kreatin** — recovered as `KERATIN`; a one-line dialect correction `keratin → kreatin`
  (curation, the ADR 0014 linguistic lane) fixes the text.
- **pinguin** — carried 1.4 s early → an operator `at_s` time-pin (the ADR 0051 lane), or
  accept the minor lead.
- **siapa tau @20** — the ONE genuine recall residual: heard and voted, but there is no
  timing anchor at 20 s (whisper timed both "siapa tau"s in the opening), so fusion piles
  the second utterance into the opening. Reordering merged words against a non-whisper
  timing signal is unsafe (risks clean-speech regression); a `at_s` pin cannot reach it
  (no occurrence within the ±3 s guard of 20 s). → the **transcript override** (ADR 0039,
  the operator's verbatim words) is the pragmatic fix for this other-speaker phrase, which
  is genuinely hard to place from the mixed VOD audio.

**Deliverable:** `caption_recall_diag` (the loss-localization instrument, `decode_variants`
+ `similar_word` exposed) and this measured reframe.

## Operator ruling + the timing follow-through (2026-07-08)

The naive fresh re-render was **rejected on the operator's eye**: it recovered the words
(`otot`/`kreatin`) but regressed timing (it carried none of the ADR 0051 pins — which were
never persisted) and reintroduced phantoms (`isi`@0, `tergantung`@5) from ensemble version
drift. A **surgical render** — the phantom-clean shipped baseline + the pins + inserted
recovered words (no regeneration) — was operator-approved as the clean target
(`_recall-lane clip3 (SURGICAL - pins + recovered words).mp4`), confirming the words are
real (not invented). But that surgical clip is a **one-off**: it does not carry to other
videos, and the per-clip mis-timing it fixes by hand is exactly what needs a general
solution. That solution is **[[0053-forced-alignment-timing-whisperx-spike]]** — wav2vec2
forced alignment, spike-validated to reproduce the operator's hand-pinned onsets
automatically (`siapa` 3.66→5.08, `sdc` 20.78→22.93, no pins). So the recall lane's
mis-timing residual and the mis-onset lane (ADR 0051) both fold into the forced-alignment
timing skeleton; the recall win itself (the ensemble hears the words) is already in the
code. `keratin→kreatin` stays dialect curation (ADR 0014).
