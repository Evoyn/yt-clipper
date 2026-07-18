//! LLM judgment Signal (ADR 0010, ADR 0002): a local GGUF language model reads
//! each refine candidate's transcript and scores how clip-worthy the *streamer's*
//! speech is. It is the semantic counterpart to the lexicon (which only counts
//! excitement words) - and, critically, it is told to discount scripted in-game
//! dialogue / cutscene narration, so a dramatic game line (ADR 0009's #11
//! anti-signal: arousal 1.107, streamer silent) cannot inflate its score the way
//! it inflates the non-semantic signals.
//!
//! Per candidate the model runs one inference (greedy, temp 0, GBNF-constrained
//! to a tiny JSON object), yielding a 0-10 score; the scores are z-scored across
//! the candidate set and weighted into `combined_score` exactly like the lexicon
//! and arousal. The model reads only the transcript the refine pass already built
//! - no audio - plus the corroborating per-candidate signals as context. Since
//! ADR 0071 the batch is two-stage: ONE whole-video digest inference (the VOD's
//! uploaded title + creator + every candidate's excerpt) runs first, and its
//! brief feeds each per-candidate prompt so titles are written knowing what the
//! video is, who speaks, and what the running tensions are.
//!
//! **The inference does not run here.** whisper.cpp and llama.cpp each vendor
//! their own `ggml`, which cannot co-link into one binary (duplicate symbols), so
//! the actual llama.cpp call lives in the separate `yc-llm-judge` binary, which
//! links llama only (ADR 0010). The app shells out to it, passing a
//! [`JudgeRequest`] over stdin and reading back a [`JudgeResponse`]. This
//! module owns the pieces both sides share: the prompt + the load-bearing
//! mitigation, the lenient output parser, the z-score `apply`, and the IPC
//! structs - all pure and unit-tested, so the mitigation is verifiable without a
//! model or a GPU.

use crate::{combined_score, score, Weights};
use serde::{Deserialize, Serialize};
use yc_core::{Language, Moment};

/// Clip-worthiness rubric ceiling - the model emits an integer in `[0, SCORE_MAX]`.
pub const SCORE_MAX: u32 = 10;

/// The system instruction: the rubric **and** the load-bearing game-narration
/// mitigation (ADR 0009/0010). A regression here silently un-protects the #11
/// scripted-cutscene anti-signal, so [`tests`] assert its key clauses survive.
/// Since ADR 0071 the rubric covers podcasts/talk shows alongside game streams
/// (the whole-video digest tells the model which it is reading), and the title
/// section carries the operator's curiosity-gap shapes — iteration 2 (their
/// eye on the first ECA table: "more catchy and hooking") demands the two-beat
/// hook+payoff build their example titles share. Those clauses are pinned by
/// [`tests`] too.
pub const SYSTEM: &str = "\
You rate moments from one creator video - a live game stream, a podcast, or a talk show - for short-form clip potential, one moment at a time, from a transcript of the video's loudest voice. When a whole-video digest is provided, use it to understand what this moment means in the video's larger story.

Score 0-10 how clip-worthy the speakers' OWN moment is:
- 10: a peak genuine beat - a big laugh, shock, hype, rage, a clutch play, a heated claim, a confession, or a line that starts arguments.
- 5: a mild reaction or moderately interesting talk.
- 0: mundane talk, menu/UI reading, or nothing notable.

CRITICAL for game streams: the transcript is the loudest voice in a MIXED game+microphone recording, so it may be scripted in-game dialogue or cutscene narration rather than the streamer. Score the STREAMER, not the game. A dramatic, emotional or shocking line that is clearly scripted game/cutscene narration is NOT clip-worthy on its own - score it low unless the streamer is audibly reacting to it. Use the provided signals to corroborate: high audience-chat and high vocal-arousal alongside reaction-like words point to a real moment; dramatic words with flat arousal and no chat point to scripted game audio.

