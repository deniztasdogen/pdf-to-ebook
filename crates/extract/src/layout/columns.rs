//! Splitting a page body into columns.
//!
//! This must run **before** line assembly, not after. Sorting words by y across
//! a two-column page interleaves the columns: on page 5 of the 1901 dime-novel
//! scan, clustering by y produced lines that alternated between the left and
//! right columns, because the two columns' baselines do not align.
//!
//! Detection works on **row occupancy**: quantise the words into text rows,
//! then for each vertical strip of the page ask what share of rows put any word
//! in it. A column gutter is a wide strip that almost no row occupies.
//!
//! Two earlier attempts failed and are worth recording. Measuring occupancy by
//! accumulated glyph *height* made a strip holding a single line of text look
//! empty. And sizing the gutter as a fraction of page width rejected the real
//! thing: the CVPR gutter in `resnet-two-column.pdf` is about 17pt on a 495pt
//! body, which is under 4%. A gutter's natural scale is the **font size** — it
//! runs 1.5x the body text or more, while an inter-word space is nearer 0.25x,
//! so that ratio separates them with room to spare.

use crate::geom::median;
use crate::model::Word;

/// A gutter must be at least this multiple of the median glyph height.
const MIN_GAP_FONT_MULTIPLE: f32 = 1.5;

/// ...and at least this fraction of the body width, so a hairline crack in
/// sparse text cannot qualify.
const MIN_GAP_FRAC: f32 = 0.02;

/// A strip counts as empty when at most this share of text rows reach into it.
///
/// Not zero, because a full-width figure or table crosses the gutter for the
/// rows it spans and must not veto the whole page. 15% leaves room for that
/// while staying far below the 70%+ occupancy every strip inside a genuine
/// single column shows — inter-word spaces are only about 3pt and do not line
/// up down the page, so no wide strip is ever mostly empty in running prose.
const EMPTY_ROW_FRAC: f32 = 0.15;

/// Columns holding fewer words than this are noise — page-turn arrows in the
/// margin of the e-reader screenshot, speckles on a scan, stray marginalia.
const MIN_WORDS_PER_COLUMN: usize = 8;

/// The column layout of the document as a whole.
///
/// Column count is a property of the document, not of each page, and using
/// that fact repairs a failure the per-page detector cannot avoid on its own:
/// page 4 of `resnet-two-column.pdf` holds a table whose internal whitespace
/// reads as two extra gutters, so that page alone comes out with four columns
/// and its text interleaved. Capping each page at the document's own column
/// count fixes it without hard-coding anything.
#[derive(Debug, Clone, Default)]
pub struct ColumnPlan {
    /// The usual number of columns.
    pub count: usize,
    /// Gutter positions as a fraction of the page's body width, so they carry
    /// across pages that differ slightly in size.
    pub positions: Vec<f32>,
}

/// Work out the document-wide layout from each page's candidate gutters.
///
/// `per_page` is one entry per page: its candidate gutters, and the x-range its
/// body occupies.
pub fn plan(per_page: &[(Vec<f32>, f32, f32)]) -> ColumnPlan {
    use std::collections::HashMap;
    let mut votes: HashMap<usize, usize> = HashMap::new();
    for (splits, _, _) in per_page {
        *votes.entry(splits.len()).or_insert(0) += 1;
    }
    // Prefer the most common count; on a tie prefer the smaller one, which is
    // the more conservative reading.
    let count = votes
        .iter()
        .max_by_key(|(k, v)| (**v, std::cmp::Reverse(**k)))
        .map(|(k, _)| *k)
        .unwrap_or(0);

    let mut positions = Vec::new();
    if count > 0 {
        for i in 0..count {
            let mut vals: Vec<f32> = per_page
                .iter()
                .filter(|(s, _, _)| s.len() == count)
                .filter_map(|(s, x0, x1)| {
                    let w = x1 - x0;
                    if w > 0.0 {
                        Some((s[i] - x0) / w)
                    } else {
                        None
                    }
                })
                .collect();
            if vals.is_empty() {
                continue;
            }
            vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
            positions.push(vals[vals.len() / 2]);
        }
    }
    ColumnPlan { count, positions }
}

