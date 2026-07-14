//! Face re-identification for the occupant-map spike (ADR 0043 gate pending):
//! "remember the face" so a seat's OCCUPANT is known per camera angle. A seat
//! track is a screen position that different humans hold across angles (ADR
//! 0042 measured it); embedding each track's face per angle segment and
//! clustering across the clip yields the map (angle, seat) -> person — the
//! de-circularizing anchor the voice lane's angle-scoped join is missing.
//!
//! Split mirrors [`crate::voice`]: everything measurable without a model —
//! the 5-point similarity alignment (Umeyama via complex least squares), the
//! bilinear warp/resize, and the YuNet raw-head decode — is pure and
//! unit-tested here; only [`FaceIdentifier`] (the YuNet + SFace ONNX
//! sessions) sits behind the `face` cargo feature. Nothing in the production
//! analysis path calls this module: the `speaker_diag` harness drives it, and
//! integration waits on the operator's gate (the ADR 0029 lesson, again).
//!
//! The models are the OpenCV zoo pair designed to work together (both
//! reference conventions lifted from OpenCV's `FaceDetectorYN` /
//! `FaceRecognizerSF` sources, the way the CAM++ fbank frontend was lifted
//! from kaldi-native-fbank):
//! - **YuNet** (`face_detection_yunet_2023mar.onnx`, MIT, 233 KB, fixed
//!   640x640 input, raw 0..255 **BGR**): anchor-free grids at strides
//!   8/16/32; per cell `cx=(c+dx)*s, cy=(r+dy)*s, w=e^w'*s, h=e^h'*s`,
//!   5 keypoints `(k+c)*s`, score `sqrt(cls*obj)`. Detection alone comes
//!   from Ultraface ([`crate::infer`]) — YuNet is here for the **landmarks**
//!   the alignment needs.
//! - **SFace** (`face_recognition_sface_2021dec.onnx`, Apache-2.0, 37 MB,
//!   112x112 input, raw 0..255 **RGB**, 128-d L2-normalized output): expects
//!   the ArcFace 5-point template warp; OpenCV's own same-identity cosine
//!   floor is 0.363 — a calibration point, not a tuned constant (the
//!   harness sweeps and prints the actual same/different gap).

use crate::FaceBox;
#[cfg(feature = "face")]
use anyhow::Result;

/// SFace alignment target: side of the aligned crop...
pub const ALIGN_SIZE: usize = 112;
/// ...and the ArcFace 5-point destination template within it (left eye,
/// right eye, nose tip, left mouth corner, right mouth corner) — OpenCV
/// `FaceRecognizerSF::alignCrop`'s exact numbers.
pub const TEMPLATE_112: [[f32; 2]; 5] = [
    [38.2946, 51.6963],
    [73.5318, 51.5014],
    [56.0252, 71.7366],
    [41.5493, 92.3655],
    [70.7299, 92.2041],
];
/// YuNet 2023mar fixed input side (the export's static shape — the ONNX
/// declares 640x640; feeding 320 is an ort shape error).
pub const YUNET_SIZE: usize = 640;
const YUNET_STRIDES: [usize; 3] = [8, 16, 32];
/// Keep YuNet detections at or above this score (sqrt(cls*obj)). Lenient
/// beside OpenCV's 0.9 default because the harness detects inside a region
/// already vouched for by an Ultraface track — the score only breaks ties.
pub const YUNET_MIN_SCORE: f32 = 0.6;
const YUNET_NMS_IOU: f32 = 0.3;

/// One YuNet detection inside the probed region: box + the 5 landmarks the
/// SFace alignment consumes, all in region pixel coordinates.
#[derive(Debug, Clone, Copy)]
pub struct FaceDet {
    pub bbox: FaceBox,
    /// left eye, right eye, nose, left mouth, right mouth.
    pub kps: [[f32; 2]; 5],
}