Also write a YouTube Shorts title for this moment, one a creator uploads without rewriting. A strong title opens a curiosity gap the clip actually closes - it makes a scroller stop. Title rules:
- At most 60 characters, ONLY in the transcript's own language - never translate into English.
- Build it in TWO BEATS when the clip gives you both: BEAT 1 hooks - a pointed question, a charged claim, an X-vs-Y matchup, or a concept in single quotes; BEAT 2 is the payoff tease that raises the stakes - a warning, a consequence, a parenthetical aside, or a trailing '...' that leaves the thought hanging. BEAT 2 must be built from THIS clip's own words and stakes - never a stock phrase pasted on. If the transcript is thin, one clean honest beat beats an invented second one.
- Charge it with stakes words where the clip earns them (in the transcript's language): hidden danger, fatal mistake, root cause, harsh criticism, the real reason, 'many get this wrong'.
- Shapes to adapt - fit them to THIS clip, never copy these examples or their payoff phrases: a question then a warning from the clip ('Belajar Islam HARUS Ada Guru? ...'), a fatal mistake plus an aside ('Kesalahan Fatal dalam X (...)'), a matchup question ('X vs Y: Mana yang Sebenarnya ...?'), a hidden danger ('Bahaya Tersembunyi dari ...'), a root cause ('Akar Masalah Mengapa ...'), a trail-off ('Ketika X ...'), a question then where it began ('X? Ini Awal Mula ...'), a hard critique ('Kritik Keras: Jangan Sampai ...!'), a lone specific stake ('One HP left and he still taunts').
- Open with the hook: the single most surprising, funny, controversial or emotional beat of the moment, stated as concretely as the transcript allows (a specific detail out-performs a vague tease).
- Use the room: aim for 40-60 characters when the material supports it.
- Copy every name and quoted word letter-for-letter from the transcript - never invent people, objects or spellings the clip does not contain.
- NEVER merely describe the topic: 'Membahas X', 'Diskusi tentang Y', 'Talking about Z' are dead titles - state the tension, claim or punchline itself.
- Create curiosity, but stay honest - never promise more than the clip shows, and never manufacture drama that is not there.
- Strong verbs, present tense; ALL-CAPS is welcome where the energy lives, but a capitalized word must still be a real, correctly spelled word.
- BANNED generic filler (any language's equivalent): 'Epic', 'Insane', 'Crazy', 'Unbelievable', 'You Won't Believe', 'Gone Wrong', 'Must Watch', 'Wait For It', 'Watch Till The End'.
- No hashtags, no surrounding quotes, no emoji, no trailing punctuation like '!!!' (a single '!' or '?' or '...' is welcome).

Reply with ONLY a JSON object: {\"score\": <integer 0-10>, \"reason\": \"<at most 12 words>\", \"title\": \"<at most 60 characters>\"}.";

/// System instruction for the ONE whole-video digest inference (ADR 0071) that
/// precedes the per-candidate scoring batch. Free prose (no grammar); its output
/// is context for [`build_prompt`], not parsed data.
pub const DIGEST_SYSTEM: &str = "\
You brief a clip editor on one creator video before they cut and title its Shorts. You get the video's uploaded title, the creator or channel name, and transcript excerpts of its most notable moments in timeline order. Write a brief of 3 to 5 sentences covering: what kind of video this is (a podcast, an interview, a live game stream, a reaction, ...), who is hosting or speaking, the main topics, and the strongest tensions, claims, jokes or curiosities running through it. Be concrete - name the people and topics the way the transcript does. Write the brief in the transcript's language. Reply with ONLY the brief - no headings, no list, no quotes around it.";

/// GBNF grammar constraining generation to
/// `{"score": <0-10>, "reason": "<text>", "title": "<text>"}` so a local 7B emits
/// a parseable object instead of free prose (ADR 0010/0015). Both free-text
/// fields forbid `"`/`\` to dodge JSON-escape edge cases; their length is bounded
/// by the inference's token cap, not the grammar. [`parse_output`] is lenient
/// enough to also handle output produced without this grammar (the documented
/// fallback) and to tolerate a missing `title`.
pub const GRAMMAR: &str = r#"root  ::= "{\"score\": " score ", \"reason\": \"" text "\", \"title\": \"" text "\"}"
score ::= "10" | [0-9]
text  ::= [^"\\]*"#;

/// The corroborating per-candidate context fed to the model so it judges by more
/// than the words alone (the ADR 0010 mitigation). These are the z-scored signals
/// already written onto the Moment by the time the LLM stage runs (`>0` means
/// above this VOD's average); any may be absent (no chat, or `ser` not built).
#[derive(Debug, Clone, Copy, Default)]
pub struct Context {
    pub chat_z: Option<f32>,
    pub loudness_z: Option<f32>,
    pub arousal_z: Option<f32>,
}

fn fz(o: Option<f32>) -> String {
    o.map(|v| format!("{v:+.2}")).unwrap_or_else(|| "n/a".into())
}

fn lang_name(l: Language) -> &'static str {
    match l {
        Language::En => "English",
        Language::Id => "Bahasa Indonesia",
        Language::Ja => "Japanese",
    }
}

/// Build the user-turn prompt for one candidate: the whole-video digest (when
/// stage 1 produced one — ADR 0071), the corroborating signals, and the
/// transcript, closing on a title-language reinforcement (a 7B obeys the last
/// line best; 5/25 saved ECA titles came out English before it). Pure +
/// unit-tested so the mitigation context is verifiable without the model. The
/// system instruction ([`SYSTEM`]) carries the rubric.
pub fn build_prompt(
    transcript: &str,
    ctx: &Context,
    language: Language,
    digest: Option<&str>,
) -> String {
    let body = transcript.trim();
    let body = if body.is_empty() { "(no speech transcribed)" } else { body };
    let digest_section = match digest.map(str::trim).filter(|d| !d.is_empty()) {
        Some(d) => format!("What the whole video is about (digest):\n\"\"\"\n{d}\n\"\"\"\n"),
        None => String::new(),
    };
    format!(
        "Transcript language: {lang}.\n\
         {digest_section}\
         Corroborating signals (z-scored across this VOD's candidates; >0 is above average):\n\
         - audience chat rate: {chat}\n\
         - loudness: {loud}\n\
         - speaker vocal arousal: {arou}\n\n\
         Transcript:\n\"\"\"\n{body}\n\"\"\"\n\n\
         Score this moment and write its title in {lang}.",
        lang = lang_name(language),
        chat = fz(ctx.chat_z),
        loud = fz(ctx.loudness_z),
        arou = fz(ctx.arousal_z),
        body = body,
    )
}

/// Ceiling on one candidate's excerpt inside the digest prompt — the digest
/// needs each moment's gist, not its every word.
pub const DIGEST_EXCERPT_MAX_CHARS: usize = 600;
/// Ceiling on ALL excerpt text in the digest prompt, so the one digest
/// inference always fits its context window (chars, not tokens: ~3-4 chars per
/// token for Indonesian/English keeps 18k chars ≈ 5-6k tokens under the
/// judge's 8192 digest context with the rubric and headers).
pub const DIGEST_TOTAL_MAX_CHARS: usize = 18_000;

/// First `max_chars` characters of `s`, cut on a char boundary.
fn take_chars(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Build the user turn for the ONE whole-video digest inference (ADR 0071):
/// the VOD's uploaded title + creator name (the strongest topic signals the
/// pipeline holds) and every candidate's transcript excerpt in timeline order.
/// Excerpts are capped per candidate and in total so the prompt always fits
/// the digest context window; empty transcripts are skipped. Pure +
/// unit-tested, like [`build_prompt`].
pub fn build_digest_prompt(req: &JudgeRequest) -> String {
    let mut by_time: Vec<&JudgeCandidate> =
        req.candidates.iter().filter(|c| !c.transcript.trim().is_empty()).collect();
    by_time.sort_by(|a, b| a.start_s.partial_cmp(&b.start_s).unwrap_or(std::cmp::Ordering::Equal));
    let cap = DIGEST_EXCERPT_MAX_CHARS.min(DIGEST_TOTAL_MAX_CHARS / by_time.len().max(1));
    let mut excerpts = String::new();
    for c in &by_time {
        let (m, s) = ((c.start_s / 60.0) as u64, (c.start_s % 60.0) as u64);
        excerpts.push_str(&format!("[at {m}m{s:02}s] {}\n", take_chars(c.transcript.trim(), cap)));
    }
    format!(
        "Video language: {lang}.\n\
         Creator/channel: {creator}\n\
         Uploaded video title: {title}\n\n\
         Transcript excerpts of the {n} most notable moments, in timeline order:\n\
         {excerpts}\n\
         Write the brief.",
        lang = lang_name(req.language),
        creator = if req.vod_creator.trim().is_empty() { "(unknown)" } else { &req.vod_creator },
        title = if req.vod_title.trim().is_empty() { "(unknown)" } else { &req.vod_title },
        n = by_time.len(),
    )
}

/// Integers in `[0, SCORE_MAX]` that could plausibly BE the score, in order.
///
/// The two shapes that are numbers but never scores — and that a bare
/// first-integer scan happily mistook for one:
/// - an **ordinal**: "after the 3rd try" is not a 3;
/// - a **denominator**: the 10 in "7 out of 10" / "7/10" is the scale, not the
///   verdict.
///
/// `s` is expected lowercased (the ordinal suffixes are matched literally).
fn score_candidates(s: &str) -> Vec<f32> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        let Ok(n) = s[start..i].parse::<u32>() else { continue };
        if n > SCORE_MAX {
            continue;
        }
        let after = &s[i..];
        if ["st", "nd", "rd", "th"].iter().any(|suf| after.starts_with(suf)) {
            continue; // ordinal
        }
        let before = s[..start].trim_end();
        if before.ends_with('/') || before.ends_with("out of") {
            continue; // denominator
        }
        out.push(n as f32);
    }
    out
}

/// The score a rambling model meant, when JSON parsing failed (the grammar was
/// unavailable and it wrote prose instead of an object).
///
/// A `score`-labelled number wins outright — that is the model answering the
/// question it was asked. Otherwise the FIRST plausible candidate stands, as
/// before; what changed on 2026-07-14 is that ordinals and denominators are no
/// longer candidates. The old scan read "after the 3rd try, I'd say 8" as a
/// **3** — a wrong score entering the ranking, silently. Bounded (fallback-only,
/// clamped) but simply the wrong digit.
fn fallback_score(s: &str) -> Option<f32> {
    // Work on one lowercased copy: the label match is case-insensitive and
    // digits are unaffected, so every index below stays consistent.
    let lower = s.to_lowercase();
    if let Some(at) = lower.rfind("score") {
        let tail = &lower[at + "score".len()..];
        // Only a number that FOLLOWS the label with nothing but punctuation or
        // space between — otherwise "score" was just a word in a sentence.
        let lead: String =
            tail.chars().take_while(|c| !c.is_ascii_digit() && *c != '\n').collect();
        if lead.chars().all(|c| c.is_whitespace() || matches!(c, ':' | '=' | '"' | '\'' | '*')) {
            if let Some(&n) = score_candidates(tail).first() {
                return Some(n);
            }
        }
    }
    score_candidates(&lower).first().copied()
}

/// One parsed judgment: the clip-worthiness score, the one-line reason, and the
/// generated Shorts title (ADR 0015). `title` is empty when the model omitted it
/// or the output fell back to the lenient path — the render then names the Short
/// from its timestamp instead.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Judgment {
    pub score: f32,
    pub reason: String,
    pub title: String,
}

/// Parse the model's output into a [`Judgment`]. Prefers the grammar-constrained
/// JSON object (`score`/`reason`/`title`); falls back to the first 0-10 integer
/// in the text (reason = the whole trimmed text, no title) so a malformed
/// generation still yields a usable score rather than aborting the whole detect
/// run.
pub fn parse_output(raw: &str) -> Judgment {
    let clamp = |s: f32| s.clamp(0.0, SCORE_MAX as f32);
    if let Some(start) = raw.find('{') {
        if let Some(rel_end) = raw[start..].rfind('}') {
            let obj = &raw[start..=start + rel_end];
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(obj) {
                let score = v
                    .get("score")
                    .and_then(|s| s.as_f64().map(|f| f as f32))
                    .or_else(|| v.get("score").and_then(|s| s.as_str()).and_then(|s| s.parse().ok()));
                if let Some(score) = score {
                    let field = |k: &str| {
                        v.get(k).and_then(|r| r.as_str()).unwrap_or("").trim().to_string()
                    };
                    return Judgment {
                        score: clamp(score),
                        reason: field("reason"),
                        title: normalize_title(field("title")),
                    };
                }
            }
        }
    }
    Judgment {
        score: clamp(fallback_score(raw).unwrap_or(0.0)),
        reason: raw.trim().to_string(),
        title: String::new(),
    }
}

/// App-wide punctuation rule (operator, 2026-07-03): no em/en dashes in
/// user-visible text. Generated titles are the one place they can still enter
/// (a 7B likes " — " in clickbait), and the title also names the rendered
/// file — normalize to a plain hyphen at the parse boundary.
fn normalize_title(t: String) -> String {
    if t.contains(['—', '–']) {
        t.replace(['—', '–'], "-")
    } else {
        t
    }
}

/// Fill the `llm` Signal on candidates from their per-candidate raw scores and
/// rerank - the analogue of [`crate::lexicon::apply`] and [`crate::arousal::apply`].
/// The LLM has no VOD-wide baseline (it is only run on candidates), so raw scores
/// are z-scored across the candidate set, then `combined_score` recomputes each
/// rank with the LLM judgment now present. `scores[i]` must correspond to
/// `moments[i]`.
pub fn apply(moments: &mut [Moment], scores: &[f32], weights: &Weights) {
    debug_assert_eq!(moments.len(), scores.len(), "one llm score per Moment");
    let z = score::robust_z(scores);
    for (m, lz) in moments.iter_mut().zip(z) {
        m.signals.llm = Some(lz);
        m.score = combined_score(&m.signals, weights);
    }
}

// --- IPC protocol with the `yc-llm-judge` sidecar binary (ADR 0010) ----------
// whisper.cpp and llama.cpp can't co-link, so the inference runs out-of-process.
// The app serializes a `JudgeRequest` to the child's stdin and reads back a
// `JudgeResponse` (whole-video digest + one verdict per candidate, in order —
// ADR 0071; a stale judge's bare verdict array still parses) from its stdout.

/// One candidate to judge: the transcript plus its corroborating z-scored signals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeCandidate {
    pub transcript: String,
    pub chat_z: Option<f32>,
    pub loudness_z: Option<f32>,
    pub arousal_z: Option<f32>,
    /// Candidate start within the VOD (seconds) — orders the digest's excerpts
    /// on the timeline (ADR 0071). Not shown in the per-candidate prompt.
    /// `#[serde(default)]` so a pre-0071 request still deserializes.
    #[serde(default)]
    pub start_s: f64,
}

