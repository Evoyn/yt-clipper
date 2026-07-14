//! The occupant map (ADR 0043 spike; production per ADR 0044): who OCCUPIES
//! each seat, per inter-cut segment. A seat track is a screen position that
//! different humans hold across camera angles (ADR 0042 measured it); this map
//! is the de-circularizing anchor that lets the voice join be PERSON-scoped —
//! a voice cluster joining different persons across cameras is impure evidence
//! that co-occurrence alone can never see.
//!
//! Pure by construction: callers embed face crops however they like (the
//! pipeline and the `speaker_diag` harness share `yc_frame::face_id`) and hand
//! the aggregated per-(segment, seat) unit embeddings here. Clustering reuses
//! [`crate::voice::cluster_cosine`]; the person cut is the LARGEST DENDROGRAM
//! GAP (printed via [`OccupantMap::merges`]/[`OccupantMap::cut`], so the
//! same-face/different-face margin is evidence, not a magic number — measured
//! on the Deddy fixture: within-person merges <=0.23, gap to 0.56, cut 0.40).
//!
//! Conservatism rules (the ADR 0044 grill decisions):
//! - A person needs 2+ entries. A SINGLETON cluster (a pose extreme — the
//!   fixture's hand-on-chin / looking-down cases were the same humans as real
//!   persons) is an **unknown occupant**, not an identity: it carries no
//!   evidence, and its presence blocks absence-proofs in its segment.
//! - Segments merge into a CAMERA only on identical, fully-known occupant
//!   maps; a segment holding any unknown occupant never merges. Merged
//!   cameras pool the voice join's co-occurrence evidence; unmerged segments
//!   stay single-visit (no local claims) but still accept person-transfer
//!   claims through their confident entries — a map placement is face
//!   evidence, not mouth echo.

use std::collections::BTreeMap;

/// Crops sampled per (segment, seat) — averaged into one embedding, so a
/// blink or a motion-blurred sample can't mint its own person.
pub const SAMPLES_PER_SEG: usize = 4;
/// Region around the tracked face box handed to YuNet (the warp needs
/// forehead-to-chin plus air; the tracked box is Ultraface-tight).
pub const REGION_EXPAND: f32 = 2.0;

/// Plan the face-sample times: per segment, bins where the MOST tracks are
/// present, at spread quantiles, inset one bin from the cut edges (a
/// cut-straddling decode would crop the wrong camera's pixels). Returns
/// `(segment, clip-relative seconds)` — one source seek per entry, every
/// present seat cropped from the same frame.
pub fn plan_samples(
    analysis: &crate::speaker::SpeakerAnalysis,
    bounds: &[f64],
    per_seg: usize,
) -> Vec<(usize, f64)> {
    let bin_s = analysis.bin_s;
    let n_bins = analysis.speaking.len();
    let n_segs = bounds.len().saturating_sub(1);
    let count_at = |b: usize| -> usize {
        analysis
            .tracks
            .iter()
            .filter(|t| t.path.get(b).map(|p| p.is_some()).unwrap_or(false))
            .count()
    };
    let mut samples: Vec<(usize, f64)> = Vec::new();
    for g in 0..n_segs {
        let b0 = (bounds[g] / bin_s).round() as usize;
        let b1 = ((bounds[g + 1] / bin_s).round() as usize).min(n_bins).max(b0);
        if b1 <= b0 {
            continue;
        }
        let (lo, hi) = if b1 - b0 > 2 { (b0 + 1, b1 - 1) } else { (b0, b1) };
        let max_c = (lo..hi).map(count_at).max().unwrap_or(0);
        if max_c == 0 {
            continue;
        }
        let cands: Vec<usize> = (lo..hi).filter(|&b| count_at(b) == max_c).collect();
        let mut picked: Vec<usize> = Vec::new();
        for q in [0.12, 0.38, 0.62, 0.88].iter().take(per_seg) {
            let b = cands[((cands.len() - 1) as f64 * q).round() as usize];
            if !picked.contains(&b) {
                picked.push(b);
            }
        }
        for b in picked {
            samples.push((g, (b as f64 + 0.5) * bin_s));
        }
    }
    samples
}

