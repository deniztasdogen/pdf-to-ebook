//! **Layer 3: extraction.**
//!
//! PDF in, markdown out. This layer owns everything about *reading* a
//! document: the text layer, OCR, layout analysis and the local-LLM
//! proofreading pass. It knows nothing about EPUB or MOBI.
//!
//! Both input paths converge on [`model::PageLayout`] as early as possible, so
//! chrome removal, column ordering, paragraph reconstruction, dehyphenation and
//! heading detection are implemented exactly once.

pub mod geom;
pub mod layout;
pub mod markdown;
pub mod model;
pub mod ocr;
pub mod pdf;
pub mod proofread;

mod pipeline;

pub use pipeline::{extract, ExtractOutcome};
