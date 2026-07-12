# Laughter-aware caption holds: trim the gap-fill at a shared-reaction onset (lane 3, measure-first)

> **Status: REFUSED at the operator's eye (2026-07-12, same day).** The A/B
> burn decided it: **"raw production is better"** — the flagged GEMOY/gemes
> is a **drawn-out word** ("gemesss…") whose vocalization stretches INTO
> the laugh, so the gap-fill hold was covering real speech, not lingering
> past it ("if its shorter than it bad, because its a long word like
> gemesssss then laugh"). The mechanism's premise — mask onset ⇒ the word
> is over — is FALSE for the huge-word genre's signature class: emphatic
> drawn-out words are exactly the words that draw group laughs, so the two
> co-occur by nature. Nothing was ever wired (R3's letter had already
> refused shipping); `trim_reaction_holds`, `caption_laugh_diag`, and the
> corpus measurements below stay in-tree as the record; production captions
> were byte-identical throughout. Do NOT revisit hold-trimming at laugh
> onsets without an **end-of-vocalization signal** (the aligner's word ENDS
> are the named candidate) behind its own pre-registered gate.

The operator's standing directive (2026-07-12): fix captions so the fix
applies to **all future videos**. With ADR 0061 defaulting new Creators to
the ensemble, the general-arc queue re-ranked lane 3 to the head: the one
caption lane whose fuel applies to **every engine's** captions — the
shared-reaction (laughter) class the camera lanes already measure and
consume in production (ADR 0045/0046).

The concrete defect is on the record twice:

- **The CORP-hold observation** (2026-07-12 curation handoff, measured
  geometry): a cue's gap-filled hold — `end = min(next onset, onset +
  MAX_HOLD_S)`, ADR 0013 — is laugh-blind. CORP's tail rode **~0.9 s over
  the laugh's start** at ~26 s; the word lingers on glass while the room
  erupts, reading as speech during laughter.
- The gt file names the clip-3 laughter moments the operator's ear flagged
  (3–4 s, 12 s faint, 26 s), and its 2026-07-08 correction pins the HARD
  RULE this slice inherits from ADR 0050's same-day reversal: a word on a
  laugh is a real word on the wrong clock — **never DELETE it; move /
  re-time / trim holds only.** This slice trims holds ONLY.

Distinct classes, deliberately NOT touched here: **pop-on-laugh** (a cue
whose *onset* sits inside a laugh span) is the mis-onset family — ADR 0051
measured auto-moving onsets unsafe, so this slice never moves an onset —
and the phrase-scale CTC re-route class (ADR 0058 / the withdrawn
global-fuzzy shape) is an alignment-input problem, not a hold problem.

Session discipline: the operator delegated ("do this automatically"); the
recorded rubric (nextprompt-caption-general.md, post-ADR-0061 banner) picks
lane 3. The bars below are committed BEFORE the first fixture run (the ADR
0049/0058 pattern). The operator's eye on a burn stays the final gate —
instruments passing is never the gate (ADR 0050/0057).

## The trim rule (pre-declared)

Over the refined units (post `refine_caption_timing[_keep_verified]`, both
engines), with the clip's shared-reaction mask as runs `[L, M)`:

- A unit `[s, e)` whose onset is **unmasked** and where the **first** mask
  run onset `L` falls strictly inside `(s, e)` gets its end trimmed to
  `e' = max(L, s + MIN_READ_S)` — never grown, floor `MIN_READ_S = 0.40 s`
  respected (a trim may never mint a sub-readable flash; ADR 0013/0049).
- A unit whose onset is masked (pop-on-laugh) is **untouched** — that class
  belongs to the mis-onset lane.
- Text, unit count, and every onset are byte-identical by construction; only
  ends shrink. Words added/removed: zero, structurally.

Mask: laughter-family steps (max over the six AudioSet classes) at the
production operating point `REACTION_TAU = 0.1` (the ADR 0045 gate's PASS
tau, ADR 0046's production threshold — reused verbatim, NOT re-swept here;
sweeping tau against these bars would be tune-to-answer), 0.25 s grid, 2 s
centered windows, `SampleScale::Unit`, `TagOutput::Probs` (the selftest-
pinned conventions), computed over the clip's **mixed** analysis.wav range
(the ADR 0050 wiring precedent and the speaker lane's input — never the
sep/enh caption variants).

