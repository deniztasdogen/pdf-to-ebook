//! Running a conversion off the UI thread.

use pdf_to_ebook_core::{Config, Event, Reporter, Stage};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::path::PathBuf;

/// What the worker sends back to the UI.
pub enum Msg {
    Event(Event),
    Finished(Result<Summary, String>),
}

pub struct Summary {
    pub written: Vec<PathBuf>,
    pub paragraphs: usize,
    pub headings: usize,
    pub page_markers: usize,
    pub characters: usize,
    pub text_pages: usize,
    pub ocr_pages: usize,
    pub llm_changed: usize,
    pub llm_rejected: usize,
    pub elapsed: std::time::Duration,
}

/// Bridges the pipeline's [`Reporter`] onto a channel, and carries the cancel
/// flag so a long run can be stopped from the UI.
struct ChannelReporter {
    tx: Sender<Msg>,
    cancel: Arc<AtomicBool>,
    /// Woken so the UI repaints even when the pointer is still.
    ctx: egui::Context,
}

impl Reporter for ChannelReporter {
    fn event(&self, e: Event) {
        let _ = self.tx.send(Msg::Event(e));
        self.ctx.request_repaint();
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

pub struct Job {
    pub rx: Receiver<Msg>,
    pub cancel: Arc<AtomicBool>,
}

impl Job {
    pub fn spawn(cfg: Config, ctx: egui::Context) -> Job {
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let reporter = ChannelReporter {
            tx: tx.clone(),
            cancel: cancel.clone(),
            ctx,
        };
        std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let result = pdf_to_ebook_orchestrator::run(&cfg, &reporter);
            let msg = match result {
                Ok(o) => Msg::Finished(Ok(Summary {
                    written: o.written,
                    paragraphs: o.paragraphs,
                    headings: o.headings,
                    page_markers: o.page_markers,
                    characters: o.characters,
                    text_pages: o.text_pages,
                    ocr_pages: o.ocr_pages,
                    llm_changed: o.llm.changed,
                    llm_rejected: o.llm.rejected,
                    elapsed: started.elapsed(),
                })),
                Err(e) => Msg::Finished(Err(format!("{e}"))),
            };
            let _ = tx.send(msg);
            reporter.ctx.request_repaint();
        });
        Job { rx, cancel }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Rough share of the whole job each stage represents, so the progress bar
/// moves smoothly instead of resetting at every stage.
pub fn stage_weight(s: Stage) -> (f32, f32) {
    match s {
        Stage::Opening => (0.00, 0.02),
        Stage::Classifying => (0.02, 0.06),
        Stage::Extracting => (0.06, 0.25),
        Stage::Ocr => (0.25, 0.55),
        Stage::Analysing => (0.55, 0.62),
        Stage::Proofreading => (0.62, 0.90),
        Stage::ReadingMarkdown => (0.00, 0.10),
        Stage::WritingMarkdown => (0.90, 0.93),
        Stage::BuildingEpub => (0.93, 0.96),
        Stage::Converting => (0.96, 0.99),
        Stage::Done => (1.00, 1.00),
    }
}
