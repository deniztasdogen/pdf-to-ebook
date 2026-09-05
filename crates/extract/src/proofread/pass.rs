//! The proofreading pass over a whole [`Document`].
//!
//! It deliberately runs on a document that came **off disk** — the markdown
//! layer 2 has just written, or a markdown file handed to the tool directly —
//! rather than on the structures layout analysis happened to leave in memory.
//! What the model sees is then exactly what a person would see if they opened
//! the file, and its output is another markdown file that can be diffed against
//! the first one.
//!
//! The only piece of extraction provenance that survives that round trip is
//! `any_ocr`, and it is the only one the filter modes ever needed: span-level
//! provenance was never tracked, so `auto` has always meant "this document has
//! OCR'd pages in it", not "this paragraph came from one".

use super::{Correction, Proofreader, Stats};
use crate::pipeline::is_suspicious;
use pdf_to_ebook_core::{
    Block, Config, Document, Event, LlmMode, Reporter, Result, Span, Stage,
};

#[derive(Default)]
pub struct Pass {
    pub stats: Stats,
    /// Accepted and rejected corrections, for the run report.
    pub corrections: Vec<Correction>,
}

impl Pass {
    /// Whether the model actually answered. False when the mode was `never`,
    /// nothing matched the filter, or ollama could not be reached — all three
    /// mean the document was not touched and there is nothing new to write.
    pub fn ran(&self) -> bool {
        self.stats.considered > 0
    }
}

/// Whether the model should be consulted at all for this run.
///
/// `any_ocr` is false for a markdown input, which is what keeps the default
/// (`auto`) from calling out to ollama for a file that never went near OCR.
pub fn wants_llm(mode: LlmMode, any_ocr: bool) -> bool {
    match mode {
        LlmMode::Never => false,
        LlmMode::Always | LlmMode::Suspicious => true,
        LlmMode::Auto => any_ocr,
    }
}

/// Proofread `doc` in place. Accepted corrections are applied; refused ones are
/// reported and the original text is kept.
pub fn proofread_document(
    cfg: &Config,
    doc: &mut Document,
    any_ocr: bool,
    rep: &dyn Reporter,
) -> Result<Pass> {
    let mut pass = Pass::default();
    if !wants_llm(cfg.llm, any_ocr) {
        return Ok(pass);
    }

    let targets: Vec<Target> = editable(doc)
        .into_iter()
        .filter(|t| match cfg.llm {
            // Auto has already been decided document-wide, above.
            LlmMode::Always | LlmMode::Auto => true,
            LlmMode::Suspicious => is_suspicious(&t.text, None),
            LlmMode::Never => false,
        })
        .collect();
    if targets.is_empty() {
        rep.event(Event::Info(
            "no paragraphs matched the proofreading filter".to_string(),
        ));
        return Ok(pass);
    }

    rep.event(Event::Stage(Stage::Proofreading));
    let mut pr = Proofreader::new(&cfg.ollama_urls, &cfg.llm_model, &cfg.lang, cfg.cache);
    match pr.preflight() {
        // A missing model must not throw away a good extraction. That holds
        // per server too: one machine of three being off is a warning and a
        // slower run, not a failure.
        Err(e) => {
            rep.event(Event::Warning(format!("skipping proofreading — {e}")));
            return Ok(pass);
        }
        Ok(warnings) => {
            for w in warnings {
                rep.event(Event::Warning(w));
            }
        }
    }
    if let Some(dir) = pr.prompt_source() {
        // A run whose prompt is not the one in the repo must say so: every
        // number in findings.md is a number about a particular prompt.
        rep.event(Event::Info(format!("prompt loaded from {}", dir.display())));
    }
    if pr.endpoints().len() > 1 {
        rep.event(Event::Info(format!(
            "proofreading on {} servers: {}",
            pr.endpoints().len(),
            pr.endpoints().join(", ")
        )));
    }

    let texts: Vec<String> = targets.iter().map(|t| t.text.clone()).collect();
    let cancelled = || rep.cancelled();
    let res = pr.run(
        &texts,
        &mut pass.stats,
        |d, t| rep.event(Event::Progress { done: d, total: t }),
        &cancelled,
    )?;
    for (target, c) in targets.iter().zip(&res) {
        if c.accepted {
            if let Block::Paragraph { spans } = &mut doc.blocks[target.block] {
                if let Some(Span::Text(t)) = spans.get_mut(target.span) {
                    *t = c.corrected.clone();
                }
            }
        }
    }
    pass.corrections = res;
    Ok(pass)
}

