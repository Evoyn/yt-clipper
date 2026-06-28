//! Auto-detect framing (M6, ADR 0011): find the streamer's **Facecam** in a
//! promoted Segment's frames and choose the Clip's [`Layout`].
//!
//! The Facecam is not fixed across a VOD — a small corner inset during
//! gameplay, a full-screen cam during a talking session — so it is detected
//! **per Segment** from a handful of sampled frames. The webcam face is the one
//! that stays in ~the same screen position across frames (low spatial variance,
//! high persistence); game characters' faces move or vanish. From the detected
//! Facecam's size and position we pick one of three Layouts:
//!
//! - small / off-center, persistent  -> `Stacked` (gameplay + facecam Panels)
//! - large / roughly-centered (talking) -> `FullFrame` on the face
//! - no persistent face              -> `FullFrame` gameplay
//!
//! This module is the **pure** half: the clustering, the three-way decision,
//! and the box->[`Crop`] expansion, all in source-pixel coordinates and unit
//! tested with no `ort` dependency. The Ultraface inference that produces the
//! per-frame [`FaceBox`]es lives in [`infer`], behind the `face` cargo feature
//! (mirrors `yc-detect`'s arousal split).

use yc_core::{Crop, Layout, LayoutPref, CANVAS_H, CANVAS_W};

#[cfg(feature = "face")]
pub mod infer;
#[cfg(feature = "face")]
pub use infer::Detector;

// ---- tunable constants (ADR 0011: seeded defaults, retune without re-arch) ----

/// Frames per second sampled from the Segment for detection.
pub const SAMPLE_FPS: f64 = 1.5;
/// Hard cap on sampled frames (bounds Ultraface work on a long Segment).
pub const MAX_FRAMES: usize = 40;
/// Minimum Ultraface confidence to keep a detection.
pub const MIN_CONF: f32 = 0.7;
/// IoU above which two same-frame detections are treated as duplicates (NMS).
pub const NMS_IOU: f32 = 0.3;
/// Cluster merge radius as a fraction of the frame diagonal: detections whose
/// centers fall within this of a cluster's running mean join it.
pub const MERGE_DIST_FRAC: f32 = 0.05;
/// A static Facecam must appear in at least this fraction of sampled frames.
pub const MIN_PERSISTENCE: f32 = 0.5;
/// Face wider than this fraction of the frame, and horizontally centered, reads
/// as a talking-session full-cam rather than a gameplay corner-cam.
pub const FULLCAM_FACE_W_FRAC: f32 = 0.28;
/// Horizontal center band [lo, hi] (fraction of width) for "roughly centered".
pub const CENTER_LO: f32 = 0.25;
pub const CENTER_HI: f32 = 0.75;
/// Default Seam position for stacked Layouts (fraction of canvas height).
pub const SEAM_DEFAULT: f32 = 0.62;
/// Facecam-box expansion to capture head + shoulders + overlay border.
pub const FACE_EXPAND_W: f32 = 1.8;
pub const FACE_EXPAND_H: f32 = 2.4;
/// Fraction of the expanded facecam height that sits above the face center
/// (headroom); the rest is below (shoulders).
pub const FACE_UPPER_FRAC: f32 = 0.38;

/// A detected face bounding box in **source-video pixel** coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FaceBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub score: f32,
}

impl FaceBox {
    pub fn cx(&self) -> f32 {
        self.x + self.w * 0.5
    }
    pub fn cy(&self) -> f32 {
        self.y + self.h * 0.5
    }
    fn area(&self) -> f32 {
        self.w.max(0.0) * self.h.max(0.0)
    }
}

