//! The Caption review-queue panel (ADR 0032): surface a Creator's harvested caption
//! to-dos in-app so the operator fills the correct word instead of hand-editing JSON.
//!
//! This module is the panel's non-egui core — turning a per-Creator dialect store's
//! `unverified` to-dos (ADR 0014/0022) into clip-grouped rows with parsed provenance,
//! building the VOD jump link, and loading/saving the store. Kept UI-free so the
//! grouping/link/round-trip logic is unit-tested without a running app; `main.rs`'s
//! `ui_review` draws these.

use std::path::PathBuf;
use yc_transcribe::{parse_harvest_note, Correction, DialectLexicon, HarvestNote};

const OTHER_GROUP: &str = "Other";

/// One to-do row: the index into the store's `corrections` (so the panel edits
/// `right`/`context` in place) plus the parsed provenance (ADR 0022) for display.
pub struct TodoRow {
    pub idx: usize,
    pub wrong: String,
    pub note: HarvestNote,
}

/// To-dos grouped under their source clip Title (ADR 0032) — the "per-export" feel
/// without needing separate per-clip files. Untitled (manual-clip) harvests collect
/// under "Other".
pub struct TodoGroup {
    pub title: String,
    pub rows: Vec<TodoRow>,
}

/// Is this correction an open to-do? A harvested garble awaiting curation has a
/// non-empty `wrong` and a blank `right` (ADR 0031 — the logic keys off `right`, not
/// `status`). A filled `right` is confirmed curation, not a to-do.
fn is_todo(c: &Correction) -> bool {
    !c.wrong.is_empty() && c.right.is_empty()
}

/// Count of open to-dos in a store (test aid; the panel shows the snapshot).
#[cfg(test)]
pub fn todo_count(corrections: &[Correction]) -> usize {
    corrections.iter().filter(|c| is_todo(c)).count()
}

/// The indices of the currently-open to-dos — the queue MEMBERSHIP SNAPSHOT.
/// The panel must group by a snapshot taken at load/save, never by the live
/// blank-`right` predicate: typing the first character into a row makes it
/// non-blank, and a live filter yanks the row (and the very input being typed
/// in) out of the UI mid-keystroke (operator bug report). A filled row leaves
/// the queue on Save, as documented (CONTEXT.md: Review queue).
pub fn open_todo_indices(corrections: &[Correction]) -> Vec<usize> {
    corrections.iter().enumerate().filter(|(_, c)| is_todo(c)).map(|(i, _)| i).collect()
}

/// Group the snapshot `queue` rows by the clip Title parsed from each note,
/// preserving first-seen order among titled groups and sinking "Other"
/// (untitled) to the bottom. The returned rows own their display data and
/// carry the source `idx`, so the caller edits `corrections[idx]` in place —
/// rows stay put while the operator types into them.
pub fn group_queue(corrections: &[Correction], queue: &[usize]) -> Vec<TodoGroup> {
    let mut groups: Vec<TodoGroup> = Vec::new();
    for &idx in queue {
        let Some(c) = corrections.get(idx) else { continue };
        let note = parse_harvest_note(&c.note);
        let title = note.title.clone().unwrap_or_else(|| OTHER_GROUP.to_string());
        let row = TodoRow { idx, wrong: c.wrong.clone(), note };
        match groups.iter_mut().find(|g| g.title == title) {
            Some(g) => g.rows.push(row),
            None => groups.push(TodoGroup { title, rows: vec![row] }),
        }
    }
    // Stable sort: titled groups keep first-seen order, "Other" sinks last.
    groups.sort_by_key(|g| g.title == OTHER_GROUP);
    groups
}

/// [`group_queue`] over a fresh live snapshot — the one-shot view (test aid).
#[cfg(test)]
pub fn group_unverified(corrections: &[Correction]) -> Vec<TodoGroup> {
    group_queue(corrections, &open_todo_indices(corrections))
}

/// A YouTube deep-link to `at_s` in a VOD, so the operator can hear a garble in
/// context (ADR 0022's provenance, made clickable). YouTube's `t=` takes whole seconds.
pub fn youtube_jump_url(video_id: &str, at_s: f64) -> String {
    format!("https://www.youtube.com/watch?v={video_id}&t={}s", at_s.max(0.0).round() as u64)
}

