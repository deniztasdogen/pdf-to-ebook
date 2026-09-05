//! Layer 0: types shared by every layer. No PDF, OCR, HTTP or UI code here, so
//! the UI can depend on this without pulling in pdfium or tesseract.

pub mod config;
pub mod doc;
pub mod error;
pub mod progress;

pub use config::{bcp47_for, label_for, parse_ollama_urls, Config, Crop, InputKind, Language,
                 LlmMode, OcrMode, OutputFormat, PageBreakMode, LANGUAGES};
pub use doc::{Block, Document, Meta, Span};
pub use error::{Error, Result};
pub use progress::{Event, Reporter, Stage};
