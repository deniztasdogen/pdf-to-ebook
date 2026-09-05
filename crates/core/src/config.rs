use crate::env::{defaults, Defaults};
use std::path::{Path, PathBuf};

/// When to fall back to OCR. The decision is made *per page*, because a single
/// document can mix both: the Turkish test book has a good text layer on 414 of
/// 419 pages and nothing usable on the other 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum OcrMode {
    /// Use the embedded text layer where it exists, OCR the pages where it does not.
    #[default]
    Auto,
    /// Never OCR. Pages without text come out empty.
    Never,
    /// OCR every page, ignoring any embedded text.
    Always,
}

/// When to run paragraphs past the local LLM for typo repair.
///
/// Note this is deliberately independent of [`OcrMode`]. A PDF can carry a text
/// layer that is *itself* OCR output — the Turkish test book was processed with
/// Acrobat ClearScan — so it has OCR typos even though we never run OCR on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum LlmMode {
    /// Proofread only paragraphs that came from pages we OCR'd ourselves.
    #[default]
    Auto,
    /// Proofread everything, including text-layer pages.
    Always,
    /// Proofread only paragraphs that look doubtful (low OCR confidence or
    /// suspicious character patterns).
    Suspicious,
    /// Skip the LLM entirely.
    Never,
}

/// How printed page boundaries are represented in the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum PageBreakMode {
    /// EPUB 3 `pagebreak` anchors plus a `page-list` nav. Text stays
    /// reflowable and still records where the printed pages fell. Note that
    /// calibre's MOBI/AZW3 writer discards these.
    #[default]
    Anchors,
    /// Real forced breaks via CSS `page-break-before`. Survives conversion to
    /// MOBI but fights reflow, so it is opt-in.
    Hard,
    /// Discard page information.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum OutputFormat {
    /// The layer 3 → layer 4 interface. Always written.
    Markdown,
    Epub,
    Mobi,
    Azw3,
    /// Layout debug dump.
    Json,
}

impl OutputFormat {
    pub fn extension(self) -> &'static str {
        match self {
            OutputFormat::Markdown => "md",
            OutputFormat::Epub => "epub",
            OutputFormat::Mobi => "mobi",
            OutputFormat::Azw3 => "azw3",
            OutputFormat::Json => "json",
        }
    }

    /// Formats produced by layer 4 from the markdown.
    pub fn is_ebook(self) -> bool {
        matches!(
            self,
            OutputFormat::Epub | OutputFormat::Mobi | OutputFormat::Azw3
        )
    }

    pub fn parse_list(s: &str) -> Result<Vec<OutputFormat>, String> {
        s.split(',')
            .map(|p| p.trim().to_ascii_lowercase())
            .filter(|p| !p.is_empty())
            .map(|p| match p.as_str() {
                "md" | "markdown" => Ok(OutputFormat::Markdown),
                "epub" => Ok(OutputFormat::Epub),
                "mobi" => Ok(OutputFormat::Mobi),
                "azw3" => Ok(OutputFormat::Azw3),
                "json" => Ok(OutputFormat::Json),
                other => Err(format!("unknown format {other:?}")),
            })
            .collect()
    }
}

/// What kind of file the input is, decided by its extension.
///
/// Markdown is not a second-class input: it *is* the layer 3 → layer 4
/// interface, so a markdown input simply enters the pipeline one layer later
/// than a PDF does. Nothing extracts, OCRs or proofreads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    Pdf,
    Markdown,
}

impl InputKind {
    /// `None` for an extension we cannot build a book from.
    pub fn of(path: &Path) -> Option<InputKind> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "pdf" => Some(InputKind::Pdf),
            "md" | "markdown" | "mdown" | "mkd" => Some(InputKind::Markdown),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            InputKind::Pdf => "PDF",
            InputKind::Markdown => "markdown",
        }
    }
}

/// A language the UI offers. `tesseract` is the OCR code, `bcp47` goes into the
/// EPUB metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Language {
    pub label: &'static str,
    pub tesseract: &'static str,
    pub bcp47: &'static str,
}

