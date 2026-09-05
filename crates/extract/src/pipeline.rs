//! The layer 3 pipeline: PDF in, [`Document`] out.

use crate::layout;
use crate::model::{PageLayout, PageSource};
use crate::ocr::{OcrEngine, TesseractCli};
use crate::pdf::{decide, ocr_wins, PageDecision, PdfDoc};
use pdftomobi_core::{
    Config, Document, Error, Event, Meta, OcrMode, Reporter, Result, Stage,
};
use rayon::prelude::*;

pub struct ExtractOutcome {
    pub document: Document,
    pub pages: Vec<PageLayout>,
    pub text_pages: usize,
    pub ocr_pages: usize,
    pub empty_pages: Vec<usize>,
    pub ambiguous_joins: Vec<String>,
}

pub fn extract(cfg: &Config, rep: &dyn Reporter) -> Result<ExtractOutcome> {
    rep.event(Event::Stage(Stage::Opening));
    let doc = PdfDoc::open(&cfg.input)?;

    rep.event(Event::Stage(Stage::Classifying));
    let infos = doc.survey(cfg.pages)?;
    if infos.is_empty() {
        return Err(Error::NoText(cfg.input.clone()));
    }
    let decisions: Vec<PageDecision> = infos.iter().map(|i| decide(i, cfg.ocr)).collect();
    // Ambiguous pages are counted on both sides here; the real split is
    // reported again once the comparison has actually been made.
    rep.event(Event::Classified {
        text_pages: decisions
            .iter()
            .filter(|d| matches!(d, PageDecision::Text | PageDecision::Compare))
            .count(),
        ocr_pages: decisions
            .iter()
            .filter(|d| matches!(d, PageDecision::Ocr | PageDecision::Compare))
            .count(),
    });
    let needs_ocr = decisions
        .iter()
        .filter(|d| matches!(d, PageDecision::Ocr | PageDecision::Compare))
        .count();

    // Preflight the OCR engine only if we are actually going to need it, so a
    // pure text-layer book works on a machine without tesseract.
    let engine = TesseractCli::default();
    if needs_ocr > 0 {
        if let Err(e) = engine.preflight(&cfg.lang) {
            if cfg.ocr == OcrMode::Always {
                return Err(e);
            }
            // Auto mode: warn and carry on with whatever text exists.
            rep.event(Event::Warning(format!(
                "{needs_ocr} page(s) need OCR but it is unavailable. {e}"
            )));
        }
    }

    // ---- pass 1: words per page -------------------------------------------
    // Text extraction is cheap (419 pages in under a second), so it runs first
    // and serially. OCR is the expensive part and is parallelised below.
    rep.event(Event::Stage(Stage::Extracting));
    struct Raw {
        index: usize,
        source: PageSource,
        words: Vec<crate::model::Word>,
        width: f32,
        height: f32,
        /// Text-layer words held aside, for an ambiguous page whose OCR result
        /// still has to be compared against them.
        text_alternative: Option<Vec<crate::model::Word>>,
    }
    let mut raw: Vec<Raw> = Vec::with_capacity(infos.len());
    let mut ocr_todo: Vec<usize> = Vec::new();

    for (n, (info, decision)) in infos.iter().zip(&decisions).enumerate() {
        if rep.cancelled() {
            return Err(Error::Cancelled);
        }
        let mut r = Raw {
            index: info.index,
            source: PageSource::Empty,
            words: Vec::new(),
            width: info.width_pt,
            height: info.height_pt,
            text_alternative: None,
        };
        match decision {
            PageDecision::Text => {
                let (words, w, h) = doc.text_words(info.index)?;
                r.source = PageSource::Text;
                r.words = words;
                r.width = w;
                r.height = h;
            }
            PageDecision::Ocr => {
                r.source = PageSource::Ocr;
                ocr_todo.push(n);
            }
            PageDecision::Compare => {
                // Keep the text layer, and let OCR try to beat it.
                let (words, w, h) = doc.text_words(info.index)?;
                r.source = PageSource::Text;
                r.width = w;
                r.height = h;
                r.text_alternative = Some(words);
                ocr_todo.push(n);
            }
            PageDecision::Empty => {}
        }
        raw.push(r);
        rep.event(Event::Progress {
            done: n + 1,
            total: infos.len(),
        });
    }

    // ---- OCR the pages that need it ---------------------------------------
    if !ocr_todo.is_empty() {
        rep.event(Event::Stage(Stage::Ocr));
        // Render serially — pdfium is not shared across threads here — then
        // recognise in parallel, which is where the second per page goes.
        let mut images = Vec::with_capacity(ocr_todo.len());
        for (k, n) in ocr_todo.iter().enumerate() {
            if rep.cancelled() {
                return Err(Error::Cancelled);
            }
            let idx = infos[*n].index;
            match doc.render_page(idx, cfg.dpi) {
                Ok(img) => images.push((*n, idx, img)),
                Err(e) => rep.event(Event::Warning(format!("could not render page {idx}: {e}"))),
            }
            rep.event(Event::Progress {
                done: k + 1,
                total: ocr_todo.len() * 2,
            });
        }
        let done = std::sync::atomic::AtomicUsize::new(0);
        let total = ocr_todo.len();
        let results: Vec<(usize, Result<Vec<crate::model::Word>>)> = images
            .par_iter()
            .map(|(n, _idx, img)| {
                let r = engine.recognise(img, &cfg.lang, cfg.dpi);
                let d = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                rep.event(Event::Progress {
                    done: total + d,
                    total: total * 2,
                });
                (*n, r)
            })
            .collect();
        for (n, r) in results {
            match r {
                Ok(ocr_words) => {
                    match raw[n].text_alternative.take() {
                        // Ambiguous page: keep whichever read better.
                        Some(text_words) => {
                            let tc: usize = text_words.iter().map(|w| w.text.chars().count()).sum();
                            let oc: usize = ocr_words.iter().map(|w| w.text.chars().count()).sum();
                            if ocr_wins(tc, oc) {
                                raw[n].source = PageSource::Ocr;
                                raw[n].words = ocr_words;
                            } else {
                                raw[n].source = PageSource::Text;
                                raw[n].words = text_words;
                            }
                        }
                        None => raw[n].words = ocr_words,
                    }
                }
                Err(e) => {
                    // Fall back to whatever the text layer had.
                    if let Some(text_words) = raw[n].text_alternative.take() {
                        raw[n].source = PageSource::Text;
                        raw[n].words = text_words;
                    }
                    rep.event(Event::Warning(format!(
                        "OCR failed on page {}: {e}",
                        infos[n].index
                    )));
                }
            }
        }
    }
    // Any ambiguous page whose render failed outright still needs its text.
    for r in raw.iter_mut() {
        if let Some(text_words) = r.text_alternative.take() {
            r.source = PageSource::Text;
            r.words = text_words;
        }
    }
    let text_pages = raw.iter().filter(|r| r.source == PageSource::Text).count();
    let ocr_pages = raw.iter().filter(|r| r.source == PageSource::Ocr).count();

    // ---- pass 2: find running furniture across pages ----------------------
    rep.event(Event::Stage(Stage::Analysing));
    let single_page = raw.len() == 1;
    let mut scan = layout::chrome::BandScan::new();
    for r in &raw {
        if r.words.is_empty() {
            continue;
        }
        let coarse = layout::coarse_lines(&r.words);
        scan.observe(&coarse, r.height);
    }
    let repeated = scan.repeated();

    // Column layout is a property of the document, so settle it once across all
    // pages before analysing any of them.
    let candidates: Vec<(Vec<f32>, f32, f32)> = raw
        .iter()
        .map(|r| {
            if r.words.is_empty() {
                (Vec::new(), 0.0, 0.0)
            } else {
                let (x0, x1) = layout::columns::body_extent(&r.words);
                (layout::columns::candidate_splits(&r.words), x0, x1)
            }
        })
        .collect();
    let plan = layout::columns::plan(
        &candidates
            .iter()
            .filter(|(_, x0, x1)| x1 > x0)
            .cloned()
            .collect::<Vec<_>>(),
    );
    if plan.count > 0 {
        rep.event(Event::Info(format!(
            "document reads as {} column(s)",
            plan.count + 1
        )));
    }
    if !repeated.is_empty() {
        rep.event(Event::Info(format!(
            "removing {} repeated header/footer line(s): {}",
            repeated.len(),
            repeated.join(" | ")
        )));
    }

    // ---- pass 3: analyse each page ----------------------------------------
    let mut pages: Vec<PageLayout> = Vec::with_capacity(raw.len());
    let mut empty_pages = Vec::new();
    for r in raw {
        let had_words = !r.words.is_empty();
        let page = layout::analyse_page(
            r.index,
            r.source,
            r.words,
            r.width,
            r.height,
            &repeated,
            single_page,
            cfg.crop,
            Some(&plan),
        );
        if !had_words || page.columns.is_empty() {
            empty_pages.push(r.index);
        }
        pages.push(page);
    }

    if pages.iter().all(|p| p.columns.is_empty()) {
        return Err(Error::NoText(cfg.input.clone()));
    }

    // ---- assemble the document --------------------------------------------
    let meta = Meta {
        title: cfg
            .title
            .clone()
            .or_else(|| doc.metadata_title())
            .unwrap_or_else(|| cfg.out_stem.clone()),
        author: cfg.author.clone().or_else(|| doc.metadata_author()),
        language: cfg.bcp47().to_string(),
        source: cfg
            .input
            .file_name()
            .map(|s| s.to_string_lossy().to_string()),
        page_count: Some(pages.len()),
        generator: Some(format!("pdftomobi {}", env!("CARGO_PKG_VERSION"))),
    };
    rep.event(Event::Info(format!(
        "used the text layer on {text_pages} page(s), OCR on {ocr_pages}"
    )));
    let assembled = layout::assemble(&pages, meta, cfg.page_breaks);

    // Proofreading is *not* done here. It runs in layer 2, over the markdown
    // this document is about to be written to, so the model's input is the same
    // file a person would edit by hand. See `proofread::pass`.
    Ok(ExtractOutcome {
        document: assembled.document,
        pages,
        text_pages,
        ocr_pages,
        empty_pages,
        ambiguous_joins: assembled.ambiguous_joins,
    })
}