/// Intersection-over-union of two boxes.
pub fn iou(a: &FaceBox, b: &FaceBox) -> f32 {
    let x1 = a.x.max(b.x);
    let y1 = a.y.max(b.y);
    let x2 = (a.x + a.w).min(b.x + b.w);
    let y2 = (a.y + a.h).min(b.y + b.h);
    let inter = (x2 - x1).max(0.0) * (y2 - y1).max(0.0);
    let union = a.area() + b.area() - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Greedy non-max suppression: keep the highest-score boxes, drop any that
/// overlap a kept box by more than `iou_thresh`. Used per frame to collapse the
/// raw Ultraface detections of one face into a single box.
pub fn nms(mut boxes: Vec<FaceBox>, iou_thresh: f32) -> Vec<FaceBox> {
    boxes.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    let mut keep: Vec<FaceBox> = Vec::new();
    for b in boxes {
        if keep.iter().all(|k| iou(k, &b) <= iou_thresh) {
            keep.push(b);
        }
    }
    keep
}

/// The detected static Facecam: a representative box plus how persistent it was.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FaceCluster {
    /// Median member box, source pixels.
    pub bbox: FaceBox,
    /// Fraction of sampled frames the cluster appeared in.
    pub persistence: f32,
}

/// One accumulating cluster of face detections at ~the same screen position.
struct Cluster {
    members: Vec<FaceBox>,
    /// Distinct frame indices this cluster appeared in (kept de-duplicated;
    /// frames are processed in increasing order so a `last()` check suffices).
    frames: Vec<usize>,
    sum_cx: f32,
    sum_cy: f32,
}

impl Cluster {
    fn mean_cx(&self) -> f32 {
        self.sum_cx / self.members.len() as f32
    }
    fn mean_cy(&self) -> f32 {
        self.sum_cy / self.members.len() as f32
    }
}

fn median_box(boxes: &[FaceBox]) -> FaceBox {
    let med = |mut v: Vec<f32>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v[v.len() / 2]
    };
    FaceBox {
        x: med(boxes.iter().map(|b| b.x).collect()),
        y: med(boxes.iter().map(|b| b.y).collect()),
        w: med(boxes.iter().map(|b| b.w).collect()),
        h: med(boxes.iter().map(|b| b.h).collect()),
        score: med(boxes.iter().map(|b| b.score).collect()),
    }
}

/// Cluster per-frame face detections by screen position and return the **static
/// Facecam** — the cluster present in the most frames — if it persists across at
/// least [`MIN_PERSISTENCE`] of them. `frames[i]` is frame i's detections (after
/// per-frame [`nms`]), in source pixels. Returns `None` when no face is
/// persistent enough (e.g. a faceless game, or only transient game characters).
pub fn cluster_static_face(
    frames: &[Vec<FaceBox>],
    frame_w: f32,
    frame_h: f32,
) -> Option<FaceCluster> {
    let total = frames.len();
    if total == 0 {
        return None;
    }
    let merge_dist = MERGE_DIST_FRAC * (frame_w * frame_w + frame_h * frame_h).sqrt();

    let mut clusters: Vec<Cluster> = Vec::new();
    for (fi, faces) in frames.iter().enumerate() {
        for f in faces {
            let mut best: Option<usize> = None;
            let mut best_d = f32::INFINITY;
            for (ci, c) in clusters.iter().enumerate() {
                let dx = f.cx() - c.mean_cx();
                let dy = f.cy() - c.mean_cy();
                let d = (dx * dx + dy * dy).sqrt();
                if d < best_d {
                    best_d = d;
                    best = Some(ci);
                }
            }
            match best {
                Some(ci) if best_d <= merge_dist => {
                    let c = &mut clusters[ci];
                    c.members.push(*f);
                    c.sum_cx += f.cx();
                    c.sum_cy += f.cy();
                    if c.frames.last() != Some(&fi) {
                        c.frames.push(fi);
                    }
                }
                _ => clusters.push(Cluster {
                    members: vec![*f],
                    frames: vec![fi],
                    sum_cx: f.cx(),
                    sum_cy: f.cy(),
                }),
            }
        }
    }

    // The static webcam is the most-persistent cluster (tie-break on member
    // count). Moving game faces fragment into many low-persistence clusters.
    let best = clusters.into_iter().max_by(|a, b| {
        a.frames.len().cmp(&b.frames.len()).then_with(|| a.members.len().cmp(&b.members.len()))
    })?;
    let persistence = best.frames.len() as f32 / total as f32;
    if persistence < MIN_PERSISTENCE {
        return None;
    }
    Some(FaceCluster { bbox: median_box(&best.members), persistence })
}

