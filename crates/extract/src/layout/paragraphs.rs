//! Grouping lines into paragraphs, and rejoining hyphenated words.
//!
//! Three independent signals, because no single one covers the corpus:
//!
//! * **First-line indent.** Strong and clean, but the threshold has to be
//!   computed per page: page 40 of the Turkish book has margin 31 / indent 54,
//!   page 300 has margin 28.5 / indent 47.
//! * **Previous line ended short.** The only signal that works on
//!   `lady-susan.pdf`, whose paragraphs are block-style with no indent at all.
//! * **Vertical gap.** Catches spaced-out paragraphs and scene breaks.

use crate::geom::{median, mode_within};
use crate::model::{HyphenKind, Line};

/// A paragraph, still tied to the page it came from so page markers can be
/// placed later.
#[derive(Debug, Clone)]
pub struct RawParagraph {
    pub text: String,
    pub lines: Vec<Line>,
    /// Page index each contributing line came from.
    pub page: usize,
    /// True when the first line was indented, i.e. this is definitely a
    /// paragraph *start* and not a continuation. Used for cross-page joining.
    pub starts_indented: bool,
    /// Mean OCR confidence, when the page was OCR'd.
    pub confidence: Option<f32>,
    /// Ambiguous hyphen joins made inside this paragraph, for the report.
    pub ambiguous_joins: Vec<String>,
    /// The last line carried an explicit continuation marker, so a paragraph
    /// that follows on the next page joins to it without a space.
    pub ends_hyphenated: bool,
}

impl RawParagraph {
    pub fn ends_with_sentence_end(&self) -> bool {
        ends_sentence(&self.text)
    }
}

pub fn ends_sentence(s: &str) -> bool {
    let t = s.trim_end();
    // Walk back past closing quotes and brackets: `anladılar."` ends a sentence.
    let mut chars = t.chars().rev();
    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' | '»' | '”' | '’' | ')' | ']' => continue,
            '.' | '!' | '?' | ':' | ';' | '…' => return true,
            _ => return false,
        }
    }
    false
}

/// Statistics a page's own geometry provides. Recomputed for every page.
#[derive(Debug, Clone, Copy)]
pub struct ColumnStats {
    pub margin: f32,
    pub right_edge: f32,
    pub font_size: f32,
    pub leading: f32,
}

pub fn column_stats(lines: &[Line]) -> ColumnStats {
    let sizes: Vec<f32> = lines.iter().map(|l| l.font_size).collect();
    let font_size = median(&sizes).max(1.0);
    let lefts: Vec<f32> = lines.iter().map(|l| l.bbox.x0).collect();
    // Cluster tolerance scales with the text so it works at any point size.
    let margin = mode_within(&lefts, font_size * 0.35);
    // The right edge is the *typical* line end, not the maximum, so one
    // over-long line cannot make every other line look short.
    let rights: Vec<f32> = lines.iter().map(|l| l.bbox.x1).collect();
    let right_edge = percentile(&rights, 0.90);
    ColumnStats {
        margin,
        right_edge,
        font_size,
        leading: super::lines::median_leading(lines),
    }
}

fn percentile(vals: &[f32], p: f32) -> f32 {
    if vals.is_empty() {
        return 0.0;
    }
    let mut v = vals.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let idx = ((v.len() - 1) as f32 * p).round() as usize;
    v[idx]
}

/// Is this line set symmetrically inside the measure, i.e. centred?
fn is_centred(line: &Line, stats: &ColumnStats) -> bool {
    let measure = (stats.right_edge - stats.margin).max(1.0);
    let left_gap = line.bbox.x0 - stats.margin;
    let right_gap = stats.right_edge - line.bbox.x1;
    (left_gap - right_gap).abs() < measure * 0.12
        && left_gap > stats.font_size * 1.5
        && right_gap > stats.font_size * 1.5
}

/// Is this line pushed against the right margin, like a dateline or a signature?
fn is_right_aligned(line: &Line, stats: &ColumnStats) -> bool {
    let measure = (stats.right_edge - stats.margin).max(1.0);
    let left_gap = line.bbox.x0 - stats.margin;
    let right_gap = stats.right_edge - line.bbox.x1;
    right_gap.abs() < stats.font_size * 1.5 && left_gap > measure * 0.3
}

fn reaches_measure(line: &Line, stats: &ColumnStats) -> bool {
    line.bbox.x1 >= stats.right_edge - stats.font_size * 2.0
}

