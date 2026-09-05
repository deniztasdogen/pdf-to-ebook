//! Removing running headers, footers, printed page numbers and UI chrome.
//!
//! Two mechanisms, because the corpus needs both:
//!
//! * **Cross-page repetition** for real books. `JANE CORRY ll KOCAMIN KARISI`
//!   sits at the top of essentially every page of the Turkish book, so it shows
//!   up as a repeated signature. Body text never repeats, so this cannot eat
//!   content.
//! * **Geometric isolation** for single-page inputs, where there are no other
//!   pages to compare against. The Dutch e-reader screenshot has a UI bar and a
//!   progress bar that only one page of evidence can identify.
//!
//! Nothing is dropped without a positive reason, and everything dropped is
//! recorded so the run report can show it.

use crate::model::{DropReason, DroppedLine, Line};

/// Fraction of page height at the top and bottom searched for chrome.
const BAND_FRAC: f32 = 0.10;

/// A signature must appear on at least this share of pages to count as running
/// furniture.
const REPEAT_FRAC: f32 = 0.25;

/// ...but never on fewer than this many pages, so a 3-page document does not
/// declare its only heading a running header.
const MIN_REPEAT_PAGES: usize = 4;

/// Reduce a line to a form that survives OCR noise and changing page numbers.
///
/// Digits and punctuation go, and so does *all* whitespace. Dropping the
/// spaces matters: the running header of the Turkish book is extracted as
/// `JANE CORRY ll KOCAMIN KARISI` on some pages and `JANE COR RY ll KOCAMIN
/// KARISI` on others, because ClearScan's synthetic fonts put the word break
/// in a different place. Ignoring word boundaries makes those identical.
pub fn signature(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_alphabetic())
        .collect()
}

/// How alike two signatures must be to count as the same furniture.
///
/// Exact equality is not enough. The same header is read as `ll` on one page
/// and `//` on another, and the second form loses those characters entirely
/// once non-letters are stripped — a two-character difference in a
/// twenty-four-character string. Fuzzy matching absorbs that; the threshold is
/// still tight enough that two genuinely different headings never merge.
const SIGNATURE_SIMILARITY: f64 = 0.85;

/// Signatures shorter than this are too weak to match on.
const MIN_SIGNATURE_LEN: usize = 8;

pub fn signatures_match(a: &str, b: &str) -> bool {
    if a.len() < MIN_SIGNATURE_LEN || b.len() < MIN_SIGNATURE_LEN {
        return false;
    }
    if a == b {
        return true;
    }
    strsim::normalized_levenshtein(a, b) >= SIGNATURE_SIMILARITY
}

struct Cluster {
    rep: String,
    pages: usize,
}

/// Counts how often each distinct band line recurs across the document.
pub struct BandScan {
    clusters: Vec<Cluster>,
    pages: usize,
}

impl BandScan {
    pub fn new() -> Self {
        BandScan {
            clusters: Vec::new(),
            pages: 0,
        }
    }

    /// Feed one page's coarse lines.
    pub fn observe(&mut self, lines: &[Line], page_height: f32) {
        self.pages += 1;
        // Which clusters this page contributed to, so a page cannot vote twice.
        let mut voted: Vec<usize> = Vec::new();
        for l in lines {
            if !in_band(l, page_height) {
                continue;
            }
            let sig = signature(&l.text());
            if sig.len() < MIN_SIGNATURE_LEN {
                continue;
            }
            let found = self
                .clusters
                .iter()
                .position(|c| signatures_match(&c.rep, &sig));
            let idx = match found {
                Some(i) => i,
                None => {
                    self.clusters.push(Cluster { rep: sig, pages: 0 });
                    self.clusters.len() - 1
                }
            };
            if !voted.contains(&idx) {
                voted.push(idx);
                self.clusters[idx].pages += 1;
            }
        }
    }

    /// Signatures that recur often enough to be running furniture.
    pub fn repeated(&self) -> Vec<String> {
        if self.pages < MIN_REPEAT_PAGES {
            return Vec::new();
        }
        let threshold = ((self.pages as f32 * REPEAT_FRAC).ceil() as usize).max(2);
        self.clusters
            .iter()
            .filter(|c| c.pages >= threshold)
            .map(|c| c.rep.clone())
            .collect()
    }
}

impl Default for BandScan {
    fn default() -> Self {
        Self::new()
    }
}