/// Reduce a page's candidate gutters to the document's column count, keeping
/// whichever candidates sit closest to the document's usual gutter positions.
pub fn reconcile(mut splits: Vec<f32>, x0: f32, x1: f32, plan: &ColumnPlan) -> Vec<f32> {
    if plan.count == 0 || splits.len() <= plan.count || plan.positions.is_empty() {
        return splits;
    }
    let w = x1 - x0;
    if w <= 0.0 {
        return splits;
    }
    let mut keep: Vec<f32> = Vec::new();
    for pos in &plan.positions {
        let target = x0 + pos * w;
        if let Some((idx, _)) = splits
            .iter()
            .enumerate()
            .filter(|(_, s)| !keep.contains(s))
            .min_by(|a, b| {
                (a.1 - target)
                    .abs()
                    .partial_cmp(&(b.1 - target).abs())
                    .unwrap()
            })
        {
            keep.push(splits[idx]);
        }
    }
    keep.sort_by(|a, b| a.partial_cmp(b).unwrap());
    splits = keep;
    splits
}

/// The x-range a page's words occupy.
pub fn body_extent(words: &[Word]) -> (f32, f32) {
    let x0 = words.iter().map(|w| w.bbox.x0).fold(f32::MAX, f32::min);
    let x1 = words.iter().map(|w| w.bbox.x1).fold(f32::MIN, f32::max);
    if words.is_empty() {
        (0.0, 0.0)
    } else {
        (x0, x1)
    }
}

/// Partition words into columns, left to right, using this page alone.
pub fn split(words: Vec<Word>) -> Vec<Vec<Word>> {
    let splits = candidate_splits(&words);
    split_at(words, &splits)
}

/// Assign words to columns given the gutter positions to use.
pub fn split_at(words: Vec<Word>, splits: &[f32]) -> Vec<Vec<Word>> {
    if splits.is_empty() {
        return vec![words];
    }
    let mut groups: Vec<Vec<Word>> = vec![Vec::new(); splits.len() + 1];
    for w in words {
        let cx = w.bbox.center_x();
        let idx = splits.iter().filter(|s| cx > **s).count();
        groups[idx].push(w);
    }
    // Drop noise columns rather than emitting them, so marginal junk does not
    // become its own block in the reading order.
    groups
        .into_iter()
        .filter(|g| g.len() >= MIN_WORDS_PER_COLUMN)
        .collect()
}

