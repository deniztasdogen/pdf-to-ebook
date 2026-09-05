//! The intermediate representation both input paths converge on.

use crate::geom::{median, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PageSource {
    /// Text came from the PDF's own text layer.
    Text,
    /// Text came from our OCR of a rendered page image.
    Ocr,
    /// Nothing was extracted.
    Empty,
}

/// One word, in points, top-left origin.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Word {
    pub text: String,
    pub bbox: Rect,
    /// Glyph size in points. On the OCR path this is the box height, which is
    /// close enough for the relative comparisons the layout code makes.
    pub font_size: f32,
    /// OCR confidence 0..100. `None` on the text path.
    pub conf: Option<f32>,
    /// Set from the PDF font weight. Always false on the OCR path, since
    /// tesseract's TSV carries no font information.
    pub bold: bool,
}

/// Half-height of the band a glyph is considered to occupy, as a multiple of
/// its font size.
const LINE_SPAN_FACTOR: f32 = 0.6;

impl Word {
    /// The vertical band this word occupies, for deciding line membership.
    ///
    /// Derived from the font size around the box centre rather than from the
    /// box itself, because ClearScan's synthetic fonts report wildly different
    /// boxes for the same nominal size: on the chapter heading of page 66 a
    /// 13.9pt glyph comes back 42pt tall, which overlaps the chapter number
    /// sitting on the line above and merges the two into `C ar 8 la`. Body
    /// text is unaffected — there the box is already about 1.25x the font size.
    ///
    /// `cap` bounds the font size used, and is the page's median. Without it a
    /// single oversized glyph gets a band tall enough to touch the text lines
    /// both above and below it and chains them into one — the box rule around
    /// the newspaper clipping on page 90 is set in 23.75pt against 17pt body
    /// text and interleaved two whole lines of the article.
    pub fn line_span(&self, cap: f32) -> (f32, f32) {
        let c = (self.bbox.y0 + self.bbox.y1) / 2.0;
        let size = if cap > 0.0 {
            self.font_size.min(cap)
        } else {
            self.font_size
        };
        let h = (size * LINE_SPAN_FACTOR).max(0.5);
        (c - h, c + h)
    }

    /// True when the word ends with a marker meaning "this word continues on
    /// the next line".
    ///
    /// Three different markers show up across the test corpus, which is why
    /// this is a set and not a single character:
    ///   * `U+0002` — what pdfium reports for the Turkish ClearScan book
    ///   * `U+00AD` — soft hyphen, what pypdf reports for the same file
    ///   * `U+00AC` — used by the 1901 dime-novel scan
    ///   * `-`      — a literal hyphen, ambiguous (see [`HyphenKind`])
    pub fn hyphen_kind(&self) -> Option<HyphenKind> {
        match self.text.chars().last()? {
            '\u{0002}' | '\u{00ad}' | '\u{00ac}' => Some(HyphenKind::Explicit),
            '-' | '\u{2010}' | '\u{2011}' => Some(HyphenKind::Literal),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HyphenKind {
    /// An unambiguous "the word continues" marker. Always join.
    Explicit,
    /// A real hyphen character. Might be a compound word (`tik-taklarını`)
    /// rather than a line break, so joining needs a second opinion.
    Literal,
}

/// A visual line of text within one column.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Line {
    pub words: Vec<Word>,
    pub bbox: Rect,
    /// Median glyph size on this line.
    pub font_size: f32,
    /// Most of this line is bold.
    pub bold: bool,
}

impl Line {
    pub fn from_words(mut words: Vec<Word>) -> Option<Line> {
        if words.is_empty() {
            return None;
        }
        words.sort_by(|a, b| a.bbox.x0.partial_cmp(&b.bbox.x0).unwrap());
        let bbox = Rect::union_all(words.iter().map(|w| &w.bbox))?;
        let sizes: Vec<f32> = words.iter().map(|w| w.font_size).collect();
        let bold_count = words.iter().filter(|w| w.bold).count();
        Some(Line {
            font_size: median(&sizes),
            bold: bold_count * 2 > words.len(),
            words,
            bbox,
        })
    }

    pub fn text(&self) -> String {
        let mut s = String::new();
        for (i, w) in self.words.iter().enumerate() {
            if i > 0 {
                s.push(' ');
            }
            s.push_str(&w.text);
        }
        s
    }

    pub fn is_blank(&self) -> bool {
        self.words.iter().all(|w| w.text.trim().is_empty())
    }

    /// Mean OCR confidence, if this line came from OCR.
    pub fn confidence(&self) -> Option<f32> {
        let vals: Vec<f32> = self.words.iter().filter_map(|w| w.conf).collect();
        if vals.is_empty() {
            None
        } else {
            Some(vals.iter().sum::<f32>() / vals.len() as f32)
        }
    }

    pub fn hyphen_kind(&self) -> Option<HyphenKind> {
        self.words.last()?.hyphen_kind()
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Column {
    pub lines: Vec<Line>,
    pub bbox: Rect,
}

/// One page, after chrome removal and column splitting.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PageLayout {
    pub index: usize,
    pub source: PageSource,
    pub width_pt: f32,
    pub height_pt: f32,
    pub columns: Vec<Column>,
    /// The printed page number recovered from the footer, e.g. `"41"`. Differs
    /// from `index` — page index 40 of the Turkish book is printed page 41.
    pub printed_label: Option<String>,
    /// Lines removed as running headers, footers or UI chrome. Kept so the run
    /// report can show what was discarded rather than losing it silently.
    pub dropped: Vec<DroppedLine>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DroppedLine {
    pub text: String,
    pub reason: DropReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DropReason {
    RunningHeader,
    RunningFooter,
    PageNumber,
    Chrome,
    Cropped,
}

impl PageLayout {
    pub fn label(&self) -> String {
        self.printed_label
            .clone()
            .unwrap_or_else(|| (self.index + 1).to_string())
    }

    pub fn all_lines(&self) -> impl Iterator<Item = &Line> {
        self.columns.iter().flat_map(|c| c.lines.iter())
    }
}
