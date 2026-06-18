//! Chat-replay parsing for the chat-rate signal (ADR 0002, ADR 0007).
//!
//! The import writes the raw `live_chat` replay as JSONL - one
//! `replayChatItemAction` per line, each carrying a `videoOffsetTimeMsec`
//! (the VOD-relative time the message appeared). We keep the offset of every
//! real *viewer* message (`liveChatTextMessageRenderer`); engagement notices,
//! pinned banners, and other system rows also carry offsets but are not
//! audience activity, so they are skipped. Pure over JSONL text, so the parser
//! is unit-tested without the 56 MB file.

use anyhow::{Context, Result};
use std::path::Path;

/// VOD-relative timestamps (seconds, ascending) of viewer chat messages.
pub fn message_offsets(chat_json: &Path) -> Result<Vec<f64>> {
    let text = std::fs::read_to_string(chat_json)
        .with_context(|| format!("reading {}", chat_json.display()))?;
    Ok(parse_offsets(&text))
}

/// `videoOffsetTimeMsec` is a string in the replay JSON ("12352"); tolerate a
/// bare number too, in case a future yt-dlp emits it unquoted.
fn offset_ms(v: &serde_json::Value) -> Option<f64> {
    v.as_str().and_then(|s| s.parse::<f64>().ok()).or_else(|| v.as_f64())
}

/// Keep the offset of each line that carries a viewer text message.
fn parse_offsets(jsonl: &str) -> Vec<f64> {
    let mut out = Vec::new();
    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let action = &v["replayChatItemAction"];
        let is_message = action["actions"].as_array().is_some_and(|acts| {
            acts.iter().any(|a| {
                !a["addChatItemAction"]["item"]["liveChatTextMessageRenderer"].is_null()
            })
        });
        if !is_message {
            continue;
        }
        if let Some(ms) = offset_ms(&action["videoOffsetTimeMsec"]) {
            out.push(ms / 1000.0);
        }
    }
    out.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // One viewer message, shape trimmed to the fields we read.
    fn msg(text: &str, off_ms: &str) -> String {
        format!(
            r#"{{"replayChatItemAction":{{"actions":[{{"addChatItemAction":{{"item":{{"liveChatTextMessageRenderer":{{"message":{{"runs":[{{"text":"{text}"}}]}}}}}}}}}}],"videoOffsetTimeMsec":"{off_ms}"}}}}"#
        )
    }

    #[test]
    fn keeps_viewer_messages_in_time_order() {
        let jsonl = format!("{}\n{}\n", msg("halo", "5000"), msg("gg", "2000"));
        assert_eq!(parse_offsets(&jsonl), vec![2.0, 5.0]);
    }

    #[test]
    fn skips_system_rows_without_a_text_renderer() {
        // Engagement notice (first replay line) + a banner row: both carry an
        // offset but neither is a viewer message.
        let engagement = r#"{"replayChatItemAction":{"actions":[{"addChatItemAction":{"item":{"liveChatViewerEngagementMessageRenderer":{"id":"x"}}}}],"videoOffsetTimeMsec":"0"}}"#;
        let banner = r#"{"replayChatItemAction":{"actions":[{"addBannerToLiveChatCommand":{"bannerRenderer":{}}}],"videoOffsetTimeMsec":"100"}}"#;
        let jsonl = format!("{engagement}\n{banner}\n{}\n", msg("real", "3500"));
        assert_eq!(parse_offsets(&jsonl), vec![3.5]);
    }

    #[test]
    fn tolerates_blank_and_unparseable_lines() {
        let jsonl = format!("\n  \nnot json\n{}\n", msg("ok", "1000"));
        assert_eq!(parse_offsets(&jsonl), vec![1.0]);
    }
}