/// Where one editable run of text lives in the document.
struct Target {
    block: usize,
    span: usize,
    text: String,
}

/// Every text span the model is allowed to rewrite.
///
/// Paragraph text only. Headings are left alone: they are short, they are what
/// the EPUB's chapter list and navigation are built from, and a "correction"
/// there is far more likely to be a rewrite than a typo fix.
fn editable(doc: &Document) -> Vec<Target> {
    let mut out = Vec::new();
    for (bi, b) in doc.blocks.iter().enumerate() {
        if let Block::Paragraph { spans } = b {
            for (si, s) in spans.iter().enumerate() {
                if let Span::Text(t) = s {
                    if !t.trim().is_empty() {
                        out.push(Target {
                            block: bi,
                            span: si,
                            text: t.clone(),
                        });
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf_to_ebook_core::progress::Silent;
    use pdf_to_ebook_core::Meta;

    fn doc(blocks: Vec<Block>) -> Document {
        Document {
            meta: Meta::default(),
            blocks,
        }
    }

    fn para(t: &str) -> Block {
        Block::Paragraph {
            spans: vec![Span::text(t)],
        }
    }

    #[test]
    fn auto_asks_for_the_model_only_when_the_run_did_its_own_ocr() {
        assert!(wants_llm(LlmMode::Auto, true));
        assert!(!wants_llm(LlmMode::Auto, false));
        // The other three do not care where the text came from.
        assert!(wants_llm(LlmMode::Always, false));
        assert!(wants_llm(LlmMode::Suspicious, false));
        assert!(!wants_llm(LlmMode::Never, true));
    }

    #[test]
    fn only_paragraph_text_is_offered_to_the_model() {
        let d = doc(vec![
            Block::Heading {
                level: 1,
                text: "A chapter".into(),
            },
            Block::Paragraph {
                spans: vec![
                    Span::text("Before the break."),
                    Span::PageBreak {
                        label: "7".into(),
                    },
                    Span::Emphasis("emphasised".into()),
                    Span::text("After the break."),
                ],
            },
            Block::Separator,
        ]);
        let t: Vec<String> = editable(&d).into_iter().map(|t| t.text).collect();
        assert_eq!(t, vec!["Before the break.", "After the break."]);
    }

    #[test]
    fn empty_spans_are_not_sent_to_the_model() {
        let d = doc(vec![para("   "), para("Real text.")]);
        assert_eq!(editable(&d).len(), 1);
    }

    #[test]
    fn never_asks_nothing_and_leaves_the_document_alone() {
        let mut d = doc(vec![para("Untouched.")]);
        let before = d.clone();
        let mut cfg = Config::new("book.pdf");
        cfg.llm = LlmMode::Never;
        cfg.cache = false;
        let pass = proofread_document(&cfg, &mut d, true, &Silent).unwrap();
        assert!(!pass.ran());
        assert_eq!(d, before);
    }

    #[test]
    fn an_unreachable_server_warns_and_keeps_the_text() {
        let mut d = doc(vec![para("Untouched.")]);
        let before = d.clone();
        let mut cfg = Config::new("book.pdf");
        cfg.llm = LlmMode::Always;
        // Port 1 is reserved and nothing can be listening on it.
        cfg.ollama_urls = vec!["http://127.0.0.1:1".to_string()];
        // Never touch the real reply cache from a test.
        cfg.cache = false;
        let pass = proofread_document(&cfg, &mut d, true, &Silent).unwrap();
        assert!(!pass.ran());
        assert_eq!(d, before);
    }
}
