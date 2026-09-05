//! **Layer 1 (terminal): input.**
//!
//! Collects a configuration and hands it to layer 2. Contains no conversion
//! logic of its own.

use clap::parser::ValueSource;
use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser};
use pdf_to_ebook_core::{
    Config, Crop, Defaults, Event, InputKind, LlmMode, OcrMode, OutputFormat, PageBreakMode,
    Reporter, Stage,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Parser, Debug)]
#[command(
    name = "pdf-to-ebook",
    about = "Build an EPUB or Kindle book from a PDF or a markdown file",
    long_about = "Build an EPUB or Kindle book from a PDF or a markdown file.\n\n\
                  A PDF goes through the whole pipeline: text layer, OCR where \
                  there is none, markdown, optional proofreading into a second \
                  markdown, then the ebook. A markdown file enters at the \
                  proofreading step — markdown is the interface between the two \
                  halves — so the OCR and page options below do nothing for it, \
                  but --llm still does.",
    version
)]
struct Args {
    /// The file to convert: a .pdf, or a .md written by a previous run or by hand.
    input: PathBuf,

    /// Output basename, without extension. Defaults to the input's name.
    #[arg(short, long)]
    out: Option<PathBuf>,

    /// Comma-separated: md, epub, mobi, azw3, json.
    #[arg(short, long, default_value = "md,epub")]
    format: String,

    /// OCR language, as a tesseract code (eng, tur, nld, …). For a markdown
    /// input it only supplies a language the front matter does not already give.
    /// Defaults to PDF_TO_EBOOK_OCR_LANG, else eng.
    #[arg(short, long, default_value_t = Defaults::from_env().lang)]
    lang: String,

    /// auto = OCR only pages with no text layer.
    #[arg(long, value_parser = ["auto", "never", "always"], default_value = "auto")]
    ocr: String,

    /// auto = proofread only what we OCR'd ourselves. Anything other than
    /// never writes the result to `<name>.proofread.md` and builds the ebook
    /// from that file.
    #[arg(long, value_parser = ["auto", "always", "suspicious", "never"], default_value = "auto")]
    llm: String,

    /// The ollama model to proofread with. Defaults to PDF_TO_EBOOK_LLM_MODEL.
    #[arg(long, default_value_t = Defaults::from_env().llm_model)]
    llm_model: String,

    /// Ollama server, or a comma-separated list of them. The paragraph
    /// batches are shared out over the list as each server comes free, so two
    /// servers halve the wait. Naming one server twice runs two batches on it.
    /// Defaults to PDF_TO_EBOOK_OLLAMA_URL, else OLLAMA_HOST, else localhost.
    #[arg(long, default_value_t = Defaults::from_env().ollama_url)]
    ollama_url: String,

    /// anchors = invisible EPUB page markers; hard = forced breaks.
    #[arg(long, value_parser = ["anchors", "hard", "none"], default_value = "anchors")]
    page_breaks: String,

    /// Rasterisation resolution used for OCR. Defaults to PDF_TO_EBOOK_DPI.
    #[arg(long, default_value_t = Defaults::from_env().dpi)]
    dpi: f32,

    /// Only these pages, 1-based inclusive, e.g. `41-46`. Useful when tuning.
    /// PDF input only.
    #[arg(long)]
    pages: Option<String>,

    /// Overrides the PDF metadata or the markdown front matter.
    #[arg(long)]
    title: Option<String>,

    /// Overrides the PDF metadata or the markdown front matter.
    #[arg(long)]
    author: Option<String>,

    /// Fraction of the page to discard, for inputs whose chrome cannot be
    /// detected automatically.
    #[arg(long, default_value_t = 0.0)]
    crop_top: f32,
    #[arg(long, default_value_t = 0.0)]
    crop_bottom: f32,
    #[arg(long, default_value_t = 0.0)]
    crop_left: f32,
    #[arg(long, default_value_t = 0.0)]
    crop_right: f32,

    /// Ignore the on-disk cache of model replies.
    #[arg(long)]
    no_cache: bool,

    /// Do not write the report file.
    #[arg(long)]
    no_report: bool,

    #[arg(short, long)]
    quiet: bool,
}

struct Cli {
    quiet: bool,
    last_total: AtomicUsize,
}

impl Reporter for Cli {
    fn event(&self, e: Event) {
        match e {
            Event::Stage(s) => {
                if s == Stage::Done {
                    return;
                }
                self.last_total.store(0, Ordering::Relaxed);
                eprintln!("==> {}", s.label());
            }
            Event::Classified {
                text_pages,
                ocr_pages,
            } => {
                eprintln!("    {text_pages} page(s) have a text layer, {ocr_pages} need OCR");
            }
            Event::Progress { done, total } => {
                if self.quiet || total == 0 {
                    return;
                }
                // Only redraw on whole-percent changes, so a 419-page book does
                // not spam the terminal.
                let pct = done * 100 / total;
                let prev = self.last_total.swap(pct, Ordering::Relaxed);
                if pct != prev || done == total {
                    eprint!("\r    {done}/{total} ({pct}%)   ");
                    if done == total {
                        eprintln!();
                    }
                }
            }
            Event::Wrote(p) => eprintln!("    wrote {}", p.display()),
            Event::Warning(w) => eprintln!("!!  {w}"),
            Event::Info(i) => eprintln!("    {i}"),
        }
    }
}