Production placement (only if the bars below pass): inside
`ensure_transcript`, immediately after the refine, before the transcript is
cached — so the Studio preview and every re-render agree (the ADR 0050
consequence pattern), on BOTH engines. Operator-edited transcripts are
exempt (they never enter the computed branch; ADR 0039 verbatim rule).
Fail-soft: tagger model/labels absent, non-`face` build, tagger error, or
`YC_LAUGH_TRIM=0` (the per-render off-switch, ADR 0055's escape-hatch
pattern) ⇒ captions byte-identical to today.

Known coupling, measured not assumed: karaoke/rolling `group_lines` splits
lines on an end→next-onset gap > `MAX_GAP_S = 1.0 s`, so a trim can flip a
line boundary. A flip AT a laugh is the after-silence rule doing its job (a
shared laugh IS a floor-less pause); R5 requires every flip to sit on a
mask run.

## Fixtures (production engines, raw stores — fresh emits)

The standing cross-clip corpus (ADR 0049/0055/0058), each on its
PRODUCTION engine per workspace/creators.json, dialect stores in production
order (all layers raw since the 2026-07-12 no-JSON ruling):

```
clip                       range           genre        engine     gt
VIOR fans Fadhil (4p)      3592.0-3653.0   huge_word    ensemble   8 onsets + laughs @3-4/12/26
Deddy Tretan/Coki (3p)     1559.6-1629.7   huge_word    ensemble   -
guru gembul Eps1034 (solo) 368.1-427.9     karaoke      ensemble   -
ANTITESA (2p)              790.0-859.0     karaoke      ensemble   -
Helmy Yahya (Nadiem)       1369.0-1413.0   karaoke      whisper    -
```

Emits: `caption_align_diag` `YC_ALIGN_EMIT=1` (the production
`ensemble::apply` end-to-end + refine) for the four ensemble fixtures;
`whisper_align_diag`'s DTW arm (production whisper downstream) for Helmy.
Instrument: `caption_laugh_diag` (new) — computes the mask from
analysis.wav, joins the emitted cues, measures pre/post through the SAME
pure trim function production would call.

Definitions (per clip): **pop-on-laugh** = cues whose onset a mask run
covers. **HOL** (hold-over-laugh) = Σ seconds of `[s, e) ∩ mask` over cues
whose onset is unmasked — the seconds a held word rides over a laugh.

## The PRE-REGISTERED bars (committed before the first run)

- **R1 (mask⇄ear, clip 3)**: a masked step within ±0.75 s of ≥ 2 of the 3
  gt-named laughter moments {3.7, 12.0, 26.0}; the one permitted miss is
  12.0 (the gt itself calls it faint background speech, and it measured
  off-mask on the shipped-clip join). FAIL ⇒ the mask does not see what
  the ear named on this clip: record, ship nothing.
- **R2 (the defect is real, clip 3)**: pre-trim HOL > 1.0 s. Below ⇒ the
  lane is measured-small on its defect clip: record the honest negative,
  ship nothing.
- **R3 (trim efficacy, clip 3 + Deddy 3p)**: post-trim HOL ≤ 30% of
  pre-trim, AND every residual masked second sits inside `[s, s+0.40)` of
  its cue (floor-protected residue only — zero trimmable seconds left).
- **R4 (invariants, every clip)**: unit count identical; texts
  byte-identical; onsets byte-identical; no end grows; every trimmed cue
  keeps dwell ≥ 0.40 s; pop-on-laugh count identical pre/post.
- **R5 (controls, guru + ANTITESA + Helmy)**: every trim NAMED (word +
  run) and sitting on a mask run; zero trims off-mask (structural, still
  printed); every karaoke line-grouping flip coincides with a mask run;
  zero changes of any kind on unmasked spans.
- **R6 (zero words, every clip)**: words added/removed = 0 (the standing
  turn-taking bar; structural for a trim, measured anyway).

Ship gate, in order: R1–R6 pass ⇒ wire the trim into `ensure_transcript` ⇒
a fresh production-order emit of clip 3 reproduces the instrument's
post-trim cues (centisecond-edge tolerance ≤ 0.01 s, the ADR 0057 recorded
class) ⇒ suites green both flavors ⇒ a `segment_seek` re-burn of clip 3 is
STAGED for the operator's eye — the actual gate. Reversal pre-committed:
`YC_LAUGH_TRIM=0` per render; full revert = one pipeline call site + one
pure function + this instrument (captions byte-identical to pre-0062).

## Measured (2026-07-12, fresh raw-store production emits, idle GPU)