/// Least-squares similarity transform (rotation + uniform scale +
/// translation, never a reflection) mapping `src` onto `dst`, as the 2x3
/// row-major matrix `[a, -b, tx; b, a, ty]`. Solved in closed form over
/// complex numbers: minimize sum |a*s_i + t - d_i|^2 — the same optimum
/// OpenCV's SVD Procrustes reaches for proper rotations, in a dozen lines.
pub fn similarity_to(src: &[[f32; 2]; 5], dst: &[[f32; 2]; 5]) -> [f32; 6] {
    let n = src.len() as f32;
    let (mut sx, mut sy, mut dx, mut dy) = (0f32, 0f32, 0f32, 0f32);
    for i in 0..src.len() {
        sx += src[i][0];
        sy += src[i][1];
        dx += dst[i][0];
        dy += dst[i][1];
    }
    let (sx, sy, dx, dy) = (sx / n, sy / n, dx / n, dy / n);
    // a = sum(d_c * conj(s_c)) / sum(|s_c|^2) over centered points.
    let (mut num_re, mut num_im, mut den) = (0f32, 0f32, 0f32);
    for i in 0..src.len() {
        let (cr, ci) = (src[i][0] - sx, src[i][1] - sy);
        let (er, ei) = (dst[i][0] - dx, dst[i][1] - dy);
        num_re += er * cr + ei * ci;
        num_im += ei * cr - er * ci;
        den += cr * cr + ci * ci;
    }
    let den = den.max(1e-12);
    let (a, b) = (num_re / den, num_im / den);
    // t = d_mean - a * s_mean (complex product).
    let tx = dx - (a * sx - b * sy);
    let ty = dy - (b * sx + a * sy);
    [a, -b, tx, b, a, ty]
}

/// Invert a similarity `[a, -b, tx; b, a, ty]` analytically.
pub fn invert_similarity(m: &[f32; 6]) -> [f32; 6] {
    let (a, b, tx, ty) = (m[0], m[3], m[2], m[5]);
    let d = (a * a + b * b).max(1e-12);
    let (ia, ib) = (a / d, -b / d);
    let itx = -(ia * tx - ib * ty);
    let ity = -(ib * tx + ia * ty);
    [ia, -ib, itx, ib, ia, ity]
}

