# Multi-face Facecam: frame everyone on a co-stream cam (ADR 0011 extension)

M6 auto-framing (ADR 0011) detects the **single** most-persistent face
(`cluster_static_face`) and `expand_facecam` crops tightly around it. The operator
clips a 2-person co-stream ("Horror Tanpa Ekspresi bersama @guntur69") where the
cam shows **two** people — and the auto-frame zoomed into **one** of them, tight
enough that the upscaled facecam Panel looked soft. Their ask: "use facedetect for
stacked mode" — detect both and frame both.

## Decision

Treat the Facecam as holding **N faces**, not one.

- **`cluster_static_faces`** returns *every* cluster that persists across at least
  `MIN_PERSISTENCE` of the frames, most-persistent first, capped at
  `MAX_FACECAM_FACES` (3). `cluster_static_face` becomes a thin "first of" wrapper.
- The layout API takes a **`&[FaceCluster]`** slice. `decide_layout`:
  - **0 faces** → full-frame gameplay (unchanged);
  - **1 face**, large + centered → full-cam; else → stacked over it (unchanged);
  - **2+ faces** → **stacked, the facecam framing all of them** (a co-stream cam).
- **`facecam_crop`** builds the facecam Crop from the faces' union:
  - **solo (1 face):** the expanded face box is *fit* to the Panel aspect, so the
    streamer fills the Panel exactly as before — **no solo regression**;
  - **2+ faces:** the crop **grows** to a Panel-aspect window that *contains the
    whole union*, so neither person is cropped out. This wider crop is also why the
    quality improves — framing both is a larger source region than a tight zoom on
    one, so the Panel upscales far less.

## Considered options

- **Solo "fill" vs multi "grow" (chosen).** The two cases want opposite shaping: a
  solo face should fill the wide-short Panel (fit/shrink, the conventional facecam),
  but a wide 2-face union must be *contained* (grow) or one face is trimmed off. A
  single shaping rule can't do both, so branch on face count.
- **A blanket minimum facecam width (anti-over-zoom for all cams).** Tried and
  rejected: a live render showed it widened a *solo* corner cam so far it displayed
  the surrounding gameplay around a small cam box — a regression. The over-zoom the
  operator hit is specifically the *multi-face-framed-as-one* case, which the union
  already fixes; the solo cam was never the complaint.
- **Cap at 2 faces.** Rejected in favour of 3: a static wall poster / chat avatar
  can be as persistent as a person, so capping at 2 risks dropping a real streamer
  for a poster. Keeping 3 means the two real people are always in the union; a
  spurious third only widens it slightly (verified harmless).

## Consequences

- `yc-frame`: `cluster_static_faces` (+ wrapper), `facecam_crop` (replaces
  `expand_facecam`), `decide_layout` / `decide_layout_with_pref` / `stacked_layout`
  / `fullcam_layout` take `&[FaceBox]` / `&[FaceCluster]`. One new const
  `MAX_FACECAM_FACES`.
- `pipeline`: `detect_facecam` returns `Vec<FaceCluster>`; `build_layout` passes the
  slice; the auto-frame log reports `cams = N`.
- The forced-Stacked / forced-FullCam preferences (ADR 0017) compose unchanged —
  they now frame all detected faces too.
- A genuine 2-person *talking* session (full-screen cam, no game) still resolves to
  stacked-union here; the operator's Layout menu (Full cam) overrides if they want
  the whole scene. A face-count-aware full-cam-for-two is a possible later refinement.

## Outcome

**Shipped + verified (2026-06-28).** 18 `yc-frame` tests green (+3:
`cluster_static_faces` returns both persistent faces; a 2-face `decide_layout`
frames both in the union; solo `facecam_crop` still fills the Panel over the face).
App tests green; builds with `face`.

**Live (the operator's own VOD `BUDS9qx2jw0`, a downloaded 30 s 2-person segment,
rendered stacked):** detection found the faces (`cams=3` — the two streamers + a
static poster), and the facecam Panel framed **both** people side-by-side (the
union), instead of zooming into one. The solo Ino segment re-rendered unchanged (no
regression). Test renders + harvest reverted.
