# The Studio: a full-window preview editor between Promote and Render

Promote used to open a docked nudge panel; the focus (2026-07, tasks 1/2/4)
asks for a production editor — "a simplified CapCut for Shorts" — plus a full
transcript editor and a modernized shell. This ADR records the page's shape
and the three decisions with teeth: caption edits as the render's verbatim
truth, pre-pass transcription, and presets as pure style data.

## The page

Promote now opens the **Studio**, taking the whole window below the brand bar:
toolbar (Back · title · Preview/Original toggle · captions/safe-area toggles ·
Export), transcript editor left, video preview center, properties right
(Camera / Framing / Caption style), timeline bottom (ruler + caption blocks +
per-speaker lanes + cut markers + playhead scrub). The library (import +
Moments + detail) is the other page; Back returns to it with nothing lost.

- **Preview = Before/After.** "Original" shows the full source frame with the
  crop tools — a draggable, corner-resizable, scroll-zoomable 9:16 box (arrow
  keys nudge, Shift for big steps, 0 resets), face overlays with Person A/B/C
  labels, rule-of-thirds guides, everything outside the crop dimmed.
  "Preview" shows the composited 9:16 output: caption overlay (ADR 0036's
  shared line model, now honouring the style's colors/outline/box), the
  Active-Speaker tracking chip, and a toggleable **safe-area guide** (the
  top/bottom/right zones Shorts/TikTok UI covers). Dragging the frame in an
  AI camera mode flips to Manual — touching the framing takes control.
- **Transcript editor (the caption truth).** Every caption unit is a row:
  `m:ss.cc` timestamp (drag or type), editable text, split / merge / censor
  (`d***`) / delete, plus add-at-playhead. Edits regroup the preview lines
  immediately. On render, an edited transcript ships as
  `transcript_override`: the render burns it **verbatim** — no whisper, no
  harvest, no silence-drop, no re-timing. The operator's words are not
  guesses to second-guess; the automated timing machinery (ADRs 0013/0019/
  0021) exists to clean *whisper's* output, not theirs. An engine flip never
  invalidates an operator transcript. Harvest/curation stays the path for
  *fixing whisper durably* (ADR 0031); the transcript editor is the path for
  *this clip, right now*.
- **Pre-pass transcription.** Prepare now auto-queues `Job::Transcribe` (the
  render path's exact transcription, cached on the PreparedClip), so captions
  are editable before any render and the eventual render is NVENC-only. Same
  total GPU cost, moved earlier. The editor stays interactive while the worker
  runs — only playback and job-starting buttons lock (the wgpu-vs-whisper
  repaint throttle still governs).
- **Caption presets are data.** Classic / TikTok / Podcast / Minimal / Gaming
  / MrBeast are nothing but `CaptionStyle` values (ADR 0004 extended with
  outline width/color, shadow, back-box, bold — serde-defaulted to the old
  hardcoded ASS values, so pre-editor styles render byte-identical). Picking a
  preset replaces the style; every field stays editable after; any tweak
  deselects the chip. No preset registry, no theme engine.
- **Export summary.** Export opens a modal — length, resolution, caption
  line-count + style, camera mode + cut count, speaker tracking, estimated
  render time (cached-transcript aware) — then Render or Back (focus's
  explicit ask).

## Rejected

- **A modal editor over the library** (the old docked panel, bigger): the
  focus sketch is a workspace, not a popup; framing + captions + timeline
  don't fit beside a Moments rail at usable sizes.
- **Manual caption re-timing sliders** (per-word start/end drag on the
  timeline): the transcript rows already expose timestamps for the rare fix;
  a full timing UI would fight the automated refine for unclear gain
  (ADR 0036 made the same call — kept, but loosened: timestamps *are* now
  editable as numbers, since operator transcripts bypass the refine anyway).
- **egui-phosphor icons**: a new dependency for glyphs; the bundled emoji
  font + text labels cover the toolbar today.

## Consequences

- `Job::Render` carries the full `CaptionStyle`, an optional `CameraPlan`,
  and an optional `transcript_override`; headless/batch pass the per-genre
  default style and neither option, byte-compatible with before.
- The Creator store still remembers only the *genre* (ADR 0016); remembering
  full custom styles per Creator is a later slice.
- The old right-docked nudge editor is gone; ADR 0012's split (Prepare vs
  Render) and ADR 0036's shared caption geometry carry over unchanged.
