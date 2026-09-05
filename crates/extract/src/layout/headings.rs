//! Heading and chapter detection.
//!
//! Font size is deliberately the *weakest* signal here. ClearScan rescales
//! synthesised glyphs per line, so on page 300 of the Turkish book the chapter
//! heading reports 10.80pt while the body reports 13.15pt — the heading is
//! smaller than the text it introduces. Any size-first heuristic gets that page
//! wrong, so centring and vertical isolation lead instead.

use crate::model::Line;
use super::paragraphs::ColumnStats;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadingKind {
    /// Opens a chapter: gets its own XHTML file in the EPUB.
    Chapter,
    /// A subtitle directly under a chapter opener, e.g. the POV name `Carla`.
    Subtitle,
}

/// Does this look like a chapter number or title on its own line?
pub fn looks_like_chapter_label(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() || t.chars().count() > 60 {
        return false;
    }
    // Chapter markers are commonly written with trailing punctuation - `I.`,
    // `44.`, `II —` - so judge the label without it.
    let core = t.trim_end_matches(|c: char| {
        matches!(c, '.' | ')' | ':' | '-' | '\u{2014}' | '\u{2013}' | ' ')
    });
    if core.is_empty() {
        return false;
    }
    // A bare number: `44`.
    if core.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    // A roman numeral: `XIV`, `I.`
    if core.chars().count() <= 8 && core.chars().all(|c| matches!(c, 'I' | 'V' | 'X' | 'L' | 'C')) {
        return true;
    }
    let lower = t.to_lowercase();
    const WORDS: &[&str] = &[
        "chapter", "bölüm", "bolum", "hoofdstuk", "kapitel", "chapitre",
        "capitolo", "capitulo", "capítulo", "kapittel", "rozdział", "part",
        "book", "kısım", "kisim", "prologue", "epilogue", "önsöz", "onsoz",
        "prolog", "epilog", "voorwoord", "proloog", "epiloog",
    ];
    WORDS.iter().any(|w| lower.starts_with(w))
}

