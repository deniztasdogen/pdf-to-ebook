//! **Layer 2: orchestration.**
//!
//! Owns the order of operations and nothing else. For a PDF that order is:
//! layer 3 extracts, the markdown is written to disk, the model proofreads
//! *that file* into a second one, and layer 4 reads the last markdown on disk
//! back and builds the ebooks.
//!
//! ```text
//! book.pdf ──► book.md ──► book.proofread.md ──► book.epub ──► book.mobi
//!              raw         only if the model ran
//! ```
//!
//! The round trip through real files is intentional. It makes the markdown the
//! genuine interface between the two halves rather than an internal detail, so
//! a bad extraction can be fixed by hand and rebuilt without re-running OCR —
//! and it makes the model's work reviewable, because the two files can simply
//! be diffed.
//!
//! A markdown input is the other half of that same bargain: it enters at the
//! boundary, so nothing extracts or OCRs it. Proofreading is the exception —
//! it reads and writes markdown, so it can run over a markdown input too, on
//! the same terms.

mod report;

use pdf_to_ebook_core::{
    Config, Document, Error, Event, InputKind, OutputFormat, Reporter, Result, Stage,
};
use pdf_to_ebook_extract as extract;
use std::path::{Path, PathBuf};

pub use report::render as render_report;

pub struct Outcome {
    pub document: Document,
    pub written: Vec<PathBuf>,
    /// The markdown as extracted, before the model saw it. For a markdown input
    /// this is the input itself.
    pub markdown_path: Option<PathBuf>,
    /// The proofread markdown, written only when the model actually ran.
    pub proofread_path: Option<PathBuf>,

    pub text_pages: usize,
    pub ocr_pages: usize,
    pub empty_pages: Vec<usize>,

    pub paragraphs: usize,
    pub headings: usize,
    pub page_markers: usize,
    pub characters: usize,

    pub llm: extract::proofread::Stats,
    pub corrections: Vec<extract::proofread::Correction>,
    pub ambiguous_joins: Vec<String>,
    /// (text, reason, number of pages it was removed from)
    pub dropped: Vec<(String, String, usize)>,
    pub warnings: Vec<String>,
}

/// A reporter wrapper that also collects warnings, so the report can list them.
struct Collect<'a> {
    inner: &'a dyn Reporter,
    warnings: std::sync::Mutex<Vec<String>>,
}

impl Reporter for Collect<'_> {
    fn event(&self, e: Event) {
        if let Event::Warning(w) = &e {
            self.warnings.lock().unwrap().push(w.clone());
        }
        self.inner.event(e);
    }
    fn cancelled(&self) -> bool {
        self.inner.cancelled()
    }
}

impl<'a> Collect<'a> {
    fn new(inner: &'a dyn Reporter) -> Self {
        Collect {
            inner,
            warnings: std::sync::Mutex::new(Vec::new()),
        }
    }
}

pub fn run(cfg: &Config, rep: &dyn Reporter) -> Result<Outcome> {
    match validate(cfg)? {
        InputKind::Pdf => run_pdf(cfg, rep),
        InputKind::Markdown => run_markdown(cfg, rep),
    }
}