/// The languages the test corpus actually needs, plus the common European ones.
/// Any tesseract code can still be passed on the CLI.
pub const LANGUAGES: &[Language] = &[
    Language {
        label: "English",
        tesseract: "eng",
        bcp47: "en",
    },
    Language {
        label: "Turkish",
        tesseract: "tur",
        bcp47: "tr",
    },
    Language {
        label: "Dutch",
        tesseract: "nld",
        bcp47: "nl",
    },
    Language {
        label: "German",
        tesseract: "deu",
        bcp47: "de",
    },
    Language {
        label: "French",
        tesseract: "fra",
        bcp47: "fr",
    },
    Language {
        label: "Spanish",
        tesseract: "spa",
        bcp47: "es",
    },
    Language {
        label: "Italian",
        tesseract: "ita",
        bcp47: "it",
    },
    Language {
        label: "Portuguese",
        tesseract: "por",
        bcp47: "pt",
    },
    Language {
        label: "Swedish",
        tesseract: "swe",
        bcp47: "sv",
    },
    Language {
        label: "Polish",
        tesseract: "pol",
        bcp47: "pl",
    },
];

pub fn bcp47_for(tesseract_code: &str) -> &str {
    LANGUAGES
        .iter()
        .find(|l| l.tesseract == tesseract_code)
        .map(|l| l.bcp47)
        .unwrap_or("und")
}

/// The human-readable name, for prompts that must name the language to the
/// model. An unknown tesseract code yields `None` rather than a guess, so the
/// caller can leave the language out of the prompt entirely.
pub fn label_for(tesseract_code: &str) -> Option<&'static str> {
    LANGUAGES
        .iter()
        .find(|l| l.tesseract == tesseract_code)
        .map(|l| l.label)
}

/// Parse a comma-separated list of ollama endpoints, e.g.
/// `http://desktop:11434,http://laptop:11434`.
///
/// A bare `host:port` gets the `http://` it plainly meant, and a trailing slash
/// is trimmed so the endpoint can be joined to `/api/chat` unconditionally.
///
/// Duplicates are deliberately **kept**. One request is in flight per entry, so
/// naming the same server twice is how you ask a box with
/// `OLLAMA_NUM_PARALLEL=2` for two batches at once.
pub fn parse_ollama_urls(s: &str) -> Result<Vec<String>, String> {
    let urls: Vec<String> = s
        .split(',')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .map(|p| {
            let p = if p.contains("://") {
                p.to_string()
            } else {
                format!("http://{p}")
            };
            p.trim_end_matches('/').to_string()
        })
        .collect();
    if urls.is_empty() {
        return Err("no ollama server given".to_string());
    }
    Ok(urls)
}

#[derive(Debug, Clone)]
pub struct Config {
    pub input: PathBuf,
    pub out_dir: PathBuf,
    /// Output filename stem, without extension.
    pub out_stem: String,
    pub formats: Vec<OutputFormat>,

    /// tesseract language code, e.g. `tur`.
    pub lang: String,
    pub ocr: OcrMode,
    pub llm: LlmMode,
    pub llm_model: String,
    /// The ollama servers to proofread on, in the order they were given. One
    /// request is in flight per entry, so a list of three servers runs three
    /// batches at once — and the same URL listed twice asks for two requests
    /// against that one server.
    pub ollama_urls: Vec<String>,
    pub page_breaks: PageBreakMode,

    /// Rasterisation resolution for the OCR path.
    pub dpi: f32,
    /// Inclusive, zero-based page range. `None` means the whole document.
    pub pages: Option<(usize, usize)>,

    pub title: Option<String>,
    pub author: Option<String>,

