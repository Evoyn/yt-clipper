//! Validation harness for the dynamic-camera filtergraph (focus 2026-07):
//! builds the EXACT graph `build_camera_filtergraph` produces for a 3-shot
//! plan (solo -> split -> solo) and prints it, so a shell wrapper can run the
//! pinned ffmpeg with `-filter_complex_script` over a synthetic source and
//! prove the graph parses + renders end-to-end.
//!
//!   cargo run -p yc-render --example camera_fg > camera.fg

use yc_core::{CameraPlan, Crop, Layout, Shot};

fn main() {
    let solo = |x: f32| Layout::FullFrame { crop: Crop { x, y: 0.0, w: 405.0, h: 720.0 } };
    let split = Layout::Stacked {
        seam: 0.5,
        gameplay: Crop { x: 40.0, y: 100.0, w: 560.0, h: 498.0 },
        facecam: Crop { x: 680.0, y: 100.0, w: 560.0, h: 498.0 },
    };
    let plan = CameraPlan {
        shots: vec![
            Shot { start_s: 0.0, end_s: 4.0, track: Some(0), layout: solo(80.0), pan_to: None },
            Shot { start_s: 4.0, end_s: 8.0, track: None, layout: split, pan_to: None },
            Shot { start_s: 8.0, end_s: 12.0, track: Some(1), layout: solo(800.0), pan_to: None },
        ],
    };
    print!("{}", yc_render::build_camera_filtergraph(&plan, "clip.ass", false));
}