/// PDF in: layers 3 and 4 both run.
fn run_pdf(cfg: &Config, rep: &dyn Reporter) -> Result<Outcome> {
    let collect = Collect::new(rep);

    // ---- layer 3: PDF -> Document ----------------------------------------
    let ex = extract::extract(cfg, &collect)?;

    let mut written: Vec<PathBuf> = Vec::new();
    std::fs::create_dir_all(&cfg.out_dir).map_err(|e| Error::io(&cfg.out_dir, e))?;

    // ---- the layer 3 / layer 4 boundary: a markdown file ------------------
    let mut markdown_path = None;
    if cfg.wants_markdown() {
        collect.event(Event::Stage(Stage::WritingMarkdown));
        let md = extract::markdown::write(&ex.document);
        let p = cfg.output_path(OutputFormat::Markdown);
        std::fs::write(&p, &md).map_err(|e| Error::io(&p, e))?;
        collect.event(Event::Wrote(p.clone()));
        written.push(p.clone());
        markdown_path = Some(p);
    }

    // ---- proofread the markdown into a second markdown --------------------
    // The model reads the file we just wrote, not the document still in memory,
    // and its answer becomes a file of its own. Neither half is thrown away:
    // `book.md` is what the PDF said, `book.proofread.md` is what the model made
    // of it, and `diff` between them is the whole audit.
    let pass = proofread_stage(cfg, &collect, markdown_path.as_deref(), ex.ocr_pages > 0)?;
    let mut document = ex.document;
    if let Some((path, proofread)) = &pass.written {
        written.push(path.clone());
        document = proofread.clone();
    }

    if cfg.formats.contains(&OutputFormat::Json) {
        let p = cfg.output_path(OutputFormat::Json);
        let json = serde_json::to_string_pretty(&ex.pages)
            .map_err(|e| Error::Other(anyhow::anyhow!("serialising layout: {e}")))?;
        std::fs::write(&p, json).map_err(|e| Error::io(&p, e))?;
        collect.event(Event::Wrote(p.clone()));
        written.push(p);
    }

    // ---- layer 4: markdown -> EPUB -> Kindle ------------------------------
    if wants_ebook(cfg) {
        let md_path = pass
            .path()
            .or(markdown_path.as_deref())
            .ok_or_else(|| Error::Other(anyhow::anyhow!("markdown is required to build an EPUB")))?;
        // Read the markdown back, so layer 4 works only from the file.
        let from_disk = pdf_to_ebook_ebook::read_markdown(md_path)?;
        build_ebooks(cfg, &collect, &from_disk, &mut written)?;
    }

    let dropped = summarise_dropped(&ex.pages);
    let outcome = Outcome {
        paragraphs: document.paragraph_count(),
        headings: document.heading_count(),
        page_markers: document.page_break_count(),
        characters: document.char_count(),
        document,
        written,
        markdown_path,
        proofread_path: pass.path().map(|p| p.to_path_buf()),
        text_pages: ex.text_pages,
        ocr_pages: ex.ocr_pages,
        empty_pages: ex.empty_pages,
        llm: pass.stats,
        corrections: pass.corrections,
        ambiguous_joins: ex.ambiguous_joins,
        dropped,
        warnings: collect.warnings.into_inner().unwrap(),
    };
    finish(cfg, rep, outcome)
}

/// Markdown in: the input already *is* the layer 3 / layer 4 boundary, so
/// nothing is extracted, OCR'd or analysed — the flags governing those are
/// inert here. Proofreading is not one of them: it takes a markdown file and
/// gives one back, so it runs over the input on exactly the same terms as it
/// runs over a markdown file we wrote ourselves. `--llm auto` still does
/// nothing, because no OCR happened.
fn run_markdown(cfg: &Config, rep: &dyn Reporter) -> Result<Outcome> {
    let collect = Collect::new(rep);
    std::fs::create_dir_all(&cfg.out_dir).map_err(|e| Error::io(&cfg.out_dir, e))?;

    collect.event(Event::Stage(Stage::ReadingMarkdown));
    let mut document = pdf_to_ebook_ebook::read_markdown(&cfg.input)?;
    apply_metadata_overrides(cfg, &mut document);
    collect.event(Event::Info(format!(
        "{} paragraph(s), {} heading(s)",
        document.paragraph_count(),
        document.heading_count()
    )));

    let mut written: Vec<PathBuf> = Vec::new();

    // `--format md` on a markdown input means "also give me the normalised
    // markdown", which is only meaningful somewhere other than the input path.
    if cfg.formats.contains(&OutputFormat::Markdown) {
        let p = cfg.output_path(OutputFormat::Markdown);
        if same_path(&p, &cfg.input) {
            collect.event(Event::Warning(format!(
                "skipped the markdown output: it would overwrite the input \
                 ({}). Use --out to write it elsewhere.",
                cfg.input.display()
            )));
        } else {
            collect.event(Event::Stage(Stage::WritingMarkdown));
            std::fs::write(&p, extract::markdown::write(&document))
                .map_err(|e| Error::io(&p, e))?;
            collect.event(Event::Wrote(p.clone()));
            written.push(p);
        }
    }

    // No OCR happened, so `auto` — the default — asks for nothing here.
    let pass = proofread_stage(cfg, &collect, Some(&cfg.input), false)?;
    if let Some((path, proofread)) = &pass.written {
        written.push(path.clone());
        document = proofread.clone();
    }

    if wants_ebook(cfg) {
        match pass.path() {
            // Read the proofread markdown back, so layer 4 still works only
            // from a file, as it does for a PDF.
            Some(p) => {
                let from_disk = pdf_to_ebook_ebook::read_markdown(p)?;
                build_ebooks(cfg, &collect, &from_disk, &mut written)?;
            }
            None => build_ebooks(cfg, &collect, &document, &mut written)?,
        }
    }

    let outcome = Outcome {
        paragraphs: document.paragraph_count(),
        headings: document.heading_count(),
        page_markers: document.page_break_count(),
        characters: document.char_count(),
        document,
        written,
        markdown_path: Some(cfg.input.clone()),
        proofread_path: pass.path().map(|p| p.to_path_buf()),
        text_pages: 0,
        ocr_pages: 0,
        empty_pages: Vec::new(),
        llm: pass.stats,
        corrections: pass.corrections,
        ambiguous_joins: Vec::new(),
        dropped: Vec::new(),
        warnings: collect.warnings.into_inner().unwrap(),
    };
    finish(cfg, rep, outcome)
}