/// Expand a face box to the webcam-overlay region (head + shoulders + border),
/// clamped to the frame: centered horizontally on the face, biased downward so
/// the face sits in the upper portion with headroom above and shoulders below.
fn expand_facecam(face: &FaceBox, src_w: f32, src_h: f32) -> Crop {
    let w = (face.w * FACE_EXPAND_W).min(src_w);
    let h = (face.h * FACE_EXPAND_H).min(src_h);
    let x = (face.cx() - w * 0.5).clamp(0.0, src_w - w);
    let y = (face.cy() - h * FACE_UPPER_FRAC).clamp(0.0, src_h - h);
    Crop { x, y, w, h }
}

/// The talking-session crop: a canvas-aspect (9:16) window the full height of
/// the source, centered horizontally on the face (clamped to the frame). If the
/// source is too narrow for a full-height 9:16 column, fall back to a
/// full-width, face-centered band instead.
fn fullcam_crop(face: &FaceBox, src_w: f32, src_h: f32) -> Crop {
    let aspect = CANVAS_W as f32 / CANVAS_H as f32;
    let w = src_h * aspect;
    if w <= src_w {
        let x = (face.cx() - w * 0.5).clamp(0.0, src_w - w);
        Crop { x, y: 0.0, w, h: src_h }
    } else {
        let h = src_w / aspect;
        let y = (face.cy() - h * 0.5).clamp(0.0, src_h - h);
        Crop { x: 0.0, y, w: src_w, h }
    }
}

/// Choose the Clip's [`Layout`] from the detected static Facecam (ADR 0011's
/// three-way decision). `None` (no persistent face) -> full-frame gameplay.
/// `seam` is the stacked Seam position (use [`SEAM_DEFAULT`]).
pub fn decide_layout(face: Option<&FaceCluster>, src_w: f32, src_h: f32, seam: f32) -> Layout {
    let Some(face) = face else {
        return Layout::FullFrame { crop: fullframe_gameplay_crop(src_w, src_h) };
    };

    let w_frac = face.bbox.w / src_w;
    let cx_frac = face.bbox.cx() / src_w;
    let centered = (CENTER_LO..=CENTER_HI).contains(&cx_frac);

    if w_frac >= FULLCAM_FACE_W_FRAC && centered {
        // Talking session: the streamer is the content.
        fullcam_layout(Some(&face.bbox), src_w, src_h)
    } else {
        // Gameplay with a corner cam: gameplay Panel above the detected facecam.
        stacked_layout(Some(&face.bbox), src_w, src_h, seam)
    }
}

/// Apply the operator's explicit [`LayoutPref`] (ADR 0017), overriding the M6
/// auto-detect. `Auto` defers to [`decide_layout`] (the unchanged ADR 0011
/// decision); a forced kind builds that Layout from the detected Facecam when
/// `face` is `Some`, else from a sensible seed — so forced Stacked / FullCam
/// still work with no `face` feature and no detected cam. Pure and unit-tested.
pub fn decide_layout_with_pref(
    pref: LayoutPref,
    face: Option<&FaceCluster>,
    src_w: f32,
    src_h: f32,
    seam: f32,
) -> Layout {
    let bbox = face.map(|f| &f.bbox);
    match pref {
        LayoutPref::Auto => decide_layout(face, src_w, src_h, seam),
        LayoutPref::Stacked => stacked_layout(bbox, src_w, src_h, seam),
        LayoutPref::FullCam => fullcam_layout(bbox, src_w, src_h),
        LayoutPref::FullGameplay => {
            Layout::FullFrame { crop: fullframe_gameplay_crop(src_w, src_h) }
        }
    }
}

