//! Rendering: turns a Clip (Layout + Caption Style + transcript slice) into
//! an ffmpeg invocation — crop/scale/vstack filtergraph, generated ASS file
//! burned via libass, h264_nvenc encode (ADR 0004). The same filtergraph at
//! reduced resolution, piped to a texture, is the framing editor's true
//! preview (ADR 0005): preview and export cannot disagree.
//!
//! The ASS generator is the single home of caption animation logic; Caption
//! Style presets are data, not code paths.

pub mod ass;
pub mod export;
pub use ass::{
    generate_ass, preview_lines, refine_caption_timing, refine_caption_timing_keep_verified,
    refine_caption_timing_traced, resolve_placement, word_states, PreviewLine, PreviewWord,
    RefineTrace, UnitOutcome, WordState,
};
pub use export::{
    build_camera_filtergraph, build_filtergraph, export_args, export_args_script, run_export,
};
