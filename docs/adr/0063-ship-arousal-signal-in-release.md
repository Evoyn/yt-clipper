# Ship the arousal Signal in release builds (wire `ser`; fail-soft a corrupt model like the llm judge)

`scripts/build-release.bat` builds `--features face,align` — no `ser`. The ser-gated refine pass (app pipeline.rs ~1036) is therefore compiled out of every production binary, degrading two Signals at once: the default `Weights` reserve 0.15 for arousal (yc-detect lib.rs:109) and renormalize it away, so Moments rank loud-vs-flat blind — the exact false-positive class ADR 0008 exists to kill — and the LLM judge's prompt corroborates against `arousal_z` (yc-detect llm.rs), which a ser-less build has fed `None` the whole time. The operator's saved ECA table shows the fingerprint: all 25 Moments carry `arousal: null`. The mixed-audio gate PASSED 2026-06-18 (ADR 0008 Outcome, real 66-min VOD); this slice validates-and-ships that gate-passed Signal into the release binary. It does not re-open the gate, does not resurrect discovery-arousal (deferred in ADR 0008 pending vocal separation), and does not retune weights.

## Considered options

- **Wire `ser` and keep the hard-fail on a present-but-corrupt model.** `Ser::load(&paths.ser_model)?` fails the whole detect job. Rejected: by the time the arousal pass runs, discovery + the full whisper refine (GPU) are already paid for, and the codebase's own convention for a refine-Signal failing mid-run is warn-and-omit — the llm judge arm does exactly that (`Err(e) if cancel.is_cancelled() => return Err(e)`, else `warn!` + omit), and the pipeline comment already reads "exactly like a missing SER model". The Downloads heal flow can't even produce a corrupt install (streamed to `.part`, pinned SHA-256 verified, then staged into place), so the hard-fail only ever punishes disk rot or a hand-copied file with a dead detect job.
- **Fail-soft per-Moment (Option per candidate).** Rejected: `arousal::apply` z-scores across the candidate set; scoring a subset would z-score over survivors and quietly skew the Signal. Whole-batch warn-and-omit keeps the semantics binary: the Signal is either present for every candidate or absent for all (renormalize), the same contract a missing model already has.
- **Soften the missing-model path further / add UI surfacing.** Not needed: missing model is already structurally soft (`is_file()` guard), the Diagnostics ▸ Downloads row heals it, and the GUI shows the per-Moment arousal column (a dead Signal is visible as an empty column, same visibility the llm Signal gets).

## Decision

1. `scripts/build-release.bat` builds `--features face,align,ser`; its comment block documents why, in the file's existing per-feature convention.
2. The arousal pass is extracted to a fallible `arousal_refine(...)` helper; the call site matches the llm-judge arm: cancellation propagates, any other error (corrupt model, unreadable wav) logs `warn!` and omits the Signal, and `combined_score` renormalizes as designed. Unit-covered with a present-but-garbage `model.onnx`.
3. Scoring itself changes by zero words: `combined_score`, `Weights`, and the 0.15 default are untouched.

## Pre-registered bars (written and committed BEFORE the first A/B run)