/// A stacked Layout at `seam`: gameplay Panel above facecam Panel. The facecam
/// Crop covers the detected Facecam when `face` is `Some`, else the bottom-right
/// [`default_facecam_crop`] seed (forced Stacked with no detected cam). Shared by
/// the Auto corner-cam branch and the forced-Stacked preference.
fn stacked_layout(face: Option<&FaceBox>, src_w: f32, src_h: f32, seam: f32) -> Layout {
    let full = Crop { x: 0.0, y: 0.0, w: src_w, h: src_h };
    let gh = (CANVAS_H as f32 * seam).round();
    let fh = CANVAS_H as f32 - gh;
    let cam_aspect = CANVAS_W as f32 / fh;
    let gameplay = full.fit_to_aspect(CANVAS_W as f32 / gh);
    let facecam = match face {
        Some(f) => expand_facecam(f, src_w, src_h).fit_to_aspect(cam_aspect),
        None => default_facecam_crop(src_w, src_h, cam_aspect),
    };
    Layout::Stacked { seam, gameplay, facecam }
}

/// A full-frame talking-cam Layout: `FullFrame` centered on the detected face
/// when `face` is `Some`, else a centered 9:16 column. Shared by the Auto
/// talking-session branch and the forced-FullCam preference.
fn fullcam_layout(face: Option<&FaceBox>, src_w: f32, src_h: f32) -> Layout {
    let crop = match face {
        Some(f) => fullcam_crop(f, src_w, src_h),
        None => centered_fullcam_crop(src_w, src_h),
    };
    Layout::FullFrame { crop }
}

// ---- editor geometry (M6 nudge editor, ADR 0012) ---------------------------
//
// Pure source-pixel operations the egui editor drives: pan/zoom a Crop within
// the source frame, re-fit a Crop when the Seam moves (panel aspect changes),
// and seed Crops when the operator overrides the auto-chosen Layout type. All
// aspect-preserving and clamped to the frame, so the editor never produces a
// Crop the render filtergraph would reject.

/// Smallest Crop edge, in source pixels: zooming in stops here so a Panel never
/// samples a sub-pixel region.
pub const MIN_CROP_EDGE: f32 = 16.0;
/// Default Facecam seed width as a fraction of source width, when overriding to
/// Stacked with no detected face (a typical corner-cam footprint).
pub const DEFAULT_FACECAM_W_FRAC: f32 = 0.30;

/// Translate `crop` by `(dx, dy)` source pixels, clamped so it stays inside the
/// frame. Size (and therefore aspect) is unchanged — this is a pure pan.
pub fn pan_crop(crop: Crop, src_w: f32, src_h: f32, dx: f32, dy: f32) -> Crop {
    Crop {
        x: (crop.x + dx).clamp(0.0, (src_w - crop.w).max(0.0)),
        y: (crop.y + dy).clamp(0.0, (src_h - crop.h).max(0.0)),
        w: crop.w,
        h: crop.h,
    }
}

/// Scale `crop` by `factor` (>1 zooms out / shows more, <1 zooms in) about the
/// source-pixel anchor `(ax, ay)` — the point under the cursor stays put.
/// Aspect is preserved (both edges scale together); `factor` is capped so the
/// result fits the frame and no edge falls below [`MIN_CROP_EDGE`].
pub fn zoom_crop(crop: Crop, src_w: f32, src_h: f32, factor: f32, ax: f32, ay: f32) -> Crop {
    // Cap zoom-out at whichever edge reaches the frame first (preserves aspect).
    let max_factor = (src_w / crop.w).min(src_h / crop.h);
    // Cap zoom-in so the smaller edge does not drop below MIN_CROP_EDGE.
    let min_factor = (MIN_CROP_EDGE / crop.w).max(MIN_CROP_EDGE / crop.h);
    let f = factor.clamp(min_factor, max_factor.max(min_factor));
    let new_w = crop.w * f;
    let new_h = crop.h * f;
    // Keep the anchor's fractional position within the crop fixed.
    let rel_x = if crop.w > 0.0 { (ax - crop.x) / crop.w } else { 0.5 };
    let rel_y = if crop.h > 0.0 { (ay - crop.y) / crop.h } else { 0.5 };
    Crop {
        x: (ax - rel_x * new_w).clamp(0.0, (src_w - new_w).max(0.0)),
        y: (ay - rel_y * new_h).clamp(0.0, (src_h - new_h).max(0.0)),
        w: new_w,
        h: new_h,
    }
}