/// Group one column's lines into paragraphs.
pub fn group(lines: &[Line], page: usize, stats: ColumnStats) -> Vec<RawParagraph> {
    let mut out: Vec<RawParagraph> = Vec::new();
    let indent_threshold = stats.margin + stats.font_size * 0.55;

    let mut cur: Vec<Line> = Vec::new();
    let mut cur_indented = false;

    for (i, line) in lines.iter().enumerate() {
        let mut starts_new = cur.is_empty();
        let indented = line.bbox.x0 > indent_threshold;

        if !cur.is_empty() {
            let prev = &lines[i - 1];
            // (1) this line is indented.
            //
            // Except when it is the centred final line of the paragraph that is
            // still running. A centred last line is inset from the left exactly
            // like an indent, so the indent test alone would split it off — it
            // is what turned "nephew, James Austen-Leigh." into a heading of
            // its own. Requiring the previous line to have stopped short before
            // trusting the indent resolves it.
            if indented && !(is_centred(line, &stats) && reaches_measure(prev, &stats)) {
                starts_new = true;
            }
            // (2) the previous line stopped well short of the measure
            if !reaches_measure(prev, &stats) && prev.hyphen_kind().is_none() {
                starts_new = true;
            }
            // (3) an unusually large vertical gap
            let gap = line.bbox.y0 - prev.bbox.y0;
            if gap > stats.leading * 1.55 {
                starts_new = true;
            }
            // (4) either side of a right-aligned line. A dateline or a
            // signature is its own block, not the opening of the paragraph
            // below it.
            if is_right_aligned(line, &stats) || is_right_aligned(prev, &stats) {
                starts_new = true;
            }
        }

        if starts_new && !cur.is_empty() {
            out.push(finish(std::mem::take(&mut cur), page, cur_indented));
        }
        if cur.is_empty() {
            cur_indented = indented;
        }
        cur.push(line.clone());
    }
    if !cur.is_empty() {
        out.push(finish(cur, page, cur_indented));
    }
    out
}

fn finish(lines: Vec<Line>, page: usize, starts_indented: bool) -> RawParagraph {
    let (text, ambiguous_joins) = join_lines(&lines);
    let confs: Vec<f32> = lines.iter().filter_map(|l| l.confidence()).collect();
    let confidence = if confs.is_empty() {
        None
    } else {
        Some(confs.iter().sum::<f32>() / confs.len() as f32)
    };
    let ends_hyphenated = lines
        .last()
        .and_then(|l| l.hyphen_kind())
        .map(|k| k == HyphenKind::Explicit)
        .unwrap_or(false);
    RawParagraph {
        text,
        lines,
        page,
        starts_indented,
        confidence,
        ambiguous_joins,
        ends_hyphenated,
    }
}

/// Concatenate a paragraph's lines, resolving end-of-line hyphenation.
pub fn join_lines(lines: &[Line]) -> (String, Vec<String>) {
    let mut out = String::new();
    let mut ambiguous = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let mut text = l.text();
        let kind = l.hyphen_kind();
        let is_last = i + 1 == lines.len();

        let join_without_space = match kind {
            // Unambiguous continuation marker: always join, drop the marker.
            Some(HyphenKind::Explicit) if !is_last => {
                text.pop();
                true
            }
            // A literal hyphen might be a compound word. Only treat it as a
            // line break when the next line starts lowercase, which is what
            // `num-`/`mer` looks like and what `tik-`/`Taklar` would not.
            Some(HyphenKind::Literal) if !is_last => {
                let next_starts_lower = lines[i + 1]
                    .text()
                    .chars()
                    .find(|c| c.is_alphabetic())
                    .map(|c| c.is_lowercase())
                    .unwrap_or(false);
                if next_starts_lower {
                    text.pop();
                    ambiguous.push(format!(
                        "{}|{}",
                        text.split_whitespace().last().unwrap_or(""),
                        lines[i + 1].text().split_whitespace().next().unwrap_or("")
                    ));
                    true
                } else {
                    false
                }
            }
            Some(HyphenKind::Explicit) => {
                // Marker on the very last line has nothing to join to.
                text.pop();
                false
            }
            _ => false,
        };

        out.push_str(text.trim_end());
        if !is_last && !join_without_space {
            out.push(' ');
        }
    }
    (normalise_spaces(&out), ambiguous)
}