/// Cheap heuristics for "this paragraph probably has OCR damage".
///
/// Used by `--llm suspicious` to spend model time only where it is likely to
/// pay off. Deliberately language-agnostic: no dictionary, because the corpus
/// spans Turkish, Dutch and English.
pub fn is_suspicious(text: &str, confidence: Option<f32>) -> bool {
    if confidence.map(|c| c < 85.0).unwrap_or(false) {
        return true;
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return false;
    }
    // Characters that almost never occur in running prose.
    let odd = chars
        .iter()
        .filter(|c| matches!(**c, '·' | '¬' | '|' | '~' | '^' | '\u{fffd}'))
        .count();
    if odd > 0 {
        return true;
    }
    // A long run with no spaces means words were welded together.
    let longest_word = text
        .split_whitespace()
        .map(|w| w.chars().count())
        .max()
        .unwrap_or(0);
    if longest_word > 24 {
        return true;
    }
    // A lot of single-letter tokens means words were split apart.
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.len() >= 8 {
        let singles = tokens.iter().filter(|t| t.chars().count() == 1).count();
        if singles * 5 > tokens.len() {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::is_suspicious;

    #[test]
    fn flags_interpunct_and_not_sign_noise() {
        assert!(is_suspicious("Onu sudan· cikarmaya calisirken", None));
        assert!(is_suspicious("takemyad¬vice", None));
    }

    #[test]
    fn flags_welded_words() {
        assert!(is_suspicious("entvoiceagainAndifyoutakemyadvicenow", None));
    }

    #[test]
    fn flags_low_confidence() {
        assert!(is_suspicious("perfectly ordinary text here", Some(60.0)));
    }

    #[test]
    fn leaves_clean_prose_alone() {
        assert!(!is_suspicious(
            "Son bolumu sakin, ifadesiz bir ses tonuyla soyluyor.",
            Some(96.0)
        ));
        assert!(!is_suspicious("Aslinda hakli.", None));
    }
}