/// Re-fit `crop` to a new Panel aspect, keeping its center and (where the frame
/// allows) its height — used when the Seam drag changes a stacked Panel's aspect
/// and its aspect-locked Crop must follow.
pub fn reaspect_keep_center(crop: Crop, new_aspect: f32, src_w: f32, src_h: f32) -> Crop {
    let cx = crop.x + crop.w * 0.5;
    let cy = crop.y + crop.h * 0.5;
    let mut h = crop.h;
    let mut w = h * new_aspect;
    if w > src_w {
        w = src_w;
        h = w / new_aspect;
    }
    if h > src_h {
        h = src_h;
        w = h * new_aspect;
    }
    Crop {
        x: (cx - w * 0.5).clamp(0.0, (src_w - w).max(0.0)),
        y: (cy - h * 0.5).clamp(0.0, (src_h - h).max(0.0)),
        w,
        h,
    }
}

/// The gameplay and facecam Panel aspect ratios (w/h) of a Stacked Layout at
/// `seam` (fraction of canvas height given to the gameplay Panel).
pub fn stacked_panel_aspects(seam: f32) -> (f32, f32) {
    let gh = (CANVAS_H as f32 * seam).round().clamp(2.0, (CANVAS_H - 2) as f32);
    let fh = CANVAS_H as f32 - gh;
    (CANVAS_W as f32 / gh, CANVAS_W as f32 / fh)
}

/// The whole source fit to the 9:16 canvas — the full-frame gameplay Crop and
/// the safe default when no Facecam is found.
pub fn fullframe_gameplay_crop(src_w: f32, src_h: f32) -> Crop {
    Crop { x: 0.0, y: 0.0, w: src_w, h: src_h }.fit_to_aspect(CANVAS_W as f32 / CANVAS_H as f32)
}

/// A centered full-height (or full-width) 9:16 column — the talking-cam
/// FullFrame Crop when overriding with no face to center on.
pub fn centered_fullcam_crop(src_w: f32, src_h: f32) -> Crop {
    let aspect = CANVAS_W as f32 / CANVAS_H as f32;
    let w = src_h * aspect;
    if w <= src_w {
        Crop { x: (src_w - w) * 0.5, y: 0.0, w, h: src_h }
    } else {
        let h = src_w / aspect;
        Crop { x: 0.0, y: (src_h - h) * 0.5, w: src_w, h }
    }
}