/// What the proofreading stage produced.
#[derive(Default)]
struct Proofread {
    stats: extract::proofread::Stats,
    corrections: Vec<extract::proofread::Correction>,
    /// The file it wrote and the document that went into it. `None` whenever
    /// the model did not answer — mode `never`, nothing matching the filter, or
    /// an unreachable ollama — in which case the raw markdown stands as the
    /// only boundary file and the ebook is built from that.
    written: Option<(PathBuf, Document)>,
}

impl Proofread {
    fn path(&self) -> Option<&Path> {
        self.written.as_ref().map(|(p, _)| p.as_path())
    }
}

/// Proofread `markdown` — a file on disk — into `<stem>.proofread.md`.
///
/// The document is read **back off disk** rather than taken from memory. That
/// is the same bargain the markdown boundary already makes: the model is given
/// precisely what a person editing the file would see, and what it hands back
/// is a file that can be edited in turn.
fn proofread_stage(
    cfg: &Config,
    rep: &dyn Reporter,
    markdown: Option<&Path>,
    any_ocr: bool,
) -> Result<Proofread> {
    let mut out = Proofread::default();
    if !extract::proofread::wants_llm(cfg.llm, any_ocr) {
        return Ok(out);
    }
    let Some(src) = markdown else {
        // Only reachable with `--format json` alone: nothing else asked for a
        // markdown file, so there is nothing for the model to read or correct.
        rep.event(Event::Warning(
            "proofreading needs a markdown file; add `md` to --format".to_string(),
        ));
        return Ok(out);
    };

    let mut document = pdf_to_ebook_ebook::read_markdown(src)?;
    // The overrides were applied to the document this file came from; applying
    // them again keeps them in the proofread file, which is what layer 4 reads.
    apply_metadata_overrides(cfg, &mut document);
    let pass = extract::proofread::proofread_document(cfg, &mut document, any_ocr, rep)?;
    let ran = pass.ran();
    out.stats = pass.stats;
    out.corrections = pass.corrections;
    if !ran {
        return Ok(out);
    }

    let p = cfg.out_dir.join(format!("{}.proofread.md", cfg.out_stem));
    if same_path(&p, src) {
        rep.event(Event::Warning(format!(
            "skipped the proofread markdown: it would overwrite {}",
            src.display()
        )));
        return Ok(out);
    }
    rep.event(Event::Stage(Stage::WritingMarkdown));
    std::fs::write(&p, extract::markdown::write(&document)).map_err(|e| Error::io(&p, e))?;
    rep.event(Event::Wrote(p.clone()));
    out.written = Some((p, document));
    Ok(out)
}