/// Is this "line" actually a horizontal rule rather than text?
///
/// Screenshots and many documents separate their furniture from the content
/// with a rule, and OCR reports it as one very wide, very flat word. That makes
/// it a far better boundary marker than any distance threshold: in
/// `bookSample.pdf` the app's header bar sits only 31pt above the body, well
/// inside the normal line spacing, so no gap test can separate them — but the
/// rule between them is unmistakable.
fn is_rule(l: &Line, page_width: f32, text_height: f32) -> bool {
    l.bbox.width() > page_width * 0.55 && l.bbox.height() < text_height * 0.35
}

/// A line is in a band when its *leading* edge is — its top edge for the top
/// band, its bottom edge for the bottom one.
///
/// Testing the trailing edge instead is a trap. The header bar of
/// `bookSample.pdf` starts 43pt down a 595pt page, comfortably inside the
/// band, but its tallest glyph reaches 54.2pt and so overshot a 53.6pt band by
/// 0.6pt — enough to make the whole header look like body text.
fn in_band(l: &Line, page_height: f32) -> bool {
    in_top_band(l, page_height) || in_bottom_band(l, page_height)
}

fn in_top_band(l: &Line, page_height: f32) -> bool {
    l.bbox.y0 <= page_height * BAND_FRAC
}

fn in_bottom_band(l: &Line, page_height: f32) -> bool {
    l.bbox.y1 >= page_height * (1.0 - BAND_FRAC)
}

/// Is this line a printed page number?
///
/// Deliberately loose about surrounding punctuation: the Turkish book renders
/// page 41 as `·41 -`, and other scans use `- 41 -` or `[41]`.
pub fn page_number(text: &str) -> Option<String> {
    let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() > 5 {
        return None;
    }
    let letters = text.chars().filter(|c| c.is_alphabetic()).count();
    if letters > 0 {
        return None;
    }
    // Guard against a line that is mostly punctuation noise with one stray digit.
    let meaningful = text.chars().filter(|c| !c.is_whitespace()).count();
    if meaningful > digits.len() + 4 {
        return None;
    }
    Some(digits)
}

pub struct ChromeResult {
    pub body: Vec<Line>,
    pub dropped: Vec<DroppedLine>,
    pub printed_label: Option<String>,
}

