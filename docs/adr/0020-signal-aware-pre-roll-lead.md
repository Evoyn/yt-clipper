# Signal-aware pre-roll lead: loud peaks start earlier for the build-up

A Moment's window starts `lead_s` before its detected peak, because chat lags the
on-screen event (ADR 0007). `lead_s` was a fixed 5 s for every peak. The operator,
clipping real VODs, found that **loud-triggered** clips — a jumpscare, a loud
mediashare-donation — need to start *several* seconds before the loud peak so the
build-up / context is captured: ≥ 5 s, ideally ~8–10 s. A chat-driven peak (people
reacting in chat after the fact) does not need the same head start.

## Decision

Make the pre-roll lead **signal-aware**, scaling with the peak's loudness. In
`rank_moments`, where each kept peak's loudness and chat z-scores are already in
hand, compute the lead with a pure ramp `score::peak_lead_s`:

- base `lead_s` (5 s, the chat-lag floor) at loudness z `min_z` (the peak
  threshold) and below — a chat-driven peak (low loudness z) keeps it;
- up to `loud_lead_s` (10 s) at loudness z `loud_lead_full_z` (3.0) and above — a
  loud-driven peak gets the full pre-roll;
- linear between.

So the louder the peak, the earlier the window starts; a chat-only spike keeps the
base lead. Loudness is the discover-time proxy for "a loud event" (arousal, which
would sharpen it, is a refine-pass signal not yet computed when the range is set —
noted as a future, range-recomputing extension).

### NMS interaction

A *variable* lead can make candidate windows overlap, which the fixed-lead NMS gap
(`min_gap = dur_s / bin_s`) assumed away: a later **loud** peak (long lead) pulls
its start earlier, into an earlier **chat** peak's window. Concretely, two peaks
`dur_s` apart no longer abut once their leads differ. Fix: widen the gap by the
**lead spread**, `min_gap = (dur_s + (loud_lead_s − lead_s)) / bin_s`, so no two
windows overlap regardless of their individual leads. With a constant lead this
reduces to the old `dur_s` gap.

## Considered options

- **Signal-aware lead by loudness magnitude (chosen).** Computable at discover
  time from the per-peak loudness z already used for ranking; directly delivers
  "loud peaks get more lead than chat peaks," with a smooth ramp rather than a
  cliff.
- **A single larger fixed `lead_s`.** Rejected: it over-leads chat-driven peaks
  (whose 5 s is calibrated to the chat lag) and ignores the operator's explicit
  loud-vs-chat distinction.
- **Model-determined lead (the LLM judge picks it).** The operator said this is
  fine but ≥ 5 s is the floor. Deferred: the judge runs in refine, *after* the
  range is set at discover, so a model-chosen lead would require recomputing the
  range in refine — a larger change. The signal-aware lead delivers the intent now;
  a judge-tuned lead can layer on later (it would still honour the floor).
- **Threshold switch (loud z above X ⇒ 10 s, else 5 s).** Rejected in favour of the
  linear ramp — a hard switch makes the lead jump 5 s across a 0.01 z boundary.

## Consequences

- Two new `DetectParams` (`loud_lead_s` 10.0, `loud_lead_full_z` 3.0) and one pure
  `score::peak_lead_s`, all "scaffolding to retune" (ADR 0002/0007). `peak_to_range`
  is unchanged — it already takes the lead as a parameter.
- The widened `min_gap` spaces candidates a little further apart (35 s vs 30 s at
  defaults), so very tightly-packed peaks yield one candidate instead of two
  overlapping ones — acceptable and intended (no overlapping windows).
- A new `detect_diag` example runs the real `discover` over a VOD's analysis wav +
  chat (no GPU) and prints each Moment's window, z-scores, and applied lead.

## Outcome

**Shipped + verified (2026-06-28).** `peak_lead_s` ramp + the per-peak lead and
widened gap in `rank_moments`; 36 detect tests green (+2: the pure ramp; an
integration test on the real `rank_moments` proving a loud peak leads ~10 s, a chat
peak ~5 s, and the windows don't overlap).

**Live (`detect_diag` on a real 105-min Ino Gemink VOD — the real `discover`):** all
25 candidate Moments are loud-driven (loudness z 8–18, well past the z=3 saturation),
so **every clip now opens with the full 10 s pre-roll** (was a fixed 5 s) — exactly
the build-up the operator asked for on a loud gaming VOD. The chat z at these peaks
is low (−0.4 .. 3.8); a chat-dominated peak would fall to the 5 s base (the
integration test covers that branch — none reached this gaming VOD's top-25).
