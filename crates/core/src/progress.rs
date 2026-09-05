use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Opening,
    Classifying,
    Extracting,
    Ocr,
    Analysing,
    Proofreading,
    ReadingMarkdown,
    WritingMarkdown,
    BuildingEpub,
    Converting,
    Done,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Stage::Opening => "Opening PDF",
            Stage::Classifying => "Checking which pages have text",
            Stage::Extracting => "Extracting text",
            Stage::Ocr => "Running OCR",
            Stage::Analysing => "Finding paragraphs and chapters",
            Stage::Proofreading => "Proofreading with local model",
            Stage::ReadingMarkdown => "Reading markdown",
            Stage::WritingMarkdown => "Writing markdown",
            Stage::BuildingEpub => "Building EPUB",
            Stage::Converting => "Converting to Kindle format",
            Stage::Done => "Done",
        }
    }
}

/// What the pipeline tells the caller while it runs. The GUI turns these into a
/// progress bar and a log; the CLI prints them.
#[derive(Debug, Clone)]
pub enum Event {
    Stage(Stage),
    /// Page counts after classification.
    Classified {
        text_pages: usize,
        ocr_pages: usize,
    },
    /// Fine-grained progress within the current stage.
    Progress {
        done: usize,
        total: usize,
    },
    Wrote(PathBuf),
    /// Something recoverable. The run continues.
    Warning(String),
    /// A completed run summary line.
    Info(String),
}

/// Implemented by each front end. Must be callable from the worker thread.
pub trait Reporter: Send + Sync {
    fn event(&self, e: Event);

    /// Polled between units of work so the UI can stop a long run.
    fn cancelled(&self) -> bool {
        false
    }
}

/// A reporter that throws everything away, for tests.
pub struct Silent;

impl Reporter for Silent {
    fn event(&self, _: Event) {}
}