fn parse_pages(s: &str) -> anyhow::Result<(usize, usize)> {
    let (a, b) = match s.split_once('-') {
        Some((a, b)) => (a.trim(), b.trim()),
        None => (s.trim(), s.trim()),
    };
    let a: usize = a
        .parse()
        .map_err(|_| anyhow::anyhow!("bad page number {a:?}"))?;
    let b: usize = b
        .parse()
        .map_err(|_| anyhow::anyhow!("bad page number {b:?}"))?;
    if a == 0 || b == 0 {
        anyhow::bail!("page numbers are 1-based");
    }
    Ok((a - 1, b - 1))
}

/// Flags that only drive extraction. A markdown input skips that work, so
/// saying they were ignored beats silently doing nothing with them.
///
/// The model flags are **not** in this list: proofreading reads a markdown file
/// and writes one, so `--llm` works on a markdown input too.
fn inert_for_markdown(m: &ArgMatches) -> Vec<String> {
    const PDF_ONLY: &[&str] = &[
        "ocr",
        "dpi",
        "crop_top",
        "crop_bottom",
        "crop_left",
        "crop_right",
    ];
    PDF_ONLY
        .iter()
        .filter(|id| m.value_source(id) == Some(ValueSource::CommandLine))
        .map(|id| format!("--{}", id.replace('_', "-")))
        .collect()
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    // Parsed via the matches rather than `Args::parse()` so we can tell a flag
    // the user typed from one that merely has a default.
    let matches = Args::command().get_matches();
    let args = Args::from_arg_matches(&matches)?;
    // The flags above already default to these, so this only matters for the
    // settings no flag covers. Precedence is flag, then environment, then
    // `.env`, then the compiled-in default.
    let mut cfg = Config::with_defaults(&args.input, &Defaults::from_env());

    if let Some(o) = &args.out {
        // A path with an extension names the file; otherwise it is a directory.
        if o.extension().is_some() {
            cfg.out_dir = o.parent().map(|p| p.to_path_buf()).unwrap_or_default();
            cfg.out_stem = o
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or(cfg.out_stem);
        } else {
            cfg.out_dir = o.clone();
        }
    }
    cfg.formats = OutputFormat::parse_list(&args.format).map_err(|e| anyhow::anyhow!(e))?;
    cfg.lang = args.lang;
    cfg.ocr = match args.ocr.as_str() {
        "never" => OcrMode::Never,
        "always" => OcrMode::Always,
        _ => OcrMode::Auto,
    };
    cfg.llm = match args.llm.as_str() {
        "always" => LlmMode::Always,
        "suspicious" => LlmMode::Suspicious,
        "never" => LlmMode::Never,
        _ => LlmMode::Auto,
    };
    cfg.llm_model = args.llm_model;
    cfg.ollama_urls =
        pdf_to_ebook_core::parse_ollama_urls(&args.ollama_url).map_err(|e| anyhow::anyhow!(e))?;
    cfg.page_breaks = match args.page_breaks.as_str() {
        "hard" => PageBreakMode::Hard,
        "none" => PageBreakMode::None,
        _ => PageBreakMode::Anchors,
    };
    cfg.dpi = args.dpi;
    cfg.pages = match &args.pages {
        Some(s) => Some(parse_pages(s)?),
        None => None,
    };
    cfg.title = args.title;
    cfg.author = args.author;
    cfg.crop = Crop {
        top: args.crop_top,
        bottom: args.crop_bottom,
        left: args.crop_left,
        right: args.crop_right,
    };
    cfg.cache = !args.no_cache;
    cfg.report = !args.no_report;

    if cfg.input_kind() == Some(InputKind::Markdown) {
        for flag in inert_for_markdown(&matches) {
            eprintln!("!!  {flag} applies to a PDF input only; ignored");
        }
    }

    let rep = Cli {
        quiet: args.quiet,
        last_total: AtomicUsize::new(0),
    };
    let started = std::time::Instant::now();
    let outcome = pdf_to_ebook_orchestrator::run(&cfg, &rep)?;

    eprintln!();
    eprintln!(
        "Done in {:.1}s — {} paragraphs, {} headings, {} page markers, {} characters",
        started.elapsed().as_secs_f32(),
        outcome.paragraphs,
        outcome.headings,
        outcome.page_markers,
        outcome.characters
    );
    Ok(())
}
