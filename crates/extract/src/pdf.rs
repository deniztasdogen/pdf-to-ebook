//! pdfium access: page classification, text-layer words, and rasterisation.

use crate::geom::{mad, median, Rect};
use crate::model::Word;
use pdftomobi_core::{Error, Result};
use pdfium_render::prelude::*;

/// Above this many extractable characters a page's text layer is trusted
/// outright. Calibrated against the corpus: body pages of the Turkish book
/// carry 1400-2000 characters, its title pages 30-118, its blanks 0.
const MIN_TEXT_CHARS: usize = 120;

/// OCR has to beat the text layer by this factor on an ambiguous page before it
/// is preferred.
///
/// This exists because guessing wrong is expensive in both directions. The
/// title page of `lady-susan.pdf` has a perfectly good 27-character text layer,
/// and OCR reads its "Jane" as "Fane" — so a bare character threshold would
/// silently corrupt the author's name. But a scanned page can also carry a
/// stray fragment of real text over an un-OCR'd image, where the text layer is
/// genuinely useless. The only reliable way to tell them apart is to try both
/// and compare, which is affordable because only a handful of pages per book
/// are ambiguous.
const OCR_MUST_BEAT_TEXT_BY: f32 = 1.5;

/// pdfium reports the synthetic `\r` and `\n` it inserts between lines as real
/// characters, with a degenerate box and this font size. They have to go before
/// any geometry is computed or they poison the margin statistics.
const SYNTHETIC_FONT_SIZE: f32 = 1.0;

pub struct PdfDoc {
    pdfium: Pdfium,
    path: std::path::PathBuf,
}

pub struct PageInfo {
    pub index: usize,
    pub width_pt: f32,
    pub height_pt: f32,
    pub text_chars: usize,
    pub image_objects: usize,
}

impl PageInfo {
    /// Whether this page's own text layer is worth using without a second look.
    ///
    /// Deliberately ignores images. In the Turkish test book 310 of 419 pages
    /// carry images *and* a full page of good text, so "has images" says
    /// nothing about whether OCR is needed.
    pub fn has_usable_text(&self) -> bool {
        self.text_chars >= MIN_TEXT_CHARS
    }

    pub fn has_no_text(&self) -> bool {
        self.text_chars == 0
    }
}

/// What to do with one page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageDecision {
    /// Trust the text layer.
    Text,
    /// The text layer is empty; OCR it.
    Ocr,
    /// Sparse text. Do both and keep whichever reads better.
    Compare,
    /// Nothing to do.
    Empty,
}

/// Given two candidate extractions for an ambiguous page, say whether OCR wins.
pub fn ocr_wins(text_chars: usize, ocr_chars: usize) -> bool {
    ocr_chars as f32 > (text_chars as f32 * OCR_MUST_BEAT_TEXT_BY).max(1.0)
}