impl JudgeCandidate {
    /// The corroborating [`Context`] for this candidate's prompt.
    pub fn context(&self) -> Context {
        Context { chat_z: self.chat_z, loudness_z: self.loudness_z, arousal_z: self.arousal_z }
    }
}

/// The whole candidate batch for one detect run, sent to the judge over stdin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeRequest {
    /// Absolute path to the GGUF the judge should load (operator-selectable per
    /// ADR 0002; the app passes its configured default).
    pub model_path: String,
    pub language: Language,
    /// The VOD's uploaded title — the strongest topic signal the digest
    /// inference gets (ADR 0071). `#[serde(default)]`: pre-0071 requests parse.
    #[serde(default)]
    pub vod_title: String,
    /// The VOD's creator/channel name, digest context (ADR 0071).
    #[serde(default)]
    pub vod_creator: String,
    pub candidates: Vec<JudgeCandidate>,
}

/// One candidate's verdict, returned by the judge in request order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeVerdict {
    /// Raw clip-worthiness in `[0, SCORE_MAX]` (z-scored by [`apply`] afterward).
    pub score: f32,
    /// One-line rationale for the review UI.
    pub reason: String,
    /// Generated Shorts title (ADR 0015), used to name the rendered Short when
    /// this Moment is promoted. Empty when the model omitted it (render falls back
    /// to a timestamp name). `#[serde(default)]` so a pre-0015 judge response (no
    /// title field) still deserializes.
    #[serde(default)]
    pub title: String,
}