    /// Use the on-disk LLM cache.
    pub cache: bool,
    /// Fractions of the page to discard before analysis, for inputs whose
    /// chrome cannot be detected automatically (a single-page screenshot has no
    /// other pages to compare against).
    pub crop: Crop,
    pub report: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Crop {
    pub top: f32,
    pub bottom: f32,
    pub left: f32,
    pub right: f32,
}

impl Crop {
    pub fn is_zero(&self) -> bool {
        self.top == 0.0 && self.bottom == 0.0 && self.left == 0.0 && self.right == 0.0
    }
}

impl Config {
    /// Defaults chosen so that `Config::new(path)` on its own does something
    /// sensible: use the text layer, OCR only what has none, proofread only
    /// what we OCR'd, write markdown and EPUB.
    ///
    /// The compiled-in defaults, never the environment. `.env` is layer 1's
    /// business — see [`Config::with_defaults`] — so that a `Config` built in
    /// a test says the same thing on every machine.
    pub fn new(input: impl Into<PathBuf>) -> Self {
        Config::with_defaults(input, &Defaults::default())
    }

    /// The same, starting from settings the front end has already read out of
    /// the environment and `.env`.
    pub fn with_defaults(input: impl Into<PathBuf>, d: &Defaults) -> Self {
        let input = input.into();
        let stem = input
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "book".to_string());
        let dir = input.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        Config {
            input,
            out_dir: dir,
            out_stem: stem,
            formats: vec![OutputFormat::Markdown, OutputFormat::Epub],
            lang: d.lang.clone(),
            ocr: OcrMode::default(),
            llm: LlmMode::default(),
            llm_model: d.llm_model.clone(),
            // A list that will not parse is not worth failing a `Config` over:
            // fall back to the one server that is always meant.
            ollama_urls: parse_ollama_urls(&d.ollama_url)
                .unwrap_or_else(|_| vec![defaults::OLLAMA_URL.to_string()]),
            page_breaks: PageBreakMode::default(),
            dpi: d.dpi,
            pages: None,
            title: None,
            author: None,
            cache: true,
            crop: Crop::default(),
            report: true,
        }
    }

    pub fn input_kind(&self) -> Option<InputKind> {
        InputKind::of(&self.input)
    }

    pub fn output_path(&self, f: OutputFormat) -> PathBuf {
        self.out_dir
            .join(format!("{}.{}", self.out_stem, f.extension()))
    }

    /// Markdown is the interface between layer 3 and layer 4, so it is written
    /// whenever any ebook format is requested, even if not asked for directly.
    pub fn wants_markdown(&self) -> bool {
        self.formats.contains(&OutputFormat::Markdown) || self.formats.iter().any(|f| f.is_ebook())
    }

    pub fn bcp47(&self) -> &str {
        bcp47_for(&self.lang)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_kind_comes_from_the_extension_case_insensitively() {
        let cases = [
            ("book.pdf", Some(InputKind::Pdf)),
            ("book.PDF", Some(InputKind::Pdf)),
            ("book.md", Some(InputKind::Markdown)),
            ("book.Markdown", Some(InputKind::Markdown)),
            ("book.txt", None),
            ("book", None),
        ];
        for (name, want) in cases {
            assert_eq!(InputKind::of(Path::new(name)), want, "{name}");
        }
    }

    #[test]
    fn a_list_of_ollama_servers_is_normalised_but_not_deduplicated() {
        assert_eq!(
            parse_ollama_urls(" http://desktop:11434/ , laptop:11434 "),
            Ok(vec![
                "http://desktop:11434".to_string(),
                "http://laptop:11434".to_string(),
            ])
        );
        // Two entries for one server means two requests in flight on it, so
        // collapsing them would silently halve the concurrency asked for.
        assert_eq!(
            parse_ollama_urls("http://a:11434,http://a:11434").map(|v| v.len()),
            Ok(2)
        );
        assert!(parse_ollama_urls("  ,  ").is_err());
    }

    #[test]
    fn a_markdown_input_still_wants_markdown_when_an_ebook_is_asked_for() {
        let mut cfg = Config::new("book.md");
        cfg.formats = vec![OutputFormat::Mobi];
        assert_eq!(cfg.input_kind(), Some(InputKind::Markdown));
        assert!(cfg.wants_markdown());
    }
}