impl PdfDoc {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            return Err(Error::InputMissing(path));
        }
        let pdfium = pdfium_bundled::bind_pdfium_silent()
            .map_err(|e| Error::Pdf(format!("could not load the pdfium library: {e}")))?;
        // Fail early with a clear message rather than at first page access.
        {
            let doc = pdfium
                .load_pdf_from_file(&path, None)
                .map_err(|e| Error::Pdf(format!("could not open {}: {e}", path.display())))?;
            let _ = doc.pages().len();
        }
        Ok(PdfDoc { pdfium, path })
    }

    fn load(&self) -> Result<PdfDocument<'_>> {
        self.pdfium
            .load_pdf_from_file(&self.path, None)
            .map_err(|e| Error::Pdf(format!("{e}")))
    }

    pub fn page_count(&self) -> Result<usize> {
        Ok(self.load()?.pages().len() as usize)
    }

    /// Pass 0: how much text each page has, and how big it is.
    pub fn survey(&self, range: Option<(usize, usize)>) -> Result<Vec<PageInfo>> {
        let doc = self.load()?;
        let n = doc.pages().len() as usize;
        let (first, last) = clamp_range(range, n);
        let mut out = Vec::with_capacity(last.saturating_sub(first) + 1);
        for i in first..=last {
            let p = doc
                .pages()
                .get(i as i32)
                .map_err(|e| Error::Pdf(format!("page {i}: {e}")))?;
            let text_chars = p
                .text()
                .map(|t| t.all().chars().filter(|c| !c.is_control()).count())
                .unwrap_or(0);
            let image_objects = p
                .objects()
                .iter()
                .filter(|o| o.object_type() == PdfPageObjectType::Image)
                .count();
            out.push(PageInfo {
                index: i,
                width_pt: p.width().value,
                height_pt: p.height().value,
                text_chars,
                image_objects,
            });
        }
        Ok(out)
    }

    /// Words from the page's own text layer, in points with a top-left origin.
    pub fn text_words(&self, index: usize) -> Result<(Vec<Word>, f32, f32)> {
        let doc = self.load()?;
        let page = doc
            .pages()
            .get(index as i32)
            .map_err(|e| Error::Pdf(format!("page {index}: {e}")))?;
        let (pw, ph) = (page.width().value, page.height().value);
        let text = page
            .text()
            .map_err(|e| Error::Pdf(format!("page {index} text: {e}")))?;

        // Collect glyphs first. `loose_bounds` is essential: `tight_bounds` is
        // the ink box, so a descender or a Turkish diacritic sits outside its
        // line's band and shatters the line into fragments like "yyy" and
        // "y,ğşyyppypğy".
        struct Glyph {
            ch: char,
            bbox: Rect,
            size: f32,
            bold: bool,
        }
        let mut glyphs: Vec<Glyph> = Vec::new();
        for c in text.chars().iter() {
            let Some(ch) = c.unicode_char() else { continue };
            let size = c.scaled_font_size().value;
            // Drop pdfium's synthetic line terminators.
            if ch == '\r' || ch == '\n' || (ch.is_control() && ch != '\u{0002}') {
                continue;
            }
            // Order matters here. A real space can be reported with font size
            // 1.0 - the space in "Mr. Vernon." on page 2 of lady-susan.pdf is
            // one - so the size filter has to come *after* whitespace is
            // recognised, or the words either side get welded together.
            if !ch.is_whitespace() && size <= SYNTHETIC_FONT_SIZE {
                continue;
            }
            let Ok(b) = c.loose_bounds() else { continue };
            // Phantom glyphs with no area. Page 66 carries a `ka` whose box is
            // x=[359.32..359.32], y=[92.78..92.79] — nothing is drawn, but left
            // in it lands in the middle of a real word ("dik ka timi").
            if !ch.is_whitespace() && (b.width().value < 0.1 || b.height().value < 0.5) {
                continue;
            }
            // pdfium is bottom-left; flip to top-left.
            let bbox = Rect::new(
                b.left().value,
                ph - b.top().value,
                b.right().value,
                ph - b.bottom().value,
            );
            // Section headings in born-digital documents are often set in the
            // same size as the body and distinguished only by weight — every
            // heading in the Transformer paper is 9.96pt bold against 9.96pt
            // roman — so weight is the only thing that can find them.
            let bold = c.font_weight().map(is_bold_weight).unwrap_or(false);
            glyphs.push(Glyph { ch, bbox, size, bold });
        }

        // Group glyphs into words.
        //
        // Splitting is driven by the PDF's own space characters, not by
        // geometry. pdfium already knows where the words break, and second
        // guessing it is destructive: ClearScan lays out glyphs with irregular
        // advances, so a gap threshold tight enough to catch a missing space
        // also fires inside words and turns "Rahatlıyorum. Tam zamanında" into
        // "Ra hatlıyorum. Tam zama nında" across the whole book.
        //
        // The geometric fallback still exists, because it is genuinely needed:
        // the second column of `under-lock-and-key.pdf` lost almost all of its
        // space characters during the original OCR. So it is switched on per
        // page, only when the page turns out to have hardly any spaces.
        let sizes: Vec<f32> = glyphs
            .iter()
            .filter(|g| !g.ch.is_whitespace())
            .map(|g| g.size)
            .collect();
        let med_size = median(&sizes).max(1.0);

        let space_count = glyphs.iter().filter(|g| g.ch.is_whitespace()).count();
        let glyph_count = glyphs.len() - space_count;
        // Running prose has roughly one space every five or six characters.
        // Under one in twelve means the spaces are missing, not sparse.
        let spaces_missing = space_count * 12 < glyph_count;
        let gap_limit = if spaces_missing {
            med_size * 0.28
        } else {
            // Wide enough never to fire inside a word, tight enough to still
            // break at a real jump such as a table column or a stray fragment.
            med_size * 1.1
        };

        let mut words: Vec<Word> = Vec::new();
        let mut cur_text = String::new();
        let mut cur_bbox: Option<Rect> = None;
        let mut cur_sizes: Vec<f32> = Vec::new();
        let mut cur_bold = 0usize;
        let mut cur_total = 0usize;
        let mut prev: Option<&Glyph> = None;

        let flush = |cur_text: &mut String,
                     cur_bbox: &mut Option<Rect>,
                     cur_sizes: &mut Vec<f32>,
                     cur_bold: &mut usize,
                     cur_total: &mut usize,
                     words: &mut Vec<Word>| {
            if let (false, Some(bbox)) = (cur_text.trim().is_empty(), *cur_bbox) {
                words.push(Word {
                    text: cur_text.clone(),
                    bbox,
                    font_size: median(cur_sizes),
                    conf: None,
                    bold: *cur_total > 0 && *cur_bold * 2 > *cur_total,
                });
            }
            cur_text.clear();
            *cur_bbox = None;
            cur_sizes.clear();
            *cur_bold = 0;
            *cur_total = 0;
        };

        for g in &glyphs {
            if g.ch.is_whitespace() {
                flush(&mut cur_text, &mut cur_bbox, &mut cur_sizes, &mut cur_bold, &mut cur_total, &mut words);
                prev = Some(g);
                continue;
            }
            // Same-line test, normalised by font size for the same reason
            // line assembly is: a heading's `C` reports a 16pt box while the
            // `ar` beside it reports 42pt, so comparing raw box tops split
            // "Carla" into "C ar la".
            //
            // A big forward jump in x, a jump back past the start of the word
            // being built, or a jump to another line, all start a new word.
            //
            // Both tests here are shaped by one ClearScan habit: pdfium
            // reports a single box covering a whole glyph cluster. In
            // "Rahatlıyorum" the `a` and `h` share x=[60.67..76.05], and the
            // `k` of "kınklığı" on page 293 comes back 58pt wide. Every
            // character after such a box therefore looks like a large jump
            // backwards.
            //
            // So the backward test measures from the *word's* left edge, not
            // from the previous glyph's right edge. Measuring from the previous
            // glyph shattered these words into single letters
            // ("Ra hatlıyorum", "k ı n k l ı ğ ı"), while a genuine jump back —
            // returning to the left column of a spread — still lands well
            // before the word's own start.
            //
            // Line membership is likewise judged on a font-size-normalised
            // band rather than the reported box, because a heading's `C` comes
            // back 16pt tall next to an `ar` of 42pt.
            if let Some(p) = prev {
                let dx = g.bbox.x0 - p.bbox.x1;
                let a = band_of(g.bbox, g.size);
                let b = band_of(p.bbox, p.size);
                let overlap = a.1.min(b.1) - a.0.max(b.0);
                let narrower = (a.1 - a.0).min(b.1 - b.0).max(0.1);
                let same_band = overlap >= narrower * 0.4;
                let jumped_back = cur_bbox
                    .map(|b: Rect| g.bbox.x0 < b.x0 - med_size * 0.5)
                    .unwrap_or(false);
                if !same_band || dx > gap_limit || jumped_back {
                    flush(&mut cur_text, &mut cur_bbox, &mut cur_sizes, &mut cur_bold, &mut cur_total, &mut words);
                }
            }
            cur_text.push(g.ch);
            cur_bbox = Some(match cur_bbox {
                Some(b) => b.union(&g.bbox),
                None => g.bbox,
            });
            cur_sizes.push(g.size);
            cur_total += 1;
            if g.bold {
                cur_bold += 1;
            }
            prev = Some(g);
        }
        flush(&mut cur_text, &mut cur_bbox, &mut cur_sizes, &mut cur_bold, &mut cur_total, &mut words);

        Ok((words, pw, ph))
    }

    /// Render a page to a grayscale image for OCR.
    ///
    /// Grayscale because tesseract works on intensity anyway, and it cuts the
    /// bitmap to a third of the size for large books.
    pub fn render_page(&self, index: usize, dpi: f32) -> Result<image::GrayImage> {
        let doc = self.load()?;
        let page = doc
            .pages()
            .get(index as i32)
            .map_err(|e| Error::Pdf(format!("page {index}: {e}")))?;
        let target_w = ((page.width().value * dpi / 72.0).round() as i32).max(1);
        let bitmap = page
            .render_with_config(&PdfRenderConfig::new().set_target_width(target_w))
            .map_err(|e| Error::Pdf(format!("render page {index}: {e}")))?;
        let img = bitmap
            .as_image()
            .map_err(|e| Error::Pdf(format!("bitmap page {index}: {e}")))?;
        Ok(img.into_luma8())
    }

    pub fn metadata_title(&self) -> Option<String> {
        let doc = self.load().ok()?;
        let t = doc.metadata().get(PdfDocumentMetadataTagType::Title)?;
        let v = t.value().trim().to_string();
        if v.is_empty() {
            None
        } else {
            Some(v)
        }
    }

    pub fn metadata_author(&self) -> Option<String> {
        let doc = self.load().ok()?;
        let t = doc.metadata().get(PdfDocumentMetadataTagType::Author)?;
        let v = t.value().trim().to_string();
        if v.is_empty() {
            None
        } else {
            Some(v)
        }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

/// The vertical band a glyph occupies, sized from its font rather than its
/// reported box. Mirrors [`crate::model::Word::line_span`].
fn band_of(bbox: Rect, size: f32) -> (f32, f32) {
    let c = (bbox.y0 + bbox.y1) / 2.0;
    let h = (size * 0.6).max(0.5);
    (c - h, c + h)
}

/// Treat semibold and above as bold.
fn is_bold_weight(w: PdfFontWeight) -> bool {
    match w {
        PdfFontWeight::Weight600
        | PdfFontWeight::Weight700Bold
        | PdfFontWeight::Weight800
        | PdfFontWeight::Weight900 => true,
        PdfFontWeight::Custom(n) => n >= 600,
        _ => false,
    }
}

pub fn clamp_range(range: Option<(usize, usize)>, n: usize) -> (usize, usize) {
    match range {
        Some((a, b)) if n > 0 => (a.min(n - 1), b.min(n - 1)),
        _ => (0, n.saturating_sub(1)),
    }
}

/// Decide what to do with one page, given the mode.
pub fn decide(info: &PageInfo, mode: pdftomobi_core::OcrMode) -> PageDecision {
    use pdftomobi_core::OcrMode;
    match mode {
        OcrMode::Always => PageDecision::Ocr,
        OcrMode::Never => {
            if info.text_chars > 0 {
                PageDecision::Text
            } else {
                PageDecision::Empty
            }
        }
        OcrMode::Auto => {
            if info.has_usable_text() {
                PageDecision::Text
            } else if info.has_no_text() {
                PageDecision::Ocr
            } else {
                PageDecision::Compare
            }
        }
    }
}

/// Glyph heights that are wild outliers are almost always drop caps. They must
/// not drag the median font size around, or the indent threshold moves with
/// them.
pub fn typical_font_size(words: &[Word]) -> f32 {
    let sizes: Vec<f32> = words.iter().map(|w| w.font_size).collect();
    let med = median(&sizes);
    let spread = mad(&sizes, med);
    if spread <= 0.0 {
        return med.max(1.0);
    }
    let kept: Vec<f32> = sizes
        .iter()
        .copied()
        .filter(|s| (s - med).abs() <= spread * 4.0)
        .collect();
    median(&kept).max(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdftomobi_core::OcrMode;

    fn info(text_chars: usize, image_objects: usize) -> PageInfo {
        PageInfo {
            index: 0,
            width_pt: 612.0,
            height_pt: 792.0,
            text_chars,
            image_objects,
        }
    }

    #[test]
    fn a_full_body_page_uses_its_text_layer() {
        // Typical Turkish book body page: text plus decorative images.
        assert_eq!(decide(&info(1700, 2), OcrMode::Auto), PageDecision::Text);
    }

    #[test]
    fn a_blank_page_goes_straight_to_ocr() {
        assert_eq!(decide(&info(0, 1), OcrMode::Auto), PageDecision::Ocr);
    }

    #[test]
    fn a_sparse_title_page_is_compared_not_assumed() {
        // lady-susan.pdf page 0: 27 good characters and 4 images. Sending this
        // straight to OCR is what turned "Jane" into "Fane".
        assert_eq!(decide(&info(27, 4), OcrMode::Auto), PageDecision::Compare);
        assert_eq!(decide(&info(118, 7), OcrMode::Auto), PageDecision::Compare);
    }

    #[test]
    fn ocr_only_wins_when_it_finds_substantially_more() {
        // Title page: text layer 27, OCR reads about the same. Keep the text.
        assert!(!ocr_wins(27, 25));
        assert!(!ocr_wins(27, 30));
        // A scan with a stray text fragment over an unread image.
        assert!(ocr_wins(27, 1900));
        assert!(ocr_wins(0, 1200));
        // Neither found anything.
        assert!(!ocr_wins(0, 0));
    }

    #[test]
    fn modes_override_the_decision() {
        assert_eq!(decide(&info(1700, 0), OcrMode::Always), PageDecision::Ocr);
        assert_eq!(decide(&info(0, 1), OcrMode::Never), PageDecision::Empty);
        assert_eq!(decide(&info(27, 0), OcrMode::Never), PageDecision::Text);
    }
}