/// Collapse runs of whitespace and tidy the space-before-punctuation that
/// scanned text is full of (`Ed' in kolu`, `güldür üyor`).
pub fn normalise_spaces(s: &str) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::with_capacity(collapsed.len());
    let chars: Vec<char> = collapsed.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // Drop a space that sits directly before closing punctuation.
        if c == ' ' {
            if let Some(&next) = chars.get(i + 1) {
                if matches!(next, ',' | '.' | ';' | ':' | '!' | '?' | '’') {
                    i += 1;
                    continue;
                }
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Rect;
    use crate::model::Word;

    fn line_at(text: &str, x0: f32, x1: f32, y: f32) -> Line {
        // Split on spaces so hyphen detection sees the real last word.
        let mut words = Vec::new();
        let parts: Vec<&str> = text.split(' ').collect();
        let span = (x1 - x0).max(1.0) / parts.len() as f32;
        for (i, p) in parts.iter().enumerate() {
            let wx0 = x0 + i as f32 * span;
            words.push(Word {
                text: p.to_string(),
                bbox: Rect::new(wx0, y, wx0 + span * 0.9, y + 12.0),
                font_size: 12.0,
                conf: None,
                bold: false,
            });
        }
        Line::from_words(words).unwrap()
    }

    fn stats(margin: f32, right: f32) -> ColumnStats {
        ColumnStats {
            margin,
            right_edge: right,
            font_size: 12.0,
            leading: 22.0,
        }
    }

    #[test]
    fn indent_starts_a_new_paragraph() {
        // Page 40 geometry: margin 31, indent 54, measure ends near 468.
        let lines = vec![
            line_at("Son bolumu sakin ifadesiz bir ses", 53.8, 468.0, 100.0),
            line_at("degil aina anlattiklarina yabancilasmis", 32.0, 468.0, 122.0),
            line_at("hakim olmaya calisan birine benziyor.", 31.9, 274.0, 144.0),
            line_at("Polisler eve geldiklerinde senin fazla", 54.0, 465.0, 166.0),
        ];
        // Lines 1-3 are one paragraph: the first is indented, the next two are
        // flush continuations, and the third ends short so the paragraph closes.
        // Line 4 is indented, so it opens the second paragraph.
        let ps = group(&lines, 40, stats(31.0, 468.0));
        assert_eq!(ps.len(), 2, "{:?}", ps.iter().map(|p| &p.text).collect::<Vec<_>>());
        assert!(ps[0].text.starts_with("Son bolumu"));
        assert!(ps[0].text.contains("degil aina"), "flush line must continue para 1");
        assert!(ps[0].text.ends_with("benziyor."));
        assert!(ps[1].text.starts_with("Polisler"));
    }

    #[test]
    fn block_paragraphs_split_on_a_short_previous_line() {
        // lady-susan.pdf style: no indents at all, every line at the margin.
        // The only signal is that a paragraph's last line stops early.
        let lines = vec![
            line_at("house I like this man pray Heaven no", 72.0, 399.0, 100.0),
            line_at("determined to be discreet to bear in", 72.0, 399.0, 114.0),
            line_at("warded for my exertions as I ought.", 72.0, 242.0, 128.0),
            line_at("Sir James did make proposals to me", 72.0, 399.0, 142.0),
        ];
        let ps = group(&lines, 3, stats(72.0, 399.0));
        assert_eq!(ps.len(), 2, "{:?}", ps.iter().map(|p| &p.text).collect::<Vec<_>>());
        assert!(ps[1].text.starts_with("Sir James"));
    }

    #[test]
    fn explicit_hyphen_marker_joins_without_a_space() {
        // U+0002 is what pdfium reports for the Turkish book.
        let lines = vec![
            line_at("bile bilmiyordum. Bu\u{0002}", 31.0, 464.0, 100.0),
            line_at("nun uzerine 999'u aradim", 31.0, 205.0, 122.0),
        ];
        let (text, _) = join_lines(&lines);
        assert!(text.contains("Bunun uzerine"), "got {text:?}");
        assert!(!text.contains('\u{0002}'));
    }

    #[test]
    fn soft_hyphen_and_not_sign_are_also_markers() {
        for marker in ['\u{00ad}', '\u{00ac}'] {
            let lines = vec![
                line_at(&format!("takemyad{marker}"), 30.0, 200.0, 100.0),
                line_at("vice", 30.0, 100.0, 122.0),
            ];
            let (text, _) = join_lines(&lines);
            assert_eq!(text, "takemyadvice", "marker {marker:?} -> {text:?}");
        }
    }

    #[test]
    fn literal_hyphen_joins_when_the_next_line_is_lowercase() {
        let lines = vec![
            line_at("de Ligusterlaan num-", 72.0, 399.0, 100.0),
            line_at("mer 4 om te snijden.", 72.0, 300.0, 114.0),
        ];
        let (text, amb) = join_lines(&lines);
        assert!(text.contains("nummer 4"), "got {text:?}");
        assert_eq!(amb.len(), 1, "the join should be flagged as ambiguous");
    }

    #[test]
    fn literal_hyphen_is_kept_when_the_next_line_is_capitalised() {
        // A compound that happens to fall at a line end must not be welded.
        let lines = vec![
            line_at("the Anglo-", 72.0, 399.0, 100.0),
            line_at("Saxon period", 72.0, 300.0, 114.0),
        ];
        let (text, _) = join_lines(&lines);
        assert!(text.contains("Anglo- Saxon") || text.contains("Anglo-Saxon"), "got {text:?}");
        assert!(text.contains("Saxon"));
    }

    #[test]
    fn sentence_end_sees_through_closing_quotes() {
        assert!(ends_sentence("Durumu yanlis anladilar.\""));
        assert!(ends_sentence("Aslinda hakli."));
        assert!(ends_sentence("kim iddia edebilir?\""));
        assert!(!ends_sentence("Sessizligi bozmaya etor"));
        assert!(!ends_sentence("Kafasi karisik"));
    }

    #[test]
    fn spaces_before_punctuation_are_tidied() {
        assert_eq!(normalise_spaces("Ed' in kolu"), "Ed' in kolu");
        assert_eq!(normalise_spaces("benziyor ."), "benziyor.");
        assert_eq!(normalise_spaces("a   b\n c"), "a b c");
    }

    #[test]
    fn a_centred_final_line_stays_in_its_paragraph() {
        // lady-susan.pdf page 1: a three-line paragraph whose last line is
        // centred. It must not become a paragraph, and so must not become a
        // heading either.
        let lines = vec![
            line_at("Lady Susan was probably written in the late", 80.66, 391.90, 100.0),
            line_at("never submitted it for publication It was first", 84.39, 387.47, 114.0),
            line_at("nephew, James Austen-Leigh.", 174.87, 294.79, 128.0),
        ];
        let s = ColumnStats { margin: 80.66, right_edge: 391.90, font_size: 9.96, leading: 13.0 };
        let ps = group(&lines, 1, s);
        assert_eq!(ps.len(), 1, "{:?}", ps.iter().map(|p| &p.text).collect::<Vec<_>>());
        assert!(ps[0].text.ends_with("Austen-Leigh."));
    }

    #[test]
    fn a_right_aligned_dateline_is_its_own_block() {
        // lady-susan.pdf page 2: "Langford, Dec." is set to the right margin
        // and must not merge into the letter body under it.
        let lines = vec![
            line_at("Langford, Dec.", 324.05, 386.50, 100.0),
            line_at("My dear Brother I can no longer refuse myself", 72.02, 398.85, 114.0),
            line_at("profiting by your kind invitation when we last", 72.02, 398.87, 128.0),
        ];
        let s = ColumnStats { margin: 72.02, right_edge: 398.87, font_size: 12.0, leading: 13.7 };
        let ps = group(&lines, 2, s);
        assert_eq!(ps.len(), 2, "{:?}", ps.iter().map(|p| &p.text).collect::<Vec<_>>());
        assert_eq!(ps[0].text, "Langford, Dec.");
        assert!(ps[1].text.starts_with("My dear Brother"));
    }

    #[test]
    fn column_stats_pick_the_margin_not_the_leftmost_line() {
        let lines = vec![
            line_at("aaa", 54.0, 468.0, 100.0),
            line_at("bbb", 31.0, 468.0, 122.0),
            line_at("ccc", 31.2, 468.0, 144.0),
            line_at("ddd", 30.9, 468.0, 166.0),
            line_at("eee", 28.0, 468.0, 188.0),
        ];
        let s = column_stats(&lines);
        assert!((s.margin - 31.0).abs() < 1.5, "margin {}", s.margin);
    }
}