/// The panel's loaded state (ADR 0032): the per-Creator store being curated, where it
/// lives on disk, and the VOD's `video_id` for jump links. Loaded on Import, edited in
/// place by the panel, written back on Save.
pub struct ReviewState {
    /// `workspace/<creator>/<lang>.json` — the per-Creator store (ADR 0031).
    pub path: PathBuf,
    /// `Some` for a YouTube VOD (enables the click-to-hear jump); `None` for local.
    pub video_id: Option<String>,
    /// The store, edited in place; `corrections[idx].right`/`.context` are the fields
    /// the panel writes.
    pub lexicon: DialectLexicon,
    /// The queue membership snapshot ([`open_todo_indices`], taken at load and
    /// refreshed on save): the rows the panel shows, stable while typing.
    pub queue: Vec<usize>,
    /// Last load/save outcome, shown in the panel footer.
    pub status: String,
}

impl ReviewState {
    /// Load the per-Creator store at `path` (ADR 0031/0032). A missing or unparseable
    /// file yields an empty store (the queue is simply empty) — not an error: a new
    /// Creator has no store until the first render harvests one.
    pub fn load(path: PathBuf, clip_stores: &[PathBuf], video_id: Option<String>) -> Self {
        let mut lexicon = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<DialectLexicon>(&s).ok())
            .unwrap_or_default();
        // Surface per-clip harvests too (ADR 0032, closing the per-clip gap): append
        // each per-clip store's unverified to-dos whose `wrong` this store doesn't
        // already know. On Save they consolidate into the per-Creator store (its
        // established home for this Creator's to-dos, like the migrated ones); a
        // confirmed fix there is never shadowed by the lingering per-clip entry
        // (yc_transcribe::merge_corrections guards that at render time).
        let mut known: std::collections::HashSet<String> =
            lexicon.corrections.iter().map(|c| c.wrong.to_lowercase()).collect();
        for cp in clip_stores {
            let Some(clip) = std::fs::read_to_string(cp)
                .ok()
                .and_then(|s| serde_json::from_str::<DialectLexicon>(&s).ok())
            else {
                continue;
            };
            for c in clip.corrections {
                if !c.wrong.is_empty() && c.right.is_empty() && known.insert(c.wrong.to_lowercase()) {
                    lexicon.corrections.push(c);
                }
            }
        }
        let queue = open_todo_indices(&lexicon.corrections);
        let status = match queue.len() {
            0 => "No caption to-dos to curate.".to_string(),
            1 => "1 caption to-do to curate.".to_string(),
            n => format!("{n} caption to-dos to curate."),
        };
        ReviewState { path, video_id, lexicon, queue, status }
    }

    /// Write the store back (ADR 0032), mirroring the crate's writer idiom
    /// (`to_string_pretty` + trailing newline). Every row the operator filled (`right`)
    /// is trimmed and marked `confirmed`; blank rows stay as harvested to-dos. Because
    /// this edits the per-Creator layer directly (the promote target), a saved fix is
    /// durable for the Creator immediately — no re-render needed to promote.
    pub fn save(&mut self) -> std::io::Result<usize> {
        let mut confirmed = 0;
        for c in &mut self.lexicon.corrections {
            let trimmed = c.right.trim();
            if !trimmed.is_empty() {
                if trimmed != c.right {
                    c.right = trimmed.to_string();
                }
                if c.status != "confirmed" {
                    c.status = "confirmed".to_string();
                }
                confirmed += 1;
            }
        }
        let s = serde_json::to_string_pretty(&self.lexicon)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        // Atomic (temp + rename): this file is the operator's curation; a torn
        // write would read back as an empty store and silently lose it all.
        yc_core::write_atomic(&self.path, &(s + "\n"))?;
        // Refresh the snapshot: rows the operator just confirmed leave the
        // queue NOW (on Save) — never mid-keystroke.
        self.queue = open_todo_indices(&self.lexicon.corrections);
        Ok(confirmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn todo(wrong: &str, note: &str) -> Correction {
        Correction {
            wrong: wrong.into(),
            right: String::new(),
            note: note.into(),
            status: "unverified".into(),
            ..Default::default()
        }
    }
    fn confirmed(wrong: &str, right: &str) -> Correction {
        Correction {
            wrong: wrong.into(),
            right: right.into(),
            status: "confirmed".into(),
            ..Default::default()
        }
    }

    #[test]
    fn groups_unverified_by_clip_title_skipping_confirmed() {
        // Real note shapes from workspace/Ino Gemink Live Streaming/id.json.
        let cs = vec![
            todo("nyoli-nyoli", "auto-harvested (conf 0.21) from \"Nyoli Setan, Kaki Tiket!\" at 1:11:11 - operator verify"),
            confirmed("cowok", "cok"), // curation history, not a to-do
            todo("dijekat", "auto-harvested (conf 0.11) from \"Diskusi game biasa\" at 31:24 - operator verify"),
            todo("Cimri", "auto-harvested (conf 0.11) from \"Diskusi game biasa\" at 31:51 - operator verify"),
            todo("dimalai", "auto-harvested (conf 0.36) at 36:54 - operator verify"), // no title
        ];
        let groups = group_unverified(&cs);
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].title, "Nyoli Setan, Kaki Tiket!");
        assert_eq!(groups[0].rows.len(), 1);
        assert_eq!(groups[1].title, "Diskusi game biasa");
        assert_eq!(groups[1].rows.len(), 2); // two garbles from one clip grouped together
        assert_eq!(groups[1].rows[0].note.confidence, Some(0.11));
        assert_eq!(groups[2].title, "Other"); // untitled sinks to the bottom
        assert_eq!(groups[2].rows[0].idx, 4); // source index preserved for in-place edit
    }

    #[test]
    fn todo_count_ignores_confirmed_and_empty_wrong() {
        let cs = vec![todo("a", ""), confirmed("b", "x"), todo("c", "")];
        assert_eq!(todo_count(&cs), 2);
    }

    #[test]
    fn youtube_jump_url_targets_whole_second() {
        assert_eq!(
            youtube_jump_url("BUDS9qx2jw0", 4271.4),
            "https://www.youtube.com/watch?v=BUDS9qx2jw0&t=4271s"
        );
    }

    #[test]
    fn save_marks_filled_rows_confirmed_and_trims() {
        let dir = std::env::temp_dir().join("yc_review_save_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("id.json");
        let lexicon = DialectLexicon {
            corrections: vec![todo("buntur", "note"), todo("cimri", "note")],
            ..Default::default()
        };
        let queue = open_todo_indices(&lexicon.corrections);
        let mut st = ReviewState { path: path.clone(), video_id: None, lexicon, queue, status: String::new() };
        // Operator fills one (with stray whitespace), leaves the other blank.
        st.lexicon.corrections[0].right = "  Guntur  ".into();
        // TYPING must not shrink the queue (the row + its input would vanish
        // mid-keystroke); the snapshot still shows both rows...
        assert_eq!(st.queue.len(), 2);
        assert_eq!(group_queue(&st.lexicon.corrections, &st.queue).iter().map(|g| g.rows.len()).sum::<usize>(), 2);
        let n = st.save().unwrap();
        assert_eq!(n, 1);
        // ...and SAVE is what retires the confirmed row from the queue.
        assert_eq!(st.queue.len(), 1);
        assert_eq!(st.lexicon.corrections[st.queue[0]].wrong, "cimri");
        // Reload from disk: the filled one is trimmed + confirmed, the blank stays a to-do.
        let reloaded: DialectLexicon =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(reloaded.corrections[0].right, "Guntur");
        assert_eq!(reloaded.corrections[0].status, "confirmed");
        assert!(reloaded.corrections[1].right.is_empty());
        assert_eq!(reloaded.corrections[1].status, "unverified");
        assert_eq!(todo_count(&reloaded.corrections), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_surfaces_per_clip_todos_deduped_against_known() {
        let dir = std::env::temp_dir().join("yc_review_clipmerge");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let creator = dir.join("id.json");
        let clip = dir.join("Big Play.id.json");
        // per-Creator: one confirmed fix + one open to-do.
        let cl = DialectLexicon {
            corrections: vec![confirmed("dijekat", "dicegat"), todo("cimri", "note")],
            ..Default::default()
        };
        std::fs::write(&creator, serde_json::to_string_pretty(&cl).unwrap()).unwrap();
        // per-clip: a fresh to-do + a dup of an already-confirmed word (must be skipped).
        let clp = DialectLexicon {
            corrections: vec![
                todo("ngomplok", "auto-harvested (conf 0.09) from \"Big Play\" at 1:00 - operator verify"),
                todo("dijekat", "note"),
            ],
            ..Default::default()
        };
        std::fs::write(&clip, serde_json::to_string_pretty(&clp).unwrap()).unwrap();

        let st = ReviewState::load(creator, &[clip], None);
        let todos: Vec<&str> =
            st.lexicon.corrections.iter().filter(|c| c.right.is_empty()).map(|c| c.wrong.as_str()).collect();
        assert!(todos.contains(&"ngomplok"), "fresh per-clip to-do surfaced");
        assert!(todos.contains(&"cimri"), "existing per-Creator to-do kept");
        assert!(!todos.contains(&"dijekat"), "already-confirmed word not re-surfaced as a to-do");
        assert_eq!(todo_count(&st.lexicon.corrections), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
