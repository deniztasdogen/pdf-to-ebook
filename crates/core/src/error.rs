use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("input file not found: {0}")]
    InputMissing(PathBuf),

    #[error("cannot build a book from {0}: expected a .pdf or a .md file")]
    UnsupportedInput(PathBuf),

    #[error("PDF error: {0}")]
    Pdf(String),

    /// The OCR engine is needed but unavailable. Carries the install hint so the
    /// UI can show something actionable rather than a bare failure.
    #[error("OCR unavailable: {reason}\nhint: {hint}")]
    OcrUnavailable { reason: String, hint: String },

    #[error("OCR failed on page {page}: {reason}")]
    OcrFailed { page: usize, reason: String },

    #[error("Ollama unreachable at {url}: {reason}\nhint: {hint}")]
    OllamaUnreachable {
        url: String,
        reason: String,
        hint: String,
    },

    #[error("ebook-convert unavailable: {reason}\nhint: {hint}")]
    ConverterUnavailable { reason: String, hint: String },

    #[error("ebook-convert failed: {0}")]
    ConverterFailed(String),

    #[error("malformed markdown at line {line}: {reason}")]
    Markdown { line: usize, reason: String },

    #[error("no text could be extracted from {0} — every page was empty")]
    NoText(PathBuf),

    #[error("cancelled")]
    Cancelled,

    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl Error {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}