/// Copy a clamped rectangle out of an rgb24 frame. Returns the crop, its
/// dimensions, and the clamped origin (region coordinates for the caller's
/// landmark math).
pub fn crop_rgb(
    src: &[u8],
    sw: usize,
    sh: usize,
    x0: i32,
    y0: i32,
    w: usize,
    h: usize,
) -> (Vec<u8>, usize, usize, i32, i32) {
    let x0 = x0.clamp(0, sw.saturating_sub(1) as i32);
    let y0 = y0.clamp(0, sh.saturating_sub(1) as i32);
    let x1 = ((x0 as usize) + w).min(sw);
    let y1 = ((y0 as usize) + h).min(sh);
    let (cw, ch) = (x1 - x0 as usize, y1 - y0 as usize);
    let mut out = vec![0u8; cw * ch * 3];
    for y in 0..ch {
        let s = ((y0 as usize + y) * sw + x0 as usize) * 3;
        out[y * cw * 3..(y + 1) * cw * 3].copy_from_slice(&src[s..s + cw * 3]);
    }
    (out, cw, ch, x0, y0)
}

/// The intended face among a region's detections: nearest to the tracked box
/// center (region coordinates), sane size relative to it — a neighbour
/// leaking into the region or a poster face must not become a seat's crop.
pub fn pick_track_face<'d>(
    dets: &'d [crate::face_id::FaceDet],
    ecx: f32,
    ecy: f32,
    fh: f32,
) -> Option<&'d crate::face_id::FaceDet> {
    dets.iter()
        .filter(|d| d.bbox.h >= 0.4 * fh && d.bbox.h <= 2.5 * fh)
        .map(|d| {
            let dc = ((d.bbox.cx() - ecx).powi(2) + (d.bbox.cy() - ecy).powi(2)).sqrt();
            (dc, d)
        })
        .filter(|(dc, _)| *dc < 0.9 * fh.max(1.0))
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, d)| d)
}

/// Who holds a seat in one segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Occupant {
    /// A person cluster with 2+ (segment, seat) entries — a usable identity.
    Person(usize),
    /// A singleton cluster (pose extreme): somebody sits here, we cannot say
    /// who. Blocks absence-proofs; contributes no identity evidence.
    Unknown,
}

/// The occupant map over a clip's segments (one entry per
/// [`crate::voice::segment_bounds`] segment, indexes aligned).
#[derive(Debug, Clone)]
pub struct OccupantMap {
    /// Per segment: seat track id -> occupant. Missing seat = no usable face
    /// was sampled there (absent, occluded, or the track isn't present).
    pub seats: Vec<BTreeMap<usize, Occupant>>,
    /// Persons with 2+ entries (usable identities), numbered `0..n_persons`.
    pub n_persons: usize,
    /// Segment -> camera id. Segments share a camera only when their occupant
    /// maps are identical and fully known (no unknowns, at least one entry).
    pub seg_camera: Vec<usize>,
    /// Per camera: how many segments it spans (the multi-visit rule reads it).
    pub camera_visits: Vec<usize>,
    /// The clustering evidence for printing: every accepted merge distance in
    /// order, and the cut placed at the largest gap.
    pub merges: Vec<f32>,
    pub cut: f32,
    /// Per input entry: its cluster id (size-ordered — ids below
    /// [`Self::n_persons`] are persons, the rest singletons). The harness
    /// contact sheet rows by this, so an operator sees singleton sightings as
    /// their own rows instead of a merged "unknown" row that could lie.
    pub assignment: Vec<usize>,
}

impl OccupantMap {
    /// The seat `person` occupies in segment `g`, if the map knows one — the
    /// person-transfer placement (valid even in single-visit segments; the
    /// identity evidence lives elsewhere, the placement is face evidence).
    pub fn seat_of(&self, g: usize, person: usize) -> Option<usize> {
        self.seats.get(g)?.iter().find_map(|(&t, &o)| (o == Occupant::Person(person)).then_some(t))
    }