/// Classify a line as a heading, given its column's statistics and the gaps
/// around it.
///
/// `gap_before`/`gap_after` are in points; `None` means the line is at the top
/// or bottom of the column.
pub fn classify(
    line: &Line,
    stats: &ColumnStats,
    column_center: f32,
    gap_before: Option<f32>,
    gap_after: Option<f32>,
) -> Option<HeadingKind> {
    let text = line.text();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let measure = (stats.right_edge - stats.margin).max(1.0);
    // Headings do not fill the measure. The bar is generous because a chapter
    // title can be long ("EEN VRESELIJKE VERJAARDAG" fills three quarters of
    // its column); centring does the real discriminating.
    let short = line.bbox.width() < measure * 0.85;
    if !short {
        return None;
    }

    // Centring is judged by *symmetric inset*, not by distance from the column
    // centre. A justified body line that happens to fill the measure also sits
    // on the column centre, so comparing centres alone would call it centred.
    // Requiring both margins to be genuinely non-zero excludes it.
    let left_gap = line.bbox.x0 - stats.margin;
    let right_gap = stats.right_edge - line.bbox.x1;
    let symmetric = (left_gap - right_gap).abs() < measure * 0.12;
    let inset = left_gap > stats.font_size * 1.5 && right_gap > stats.font_size * 1.5;
    let centred = symmetric && inset;
    let _ = column_center;

    // A heading is set off vertically. Either side is enough: a chapter number
    // and the subtitle under it sit close together, so the subtitle has no gap
    // above it — page 300 of the Turkish book has 24pt against a 22pt leading.
    let big_before = gap_before.map(|g| g > stats.leading * 1.4).unwrap_or(true);
    let big_after = gap_after.map(|g| g > stats.leading * 1.25).unwrap_or(true);
    let isolated = big_before || big_after;

    // A left-aligned heading, the usual form in non-fiction and papers. Bold is
    // the only thing that separates it from body text: every section heading in
    // the Transformer paper is 9.96pt bold against 9.96pt roman body, so no
    // size or position test can find them.
    //
    // Requires isolation on both sides, which is what keeps a bold lead-in
    // phrase at the start of a paragraph ("Encoder: The encoder is composed
    // of...") from being promoted.
    let flush_left = (line.bbox.x0 - stats.margin).abs() < stats.font_size * 0.6;
    let big_before_strict = gap_before.map(|g| g > stats.leading * 1.4).unwrap_or(true);
    let big_after_strict = gap_after.map(|g| g > stats.leading * 1.25).unwrap_or(true);
    if line.bold && flush_left && big_before_strict && big_after_strict {
        let words = trimmed.split_whitespace().count();
        if words <= 14 {
            return Some(if looks_like_chapter_label(trimmed) {
                HeadingKind::Chapter
            } else {
                HeadingKind::Subtitle
            });
        }
    }

    let labelled = looks_like_chapter_label(trimmed);
    // Size is a tiebreaker only, and it is allowed to be *smaller* than the
    // body, which is why this compares against a generous lower bound.
    let size_ok = line.font_size >= stats.font_size * 0.75;

    if !size_ok {
        return None;
    }

    // A recognised chapter word or number that is centred and isolated is a
    // chapter opener even if it is tiny.
    if labelled && centred && big_before {
        return Some(HeadingKind::Chapter);
    }
    // Otherwise require the full geometric case.
    if centred && isolated {
        if labelled {
            return Some(HeadingKind::Chapter);
        }
        // Short, centred, isolated and only a few words: a title.
        //
        // A trailing period is not disqualifying. The letter headings of
        // `lady-susan.pdf` read "Lady Susan Vernon to Mr. Vernon." and are
        // genuine subtitles; what rules out body text is that a paragraph's
        // final line is left-aligned, not centred, so `centred` has already
        // done that work.
        let words = trimmed.split_whitespace().count();
        if words <= 12 && trimmed.chars().count() <= 70 {
            return Some(HeadingKind::Subtitle);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Rect;
    use crate::model::Word;

    fn line(text: &str, x0: f32, x1: f32, y: f32, size: f32) -> Line {
        styled(text, x0, x1, y, size, false)
    }

    fn styled(text: &str, x0: f32, x1: f32, y: f32, size: f32, bold: bool) -> Line {
        Line::from_words(vec![Word {
            text: text.to_string(),
            bbox: Rect::new(x0, y, x1, y + size),
            font_size: size,
            conf: None,
            bold,
        }])
        .unwrap()
    }

    fn stats() -> ColumnStats {
        // Page 300 of the Turkish book: margin 28.5, measure to ~456, body 13.15.
        ColumnStats {
            margin: 28.5,
            right_edge: 456.0,
            font_size: 13.15,
            leading: 22.0,
        }
    }

    #[test]
    fn chapter_number_smaller_than_body_is_still_a_heading() {
        // The exact case that defeats a font-size-first rule: "44" at 10.80pt
        // against 13.15pt body text.
        let l = line("44", 228.0, 246.8, 718.0, 10.80);
        let kind = classify(&l, &stats(), 242.0, Some(43.0), Some(24.0));
        assert_eq!(kind, Some(HeadingKind::Chapter), "size must not veto");
    }

    #[test]
    fn pov_name_under_a_chapter_is_a_subtitle() {
        let l = line("Carla", 217.2, 258.0, 694.0, 12.45);
        let kind = classify(&l, &stats(), 242.0, Some(24.0), Some(48.0));
        assert_eq!(kind, Some(HeadingKind::Subtitle));
    }

    #[test]
    fn a_normal_body_line_is_not_a_heading() {
        let l = line(
            "Tabii ki yeni resmin tanitimi onlari bir araya getirmekte etkili ol",
            28.5,
            451.0,
            645.0,
            13.15,
        );
        assert_eq!(classify(&l, &stats(), 242.0, Some(22.0), Some(22.0)), None);
    }

    #[test]
    fn a_short_but_left_aligned_line_is_not_a_heading() {
        // The last line of a paragraph is short, but it is not centred.
        let l = line("hakim olmaya calisan birine benziyor.", 28.5, 274.0, 584.0, 13.15);
        assert_eq!(classify(&l, &stats(), 242.0, Some(22.0), Some(22.0)), None);
    }

    #[test]
    fn a_bold_left_aligned_section_heading_is_found() {
        // Transformer paper page 3: "3.1 Encoder and Decoder Stacks", bold,
        // flush left, same 9.96pt size as the body around it.
        let s = ColumnStats { margin: 108.0, right_edge: 506.0, font_size: 9.96, leading: 11.5 };
        let l = styled("3.1 Encoder and Decoder Stacks", 108.0, 253.0, 301.0, 9.96, true);
        assert_eq!(classify(&l, &s, 307.0, Some(22.0), Some(20.0)), Some(HeadingKind::Subtitle));
    }

    #[test]
    fn a_bold_lead_in_inside_a_paragraph_is_not_a_heading() {
        // "Encoder: The encoder is composed of a stack of..." begins with bold
        // text but runs the full measure and has normal leading around it.
        let s = ColumnStats { margin: 108.0, right_edge: 506.0, font_size: 9.96, leading: 11.5 };
        let l = styled("Encoder: The encoder is composed of a stack", 108.0, 506.0, 320.0, 9.96, true);
        assert_eq!(classify(&l, &s, 307.0, Some(11.5), Some(11.5)), None);
    }

    #[test]
    fn a_non_bold_left_aligned_short_line_is_not_a_heading() {
        // The last line of a paragraph: short, flush left, but not bold.
        let s = ColumnStats { margin: 108.0, right_edge: 506.0, font_size: 9.96, leading: 11.5 };
        let l = line("respectively.", 108.0, 160.0, 320.0, 9.96);
        assert_eq!(classify(&l, &s, 307.0, Some(11.5), Some(22.0)), None);
    }

    #[test]
    fn recognises_chapter_words_across_languages() {
        assert!(looks_like_chapter_label("44"));
        assert!(looks_like_chapter_label("XIV"));
        // The epistolary chapter markers of lady-susan.pdf.
        assert!(looks_like_chapter_label("I."));
        assert!(looks_like_chapter_label("XVIII."));
        assert!(looks_like_chapter_label("12."));
        assert!(looks_like_chapter_label("HOOFDSTUK 1"));
        assert!(looks_like_chapter_label("BÖLÜM 12"));
        assert!(looks_like_chapter_label("Chapter One"));
        assert!(!looks_like_chapter_label("Bakislarinin sertlestigini fark ediyorum"));
        assert!(!looks_like_chapter_label(""));
    }

    #[test]
    fn dutch_chapter_heading_pair_is_detected() {
        // bookSample.pdf: "HOOFDSTUK 1" then "EEN VRESELIJKE VERJAARDAG".
        let s = ColumnStats { margin: 100.0, right_edge: 590.0, font_size: 14.0, leading: 24.0 };
        let a = line("HOOFDSTUK 1", 285.0, 400.0, 220.0, 13.0);
        assert_eq!(classify(&a, &s, 343.0, None, Some(40.0)), Some(HeadingKind::Chapter));
        let b = line("EEN VRESELIJKE VERJAARDAG", 168.0, 520.0, 270.0, 18.0);
        assert_eq!(classify(&b, &s, 343.0, Some(40.0), Some(100.0)), Some(HeadingKind::Subtitle));
    }
}