/// Bilinear sample of an rgb24 buffer at fractional coordinates, edges
/// clamped.
fn sample_bilinear(src: &[u8], sw: usize, sh: usize, x: f32, y: f32, out: &mut [f32; 3]) {
    let x = x.clamp(0.0, (sw - 1) as f32);
    let y = y.clamp(0.0, (sh - 1) as f32);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(sw - 1), (y0 + 1).min(sh - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    for c in 0..3 {
        let p00 = src[(y0 * sw + x0) * 3 + c] as f32;
        let p10 = src[(y0 * sw + x1) * 3 + c] as f32;
        let p01 = src[(y1 * sw + x0) * 3 + c] as f32;
        let p11 = src[(y1 * sw + x1) * 3 + c] as f32;
        out[c] = p00 * (1.0 - fx) * (1.0 - fy)
            + p10 * fx * (1.0 - fy)
            + p01 * (1.0 - fx) * fy
            + p11 * fx * fy;
    }
}

/// Warp an rgb24 buffer through the **destination -> source** similarity
/// `m_inv`, writing a `ow x oh` rgb24 crop (bilinear, edge-clamped) — the
/// `warpAffine(WARP_INVERSE_MAP)` of the alignment path.
pub fn warp_rgb(
    src: &[u8],
    sw: usize,
    sh: usize,
    m_inv: &[f32; 6],
    out: &mut [u8],
    ow: usize,
    oh: usize,
) {
    debug_assert!(src.len() >= sw * sh * 3 && out.len() >= ow * oh * 3);
    let mut px = [0f32; 3];
    for y in 0..oh {
        for x in 0..ow {
            let (fx, fy) = (x as f32, y as f32);
            let sx = m_inv[0] * fx + m_inv[1] * fy + m_inv[2];
            let sy = m_inv[3] * fx + m_inv[4] * fy + m_inv[5];
            sample_bilinear(src, sw, sh, sx, sy, &mut px);
            for c in 0..3 {
                out[(y * ow + x) * 3 + c] = px[c].round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

/// Bilinear rgb24 resize (the YuNet letterbox path; nearest — fine for mouth
/// motion — visibly degrades a 112 px recognition crop).
pub fn resize_rgb(src: &[u8], sw: usize, sh: usize, dst: &mut [u8], dw: usize, dh: usize) {
    debug_assert!(src.len() >= sw * sh * 3 && dst.len() >= dw * dh * 3);
    let mut px = [0f32; 3];
    for y in 0..dh {
        for x in 0..dw {
            let sx = (x as f32 + 0.5) * sw as f32 / dw as f32 - 0.5;
            let sy = (y as f32 + 0.5) * sh as f32 / dh as f32 - 0.5;
            sample_bilinear(src, sw, sh, sx, sy, &mut px);
            for c in 0..3 {
                dst[(y * dw + x) * 3 + c] = px[c].round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

/// Decode one YuNet stride head over a `in_w x in_h` input: anchor-free —
/// each grid cell emits score `sqrt(cls*obj)`, box center offset in cells,
/// log-scaled size, and 5 keypoint offsets in cells.
pub fn decode_yunet_stride(
    cls: &[f32],
    obj: &[f32],
    bbox: &[f32],
    kps: &[f32],
    stride: usize,
    in_w: usize,
    in_h: usize,
    min_score: f32,
    out: &mut Vec<(f32, FaceDet)>,
) {
    let (cols, rows) = (in_w / stride, in_h / stride);
    let cells = cols * rows;
    if cls.len() < cells || obj.len() < cells || bbox.len() < cells * 4 || kps.len() < cells * 10 {
        return; // malformed head — leave this stride out rather than panic
    }
    for r in 0..rows {
        for c in 0..cols {
            let i = r * cols + c;
            let score = (cls[i].clamp(0.0, 1.0) * obj[i].clamp(0.0, 1.0)).sqrt();
            if score < min_score {
                continue;
            }
            let s = stride as f32;
            let cx = (c as f32 + bbox[i * 4]) * s;
            let cy = (r as f32 + bbox[i * 4 + 1]) * s;
            let w = bbox[i * 4 + 2].exp() * s;
            let h = bbox[i * 4 + 3].exp() * s;
            let mut det = FaceDet {
                bbox: FaceBox { x: cx - w * 0.5, y: cy - h * 0.5, w, h, score },
                kps: [[0.0; 2]; 5],
            };
            for k in 0..5 {
                det.kps[k] = [
                    (kps[i * 10 + 2 * k] + c as f32) * s,
                    (kps[i * 10 + 2 * k + 1] + r as f32) * s,
                ];
            }
            out.push((score, det));
        }
    }
}

/// Greedy NMS over scored detections (keypoints ride along with their box).
pub fn nms_dets(mut dets: Vec<(f32, FaceDet)>, iou_thr: f32) -> Vec<FaceDet> {
    dets.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut keep: Vec<FaceDet> = Vec::new();
    for (_, d) in dets {
        if keep.iter().all(|k| crate::iou(&k.bbox, &d.bbox) <= iou_thr) {
            keep.push(d);
        }
    }
    keep
}

/// Landmarks synthesized from a bare face box by assuming the box IS the
/// aligned 112 frame scaled — the no-landmark fallback pipeline the selftest
/// A/Bs against real YuNet landmarks (rotation and box-tightness errors pass
/// straight through to the embedding; measure before trusting).
pub fn box_pseudo_landmarks(b: &FaceBox) -> [[f32; 2]; 5] {
    let mut kps = [[0.0f32; 2]; 5];
    for (k, t) in TEMPLATE_112.iter().enumerate() {
        kps[k] = [
            b.x + t[0] / ALIGN_SIZE as f32 * b.w,
            b.y + t[1] / ALIGN_SIZE as f32 * b.h,
        ];
    }
    kps
}

/// Mean of unit embeddings, renormalized — one embedding per (segment, seat)
/// from several sampled crops (a blink or a motion-blurred sample averages
/// out instead of minting its own identity).
pub fn aggregate_unit(embs: &[Vec<f32>]) -> Option<Vec<f32>> {
    let first = embs.first()?;
    let mut mean = vec![0f32; first.len()];
    for e in embs {
        for (m, v) in mean.iter_mut().zip(e.iter()) {
            *m += v;
        }
    }
    let norm = mean.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-12);
    for m in mean.iter_mut() {
        *m /= norm;
    }
    Some(mean)
}

/// Resident YuNet + SFace sessions (`face` feature): full-res rgb24 region in,
/// landmark-aligned 128-d unit embeddings out. Mirrors
/// [`crate::infer::Detector`]'s loading shape.
#[cfg(feature = "face")]
pub struct FaceIdentifier {
    yunet: ort::session::Session,
    yunet_input: String,
    sface: ort::session::Session,
    sface_input: String,
    sface_output: String,
}

#[cfg(feature = "face")]
impl FaceIdentifier {
    /// Load both ONNX models on the CPU execution provider.
    pub fn load(yunet: &std::path::Path, sface: &std::path::Path) -> Result<Self> {
        use anyhow::Context;
        let oerr = |e: ort::Error| anyhow::anyhow!("{e}");
        let load = |p: &std::path::Path| -> Result<ort::session::Session> {
            ort::session::Session::builder()
                .and_then(|mut b| b.commit_from_file(p))
                .map_err(oerr)
                .with_context(|| format!("loading face model {}", p.display()))
        };
        let yunet_s = load(yunet)?;
        let yunet_input = yunet_s
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .ok_or_else(|| anyhow::anyhow!("yunet has no inputs"))?;
        let sface_s = load(sface)?;
        let sface_input = sface_s
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .ok_or_else(|| anyhow::anyhow!("sface has no inputs"))?;
        let sface_output = sface_s
            .outputs()
            .first()
            .map(|o| o.name().to_string())
            .ok_or_else(|| anyhow::anyhow!("sface has no outputs"))?;
        tracing::info!(yunet = %yunet_input, sface = %sface_input, "face id models loaded");
        Ok(Self { yunet: yunet_s, yunet_input, sface: sface_s, sface_input, sface_output })
    }

    /// Detect faces (with landmarks) in an rgb24 region: letterboxed to the
    /// fixed 640x640 input (scale preserved, bottom/right zero pad — the
    /// reference pads to the stride divisor the same way), raw 0..255 **BGR**
    /// planes, results mapped back to region pixels.
    pub fn detect(&mut self, rgb: &[u8], w: usize, h: usize) -> Result<Vec<FaceDet>> {
        let oerr = |e: ort::Error| anyhow::anyhow!("{e}");
        anyhow::ensure!(w > 0 && h > 0 && rgb.len() >= w * h * 3, "bad region");
        let scale = (YUNET_SIZE as f32 / w as f32).min(YUNET_SIZE as f32 / h as f32).min(1.0);
        let (rw, rh) = (
            ((w as f32 * scale).round() as usize).clamp(1, YUNET_SIZE),
            ((h as f32 * scale).round() as usize).clamp(1, YUNET_SIZE),
        );
        let mut resized = vec![0u8; rw * rh * 3];
        resize_rgb(rgb, w, h, &mut resized, rw, rh);
        // NCHW BGR planes over the zero-padded square.
        let mut input = vec![0f32; 3 * YUNET_SIZE * YUNET_SIZE];
        for y in 0..rh {
            for x in 0..rw {
                let p = (y * rw + x) * 3;
                for c in 0..3 {
                    // plane 0 = B, 1 = G, 2 = R  <-  rgb24
                    input[(2 - c) * YUNET_SIZE * YUNET_SIZE + y * YUNET_SIZE + x] =
                        resized[p + c] as f32;
                }
            }
        }
        let tensor = ort::value::Tensor::from_array((
            [1_i64, 3, YUNET_SIZE as i64, YUNET_SIZE as i64],
            input,
        ))
        .map_err(oerr)?;
        let outputs =
            self.yunet.run(ort::inputs![self.yunet_input.as_str() => tensor]).map_err(oerr)?;
        let mut scored: Vec<(f32, FaceDet)> = Vec::new();
        for stride in YUNET_STRIDES {
            let get = |name: String| -> Result<Vec<f32>> {
                let (_, v) = outputs[name.as_str()].try_extract_tensor::<f32>().map_err(oerr)?;
                Ok(v.to_vec())
            };
            let cls = get(format!("cls_{stride}"))?;
            let obj = get(format!("obj_{stride}"))?;
            let bbox = get(format!("bbox_{stride}"))?;
            let kps = get(format!("kps_{stride}"))?;
            decode_yunet_stride(
                &cls,
                &obj,
                &bbox,
                &kps,
                stride,
                YUNET_SIZE,
                YUNET_SIZE,
                YUNET_MIN_SCORE,
                &mut scored,
            );
        }
        let mut dets = nms_dets(scored, YUNET_NMS_IOU);
        // Letterbox back to region pixels.
        for d in dets.iter_mut() {
            d.bbox.x /= scale;
            d.bbox.y /= scale;
            d.bbox.w /= scale;
            d.bbox.h /= scale;
            for k in d.kps.iter_mut() {
                k[0] /= scale;
                k[1] /= scale;
            }
        }
        Ok(dets)
    }

    /// Embed one face from an rgb24 region given its 5 landmarks: similarity
    /// warp to the 112 template, raw 0..255 **RGB** planes (the reference
    /// feeds BGR with swapRB=true — same thing), L2-normalized 128-d out.
    pub fn embed(&mut self, rgb: &[u8], w: usize, h: usize, kps: &[[f32; 2]; 5]) -> Result<Vec<f32>> {
        let m = similarity_to(kps, &TEMPLATE_112);
        let m_inv = invert_similarity(&m);
        let mut aligned = vec![0u8; ALIGN_SIZE * ALIGN_SIZE * 3];
        warp_rgb(rgb, w, h, &m_inv, &mut aligned, ALIGN_SIZE, ALIGN_SIZE);
        self.embed_aligned(&aligned)
    }

    /// Embed an already-aligned 112x112 rgb24 crop (the contact sheet keeps
    /// these — one warp, two consumers).
    pub fn embed_aligned(&mut self, aligned: &[u8]) -> Result<Vec<f32>> {
        let oerr = |e: ort::Error| anyhow::anyhow!("{e}");
        anyhow::ensure!(aligned.len() >= ALIGN_SIZE * ALIGN_SIZE * 3, "bad aligned crop");
        let mut input = vec![0f32; 3 * ALIGN_SIZE * ALIGN_SIZE];
        for i in 0..ALIGN_SIZE * ALIGN_SIZE {
            for c in 0..3 {
                input[c * ALIGN_SIZE * ALIGN_SIZE + i] = aligned[i * 3 + c] as f32;
            }
        }
        let tensor = ort::value::Tensor::from_array((
            [1_i64, 3, ALIGN_SIZE as i64, ALIGN_SIZE as i64],
            input,
        ))
        .map_err(oerr)?;
        let outputs =
            self.sface.run(ort::inputs![self.sface_input.as_str() => tensor]).map_err(oerr)?;
        let (_, emb) =
            outputs[self.sface_output.as_str()].try_extract_tensor::<f32>().map_err(oerr)?;
        let norm = emb.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-12);
        Ok(emb.iter().map(|v| v / norm).collect())
    }

    /// Align (returning the 112 crop) then embed — the harness wants both.
    pub fn align_and_embed(
        &mut self,
        rgb: &[u8],
        w: usize,
        h: usize,
        kps: &[[f32; 2]; 5],
    ) -> Result<(Vec<u8>, Vec<f32>)> {
        let m = similarity_to(kps, &TEMPLATE_112);
        let m_inv = invert_similarity(&m);
        let mut aligned = vec![0u8; ALIGN_SIZE * ALIGN_SIZE * 3];
        warp_rgb(rgb, w, h, &m_inv, &mut aligned, ALIGN_SIZE, ALIGN_SIZE);
        let emb = self.embed_aligned(&aligned)?;
        Ok((aligned, emb))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(m: &[f32; 6], p: [f32; 2]) -> [f32; 2] {
        [m[0] * p[0] + m[1] * p[1] + m[2], m[3] * p[0] + m[4] * p[1] + m[5]]
    }

    #[test]
    fn similarity_recovers_a_known_transform() {
        // dst = rotate 30 deg, scale 1.7, translate (11, -4) of src.
        let (th, s) = (30f32.to_radians(), 1.7f32);
        let (a, b) = (s * th.cos(), s * th.sin());
        let truth = [a, -b, 11.0, b, a, -4.0];
        let src = TEMPLATE_112;
        let mut dst = [[0f32; 2]; 5];
        for i in 0..5 {
            dst[i] = apply(&truth, src[i]);
        }
        let m = similarity_to(&src, &dst);
        for (got, want) in m.iter().zip(truth.iter()) {
            assert!((got - want).abs() < 1e-3, "{m:?} vs {truth:?}");
        }
        // And the inverse takes dst back to src.
        let inv = invert_similarity(&m);
        for i in 0..5 {
            let back = apply(&inv, dst[i]);
            assert!((back[0] - src[i][0]).abs() < 1e-2 && (back[1] - src[i][1]).abs() < 1e-2);
        }
    }

    #[test]
    fn similarity_identity_is_identity() {
        let m = similarity_to(&TEMPLATE_112, &TEMPLATE_112);
        let want = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        for (got, w) in m.iter().zip(want.iter()) {
            assert!((got - w).abs() < 1e-4, "{m:?}");
        }
    }

    #[test]
    fn warp_identity_copies_pixels() {
        let (w, h) = (8usize, 6usize);
        let src: Vec<u8> = (0..w * h * 3).map(|i| (i * 7 % 251) as u8).collect();
        let mut out = vec![0u8; w * h * 3];
        warp_rgb(&src, w, h, &[1.0, 0.0, 0.0, 0.0, 1.0, 0.0], &mut out, w, h);
        assert_eq!(src, out);
    }

    #[test]
    // Row 0 spelled out (`0 * w`) to match the row-math shape of its
    // neighbours — the (row * w + col) layout is the point.
    #[allow(clippy::erasing_op)]
    fn warp_translation_shifts() {
        // dest->src map x+2: dest (0,0) shows src (2,0).
        let (w, h) = (6usize, 4usize);
        let mut src = vec![0u8; w * h * 3];
        src[(0 * w + 2) * 3] = 200; // red pixel at (2,0)
        let mut out = vec![0u8; w * h * 3];
        warp_rgb(&src, w, h, &[1.0, 0.0, 2.0, 0.0, 1.0, 0.0], &mut out, w, h);
        assert_eq!(out[0], 200, "translated sample");
    }

    #[test]
    fn resize_preserves_flat_and_interpolates() {
        let (w, h) = (4usize, 4usize);
        let src = vec![100u8; w * h * 3];
        let mut dst = vec![0u8; 8 * 8 * 3];
        resize_rgb(&src, w, h, &mut dst, 8, 8);
        assert!(dst.iter().all(|&v| v == 100), "flat image stays flat");
        // A left-black right-white image resized down keeps the gradient order.
        let mut grad = vec![0u8; 8 * 1 * 3];
        for x in 4..8 {
            for c in 0..3 {
                grad[(x) * 3 + c] = 255;
            }
        }
        let mut small = vec![0u8; 4 * 1 * 3];
        resize_rgb(&grad, 8, 1, &mut small, 4, 1);
        assert!(small[0] < small[9], "gradient order kept: {small:?}");
    }

    #[test]
    fn yunet_decode_places_a_cell() {
        // One hot cell at (r=2, c=3), stride 8: box centered (3+0.5, 2+0.25)
        // cells, size e^0 * 8 px; first keypoint at cell + (0.1, 0.2).
        let (in_w, in_h, s) = (64usize, 64usize, 8usize);
        let cells = (in_w / s) * (in_h / s);
        let (mut cls, mut obj) = (vec![0f32; cells], vec![0f32; cells]);
        let mut bbox = vec![0f32; cells * 4];
        let mut kps = vec![0f32; cells * 10];
        let i = 2 * (in_w / s) + 3;
        cls[i] = 0.81;
        obj[i] = 1.0;
        bbox[i * 4] = 0.5;
        bbox[i * 4 + 1] = 0.25;
        bbox[i * 4 + 2] = 0.0;
        bbox[i * 4 + 3] = 0.0;
        kps[i * 10] = 0.1;
        kps[i * 10 + 1] = 0.2;
        let mut out = Vec::new();
        decode_yunet_stride(&cls, &obj, &bbox, &kps, s, in_w, in_h, 0.5, &mut out);
        assert_eq!(out.len(), 1);
        let (score, d) = out[0];
        assert!((score - 0.9).abs() < 1e-4);
        assert!((d.bbox.cx() - 28.0).abs() < 1e-3, "cx {}", d.bbox.cx());
        assert!((d.bbox.cy() - 18.0).abs() < 1e-3);
        assert!((d.bbox.w - 8.0).abs() < 1e-3 && (d.bbox.h - 8.0).abs() < 1e-3);
        assert!((d.kps[0][0] - 24.8).abs() < 1e-3 && (d.kps[0][1] - 17.6).abs() < 1e-3);
    }

    #[test]
    fn nms_keeps_the_best_of_an_overlap() {
        let d = |x: f32, score: f32| FaceDet {
            bbox: FaceBox { x, y: 0.0, w: 10.0, h: 10.0, score },
            kps: [[0.0; 2]; 5],
        };
        let kept = nms_dets(vec![(0.7, d(0.0, 0.7)), (0.9, d(1.0, 0.9)), (0.8, d(30.0, 0.8))], 0.3);
        assert_eq!(kept.len(), 2);
        assert!((kept[0].bbox.x - 1.0).abs() < 1e-6, "winner first");
    }

    #[test]
    fn pseudo_landmarks_of_the_canonical_box_are_the_template() {
        let b = FaceBox { x: 0.0, y: 0.0, w: 112.0, h: 112.0, score: 1.0 };
        let kps = box_pseudo_landmarks(&b);
        for (k, t) in kps.iter().zip(TEMPLATE_112.iter()) {
            assert!((k[0] - t[0]).abs() < 1e-4 && (k[1] - t[1]).abs() < 1e-4);
        }
    }

    #[test]
    fn aggregate_renormalizes() {
        let e1 = vec![1.0, 0.0];
        let e2 = vec![0.0, 1.0];
        let m = aggregate_unit(&[e1, e2]).unwrap();
        let n = (m[0] * m[0] + m[1] * m[1]).sqrt();
        assert!((n - 1.0).abs() < 1e-5);
        assert!((m[0] - m[1]).abs() < 1e-5);
        assert!(aggregate_unit(&[]).is_none());
    }
}