/// Classify a page's coarse lines into body versus furniture.
pub fn strip(
    lines: Vec<Line>,
    page_width: f32,
    page_height: f32,
    repeated: &[String],
    single_page: bool,
    gutters: &[f32],
) -> ChromeResult {
    let mut body = Vec::new();
    let mut dropped = Vec::new();
    let mut printed_label = None;

    // Rules near the top or bottom bound the content. Anything beyond the
    // outermost one is furniture, whatever it looks like.
    let text_height = {
        let hs: Vec<f32> = lines
            .iter()
            .filter(|l| !l.is_blank())
            .map(|l| l.bbox.height())
            .collect();
        crate::geom::median(&hs).max(1.0)
    };
    let mut top_rule: Option<f32> = None;
    let mut bottom_rule: Option<f32> = None;
    for l in &lines {
        if !is_rule(l, page_width, text_height) {
            continue;
        }
        let mid = (l.bbox.y0 + l.bbox.y1) / 2.0;
        if mid < page_height * 0.2 {
            top_rule = Some(top_rule.map_or(l.bbox.y1, |v: f32| v.max(l.bbox.y1)));
        } else if mid > page_height * 0.8 {
            bottom_rule = Some(bottom_rule.map_or(l.bbox.y0, |v: f32| v.min(l.bbox.y0)));
        }
    }

    // Extent of the non-band lines, used by the isolation rule below. Measured
    // before `lines` is consumed.
    let (core_count, core_top, core_bottom, core_x0, core_x1) = {
        let core: Vec<&Line> = lines.iter().filter(|l| !in_band(l, page_height)).collect();
        let top = core.iter().map(|l| l.bbox.y0).fold(f32::MAX, f32::min);
        let bottom = core.iter().map(|l| l.bbox.y1).fold(f32::MIN, f32::max);
        let a = core.iter().map(|l| l.bbox.x0).fold(f32::MAX, f32::min);
        let b = core.iter().map(|l| l.bbox.x1).fold(f32::MIN, f32::max);
        (core.len(), top, bottom, a, b)
    };
    let core_width = (core_x1 - core_x0).max(1.0);
    let leading = super::lines::median_leading(&lines);

    for l in lines {
        let text = l.text();
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }

        // 0a. In a multi-column layout, a line that crosses a gutter is not
        // body text — body lines stay inside their column. Confined to the
        // outer bands so that a genuinely full-width figure or table in the
        // middle of a page is left alone.
        //
        // This is what identifies the e-reader app's header bar in
        // `bookSample.pdf`: it sits only 25pt above the body, closer than two
        // lines of text, and the page's one horizontal rule is above it rather
        // than below, so neither a gap test nor the rule can separate them.
        if !gutters.is_empty() && in_band(&l, page_height) {
            let straddles = gutters
                .iter()
                .any(|g| l.bbox.x0 < *g - 1.0 && l.bbox.x1 > *g + 1.0);
            if straddles {
                if let Some(n) = page_number(trimmed) {
                    if printed_label.is_none() {
                        printed_label = Some(n);
                    }
                    dropped.push(DroppedLine {
                        text: trimmed.to_string(),
                        reason: DropReason::PageNumber,
                    });
                    continue;
                }
                dropped.push(DroppedLine {
                    text: trimmed.to_string(),
                    reason: if in_top_band(&l, page_height) {
                        DropReason::RunningHeader
                    } else {
                        DropReason::RunningFooter
                    },
                });
                continue;
            }
        }

        // 0b. A band line lying entirely outside the body's own left/right
        // extent is furniture in the margin. This is what catches the
        // "< Back to store" button in `bookSample.pdf`: it sits far left of
        // both text columns, so it crosses no gutter, but it is plainly not
        // part of the page's text.
        if core_count >= 3 && in_band(&l, page_height) {
            let margin = text_height;
            let outside_left = l.bbox.x1 < core_x0 - margin;
            let outside_right = l.bbox.x0 > core_x1 + margin;
            if outside_left || outside_right {
                dropped.push(DroppedLine {
                    text: trimmed.to_string(),
                    reason: DropReason::Chrome,
                });
                continue;
            }
        }

        // 0c. Beyond a bounding rule, or the rule itself.
        if is_rule(&l, page_width, text_height) {
            dropped.push(DroppedLine {
                text: format!("(horizontal rule, {:.0}pt wide)", l.bbox.width()),
                reason: DropReason::Chrome,
            });
            continue;
        }
        let beyond_rule = top_rule.map(|r| l.bbox.y1 <= r + text_height * 0.5).unwrap_or(false)
            || bottom_rule
                .map(|r| l.bbox.y0 >= r - text_height * 0.5)
                .unwrap_or(false);
        if beyond_rule {
            // A page number below a footer rule is still a page number.
            if let Some(n) = page_number(trimmed) {
                if printed_label.is_none() {
                    printed_label = Some(n);
                }
                dropped.push(DroppedLine {
                    text: trimmed.to_string(),
                    reason: DropReason::PageNumber,
                });
                continue;
            }
            dropped.push(DroppedLine {
                text: trimmed.to_string(),
                reason: DropReason::Chrome,
            });
            continue;
        }

        if in_band(&l, page_height) {
            // 1. A repeated signature is running furniture.
            let sig = signature(trimmed);
            if repeated.iter().any(|r| signatures_match(r, &sig)) {
                dropped.push(DroppedLine {
                    text: trimmed.to_string(),
                    reason: if in_top_band(&l, page_height) {
                        DropReason::RunningHeader
                    } else {
                        DropReason::RunningFooter
                    },
                });
                continue;
            }
            // 2. A bare number in a band is the printed page label.
            if let Some(n) = page_number(trimmed) {
                if printed_label.is_none() {
                    printed_label = Some(n);
                }
                dropped.push(DroppedLine {
                    text: trimmed.to_string(),
                    reason: DropReason::PageNumber,
                });
                continue;
            }
            // 3. Single-page fallback: a short line in a band, cut off from the
            //    body by a big vertical gap, is UI chrome. Only applied when
            //    repetition cannot help, so a real book's first-page title is
            //    never eaten by it.
            if single_page && core_count >= 3 {
                let gap = if in_top_band(&l, page_height) {
                    core_top - l.bbox.y1
                } else {
                    l.bbox.y0 - core_bottom
                };
                let short = l.bbox.width() < core_width * 0.75;
                if gap > leading * 2.0 && short {
                    dropped.push(DroppedLine {
                        text: trimmed.to_string(),
                        reason: DropReason::Chrome,
                    });
                    continue;
                }
            }
        }
        body.push(l);
    }

    ChromeResult {
        body,
        dropped,
        printed_label,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Rect;
    use crate::model::Word;

    fn line(text: &str, x0: f32, y0: f32, x1: f32, y1: f32) -> Line {
        Line::from_words(vec![Word {
            text: text.to_string(),
            bbox: Rect::new(x0, y0, x1, y1),
            font_size: y1 - y0,
            conf: None,
            bold: false,
        }])
        .unwrap()
    }

    #[test]
    fn the_same_header_matches_across_its_misreadings() {
        // All four forms occur in the Turkish book: the separator is read as
        // `ll` or `//`, the page number varies, and ClearScan sometimes splits
        // CORRY into two words.
        let forms = [
            "JANE CORRY ll KOCAMIN KARISI",
            "JANE CORRY // KOCAMIN KARISI 41",
            "JANE COR RY ll KOCAMIN KARISI",
            "JANE CORRY ll KOCAMIN KARISI 302",
        ];
        let base = signature(forms[0]);
        for f in forms {
            assert!(
                signatures_match(&base, &signature(f)),
                "{f:?} -> {:?} did not match {base:?}",
                signature(f)
            );
        }
    }

    #[test]
    fn different_headings_do_not_match() {
        assert!(!signatures_match(
            &signature("JANE CORRY ll KOCAMIN KARISI"),
            &signature("Harry Potter en de Geheime Kamer")
        ));
        // Short strings are never matched on.
        assert!(!signatures_match(&signature("Carla"), &signature("Carlo")));
    }

    #[test]
    fn detects_the_turkish_page_number_forms() {
        assert_eq!(page_number("·41 -").as_deref(), Some("41"));
        assert_eq!(page_number("- 123 -").as_deref(), Some("123"));
        assert_eq!(page_number("7").as_deref(), Some("7"));
        // Real text with a number in it is not a page number.
        assert_eq!(page_number("Bunun üzerine 999'u aradım"), None);
        assert_eq!(page_number("Chapter 4"), None);
    }

    #[test]
    fn repeated_header_is_found_and_stripped() {
        let mut scan = BandScan::new();
        let h = 800.0;
        for _ in 0..10 {
            scan.observe(
                &[
                    line("JANE CORRY ll KOCAMIN KARISI", 150.0, 30.0, 340.0, 42.0),
                    line("body text here", 30.0, 200.0, 400.0, 212.0),
                ],
                h,
            );
        }
        let rep = scan.repeated();
        assert!(
            rep.iter().any(|r| signatures_match(r, &signature("JANE CORRY ll KOCAMIN KARISI"))),
            "got {rep:?}"
        );

        let r = strip(
            vec![
                line("JANE CORRY ll KOCAMIN KARISI", 150.0, 30.0, 340.0, 42.0),
                line("body text here", 30.0, 200.0, 400.0, 212.0),
                line("·41 -", 230.0, 770.0, 260.0, 782.0),
            ],
            500.0,
            h,
            &rep,
            false,
            &[],
        );
        assert_eq!(r.body.len(), 1);
        assert_eq!(r.body[0].text(), "body text here");
        assert_eq!(r.printed_label.as_deref(), Some("41"));
        assert_eq!(r.dropped.len(), 2);
    }

    #[test]
    fn body_text_is_never_treated_as_repeated() {
        // Every page has different prose, so no signature repeats.
        let words = [
            "alpha bravo charlie", "delta echo foxtrot", "golf hotel india",
            "juliett kilo lima", "mike november oscar", "papa quebec romeo",
            "sierra tango uniform", "victor whiskey xray",
        ];
        let mut scan = BandScan::new();
        for w in words {
            scan.observe(&[line(w, 30.0, 20.0, 400.0, 32.0)], 800.0);
        }
        assert!(scan.repeated().is_empty(), "got {:?}", scan.repeated());
    }

    #[test]
    fn short_document_does_not_invent_a_running_header() {
        let mut scan = BandScan::new();
        for _ in 0..3 {
            scan.observe(&[line("A Title", 150.0, 20.0, 250.0, 32.0)], 800.0);
        }
        assert!(scan.repeated().is_empty(), "3 pages is too few to conclude");
    }

    #[test]
    fn a_band_line_crossing_the_gutter_is_chrome() {
        // bookSample.pdf: the app title bar is centred across a two-column
        // spread, so it crosses the gutter that no body line ever crosses.
        let (w, h) = (842.0, 595.0);
        let gutters = [420.0];
        let mut lines = vec![
            line("Back to store", 27.0, 43.0, 90.0, 52.0),
            line("Harry Potter en de Geheime Kamer (Dutch Edition)", 300.0, 43.0, 560.0, 52.0),
        ];
        for i in 0..14 {
            let y = 78.0 + i as f32 * 19.0;
            lines.push(line("left column text here", 150.0, y, 400.0, y + 12.0));
            lines.push(line("right column text here", 440.0, y, 690.0, y + 12.0));
        }
        let r = strip(lines, w, h, &[], true, &gutters);
        let dropped: Vec<String> = r.dropped.iter().map(|d| d.text.clone()).collect();
        assert!(
            dropped.iter().any(|d| d.contains("Harry Potter")),
            "the gutter-crossing title bar must go, dropped {dropped:?}"
        );
        assert!(
            dropped.iter().any(|d| d == "Back to store"),
            "the button left of both columns must go too, dropped {dropped:?}"
        );
        assert_eq!(r.body.len(), 28, "no body line may be lost");
    }

    #[test]
    fn a_full_width_line_in_the_middle_of_a_page_is_kept() {
        // A figure caption spanning both columns is content, not furniture,
        // and it is nowhere near a band.
        let (w, h) = (842.0, 595.0);
        let gutters = [420.0];
        let mut lines = vec![line("Figure 3: a caption spanning both columns", 150.0, 300.0, 690.0, 312.0)];
        for i in 0..14 {
            let y = 78.0 + i as f32 * 15.0;
            lines.push(line("left column text", 150.0, y, 400.0, y + 12.0));
        }
        let r = strip(lines, w, h, &[], true, &gutters);
        assert!(r.body.iter().any(|l| l.text().contains("Figure 3")));
    }

    #[test]
    fn a_header_rule_bounds_the_content() {
        // bookSample.pdf: the app's header bar sits 31pt above the body, which
        // is inside normal line spacing, so only the rule between them can
        // separate the two.
        let h = 595.0;
        let w = 842.0;
        let mut lines = vec![
            line("Back to store", 28.0, 43.0, 90.0, 52.0),
            line("Harry Potter en de Geheime Kamer", 300.0, 43.0, 560.0, 52.0),
            // The rule: 96% of the page wide, 1.5pt tall.
            line("", 14.0, 59.5, 828.0, 61.0),
        ];
        for i in 0..14 {
            lines.push(line("body body body body body", 150.0, 83.0 + i as f32 * 19.0, 690.0, 95.0 + i as f32 * 19.0));
        }
        let r = strip(lines, w, h, &[], true, &[]);
        let dropped: Vec<&str> = r.dropped.iter().map(|d| d.text.as_str()).collect();
        assert!(dropped.contains(&"Back to store"), "got {dropped:?}");
        assert!(
            dropped.iter().any(|d| d.contains("Harry Potter")),
            "the title bar must go too, got {dropped:?}"
        );
        assert_eq!(r.body.len(), 14, "all body lines must survive");
    }

    #[test]
    fn a_footer_rule_keeps_the_page_number() {
        let h = 800.0;
        let w = 500.0;
        let mut lines = Vec::new();
        for i in 0..20 {
            lines.push(line("body text line here", 40.0, 100.0 + i as f32 * 20.0, 460.0, 112.0 + i as f32 * 20.0));
        }
        lines.push(line("", 30.0, 700.0, 470.0, 701.0)); // footer rule
        lines.push(line("- 41 -", 230.0, 760.0, 270.0, 772.0));
        let r = strip(lines, w, h, &[], true, &[]);
        assert_eq!(r.printed_label.as_deref(), Some("41"));
        assert_eq!(r.body.len(), 20);
    }

    #[test]
    fn single_page_isolation_removes_ui_chrome() {
        let h = 600.0;
        // Top bar, far above a dense body block, and a bottom progress line.
        let mut lines = vec![line("Back to store", 20.0, 10.0, 120.0, 22.0)];
        for i in 0..12 {
            lines.push(line("body body body body", 30.0, 120.0 + i as f32 * 14.0, 500.0, 132.0 + i as f32 * 14.0));
        }
        lines.push(line("Location 5 of 139 0%", 200.0, 570.0, 330.0, 582.0));
        let r = strip(lines, 600.0, h, &[], true, &[]);
        let dropped: Vec<&str> = r.dropped.iter().map(|d| d.text.as_str()).collect();
        assert!(dropped.contains(&"Back to store"), "got {dropped:?}");
        assert_eq!(r.body.len(), 12);
    }
}