fn wants_ebook(cfg: &Config) -> bool {
    cfg.formats.iter().any(|f| f.is_ebook())
}

/// **Layer 4.** `doc` must be a document that came off disk — for a PDF that is
/// the markdown we just wrote, read back; for a markdown input it is the input
/// itself. Building from anything else would quietly turn the markdown back
/// into an internal detail.
fn build_ebooks(
    cfg: &Config,
    rep: &dyn Reporter,
    doc: &Document,
    written: &mut Vec<PathBuf>,
) -> Result<()> {
    let wants_epub = cfg.formats.contains(&OutputFormat::Epub);
    let kindle: Vec<OutputFormat> = cfg
        .formats
        .iter()
        .copied()
        .filter(|f| matches!(f, OutputFormat::Mobi | OutputFormat::Azw3))
        .collect();

    // A Kindle format needs an EPUB to convert from, even if none was asked for.
    rep.event(Event::Stage(Stage::BuildingEpub));
    let (epub_path, epub_is_temporary) = if wants_epub {
        (cfg.output_path(OutputFormat::Epub), false)
    } else {
        (cfg.out_dir.join(format!("{}.tmp.epub", cfg.out_stem)), true)
    };
    pdf_to_ebook_ebook::epub::write(
        doc,
        &epub_path,
        &pdf_to_ebook_ebook::epub::EpubOptions {
            page_breaks: cfg.page_breaks,
        },
    )?;
    if wants_epub {
        rep.event(Event::Wrote(epub_path.clone()));
        written.push(epub_path.clone());
    }

    for f in kindle {
        rep.event(Event::Stage(Stage::Converting));
        let out = cfg.output_path(f);
        match pdf_to_ebook_ebook::kindle_from_epub(&epub_path, &out, f) {
            Ok(()) => {
                rep.event(Event::Wrote(out.clone()));
                written.push(out);
                if f == OutputFormat::Mobi
                    && cfg.page_breaks == pdf_to_ebook_core::PageBreakMode::Anchors
                {
                    // Verified: calibre's KF8 writer discards these.
                    rep.event(Event::Warning(
                        "MOBI/AZW3 does not keep the EPUB page markers. \
                         The EPUB does; use --page-breaks hard if you need \
                         real breaks on a Kindle."
                            .to_string(),
                    ));
                }
            }
            Err(e) => rep.event(Event::Warning(format!("{f:?} conversion failed: {e}"))),
        }
    }
    if epub_is_temporary {
        let _ = std::fs::remove_file(&epub_path);
    }
    Ok(())
}

/// `--title` and `--author` are explicit overrides, so they beat the front
/// matter. `--lang` has a default and cannot be told apart from one, so it only
/// fills in a language the file does not already state.
fn apply_metadata_overrides(cfg: &Config, doc: &mut Document) {
    if let Some(t) = &cfg.title {
        doc.meta.title = t.clone();
    }
    if cfg.author.is_some() {
        doc.meta.author = cfg.author.clone();
    }
    if doc.meta.language.trim().is_empty() || doc.meta.language == "und" {
        doc.meta.language = cfg.bcp47().to_string();
    }
    if doc.meta.title.trim().is_empty() || doc.meta.title == "Untitled" {
        doc.meta.title = cfg.out_stem.clone();
    }
    if doc.meta.source.is_none() {
        doc.meta.source = cfg
            .input
            .file_name()
            .map(|s| s.to_string_lossy().to_string());
    }
}

/// Write the report, if asked for, and close the run out.
fn finish(cfg: &Config, rep: &dyn Reporter, mut outcome: Outcome) -> Result<Outcome> {
    if cfg.report {
        let text = report::render(cfg, &outcome);
        let p = cfg.out_dir.join(format!("{}.report.md", cfg.out_stem));
        std::fs::write(&p, text).map_err(|e| Error::io(&p, e))?;
        rep.event(Event::Wrote(p.clone()));
        outcome.written.push(p);
    }
    rep.event(Event::Stage(Stage::Done));
    Ok(outcome)
}

