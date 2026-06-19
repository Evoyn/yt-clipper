//! Ultraface RFB-320 face detection via ONNX Runtime (ADR 0011), behind the
//! `face` cargo feature so default builds and the pure-geometry tests need
//! neither the `ort` binary nor the model.
//!
//! The RFB-320 ONNX bakes the prior/stride box decode into the graph, so its
//! outputs are already the two arrays we want: `scores` `[1, N, 2]` (column 1 is
//! the face probability) and `boxes` `[1, N, 4]` (normalized `[x1, y1, x2, y2]`).
//! Post-processing is just a confidence threshold + NMS — the reason this model
//! was chosen over YuNet, whose raw decode would have to be reimplemented here.
//!
//! Because the boxes are **normalized** [0, 1], the 320x240 detection size
//! cancels out: we scale straight to the source resolution, which also undoes
//! the aspect stretch of resizing a 16:9 frame to the model's 4:3 input (per-axis
//! linear scaling leaves normalized coordinates invariant).
//!
//! NOTE (as in `yc-detect`'s SER module): the `ort 2.0.0-rc.12` binding surface
//! below is written from the documented API and verified on the first
//! `--features face` build — expect minor signature fixups isolated to this file.

use crate::{nms, FaceBox, MIN_CONF, NMS_IOU};
use anyhow::{Context, Result};
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;

/// Ultraface RFB-320 fixed input size (W x H), 4:3.
pub const DET_W: usize = 320;
pub const DET_H: usize = 240;

/// `ort::Error` holds raw pointers (not `Send + Sync`), so `?` can't convert it
/// into `anyhow::Error`. Stringify it (same as the SER module).
fn oerr(e: ort::Error) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}

/// Resident Ultraface detector. Loaded once per detect run, then run over each
/// sampled frame.
pub struct Detector {
    session: Session,
    input_name: String,
    scores_name: String,
    boxes_name: String,
}

impl Detector {
    /// Load the Ultraface ONNX on the CPU execution provider (default).
    pub fn load(onnx: &Path) -> Result<Self> {
        let session = Session::builder()
            .and_then(|mut b| b.commit_from_file(onnx))
            .map_err(oerr)
            .with_context(|| format!("loading face model {}", onnx.display()))?;

        let input_name = session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .ok_or_else(|| anyhow::anyhow!("face model has no inputs"))?;

        // Ultraface exports `scores` then `boxes`. Prefer the names; else fall
        // back to output order.
        let outs = session.outputs();
        let by_name = |needle: &str| {
            outs.iter().find(|o| o.name().to_lowercase().contains(needle)).map(|o| o.name().to_string())
        };
        let scores_name = by_name("score")
            .or_else(|| outs.first().map(|o| o.name().to_string()))
            .ok_or_else(|| anyhow::anyhow!("face model has no outputs"))?;
        let boxes_name = by_name("box")
            .or_else(|| outs.get(1).map(|o| o.name().to_string()))
            .ok_or_else(|| anyhow::anyhow!("face model has no boxes output"))?;

        tracing::info!(
            input = %input_name,
            scores = %scores_name,
            boxes = %boxes_name,
            "face model loaded"
        );
        Ok(Self { session, input_name, scores_name, boxes_name })
    }

    /// Detect faces in one `DET_W` x `DET_H` rgb24 frame, returning boxes in
    /// **source pixels** (`src_w` x `src_h`), thresholded at [`MIN_CONF`] and
    /// NMS'd. The caller resizes the Segment frame to `DET_W` x `DET_H` (ffmpeg
    /// `scale=320:240`); the normalized model boxes are then scaled to source.
    pub fn detect(&mut self, rgb: &[u8], src_w: f32, src_h: f32) -> Result<Vec<FaceBox>> {
        anyhow::ensure!(
            rgb.len() == DET_W * DET_H * 3,
            "frame must be {DET_W}x{DET_H} rgb24 ({} bytes), got {}",
            DET_W * DET_H * 3,
            rgb.len()
        );

        // NCHW f32, (px - 127) / 128, RGB order — the Ultraface preprocessing.
        let mut chw = vec![0f32; 3 * DET_H * DET_W];
        let plane = DET_H * DET_W;
        for y in 0..DET_H {
            for x in 0..DET_W {
                let src = (y * DET_W + x) * 3;
                let dst = y * DET_W + x;
                for c in 0..3 {
                    chw[c * plane + dst] = (rgb[src + c] as f32 - 127.0) / 128.0;
                }
            }
        }

        let input =
            Tensor::from_array(([1_i64, 3, DET_H as i64, DET_W as i64], chw)).map_err(oerr)?;
        let outputs =
            self.session.run(ort::inputs![self.input_name.as_str() => input]).map_err(oerr)?;
        let (_s_shape, scores) =
            outputs[self.scores_name.as_str()].try_extract_tensor::<f32>().map_err(oerr)?;
        let (_b_shape, boxes) =
            outputs[self.boxes_name.as_str()].try_extract_tensor::<f32>().map_err(oerr)?;

        // scores: [N, 2] (col 1 = face prob); boxes: [N, 4] normalized LTRB.
        let n = scores.len() / 2;
        let mut faces = Vec::new();
        for i in 0..n {
            let p = scores[i * 2 + 1];
            if p < MIN_CONF {
                continue;
            }
            let (x1, y1, x2, y2) =
                (boxes[i * 4], boxes[i * 4 + 1], boxes[i * 4 + 2], boxes[i * 4 + 3]);
            faces.push(FaceBox {
                x: x1 * src_w,
                y: y1 * src_h,
                w: (x2 - x1) * src_w,
                h: (y2 - y1) * src_h,
                score: p,
            });
        }
        Ok(nms(faces, NMS_IOU))
    }
}