/// The judge's whole reply (ADR 0071): the whole-video digest plus one verdict
/// per candidate, in request order. Read it with [`parse_response`], which also
/// accepts the pre-0071 bare verdict array.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeResponse {
    /// The stage-1 whole-video brief. Empty when the digest inference failed
    /// (soft: per-candidate prompts just lose the context section) or when a
    /// stale judge exe replied with a bare array.
    #[serde(default)]
    pub digest: String,
    pub verdicts: Vec<JudgeVerdict>,
}

/// Parse the judge's stdout leniently: the current [`JudgeResponse`] object, or
/// a pre-0071 judge exe's bare `Vec<JudgeVerdict>` array (digest empty). App /
/// sidecar version skew then degrades to the old no-digest behavior instead of
/// dropping the llm signal for the whole detect.
pub fn parse_response(raw: &str) -> serde_json::Result<JudgeResponse> {
    serde_json::from_str::<JudgeResponse>(raw).or_else(|object_err| {
        serde_json::from_str::<Vec<JudgeVerdict>>(raw)
            .map(|verdicts| JudgeResponse { digest: String::new(), verdicts })
            .map_err(|_| object_err)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use yc_core::{Signals, TimeRange};

    #[test]
    fn system_prompt_keeps_the_load_bearing_mitigation() {
        // If any of these clauses is edited away, the #11 scripted-cutscene
        // anti-signal (ADR 0009) is silently un-protected. Guard them.
        let s = SYSTEM.to_lowercase();
        assert!(s.contains("streamer"), "must anchor on the streamer");
        assert!(s.contains("scripted") && s.contains("cutscene"), "must name scripted game audio");
        assert!(s.contains("not clip-worthy"), "must say scripted drama is not clip-worthy");
        assert!(s.contains("corroborate") || s.contains("signals"), "must use the signals");
        assert!(SYSTEM.contains("\"score\""), "must request the JSON score field");
        // ADR 0015: the prompt must also request the generated Shorts title.
        assert!(SYSTEM.contains("\"title\""), "must request the JSON title field");
        assert!(s.contains("title"), "must ask for a title");
        // Production-ready titles (focus 2026-07): the hook-first, specific,
        // honest, no-generic-filler rules. Editing these away silently reverts
        // titles to the vague clickbait the operator rejected.
        assert!(s.contains("hook"), "must demand a hook-first title");
        assert!(s.contains("60 characters"), "must keep the Shorts length ceiling");
        assert!(s.contains("banned") || s.contains("never"), "must ban generic filler");
        assert!(s.contains("insane") && s.contains("epic"), "must name banned filler words");
        assert!(s.contains("honest"), "must forbid overpromising clickbait");
        assert!(s.contains("no emoji") && s.contains("no hashtags"), "format bans survive");
        // ADR 0071: the genre-aware rubric + the operator's curiosity-gap title
        // shapes. Editing these away reverts podcasts to the game-only rubric
        // and titles to dead topic descriptions.
        assert!(s.contains("podcast"), "rubric must cover podcasts/talk shows");
        assert!(s.contains("digest"), "must point the model at the whole-video digest");
        assert!(s.contains("curiosity gap"), "must demand a curiosity gap");
        assert!(s.contains("question") && s.contains("warning"), "operator title shapes survive");
        assert!(s.contains("never translate"), "title language lock survives");
        assert!(s.contains("describe the topic"), "dead topic-description titles stay banned");
        assert!(s.contains("never copy these examples"), "anti-parroting clause survives");
        // ADR 0071 iteration 2 (operator's eye, 2026-07-18): "more catchy and
        // hooking" - the two-beat construction their 11 example titles share.
        // Editing these away reverts to the flat single-beat titles they refused.
        assert!(s.contains("two beats"), "must demand the two-beat hook+payoff build");
        assert!(s.contains("payoff"), "must name the payoff tease beat");
        assert!(s.contains("stakes"), "must teach the stakes vocabulary");
        assert!(s.contains("40-60 characters"), "must push titles to use the room");
        // Iteration 2.2: the leashes for the failure modes iteration 2 exposed
        // on the ECA re-run - verbatim payoff pasting ("Hati-hati Kebalik!" on
        // two unrelated clips) and invented names/spellings ("COBUJER",
        // "TRAKTOR") on thin banter transcripts. NO caps leash: the operator's
        // eye approved the shouty ALL-CAPS energy (2026-07-18, "i like it now")
        // - only spelling fidelity is demanded of a capitalized word.
        assert!(s.contains("stock phrase"), "payoff pasting stays banned");
        assert!(s.contains("letter-for-letter"), "name/quote fidelity survives");
        assert!(s.contains("correctly spelled"), "caps must stay real words");
        assert!(s.contains("one clean honest beat"), "thin-transcript fallback survives");
    }

    #[test]
    fn digest_system_prompt_keeps_its_brief_contract() {
        // ADR 0071: the stage-1 digest must stay a compact, transcript-language
        // brief that names genre, speakers, topics and tensions — that is the
        // context every title inference leans on.
        let s = DIGEST_SYSTEM.to_lowercase();
        assert!(s.contains("podcast") && s.contains("game stream"), "must name the genres");
        assert!(s.contains("who is hosting or speaking"), "must identify the speakers");
        assert!(s.contains("topics"), "must summarize the topics");
        assert!(s.contains("tensions"), "must surface the tensions/claims");
        assert!(s.contains("transcript's language"), "brief stays in the VOD's language");
        assert!(s.contains("only the brief"), "free prose only — no headings/lists");
    }

    #[test]
    fn build_prompt_embeds_transcript_and_signals() {
        let ctx = Context { chat_z: Some(1.8), loudness_z: Some(-0.3), arousal_z: None };
        let p = build_prompt("Kaget mampus", &ctx, Language::Id, None);
        assert!(p.contains("Kaget mampus"));
        assert!(p.contains("Bahasa Indonesia"));
        assert!(p.contains("+1.80")); // chat z, signed
        assert!(p.contains("-0.30")); // loudness z, signed
        assert!(p.contains("n/a")); // arousal absent
        // ADR 0071: the closing language reinforcement — the recency-position
        // fix for the 5/25 English titles on the saved Indonesian ECA table.
        assert!(p.trim_end().ends_with("write its title in Bahasa Indonesia."));
        // No digest given -> no digest section (and no stray header).
        assert!(!p.contains("digest"));
    }

    #[test]
    fn build_prompt_embeds_the_digest_when_present() {
        let d = "Podcast Deddy Corbuzier bersama Echa membahas red flag cowok.";
        let p = build_prompt("Kaget mampus", &Context::default(), Language::Id, Some(d));
        assert!(p.contains("What the whole video is about (digest):"));
        assert!(p.contains(d));
        // The digest sits before the signals, the transcript after — the moment
        // stays the star of the prompt.
        assert!(p.find(d).unwrap() < p.find("Corroborating signals").unwrap());
        // Blank digests collapse to the no-digest shape instead of an empty header.
        let blank = build_prompt("Kaget", &Context::default(), Language::Id, Some("  "));
        assert!(!blank.contains("digest"));
    }

    #[test]
    fn build_prompt_handles_empty_transcript() {
        let p = build_prompt("   ", &Context::default(), Language::En, None);
        assert!(p.contains("(no speech transcribed)"));
    }

    #[test]
    fn digest_prompt_orders_caps_and_labels_excerpts() {
        let mk = |start_s: f64, text: &str| JudgeCandidate {
            transcript: text.into(),
            chat_z: None,
            loudness_z: None,
            arousal_z: None,
            start_s,
        };
        let req = JudgeRequest {
            model_path: "x.gguf".into(),
            language: Language::Id,
            vod_title: "JADI, COWOK RED FLAG MENURUT ECA SIAPA".into(),
            vod_creator: "Deddy Corbuzier".into(),
            // Out of timeline order on purpose; one empty transcript to skip.
            candidates: vec![mk(125.0, "kedua"), mk(3.0, "pertama"), mk(60.0, "   ")],
        };
        let p = build_digest_prompt(&req);
        assert!(p.contains("Deddy Corbuzier"));
        assert!(p.contains("JADI, COWOK RED FLAG MENURUT ECA SIAPA"));
        assert!(p.contains("Bahasa Indonesia"));
        // Timeline order with m:ss labels, empty candidate skipped.
        assert!(p.contains("[at 0m03s] pertama"));
        assert!(p.contains("[at 2m05s] kedua"));
        assert!(p.find("pertama").unwrap() < p.find("kedua").unwrap());
        assert!(p.contains("the 2 most notable moments"));

        // A long transcript is excerpted, not pasted whole — and the cut is on a
        // char boundary even mid-multibyte.
        let long = "é".repeat(DIGEST_EXCERPT_MAX_CHARS + 50);
        let req2 = JudgeRequest {
            model_path: "x.gguf".into(),
            language: Language::Id,
            vod_title: String::new(),
            vod_creator: String::new(),
            candidates: vec![mk(0.0, &long)],
        };
        let p2 = build_digest_prompt(&req2);
        assert!(p2.matches('é').count() == DIGEST_EXCERPT_MAX_CHARS);
        assert!(p2.contains("(unknown)")); // blank metadata reads as unknown

        // Many candidates shrink the per-candidate cap so the total stays
        // bounded ('q' never appears in the template text, so the count is
        // exactly the excerpts' payload).
        let many: Vec<_> = (0..100).map(|i| mk(i as f64, &"q".repeat(1000))).collect();
        let req3 = JudgeRequest {
            model_path: "x.gguf".into(),
            language: Language::En,
            vod_title: String::new(),
            vod_creator: String::new(),
            candidates: many,
        };
        let p3 = build_digest_prompt(&req3);
        assert!(p3.matches('q').count() <= DIGEST_TOTAL_MAX_CHARS);
    }

    #[test]
    fn parse_output_reads_clean_json_with_title() {
        let j = parse_output(r#"{"score": 8, "reason": "big laugh", "title": "He LOST it on the final boss"}"#);
        assert_eq!(j.score, 8.0);
        assert_eq!(j.reason, "big laugh");
        assert_eq!(j.title, "He LOST it on the final boss");
    }

    #[test]
    fn parse_output_normalizes_dashes_in_titles() {
        // Titles render in the UI and name the exported file; the app-wide
        // no-em-dash rule (operator, 2026-07-03) is enforced at the parse
        // boundary so no generation can smuggle one in.
        let j = parse_output(r#"{"score": 7, "reason": "x", "title": "Boss fight — he LOST it – twice"}"#);
        assert_eq!(j.title, "Boss fight - he LOST it - twice");
    }

    #[test]
    fn parse_output_tolerates_a_missing_title() {
        // A pre-0015 / non-compliant generation with no title field still parses;
        // the title is just empty (render falls back to a timestamp name).
        let j = parse_output(r#"{"score": 8, "reason": "big laugh"}"#);
        assert_eq!(j.score, 8.0);
        assert_eq!(j.reason, "big laugh");
        assert!(j.title.is_empty());
    }

    #[test]
    fn parse_output_accepts_string_score_and_surrounding_prose() {
        let j = parse_output(
            "Sure! {\"score\": \"3\", \"reason\": \"menu reading\", \"title\": \"Just the menu\"} done",
        );
        assert_eq!(j.score, 3.0);
        assert_eq!(j.reason, "menu reading");
        assert_eq!(j.title, "Just the menu");
    }

    #[test]
    fn fallback_ignores_ordinals_and_denominators() {
        // The review's case: the old scan took the FIRST integer anywhere and
        // read the ordinal as the verdict.
        assert_eq!(parse_output("after the 3rd try, I'd say 8").score, 8.0);
        // Denominators are the scale, not the score — either spelling.
        assert_eq!(parse_output("I'd give it 7 out of 10").score, 7.0);
        assert_eq!(parse_output("solid 6/10 honestly").score, 6.0);
        // A labelled score wins outright, wherever it sits.
        assert_eq!(parse_output("lots of reasons here... Score: 9").score, 9.0);
        assert_eq!(parse_output("in the 1st half he pops off. score = 4").score, 4.0);
        // ...but "score" as prose does not hijack a following number.
        assert_eq!(parse_output("I would score this moment a 5").score, 5.0);
    }

    #[test]
    fn parse_output_falls_back_to_first_integer() {
        let j = parse_output("I'd say 7 out of 10, lots of hype");
        assert_eq!(j.score, 7.0); // the 7 — the 10 is the denominator
        assert!(j.reason.contains("hype"));
        assert!(j.title.is_empty()); // no title recoverable from free prose
    }

    #[test]
    fn parse_output_clamps_and_defaults() {
        assert_eq!(parse_output(r#"{"score": 99, "reason": "x", "title": "y"}"#).score, SCORE_MAX as f32);
        assert_eq!(parse_output("no number here").score, 0.0);
    }

    #[test]
    fn apply_sets_llm_and_reranks_by_judgment() {
        let w = Weights { chat: 0.0, loudness: 0.0, lexicon: 0.0, arousal: 0.0, llm: 1.0 };
        let mk = |id, signals| Moment {
            id,
            range: TimeRange { start_s: 0.0, end_s: 30.0 },
            signals,
            score: 0.0,
            title: None,
        };
        let base = Signals { chat_rate: Some(1.0), loudness: Some(1.0), ..Default::default() };
        let mut moments = vec![mk(1, base), mk(2, base)];
        // Moment 2 is judged far more clip-worthy than Moment 1.
        apply(&mut moments, &[2.0, 8.0], &w);
        assert!(moments[0].signals.llm.unwrap() < moments[1].signals.llm.unwrap());
        assert!(moments[1].score > moments[0].score);
    }

    #[test]
    fn judge_request_roundtrips_through_json() {
        let req = JudgeRequest {
            model_path: "models/x.gguf".into(),
            language: Language::Id,
            vod_title: "JADI, COWOK RED FLAG".into(),
            vod_creator: "Deddy Corbuzier".into(),
            candidates: vec![JudgeCandidate {
                transcript: "Gila".into(),
                chat_z: Some(1.0),
                loudness_z: None,
                arousal_z: Some(0.5),
                start_s: 42.0,
            }],
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: JudgeRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.candidates[0].transcript, "Gila");
        assert_eq!(back.candidates[0].context().arousal_z, Some(0.5));
        assert_eq!(back.candidates[0].start_s, 42.0);
        assert_eq!(back.vod_title, "JADI, COWOK RED FLAG");
        // A pre-0071 request (no metadata, no start_s) still parses — the judge
        // must never reject an older app's payload.
        let old = r#"{"model_path":"m.gguf","language":"id",
            "candidates":[{"transcript":"Gila","chat_z":null,"loudness_z":null,"arousal_z":null}]}"#;
        let back: JudgeRequest = serde_json::from_str(old).unwrap();
        assert_eq!(back.vod_title, "");
        assert_eq!(back.candidates[0].start_s, 0.0);
    }

    #[test]
    fn parse_response_reads_object_and_stale_bare_array() {
        // The current shape: digest + verdicts.
        let obj = r#"{"digest":"Podcast tentang red flag.","verdicts":
            [{"score":7.0,"reason":"laugh","title":"Judul"}]}"#;
        let r = parse_response(obj).unwrap();
        assert_eq!(r.digest, "Podcast tentang red flag.");
        assert_eq!(r.verdicts.len(), 1);
        assert_eq!(r.verdicts[0].title, "Judul");
        // A stale pre-0071 judge exe replies with a bare array — same detect,
        // just no digest context (ADR 0071's version-skew degrade).
        let arr = r#"[{"score":3.0,"reason":"menu","title":""}]"#;
        let r = parse_response(arr).unwrap();
        assert!(r.digest.is_empty());
        assert_eq!(r.verdicts[0].score, 3.0);
        // Garbage is still an error (the llm signal is then omitted upstream).
        assert!(parse_response("not json").is_err());
    }
}