    /// POSITIVE ABSENCE (the only evidence the off-screen flag may act on):
    /// true only when segment `g`'s occupants are fully known — every track in
    /// `present` (the seats actually on screen there) has a KNOWN person — and
    /// `person` is not among them. Any ignorance (an unsampled seat, an
    /// unknown occupant, an empty segment) means no proof.
    pub fn absent(&self, g: usize, person: usize, present: &[usize]) -> bool {
        let Some(seats) = self.seats.get(g) else { return false };
        if present.is_empty() || seats.is_empty() {
            return false;
        }
        for t in present {
            match seats.get(t) {
                Some(Occupant::Person(p)) if *p == person => return false,
                Some(Occupant::Person(_)) => {}
                Some(Occupant::Unknown) | None => return false,
            }
        }
        true
    }

    /// Cameras seen 2+ times — the pooled-evidence cameras the join may learn
    /// local edges from.
    pub fn multi_visit(&self, camera: usize) -> bool {
        self.camera_visits.get(camera).copied().unwrap_or(0) >= 2
    }
}

/// One aggregated face-identity entry: the averaged unit embedding of a
/// (segment, seat)'s sampled crops ([`crate::face_id::aggregate_unit`]).
#[derive(Debug, Clone)]
pub struct FaceEntry {
    pub seg: usize,
    pub track: usize,
    pub emb: Vec<f32>,
}

/// Cut threshold from a full merge trail: the midpoint of the largest gap
/// between consecutive merge distances (0.5 when fewer than 2 merges — one
/// pair of entries carries no gap evidence).
pub fn gap_cut(merges: &[f32]) -> f32 {
    if merges.len() < 2 {
        return 0.5;
    }
    let mut best = (0usize, 0f32);
    for i in 0..merges.len() - 1 {
        let d = merges[i + 1] - merges[i];
        if d > best.1 {
            best = (i, d);
        }
    }
    (merges[best.0] + merges[best.0 + 1]) * 0.5
}