Emits: `caption_align_diag` `YC_ALIGN_EMIT=1` per ensemble fixture (fresh 5
sidecar decodes each; clip-3 vote reproduced 117 words / 119 cues — decode
determinism holding); Helmy = the same-day ADR 0058 `clip_wdtw.ass`
(whisper production downstream, code untouched since). Instrument:
`caption_laugh_diag` (tau 0.1 verbatim, mixed analysis audio).

```
clip           cues  mask-runs/s   pop-on-laugh  HOL pre  trims  HOL post  residual  regroup-flips
VIOR 4p        119   15 / 21.00s   27            3.64s    7      1.48s     41%       1 (on a trimmed unit)
Deddy 3p       214    2 /  4.75s    9            0.02s    0      0.02s     (no defect: < R2 floor)
guru solo      164    ~ /  low      1            0.14s    0      0.14s     (no defect)
ANTITESA       184    real laugh   12            0.76s    3      0.56s     74% (floor-protected)
Helmy whisper   83    none          0            0.00s    0      0.00s     n/a
```

- **R1 PASS (3/3)** — the mask covers every operator-named laughter moment
  on clip 3, including the "faint" 12 s row; the Deddy-3p mask lands on the
  ADR 0045 ear-confirmed 25.5–30.25 span (25.25–26.25 + 26.75–30.50), an
  independent reproduction of the annotation of record.
- **R2 PASS** — clip 3 pre-trim HOL 3.64 s (headline riders: the raw
  emit's floating GEMOY@51.20 rides 1.15 s into the 51.25–56 laugh; TUH
  0.80 s; NGELEDEKIN 0.50 s; MAU 0.42 s). The defect is real and visible.
- **R3 clause 2 PASS everywhere**: after the trim, zero trimmable seconds
  remain on any clip — every residual masked second sits inside its cue's
  `[onset, onset+0.40)` floor window. **R3 clause 1 FAIL at its letter**:
  clip 3 residual 41% (> 30%). The 30% number was mis-calibrated against
  the floor's physics: in dense overlap the room erupts within 0.40 s of a
  word's pop, so 1.48 s of the 3.64 s ride is the READABILITY FLOOR's
  protectorate (ISI/KERATIN/MASALAH/KAN/TUH-class 0.10–0.25 s residues),
  which the pre-declared rule itself refuses to trim — by design
  (ADR 0013/0049: a trim may never mint a flash). Deddy 3p's ratio is a
  degenerate denominator (0.02 s pre — below R2's own 1.0 s defect floor).
- **R4 PASS (all five)** — counts, texts, onsets byte-identical; ends only
  shrink; every trimmed cue ≥ 0.40 s; pop-on-laugh counts identical.
- **R5 PASS (all five)** — every trim named and on a mask run (guru 0,
  Helmy 0, ANTITESA 3: KALI −0.02 s, NANTI −0.40 s, YA −0.18 s at its real
  ~62 s laugh); the ONE karaoke regroup flip (clip 3: JALANANNYA starts a
  fresh line after the laugh gap) sits on the trimmed GEMOY — the
  after-silence rule treating the laugh as the pause it is.
- **R6 PASS (all five)** — zero words added/removed.

## Verdict: NOT WIRED (the pre-registration rules), mechanism unrefuted — the re-pin is the operator's

R3 clause 1 failed as written, so per the pre-registration **nothing ships
this session**: production renders are byte-identical; the pure function,
its tests, and both instruments stay in-tree; the emit trim stays behind the
explicit `YC_LAUGH_TRIM=1` opt-in so no diag artifact can silently diverge
from unwired production.

What the measurement actually says: the mechanism did exactly what it
pre-declared, with zero measured harm (no onset moved, no word touched, no
off-mask change, floor never violated, controls clean) — the failed clause
measured the FLOOR, not the trim. The honest fix is a re-pin of clause 1 to
its clause-2 form ("zero trimmable seconds left"), and after the ADR 0050
lesson a bar re-pin plus a caption look-change is the operator's eye's call,
not a session's: the A/B burn is staged (raw production vs trimmed holds,
same 5-decode emit path, only 7 cue ends differ) and the one-sentence ruling
this ADR waits on is **"trim reads better — re-pin and wire"** or **"leave
holds alone"**. The wiring diff (ensure_transcript, post-refine, the ADR
0050 fail-soft shape + `YC_LAUGH_TRIM=0` off-switch) is recorded in the
session handoff, one paste away.

**The ruling landed the same day: A — refused** (see the status banner at
the top for the reason and the one recorded path a successor would need).