/// Find this page's candidate gutters, as absolute x positions.
pub fn candidate_splits(words: &[Word]) -> Vec<f32> {
    if words.len() < MIN_WORDS_PER_COLUMN * 2 {
        return Vec::new();
    }
    let x0 = words.iter().map(|w| w.bbox.x0).fold(f32::MAX, f32::min);
    let x1 = words.iter().map(|w| w.bbox.x1).fold(f32::MIN, f32::max);
    let y0 = words.iter().map(|w| w.bbox.y0).fold(f32::MAX, f32::min);
    let y1 = words.iter().map(|w| w.bbox.y1).fold(f32::MIN, f32::max);
    let body_w = x1 - x0;
    let body_h = y1 - y0;
    if body_w <= 0.0 || body_h <= 0.0 {
        return Vec::new();
    }
    let heights: Vec<f32> = words.iter().map(|w| w.bbox.height()).collect();
    let med_h = median(&heights).max(1.0);

    // Quantise into text rows. Row occupancy is what makes a figure crossing
    // the gutter cost only the handful of rows it actually spans.
    let row_h = med_h.max(1.0);
    let rows = (((body_h / row_h).ceil()) as usize).clamp(1, 4096);

    let bins = 300usize;
    let bin_w = body_w / bins as f32;
    let bin_of = |x: f32| (((x - x0) / bin_w) as isize).clamp(0, bins as isize - 1) as usize;

    // occupied[row][bin] collapsed to a per-bin count of distinct rows.
    let mut row_hits: Vec<Vec<bool>> = vec![vec![false; bins]; rows];
    for w in words {
        let r = (((w.bbox.y0 + w.bbox.y1) / 2.0 - y0) / row_h) as isize;
        let r = r.clamp(0, rows as isize - 1) as usize;
        let a = bin_of(w.bbox.x0);
        let b = bin_of(w.bbox.x1);
        for hit in row_hits[r][a..=b].iter_mut() {
            *hit = true;
        }
    }
    let mut occupied_rows = vec![0usize; bins];
    for r in &row_hits {
        for (b, hit) in r.iter().enumerate() {
            if *hit {
                occupied_rows[b] += 1;
            }
        }
    }
    let non_empty_rows = row_hits.iter().filter(|r| r.iter().any(|h| *h)).count().max(1);
    let empty_limit = (non_empty_rows as f32 * EMPTY_ROW_FRAC).floor() as usize;

    // Runs of empty bins.
    let min_gap_pt = (med_h * MIN_GAP_FONT_MULTIPLE).max(body_w * MIN_GAP_FRAC);
    let min_gap_bins = ((min_gap_pt / bin_w).ceil() as usize).max(2);
    let edge = ((bins as f32 * 0.08) as usize).max(1);

    let mut splits: Vec<f32> = Vec::new();
    let mut run_start: Option<usize> = None;
    for b in 0..=bins {
        let empty = b < bins && occupied_rows[b] <= empty_limit;
        match (empty, run_start) {
            (true, None) => run_start = Some(b),
            (false, Some(st)) => {
                let width = b - st;
                // Ignore runs that touch the outer margins: those are margins.
                if width >= min_gap_bins && st > edge && b < bins - edge {
                    splits.push(x0 + (st + b) as f32 / 2.0 * bin_w);
                }
                run_start = None;
            }
            _ => {}
        }
    }

    splits.sort_by(|a, b| a.partial_cmp(b).unwrap());
    splits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Rect;

    fn word(x0: f32, y0: f32) -> Word {
        // 48 wide on a 50 pitch, i.e. a 2pt inter-word space, which is what
        // real running text looks like.
        Word {
            text: "xx".to_string(),
            bbox: Rect::new(x0, y0, x0 + 48.0, y0 + 10.0),
            font_size: 10.0,
            conf: None,
            bold: false,
        }
    }

    #[test]
    fn the_document_plan_caps_an_over_split_page() {
        // Most pages find one gutter at the middle; one page with a table finds
        // three. The plan must cut that page back to the usual single gutter,
        // keeping the one nearest the consensus position.
        let per_page = vec![
            (vec![300.0], 50.0, 545.0),
            (vec![301.0], 50.0, 545.0),
            (vec![299.0], 50.0, 545.0),
            (vec![120.0, 190.0, 290.0], 50.0, 545.0),
        ];
        let p = plan(&per_page);
        assert_eq!(p.count, 1);
        let kept = reconcile(vec![120.0, 190.0, 290.0], 50.0, 545.0, &p);
        assert_eq!(kept, vec![290.0], "keeps the gutter nearest the consensus");
    }

    #[test]
    fn the_plan_leaves_a_page_that_found_fewer_gutters_alone() {
        // A page holding a full-width table genuinely has no gutter; forcing
        // the document's split onto it would cut its rows in half.
        let per_page = vec![
            (vec![300.0], 50.0, 545.0),
            (vec![300.0], 50.0, 545.0),
            (vec![], 50.0, 545.0),
        ];
        let p = plan(&per_page);
        assert_eq!(p.count, 1);
        assert!(reconcile(vec![], 50.0, 545.0, &p).is_empty());
    }

    #[test]
    fn a_single_column_document_plans_zero_gutters() {
        let per_page = vec![(vec![], 30.0, 470.0); 6];
        assert_eq!(plan(&per_page).count, 0);
    }

    #[test]
    fn single_column_stays_one_group() {
        let words: Vec<Word> = (0..40).map(|i| word(30.0, 40.0 + i as f32 * 14.0)).collect();
        assert_eq!(split(words).len(), 1);
    }

    #[test]
    fn two_columns_are_separated() {
        // Left column at x=30..78, right at x=200..248. The 122pt separator is
        // far wider than both the 4.5% relative bar and the absolute one.
        let mut words = Vec::new();
        for i in 0..30 {
            words.push(word(30.0, 40.0 + i as f32 * 14.0));
            words.push(word(200.0, 40.0 + i as f32 * 14.0));
        }
        let groups = split(words);
        assert_eq!(groups.len(), 2, "expected two columns");
        assert!(groups[0].iter().all(|w| w.bbox.x0 < 100.0));
        assert!(groups[1].iter().all(|w| w.bbox.x0 > 100.0));
    }

    #[test]
    fn marginal_noise_is_dropped_not_promoted() {
        // A two-column body plus two stray glyphs in the far margins, like the
        // page-turn arrows in the e-reader screenshot.
        let mut words = Vec::new();
        for i in 0..30 {
            words.push(word(60.0, 40.0 + i as f32 * 14.0));
            words.push(word(220.0, 40.0 + i as f32 * 14.0));
        }
        words.push(word(2.0, 200.0));
        words.push(word(400.0, 200.0));
        let groups = split(words);
        // The two stray words must not become columns of their own.
        assert!(groups.len() <= 2, "got {} groups", groups.len());
        assert!(groups.iter().all(|g| g.len() >= MIN_WORDS_PER_COLUMN));
    }

    #[test]
    fn a_narrow_cvpr_style_gutter_is_found() {
        // resnet-two-column.pdf: a 495pt body with a ~17pt gutter, i.e. under
        // 4% of the width. Sized against the 10pt glyphs it is 1.7x, which is
        // what makes it recognisable.
        let mut words = Vec::new();
        for i in 0..45 {
            let y = 75.0 + i as f32 * 14.0;
            // Left column ends at 293 (245 + 48), right column starts at 313:
            // a 20pt gutter, 4% of the 495pt body and 2x the 10pt glyphs.
            for x in [50.0, 100.0, 150.0, 200.0, 245.0] {
                words.push(word(x, y));
            }
            for x in [313.0, 363.0, 413.0, 463.0, 497.0] {
                words.push(word(x, y));
            }
        }
        let groups = split(words);
        assert_eq!(groups.len(), 2, "a 17pt gutter must still split");
        assert!(groups[0].iter().all(|w| w.bbox.x1 <= 293.0));
        assert!(groups[1].iter().all(|w| w.bbox.x0 >= 313.0));
    }

    #[test]
    fn a_figure_crossing_the_gutter_does_not_hide_it() {
        // A few rows span both columns, as a wide figure or table does. Row
        // occupancy makes that cost only those rows.
        let mut words = Vec::new();
        for i in 0..45 {
            let y = 75.0 + i as f32 * 14.0;
            if (10..16).contains(&i) {
                // full-width band
                for x in [50.0, 130.0, 210.0, 290.0, 370.0, 450.0] {
                    words.push(word(x, y));
                }
                continue;
            }
            for x in [50.0, 100.0, 150.0, 200.0, 245.0] {
                words.push(word(x, y));
            }
            for x in [313.0, 363.0, 413.0, 463.0, 497.0] {
                words.push(word(x, y));
            }
        }
        assert_eq!(split(words).len(), 2, "6 crossing rows of 45 must not veto the gutter");
    }

    #[test]
    fn a_strip_holding_one_line_of_text_is_not_a_gutter() {
        // The bug in the previous implementation: measuring occupancy by
        // accumulated glyph height made a strip with a single line look empty.
        let mut words = Vec::new();
        for i in 0..40 {
            let y = 75.0 + i as f32 * 14.0;
            for x in [50.0, 100.0, 150.0, 200.0] {
                words.push(word(x, y));
            }
        }
        // One lone line reaching across what would otherwise be empty space.
        for x in [260.0, 310.0, 360.0] {
            words.push(word(x, 300.0));
        }
        let groups = split(words);
        assert!(groups.len() <= 2, "got {} groups", groups.len());
    }

    #[test]
    fn a_centred_heading_does_not_create_a_column_split() {
        // Full-width body lines plus one short centred heading. The whitespace
        // beside the heading is only a few lines tall, so it must be ignored.
        let mut words = Vec::new();
        words.push(word(150.0, 20.0)); // centred heading
        for i in 0..30 {
            for x in [30.0, 80.0, 130.0, 180.0, 230.0] {
                words.push(word(x, 60.0 + i as f32 * 14.0));
            }
        }
        assert_eq!(split(words).len(), 1, "a heading's side whitespace is not a column gap");
    }
}