/// Whether two paths name the same file. The output usually does not exist
/// yet, so only the directory can be canonicalised.
fn same_path(a: &Path, b: &Path) -> bool {
    fn norm(p: &Path) -> PathBuf {
        let dir = match p.parent() {
            Some(d) if !d.as_os_str().is_empty() => d,
            _ => Path::new("."),
        };
        match dir.canonicalize() {
            Ok(d) => d.join(p.file_name().unwrap_or_default()),
            Err(_) => p.to_path_buf(),
        }
    }
    norm(a) == norm(b)
}

fn validate(cfg: &Config) -> Result<InputKind> {
    if !cfg.input.exists() {
        return Err(Error::InputMissing(cfg.input.clone()));
    }
    let kind = cfg
        .input_kind()
        .ok_or_else(|| Error::UnsupportedInput(cfg.input.clone()))?;
    if cfg.formats.is_empty() {
        return Err(Error::Other(anyhow::anyhow!("no output format selected")));
    }
    if kind == InputKind::Markdown {
        if cfg.formats.contains(&OutputFormat::Json) {
            return Err(Error::Other(anyhow::anyhow!(
                "`json` dumps the PDF page layout, so it needs a PDF input"
            )));
        }
        if cfg.pages.is_some() {
            return Err(Error::Other(anyhow::anyhow!(
                "--pages selects PDF pages, so it needs a PDF input"
            )));
        }
    }
    if let Some((a, b)) = cfg.pages {
        if a > b {
            return Err(Error::Other(anyhow::anyhow!(
                "page range start ({}) is after its end ({})",
                a + 1,
                b + 1
            )));
        }
    }
    Ok(kind)
}

fn summarise_dropped(pages: &[extract::model::PageLayout]) -> Vec<(String, String, usize)> {
    use std::collections::HashMap;
    let mut counts: HashMap<(String, String), usize> = HashMap::new();
    for p in pages {
        for d in &p.dropped {
            let reason = format!("{:?}", d.reason);
            // Page numbers differ on every page; collapse them into one row.
            let key = if d.reason == extract::model::DropReason::PageNumber {
                ("(page numbers)".to_string(), reason)
            } else {
                (d.text.clone(), reason)
            };
            *counts.entry(key).or_insert(0) += 1;
        }
    }
    let mut v: Vec<(String, String, usize)> = counts
        .into_iter()
        .map(|((t, r), n)| (t, r, n))
        .collect();
    v.sort_by(|a, b| b.2.cmp(&a.2));
    v
}