/// A default Facecam Crop for `panel_aspect`, parked bottom-right (where gaming
/// corner-cams usually sit) — the seed when overriding to Stacked with no
/// detected face. The operator then pans/zooms it onto the real cam.
pub fn default_facecam_crop(src_w: f32, src_h: f32, panel_aspect: f32) -> Crop {
    let mut w = src_w * DEFAULT_FACECAM_W_FRAC;
    let mut h = w / panel_aspect;
    if h > src_h {
        h = src_h;
        w = h * panel_aspect;
    }
    if w > src_w {
        w = src_w;
        h = w / panel_aspect;
    }
    Crop { x: src_w - w, y: src_h - h, w, h }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fb(x: f32, y: f32, w: f32, h: f32) -> FaceBox {
        FaceBox { x, y, w, h, score: 0.9 }
    }

    #[test]
    fn iou_of_identical_is_one_disjoint_is_zero() {
        let a = fb(0.0, 0.0, 100.0, 100.0);
        assert!((iou(&a, &a) - 1.0).abs() < 1e-5);
        let b = fb(500.0, 500.0, 100.0, 100.0);
        assert_eq!(iou(&a, &b), 0.0);
    }

    #[test]
    fn nms_keeps_higher_score_among_overlaps_and_both_when_disjoint() {
        let mut a = fb(0.0, 0.0, 100.0, 100.0);
        a.score = 0.8;
        let mut b = fb(10.0, 10.0, 100.0, 100.0); // heavily overlaps a
        b.score = 0.95;
        let c = fb(800.0, 800.0, 100.0, 100.0); // disjoint
        let kept = nms(vec![a, b, c], NMS_IOU);
        assert_eq!(kept.len(), 2);
        assert!((kept[0].score - 0.95).abs() < 1e-6); // b survives the overlap
        assert!(kept.iter().any(|k| (k.x - 800.0).abs() < 1e-6)); // c kept
    }

    /// A face fixed in the bottom-right corner across every frame, plus a face
    /// that jumps around frame to frame, yields the fixed one as the Facecam.
    #[test]
    fn cluster_finds_the_static_face_amid_a_moving_one() {
        let (w, h) = (1920.0_f32, 1080.0_f32);
        let mut frames = Vec::new();
        for i in 0..10 {
            let static_face = fb(1600.0, 820.0, 200.0, 200.0); // cx 1700, cy 920
            let moving = fb(100.0 + i as f32 * 150.0, 200.0, 120.0, 120.0); // jumps 150px/frame
            frames.push(vec![static_face, moving]);
        }
        let c = cluster_static_face(&frames, w, h).expect("a persistent face");
        assert!((c.persistence - 1.0).abs() < 1e-6);
        assert!((c.bbox.cx() - 1700.0).abs() < 1.0);
        assert!((c.bbox.cy() - 920.0).abs() < 1.0);
    }

    /// Only transient faces (each in a single frame, far apart) -> no Facecam.
    #[test]
    fn cluster_returns_none_when_nothing_persists() {
        let (w, h) = (1920.0_f32, 1080.0_f32);
        let mut frames = Vec::new();
        for i in 0..6 {
            frames.push(vec![fb(100.0 + i as f32 * 200.0, 100.0, 120.0, 120.0)]);
        }
        assert!(cluster_static_face(&frames, w, h).is_none());
    }

    #[test]
    fn no_face_gives_full_frame_gameplay() {
        let layout = decide_layout(None, 1920.0, 1080.0, SEAM_DEFAULT);
        match layout {
            Layout::FullFrame { crop } => {
                // Full 16:9 fit to 9:16 trims width to a centered column.
                let aspect = crop.w / crop.h;
                assert!((aspect - CANVAS_W as f32 / CANVAS_H as f32).abs() < 1e-3);
            }
            other => panic!("expected FullFrame gameplay, got {other:?}"),
        }
    }

    #[test]
    fn small_corner_face_gives_stacked_with_facecam_over_the_face() {
        let face = FaceCluster { bbox: fb(1600.0, 820.0, 200.0, 200.0), persistence: 1.0 };
        match decide_layout(Some(&face), 1920.0, 1080.0, SEAM_DEFAULT) {
            Layout::Stacked { seam, gameplay, facecam } => {
                assert!((seam - SEAM_DEFAULT).abs() < 1e-6);
                // gameplay fills the top Panel aspect from the full frame
                assert!(gameplay.w > 0.0 && gameplay.h > 0.0);
                // the facecam Crop covers the detected face center
                let (fcx, fcy) = (face.bbox.cx(), face.bbox.cy());
                assert!(facecam.x <= fcx && fcx <= facecam.x + facecam.w, "face cx in facecam");
                assert!(facecam.y <= fcy && fcy <= facecam.y + facecam.h, "face cy in facecam");
            }
            other => panic!("expected Stacked, got {other:?}"),
        }
    }

    #[test]
    fn large_centered_face_gives_full_cam_centered_on_the_face() {
        // Face 600 wide (31% of 1920) centered at cx 960 -> talking full-cam.
        let face = FaceCluster { bbox: fb(660.0, 200.0, 600.0, 600.0), persistence: 1.0 };
        match decide_layout(Some(&face), 1920.0, 1080.0, SEAM_DEFAULT) {
            Layout::FullFrame { crop } => {
                let aspect = crop.w / crop.h;
                assert!((aspect - CANVAS_W as f32 / CANVAS_H as f32).abs() < 1e-3, "9:16 column");
                // column horizontally centered on the face
                let col_center = crop.x + crop.w * 0.5;
                assert!((col_center - face.bbox.cx()).abs() < 1.0);
                assert!((crop.h - 1080.0).abs() < 1e-3, "full source height");
            }
            other => panic!("expected FullFrame full-cam, got {other:?}"),
        }
    }

    #[test]
    fn expand_facecam_contains_the_face_and_grows_it() {
        let face = fb(1600.0, 820.0, 200.0, 200.0);
        let c = expand_facecam(&face, 1920.0, 1080.0);
        assert!(c.w > face.w && c.h > face.h, "expanded");
        assert!(c.x <= face.x && c.x + c.w >= face.x + face.w, "contains face horizontally");
        assert!(c.y <= face.y && c.y + c.h >= face.y + face.h, "contains face vertically");
        // stays inside the frame
        assert!(c.x >= 0.0 && c.y >= 0.0 && c.x + c.w <= 1920.0 && c.y + c.h <= 1080.0);
    }

    // ---- explicit Layout preference (ADR 0017) ----

    #[test]
    fn pref_auto_matches_decide_layout_with_and_without_a_face() {
        // Auto must be byte-for-byte the ADR 0011 decision (no regression).
        let face = FaceCluster { bbox: fb(1600.0, 820.0, 200.0, 200.0), persistence: 1.0 };
        for f in [None, Some(&face)] {
            let auto = decide_layout_with_pref(LayoutPref::Auto, f, 1920.0, 1080.0, SEAM_DEFAULT);
            let base = decide_layout(f, 1920.0, 1080.0, SEAM_DEFAULT);
            assert_eq!(auto, base);
        }
    }

    #[test]
    fn pref_stacked_forces_stacked_even_for_a_large_centered_face() {
        // A big centered face Auto would frame as full-cam; the preference must
        // still produce Stacked, with the facecam Panel over the detected face.
        let face = FaceCluster { bbox: fb(660.0, 200.0, 600.0, 600.0), persistence: 1.0 };
        assert!(matches!(
            decide_layout(Some(&face), 1920.0, 1080.0, SEAM_DEFAULT),
            Layout::FullFrame { .. }
        ));
        match decide_layout_with_pref(LayoutPref::Stacked, Some(&face), 1920.0, 1080.0, SEAM_DEFAULT) {
            Layout::Stacked { seam, facecam, .. } => {
                assert!((seam - SEAM_DEFAULT).abs() < 1e-6);
                let (fcx, fcy) = (face.bbox.cx(), face.bbox.cy());
                assert!(facecam.x <= fcx && fcx <= facecam.x + facecam.w);
                assert!(facecam.y <= fcy && fcy <= facecam.y + facecam.h);
            }
            other => panic!("expected forced Stacked, got {other:?}"),
        }
    }

    #[test]
    fn pref_stacked_with_no_face_uses_the_bottom_right_seed() {
        // Forced Stacked must work with no detected cam (e.g. a non-`face` build):
        // a bottom-right default facecam Crop, matching the facecam Panel aspect.
        match decide_layout_with_pref(LayoutPref::Stacked, None, 1920.0, 1080.0, SEAM_DEFAULT) {
            Layout::Stacked { seam, facecam, .. } => {
                let (_, fa) = stacked_panel_aspects(seam);
                assert!((facecam.w / facecam.h - fa).abs() < 1e-2, "facecam matches panel aspect");
                assert!((facecam.x + facecam.w - 1920.0).abs() < 1e-3, "parked at right edge");
                assert!((facecam.y + facecam.h - 1080.0).abs() < 1e-3, "parked at bottom edge");
            }
            other => panic!("expected forced Stacked, got {other:?}"),
        }
    }

    #[test]
    fn pref_full_cam_and_full_gameplay_force_full_frame() {
        // Full cam with no face -> a centered 9:16 column.
        match decide_layout_with_pref(LayoutPref::FullCam, None, 1920.0, 1080.0, SEAM_DEFAULT) {
            Layout::FullFrame { crop } => {
                assert!((crop.w / crop.h - CANVAS_W as f32 / CANVAS_H as f32).abs() < 1e-3);
                assert!((crop.x + crop.w * 0.5 - 960.0).abs() < 1e-3, "centered");
            }
            other => panic!("expected FullFrame full-cam, got {other:?}"),
        }
        // Full gameplay is the whole frame fit to 9:16, regardless of any face.
        let face = FaceCluster { bbox: fb(1600.0, 820.0, 200.0, 200.0), persistence: 1.0 };
        assert_eq!(
            decide_layout_with_pref(LayoutPref::FullGameplay, Some(&face), 1920.0, 1080.0, SEAM_DEFAULT),
            Layout::FullFrame { crop: fullframe_gameplay_crop(1920.0, 1080.0) }
        );
    }

    // ---- editor geometry (ADR 0012) ----

    #[test]
    fn pan_translates_and_clamps_to_the_frame() {
        let c = Crop { x: 100.0, y: 100.0, w: 200.0, h: 200.0 };
        let moved = pan_crop(c, 1920.0, 1080.0, 50.0, -30.0);
        assert_eq!((moved.x, moved.y, moved.w, moved.h), (150.0, 70.0, 200.0, 200.0));
        // panning past the right/bottom edge clamps so the crop stays inside.
        let pinned = pan_crop(c, 1920.0, 1080.0, 1e6, 1e6);
        assert!((pinned.x - (1920.0 - 200.0)).abs() < 1e-3);
        assert!((pinned.y - (1080.0 - 200.0)).abs() < 1e-3);
    }

    #[test]
    fn zoom_preserves_aspect_holds_the_anchor_and_stays_in_bounds() {
        let c = Crop { x: 400.0, y: 300.0, w: 400.0, h: 300.0 }; // 4:3
        // zoom in around the crop's own center: aspect held, anchor fixed.
        let (ax, ay) = (600.0, 450.0);
        let z = zoom_crop(c, 1920.0, 1080.0, 0.5, ax, ay);
        assert!((z.w / z.h - 400.0 / 300.0).abs() < 1e-3, "aspect preserved");
        assert!((z.x + z.w * 0.5 - ax).abs() < 1e-3 && (z.y + z.h * 0.5 - ay).abs() < 1e-3);
        // zoom-out is capped so the crop can never exceed the frame.
        let out = zoom_crop(c, 1920.0, 1080.0, 100.0, ax, ay);
        assert!(out.w <= 1920.0 + 1e-3 && out.h <= 1080.0 + 1e-3);
        assert!(out.x >= -1e-3 && out.y >= -1e-3);
        assert!((out.w / out.h - 400.0 / 300.0).abs() < 1e-2, "aspect preserved at the cap");
    }

    #[test]
    fn reaspect_adopts_the_new_aspect_keeps_center_and_fits() {
        let c = Crop { x: 800.0, y: 400.0, w: 300.0, h: 300.0 };
        let (cx, cy) = (c.x + c.w * 0.5, c.y + c.h * 0.5);
        let r = reaspect_keep_center(c, 1080.0 / 730.0, 1920.0, 1080.0);
        assert!((r.w / r.h - 1080.0 / 730.0).abs() < 1e-3, "new aspect");
        assert!((r.x + r.w * 0.5 - cx).abs() < 1e-3 && (r.y + r.h * 0.5 - cy).abs() < 1e-3);
        assert!(r.x >= 0.0 && r.y >= 0.0 && r.x + r.w <= 1920.0 && r.y + r.h <= 1080.0);
    }

    #[test]
    fn seed_crops_match_their_panel_aspect_and_sit_inside_the_frame() {
        let (src_w, src_h) = (1920.0, 1080.0);
        let (ga, fa) = stacked_panel_aspects(SEAM_DEFAULT);
        let game = fullframe_gameplay_crop(src_w, src_h);
        assert!((game.w / game.h - CANVAS_W as f32 / CANVAS_H as f32).abs() < 1e-3);
        let cam = default_facecam_crop(src_w, src_h, fa);
        assert!((cam.w / cam.h - fa).abs() < 1e-2, "facecam matches its panel aspect");
        // parked bottom-right, inside the frame.
        assert!((cam.x + cam.w - src_w).abs() < 1e-3 && (cam.y + cam.h - src_h).abs() < 1e-3);
        let col = centered_fullcam_crop(src_w, src_h);
        assert!((col.w / col.h - CANVAS_W as f32 / CANVAS_H as f32).abs() < 1e-3, "9:16 column");
        assert!((col.x + col.w * 0.5 - src_w * 0.5).abs() < 1e-3, "centered");
        // gameplay seed is wider/taller than the facecam panel's aspect differs.
        assert!(ga > 0.0 && fa > 0.0);
    }
}