/// Build the occupant map from aggregated entries. `None` when fewer than two
/// entries embedded (nothing to cluster over). `n_segs` sizes the map — the
/// caller's [`crate::voice::segment_bounds`] segment count.
pub fn build_occupant_map(entries: &[FaceEntry], n_segs: usize) -> Option<OccupantMap> {
    if entries.len() < 2 {
        return None;
    }
    let embs: Vec<Vec<f32>> = entries.iter().map(|e| e.emb.clone()).collect();
    // Run the agglomeration to ONE cluster for the full trail, then cut at
    // the largest gap — the same-face/different-face margin decides, and the
    // trail is kept on the map for printing.
    let trail = crate::voice::agglomerate(&embs);
    let cut = gap_cut(trail.merges());
    let cl = trail.cut(cut);
    // cluster_cosine orders ids by size (desc), so every 2+-entry person
    // precedes every singleton: persons are exactly the ids below n_persons.
    let mut counts = vec![0usize; cl.k];
    for &a in &cl.assignment {
        counts[a] += 1;
    }
    let n_persons = counts.iter().filter(|&&n| n >= 2).count();
    let mut seats: Vec<BTreeMap<usize, Occupant>> = vec![BTreeMap::new(); n_segs];
    for (i, e) in entries.iter().enumerate() {
        if e.seg >= n_segs {
            continue; // malformed caller input — drop rather than panic
        }
        let occ = if cl.assignment[i] < n_persons {
            Occupant::Person(cl.assignment[i])
        } else {
            Occupant::Unknown
        };
        seats[e.seg].insert(e.track, occ);
    }
    // CAMERA merge: identical fully-known occupant maps share a camera; any
    // unknown occupant (or an empty segment) keeps its segment single-visit.
    let mut keys: Vec<Vec<(usize, usize)>> = Vec::new(); // known (track, person) sets
    let mut seg_camera = vec![0usize; n_segs];
    let mut camera_visits: Vec<usize> = Vec::new();
    for g in 0..n_segs {
        let mergeable = !seats[g].is_empty()
            && seats[g].values().all(|o| matches!(o, Occupant::Person(_)));
        let cam = if mergeable {
            let key: Vec<(usize, usize)> = seats[g]
                .iter()
                .map(|(&t, &o)| match o {
                    Occupant::Person(p) => (t, p),
                    Occupant::Unknown => unreachable!("mergeable checked"),
                })
                .collect();
            match keys.iter().position(|k| *k == key) {
                Some(c) => c,
                None => {
                    keys.push(key);
                    camera_visits.push(0);
                    keys.len() - 1
                }
            }
        } else {
            // A non-mergeable segment is its own camera, never revisited.
            keys.push(vec![(usize::MAX, g)]); // unmatchable sentinel key
            camera_visits.push(0);
            keys.len() - 1
        };
        seg_camera[g] = cam;
        camera_visits[cam] += 1;
    }
    Some(OccupantMap {
        seats,
        n_persons,
        seg_camera,
        camera_visits,
        merges: trail.merges().to_vec(),
        cut,
        assignment: cl.assignment,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(seg: usize, track: usize, emb: &[f32]) -> FaceEntry {
        let n = emb.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-12);
        FaceEntry { seg, track, emb: emb.iter().map(|v| v / n).collect() }
    }

    /// Two alternating cameras, two persons each — the Deddy shape in
    /// miniature. Persons are well-separated unit vectors.
    fn two_camera_entries() -> Vec<FaceEntry> {
        let p0 = [1.0f32, 0.0, 0.0, 0.0];
        let p1 = [0.0f32, 1.0, 0.0, 0.0];
        let p2 = [0.0f32, 0.0, 1.0, 0.0];
        let p3 = [0.0f32, 0.0, 0.0, 1.0];
        vec![
            e(0, 0, &p0),
            e(0, 1, &p1),
            e(1, 0, &p2),
            e(1, 1, &p3),
            e(2, 0, &p0),
            e(2, 1, &p1),
            e(3, 0, &p2),
            e(3, 1, &p3),
        ]
    }

    #[test]
    fn merges_identical_occupants_into_cameras() {
        let map = build_occupant_map(&two_camera_entries(), 4).expect("map builds");
        assert_eq!(map.n_persons, 4);
        assert_eq!(map.seg_camera[0], map.seg_camera[2], "same occupants, same camera");
        assert_eq!(map.seg_camera[1], map.seg_camera[3]);
        assert_ne!(map.seg_camera[0], map.seg_camera[1]);
        assert!(map.multi_visit(map.seg_camera[0]));
        assert!(map.multi_visit(map.seg_camera[1]));
    }

    #[test]
    fn seat_of_places_a_person_per_segment() {
        let map = build_occupant_map(&two_camera_entries(), 4).expect("map builds");
        // The person at seg0 seat 0 sits at seat 0 in seg2 too, and nowhere
        // in seg1 (the other camera).
        let p = match map.seats[0][&0] {
            Occupant::Person(p) => p,
            Occupant::Unknown => panic!("known person expected"),
        };
        assert_eq!(map.seat_of(0, p), Some(0));
        assert_eq!(map.seat_of(2, p), Some(0));
        assert_eq!(map.seat_of(1, p), None);
    }

    #[test]
    fn absence_needs_full_known_coverage() {
        let map = build_occupant_map(&two_camera_entries(), 4).expect("map builds");
        let p_cam0 = match map.seats[0][&0] {
            Occupant::Person(p) => p,
            Occupant::Unknown => panic!(),
        };
        // Positively absent from the other camera's segment (both seats known).
        assert!(map.absent(1, p_cam0, &[0, 1]));
        // Not absent where seated.
        assert!(!map.absent(0, p_cam0, &[0, 1]));
        // A present track the map has no entry for = ignorance, no proof.
        assert!(!map.absent(1, p_cam0, &[0, 1, 2]));
        // An empty present list proves nothing.
        assert!(!map.absent(1, p_cam0, &[]));
    }

    #[test]
    fn singleton_is_unknown_and_blocks_merge_and_absence() {
        // Cameras as before, plus seg4 where seat 1's face went pose-extreme
        // (a far-off embedding, singleton) while seat 0 stays person 0.
        let mut entries = two_camera_entries();
        let p0 = [1.0f32, 0.0, 0.0, 0.0];
        let odd = [0.5f32, -0.5, 0.5, -0.5];
        entries.push(e(4, 0, &p0));
        entries.push(e(4, 1, &odd));
        let map = build_occupant_map(&entries, 5).expect("map builds");
        assert_eq!(map.n_persons, 4, "the singleton mints no person");
        assert_eq!(map.seats[4][&1], Occupant::Unknown);
        // seg4 must not merge with the camera whose known half it matches.
        assert_ne!(map.seg_camera[4], map.seg_camera[0]);
        assert!(!map.multi_visit(map.seg_camera[4]));
        // The unknown blocks every absence proof in seg4...
        let p1 = match map.seats[0][&1] {
            Occupant::Person(p) => p,
            Occupant::Unknown => panic!(),
        };
        assert!(!map.absent(4, p1, &[0, 1]), "unknown occupant = no absence proof");
        // ...but the confident entry still places its person there.
        let p_seat0 = match map.seats[4][&0] {
            Occupant::Person(p) => p,
            Occupant::Unknown => panic!(),
        };
        assert_eq!(map.seat_of(4, p_seat0), Some(0));
    }

    #[test]
    fn partial_segments_merge_only_with_identical_partials() {
        // Two segments knowing only seat 1 = person X merge; a fuller segment
        // with the same person does not join them.
        let p0 = [1.0f32, 0.0, 0.0];
        let p1 = [0.0f32, 1.0, 0.0];
        let entries = vec![
            e(0, 0, &p0),
            e(0, 1, &p1),
            e(1, 1, &p1),
            e(2, 1, &p1),
            e(3, 0, &p0),
            e(3, 1, &p1),
        ];
        let map = build_occupant_map(&entries, 4).expect("map builds");
        assert_eq!(map.seg_camera[1], map.seg_camera[2], "identical partial maps merge");
        assert_ne!(map.seg_camera[0], map.seg_camera[1], "partial never joins the full camera");
        assert_eq!(map.seg_camera[0], map.seg_camera[3]);
    }

    #[test]
    fn empty_segments_never_merge() {
        let p0 = [1.0f32, 0.0];
        let p1 = [0.0f32, 1.0];
        let entries = vec![e(0, 0, &p0), e(2, 0, &p0), e(0, 1, &p1), e(2, 1, &p1)];
        let map = build_occupant_map(&entries, 4).expect("map builds");
        assert_ne!(map.seg_camera[1], map.seg_camera[3], "no-face segments stay single-visit");
        assert!(!map.multi_visit(map.seg_camera[1]));
        assert!(map.absent(1, 99, &[0]) == false, "empty segment proves nothing");
    }

    #[test]
    fn too_few_entries_yields_no_map() {
        assert!(build_occupant_map(&[], 3).is_none());
        assert!(build_occupant_map(&[e(0, 0, &[1.0, 0.0])], 3).is_none());
    }

    #[test]
    fn gap_cut_finds_the_largest_gap() {
        // Deddy-shaped trail: tight within-person merges, one wide gap.
        let merges = [0.05, 0.1, 0.15, 0.2, 0.23, 0.56, 0.66];
        let cut = gap_cut(&merges);
        assert!((cut - 0.395).abs() < 1e-6, "cut {cut}");
        assert_eq!(gap_cut(&[0.3]), 0.5, "one merge carries no gap evidence");
    }
}