/// Whether the external Kindle converter is installed. Exposed so a front end
/// can warn before starting a long run rather than after it.
pub fn converter_available() -> bool {
    pdf_to_ebook_ebook::mobi::available()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf_to_ebook_core::{LlmMode, OutputFormat};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};

    /// Keeps the warnings so a test can assert on what the run said, rather
    /// than only on what it wrote.
    struct Recorder(std::sync::Mutex<Vec<String>>);

    impl Reporter for Recorder {
        fn event(&self, e: Event) {
            if let Event::Warning(w) = e {
                self.0.lock().unwrap().push(w);
            }
        }
    }

    impl Recorder {
        fn new() -> Self {
            Recorder(std::sync::Mutex::new(Vec::new()))
        }
        fn warnings(&self) -> Vec<String> {
            self.0.lock().unwrap().clone()
        }
    }

    const SAMPLE: &str = "---\n\
                          title: Front Matter Title\n\
                          author: Front Matter Author\n\
                          language: nl\n\
                          ---\n\
                          \n\
                          # One\n\
                          \n\
                          First paragraph.\n\
                          \n\
                          <!-- page: 7 -->\n\
                          \n\
                          Second paragraph.\n";

    /// A fresh directory holding `input.md`, plus a config that writes into it.
    fn fixture(name: &str) -> (PathBuf, Config) {
        let dir = std::env::temp_dir().join(format!(
            "pdf-to-ebook-orch-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("input.md");
        std::fs::write(&input, SAMPLE).unwrap();
        let mut cfg = Config::new(&input);
        cfg.report = false;
        (dir, cfg)
    }

    /// A stand-in for ollama: just enough of `/api/tags` and `/api/chat` to
    /// drive the whole proofreading stage on a machine with no model on it.
    ///
    /// The thread is left running; the process exits out from under it at the
    /// end of the test binary, which is all the cleanup a listener needs.
    struct FakeOllama {
        url: String,
    }

    const FAKE_MODEL: &str = "fake-proofreader";

    impl FakeOllama {
        /// `fix` stands in for the model: it is handed each paragraph and
        /// returns the corrected one.
        fn start(fix: fn(&str) -> String) -> FakeOllama {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    match stream {
                        // A connection per thread: the proofreader can now have
                        // several requests in flight, and an accept loop that
                        // answers them one at a time would hide that.
                        Ok(mut s) => {
                            std::thread::spawn(move || serve(&mut s, fix));
                        }
                        Err(_) => break,
                    }
                }
            });
            FakeOllama { url }
        }
    }

    fn serve(stream: &mut TcpStream, fix: fn(&str) -> String) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request = String::new();
        if reader.read_line(&mut request).unwrap_or(0) == 0 {
            return;
        }
        let mut length = 0usize;
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                break;
            }
            let lower = header.to_ascii_lowercase();
            if let Some(v) = lower.strip_prefix("content-length:") {
                length = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; length];
        if length > 0 {
            reader.read_exact(&mut body).unwrap();
        }

        let reply = if request.contains("/api/chat") {
            let sent: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let user = sent["messages"].as_array().unwrap().last().unwrap()["content"]
                .as_str()
                .unwrap()
                .to_string();
            let asked: serde_json::Value = serde_json::from_str(&user).unwrap();
            let fixed: Vec<String> = asked["paragraphs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| fix(p.as_str().unwrap()))
                .collect();
            let content = serde_json::json!({ "paragraphs": fixed }).to_string();
            serde_json::json!({ "message": { "content": content } }).to_string()
        } else {
            serde_json::json!({ "models": [ { "name": FAKE_MODEL } ] }).to_string()
        };
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{reply}",
            reply.len()
        );
    }

    /// Everything the EPUB says, concatenated, so a test can ask whether a
    /// correction reached the book itself rather than only the markdown.
    fn epub_text(path: &Path) -> String {
        let f = std::fs::File::open(path).unwrap();
        let mut zip = zip::ZipArchive::new(f).unwrap();
        let mut out = String::new();
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).unwrap();
            if entry.name().ends_with(".xhtml") {
                let mut s = String::new();
                entry.read_to_string(&mut s).unwrap();
                out.push_str(&s);
            }
        }
        out
    }

    /// The stage the whole change exists for: the markdown is written, the
    /// model reads *that file*, its answer becomes a second file, and the ebook
    /// is built from the second one.
    #[test]
    fn proofreading_writes_a_second_markdown_and_the_epub_is_built_from_it() {
        let (dir, mut cfg) = fixture("proofread");
        std::fs::write(
            dir.join("input.md"),
            "# One\n\nOnu sudan\u{b7} cikarmaya calisirken.\n",
        )
        .unwrap();
        let server = FakeOllama::start(|s| s.replace('\u{b7}', ""));
        cfg.formats = vec![OutputFormat::Epub];
        cfg.llm = LlmMode::Always;
        cfg.llm_model = FAKE_MODEL.to_string();
        cfg.ollama_urls = vec![server.url.clone()];
        cfg.cache = false;

        let o = run(&cfg, &Recorder::new()).unwrap();

        let proofread = dir.join("input.proofread.md");
        assert_eq!(o.proofread_path.as_deref(), Some(proofread.as_path()));
        assert!(o.written.contains(&proofread));
        assert!(std::fs::read_to_string(&proofread)
            .unwrap()
            .contains("Onu sudan cikarmaya"));
        // The extracted markdown is left exactly as it was.
        assert!(std::fs::read_to_string(dir.join("input.md"))
            .unwrap()
            .contains("Onu sudan\u{b7} cikarmaya"));
        // And the book carries the correction, which it can only do if layer 4
        // read the proofread file rather than the original.
        assert!(epub_text(&dir.join("input.epub")).contains("Onu sudan cikarmaya"));
        assert_eq!((o.llm.considered, o.llm.changed), (1, 1));
    }

    #[test]
    fn a_correction_the_drift_guard_refuses_never_reaches_the_second_file() {
        let (dir, mut cfg) = fixture("drift");
        std::fs::write(dir.join("input.md"), "# One\n\nOnu sudan cikarmaya calisirken.\n")
            .unwrap();
        // A rewrite, not a typo fix — exactly what the guard exists to refuse.
        let server = FakeOllama::start(|_| "Something else entirely.".to_string());
        cfg.formats = vec![OutputFormat::Epub];
        cfg.llm = LlmMode::Always;
        cfg.llm_model = FAKE_MODEL.to_string();
        cfg.ollama_urls = vec![server.url.clone()];
        cfg.cache = false;

        let o = run(&cfg, &Recorder::new()).unwrap();

        assert_eq!((o.llm.changed, o.llm.rejected), (0, 1));
        // The file is still written — the model ran — but it says what the
        // input said.
        let text = std::fs::read_to_string(dir.join("input.proofread.md")).unwrap();
        assert!(text.contains("Onu sudan cikarmaya calisirken."), "{text}");
        assert!(!text.contains("Something else"));
    }

    #[test]
    fn suspicious_mode_sends_only_the_damaged_paragraphs() {
        let (dir, mut cfg) = fixture("suspicious");
        std::fs::write(
            dir.join("input.md"),
            "# One\n\nAslinda hakli ve gayet sakin bir sekilde konusuyor.\n\n\
             takemyad\u{ac}vice\n",
        )
        .unwrap();
        let server = FakeOllama::start(|s| s.replace('\u{ac}', ""));
        cfg.formats = vec![OutputFormat::Markdown];
        cfg.llm = LlmMode::Suspicious;
        cfg.llm_model = FAKE_MODEL.to_string();
        cfg.ollama_urls = vec![server.url.clone()];
        cfg.cache = false;

        let o = run(&cfg, &Recorder::new()).unwrap();

        assert_eq!(o.llm.considered, 1);
        let text = std::fs::read_to_string(dir.join("input.proofread.md")).unwrap();
        assert!(text.contains("takemyadvice"), "{text}");
        assert!(text.contains("Aslinda hakli"), "{text}");
    }

    /// The default. A markdown input never ran OCR, so nothing should go over
    /// the network — the unreachable URL here would fail loudly if it did.
    #[test]
    fn auto_does_not_call_the_model_for_a_markdown_input() {
        let (dir, mut cfg) = fixture("auto");
        cfg.formats = vec![OutputFormat::Epub];
        cfg.llm = LlmMode::Auto;
        cfg.ollama_urls = vec!["http://127.0.0.1:1".to_string()];
        let o = run(&cfg, &Recorder::new()).unwrap();

        assert_eq!(o.llm.considered, 0);
        assert_eq!(o.proofread_path, None);
        assert!(!dir.join("input.proofread.md").exists());
    }

    #[test]
    fn an_unreachable_model_warns_and_still_builds_the_book() {
        let (dir, mut cfg) = fixture("no-ollama");
        cfg.formats = vec![OutputFormat::Epub];
        cfg.llm = LlmMode::Always;
        // Port 1 is reserved, so nothing can be listening on it.
        cfg.ollama_urls = vec!["http://127.0.0.1:1".to_string()];
        cfg.cache = false;
        let rec = Recorder::new();
        let o = run(&cfg, &rec).unwrap();

        assert_eq!(o.proofread_path, None);
        assert!(dir.join("input.epub").is_file());
        assert!(
            rec.warnings().iter().any(|w| w.contains("skipping proofreading")),
            "{:?}",
            rec.warnings()
        );
    }

    #[test]
    fn markdown_input_builds_an_epub() {
        let (dir, mut cfg) = fixture("epub");
        cfg.formats = vec![OutputFormat::Epub];
        let o = run(&cfg, &Recorder::new()).unwrap();

        assert_eq!(o.written, vec![dir.join("input.epub")]);
        assert!(dir.join("input.epub").is_file());
        assert_eq!(o.paragraphs, 2);
        assert_eq!(o.headings, 1);
        assert_eq!(o.page_markers, 1);
        // Nothing extracted, so none of the layer 3 counters may claim work.
        assert_eq!((o.text_pages, o.ocr_pages), (0, 0));
        assert_eq!(o.llm.considered, 0);
    }

    #[test]
    fn markdown_output_over_the_input_is_skipped_not_truncated() {
        let (dir, mut cfg) = fixture("clobber");
        cfg.formats = vec![OutputFormat::Markdown];
        let rec = Recorder::new();
        let o = run(&cfg, &rec).unwrap();

        assert!(o.written.is_empty());
        assert_eq!(std::fs::read_to_string(dir.join("input.md")).unwrap(), SAMPLE);
        assert!(
            rec.warnings().iter().any(|w| w.contains("overwrite the input")),
            "{:?}",
            rec.warnings()
        );
    }

    #[test]
    fn markdown_output_elsewhere_is_written() {
        let (dir, mut cfg) = fixture("md-elsewhere");
        cfg.formats = vec![OutputFormat::Markdown];
        cfg.out_dir = dir.join("sub");
        let o = run(&cfg, &Recorder::new()).unwrap();

        let p = dir.join("sub/input.md");
        assert_eq!(o.written, vec![p.clone()]);
        assert!(std::fs::read_to_string(p).unwrap().contains("# One"));
    }

    #[test]
    fn title_and_author_flags_beat_the_front_matter() {
        let (_dir, mut cfg) = fixture("override");
        cfg.formats = vec![OutputFormat::Epub];
        cfg.title = Some("Flag Title".into());
        cfg.author = Some("Flag Author".into());
        let o = run(&cfg, &Recorder::new()).unwrap();

        assert_eq!(o.document.meta.title, "Flag Title");
        assert_eq!(o.document.meta.author.as_deref(), Some("Flag Author"));
    }

    #[test]
    fn front_matter_language_survives_the_lang_default() {
        let (_dir, mut cfg) = fixture("lang");
        cfg.formats = vec![OutputFormat::Epub];
        cfg.lang = "eng".into(); // the default, so it must not win
        let o = run(&cfg, &Recorder::new()).unwrap();
        assert_eq!(o.document.meta.language, "nl");
    }

    #[test]
    fn lang_fills_in_a_language_the_front_matter_omits() {
        let (dir, mut cfg) = fixture("lang-missing");
        std::fs::write(dir.join("input.md"), "# One\n\nA paragraph.\n").unwrap();
        cfg.formats = vec![OutputFormat::Epub];
        cfg.lang = "tur".into();
        let o = run(&cfg, &Recorder::new()).unwrap();
        assert_eq!(o.document.meta.language, "tr");
    }

    #[test]
    fn an_unsupported_extension_is_rejected() {
        let (dir, mut cfg) = fixture("ext");
        let odt = dir.join("input.odt");
        std::fs::write(&odt, "x").unwrap();
        cfg.input = odt;
        cfg.formats = vec![OutputFormat::Epub];
        assert!(matches!(
            run(&cfg, &Recorder::new()),
            Err(Error::UnsupportedInput(_))
        ));
    }

    #[test]
    fn pdf_only_formats_and_options_are_rejected_for_markdown() {
        let (_dir, mut cfg) = fixture("pdf-only");
        cfg.formats = vec![OutputFormat::Json];
        assert!(run(&cfg, &Recorder::new()).is_err());

        cfg.formats = vec![OutputFormat::Epub];
        cfg.pages = Some((0, 3));
        assert!(run(&cfg, &Recorder::new()).is_err());
    }
}