A/B protocol: same VOD (the ECA podcast, `LeR59VmXiSc`, 4801 s, no chat replay — loudness-led discovery), same params (defaults, top_n 25), same cached `analysis.wav`, greedy/temp-0 llm judge (reproducible, so any llm delta is the `arousal_z` prompt line, i.e. the Signal's designed corroboration). A = today's ser-less release exe (archived aside), B = the `face,align,ser` build. The operator's saved `project.json` + `review.json` are byte-backed-up (SHA-256 recorded) and restored after; detect overwrites both.

The wire is sane iff:

1. **Coverage**: every refined Moment in the B table carries `Some(arousal)` — 25/25, no holes.
2. **Explainability**: every rank move A→B is explainable by the arousal column (directly through the 0.15 weight, or second-order through the judge reading `arousal_z`) — the known classes move the known way: emotionally activated reactions up, loud-but-flat down. A move the arousal column cannot explain (e.g. changed bounds, lexicon shifts) is investigated before the wire ships, not explained away.
3. **A-side integrity**: the A table carries `arousal: null` on every row (proves the archived exe is the ser-less binary, not a stale build).
4. **Fail-soft**: the corrupt-model unit test passes — `arousal_refine` returns `Err` on garbage bytes and the call site's warn+skip keeps detect alive.
5. **Cost of record**: per-window CPU wall and the refine-batch wall re-measured today, recorded beside ADR 0008's numbers (~0.35–0.46 s / 4 s window, ~163 s / 25-candidate batch on 2026-06-18 hardware).
6. **Restore**: the operator's `project.json` + `review.json` back in place, SHA-256 byte-identical to the backup.

Boot check after the wire: release GUI opens, Diagnostics SER row green, a detect surfaces the "Refining moments (arousal, CPU)" stage.

The re-ranked detect is a behavior change, so the before/after top-N table with per-Signal columns plus the measured cost line goes to the operator's eye (ADR 0050/0057 lesson); their eye rules the final gate.

## Outcome (A/B on the real VOD, 2026-07-12)

**All six pre-registered bars PASS — `ser` is wired into `build-release.bat`.** A = the morning's ser-less release exe (archived aside before the rebuild), B = the wired script's own build; both ran headless `--detect` on the ECA podcast (`LeR59VmXiSc`, 80 min, chatless → loudness-led discovery) over the same cached `analysis.wav`.

1. **Coverage**: 25/25 B Moments carry `Some(arousal)`; all 25 ranges join A↔B exactly (whisper bounds reproduced across the two builds — no bound drift to hide behind).
2. **Explainability**: every move is the arousal column's. The known classes moved the known way — the loud-but-flat cold-open (0:12–0:45, loud 2.64, **arousal −1.85**) fell #7→#21; the music-led "Botak Botak Mania" moment (loud 2.86, arousal −0.24) fell #2→#6; the loud singing-request at 31:01 (loud 2.28, arousal −1.05) fell #10→#19. High-activation conversation rose: #12→#1 (arousal **2.47**), #23→#3 (arousal 0.99 **plus** the judge flipping its verdict up out of the −2 band), #17→#9 (1.14). The second-order lane is confirmed causal, not noise: the judge is greedy/temp-0 and the transcripts are range-identical, so its verdict flips and a handful of title rewrites can only come from the `arousal_z` prompt line it now receives — the corroboration ADR 0010 designed, fed `None` until today.
3. **A-side integrity**: every A row prints `arou -` (so does the operator's saved table — the defect's fingerprint).
4. **Fail-soft**: `arousal_refine_errs_on_a_corrupt_model_instead_of_failing_detect` green in the `face,align,ser` app suite (garbage `model.onnx` → contexted `Err` → the warn+omit arm; cancel still propagates). Suites all green: workspace default 388 tests, `yc-detect --features ser` 52, app `face,align,ser` 31.
5. **Cost of record (2026-07-12, same laptop, GPU/CPU otherwise idle)**: **0.213 s per 4-s window** net of load+decode (release `ort`, 121-vs-31-window probe subtraction) and **164.7 s for the 25-candidate refine batch** (stage-log timestamps 05:44:14.7→05:46:59.4) — beside ADR 0008's ~0.35–0.46 s and ~163 s from 2026-06-18. Faster per window, same batch shape (longer candidates on this VOD).
6. **Restore**: the operator's saved `project.json` + `review.json` are back byte-identical (SHA-256 `DA7B6640…5FDA8B` / `CDBF3B17…81BAF1`); the byte-backups stay at `data/_backup_arousal_ab/` (deletable at will).

Boot check: the wired release GUI opens and closes cleanly, the Diagnostics SER row's green condition (model file present, 661 MB) holds on disk, and the B run surfaced **"Refining moments (arousal, CPU)"** on this exact binary.

The re-ranked detect is a behavior change, so the table below is staged for the operator's eye (ADR 0050/0057 — the eye rules; code shipped because every pre-registered bar passed, and the eye can still reverse the wire with a one-line revert of `build-release.bat`).

```text
rank moves B (ser) vs A (ser-less), ECA podcast LeR59VmXiSc - +N = rose N ranks
  B#   A#  move        range    dur   score    loud     lex    AROU     llm  title (B)
   1   12   +11  19:03-20:10    67s    1.11    1.85   -0.37    2.47    0.29  Diskusi tentang komentar di kolom
   2    3    +1  15:53-16:43    50s    1.01    2.30    1.01    0.47    0.29  Faddy ngomongin komentar fans
   3   23   +20    5:27-6:26    59s    0.97    1.97    0.67    0.99    0.29  Diskusi tentang hadiah dan zodiak
   4    8    +4  11:52-13:07    75s    0.95    2.10   -0.37    1.39    0.29  Fadi bercakap tentang Nino
   5    6    +1  44:03-45:24    81s    0.92    1.69    1.64    0.49    0.29  Diskusi tentang kamera dan vibe
   6    2    -4 1:19:01-1:19:35   34s    0.91    2.86    0.27   -0.24    0.29  Membahas Lagu Baru Botak Botak Mania
   7    9    +2  52:09-53:27    78s    0.88    1.86    0.11    1.08    0.29  Diskusi tentang Om Deddy dan onde-onde
   8    1    -7  38:01-38:49    48s    0.79    1.48    3.04   -0.80    0.29  Bapak 42 Tahu Gak Umurnya?
   9   17    +8    7:36-9:06    90s    0.79    1.75   -0.41    1.14    0.29  Mengembalikan Jaket yang Dipinjam
  10    5    -5    3:01-4:21    80s    0.79    1.91    1.28   -0.21    0.29  Diskusi tentang zodiak Gemini
  11   14    +3  21:16-22:31    75s    0.67    1.64    0.02    0.45    0.29  Debating on Twitter, 2009
  12   13    +1  39:25-40:12    47s    0.63    1.47    0.37    0.25    0.29  Echa Minta Lagu Baru
  13    4    -9  29:10-30:07    57s    0.59    1.49    2.19   -1.16    0.29  Om Deddy menganggap tantangan sulit
  14   15    +1  32:16-33:46    90s    0.59    1.58    0.05    0.13    0.29  Muka Gue Nuduh Mabuk
  15   11    -4  42:33-43:47    74s    0.52    1.99   -0.46   -0.38    0.29  Pertanyaan Unik dari Orang Tua
  16   19    +3  48:12-49:02    50s    0.51    1.57   -0.55    0.16    0.29  Membicarakan Proses Membuat Lagu
  17   18    +1  33:41-35:02    81s    0.50    1.86   -0.90    0.00    0.29  Kepala kayak biji wijen?
  18   22    +4  10:09-11:29    80s    0.43    1.58   -0.90    0.04    0.29  Framing dan Psikologi
  19   10    -9  31:01-32:16    75s    0.40    2.28   -0.90   -1.05    0.29  Bercanda tentang menjadi korban
  20   16    -4  36:44-37:40    56s    0.40    1.55    0.00   -0.67    0.29  Gemuk Banget, Sekarang Lebih Baik
  21    7   -14    0:12-0:45    33s    0.33    2.64   -0.90   -1.85    0.29  Diskusi tentang generasi Z
  22   21    -1  45:22-46:47    86s    0.32    1.59   -0.90   -0.50    0.29  Daddy's gossip style revealed
  23   20    -3  51:12-51:59    47s    0.31    1.55   -0.54   -0.73    0.29  Momen Pertama di YouTube Ngobrol Lama
  24   24     = 1:17:29-1:18:18   49s   -0.45    1.75    0.70   -0.90   -2.39  Tanya Lagu Baru Lin Buat Gamilla
  25   25     =  54:20-55:21    62s   -1.42    1.52   -0.90   -1.10   -4.17  Bicara tentang sabun multi tujuan
(chat column omitted: chatless VOD, all `-`. A-side per-Signal values live in the
session handoff's A-table; every A row has arou `-`.)
```

One measured caveat for the eye: this podcast's candidate set is *uniformly* speech-activated (the probe shows even the singing moment peaking ~0.99 absolute), so arousal's z-scores here express "activated relative to an already-hot set" — the demotions (cold-open, music-led loudness) are exactly the ADR 0008 classes, but the spread is gentler than a gaming VOD's explosions-vs-reactions would be.
