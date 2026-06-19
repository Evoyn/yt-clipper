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
pub use ass::{generate_ass, refine_caption_timing};
pub use export::{build_filtergraph, export_args, run_export};
