//! **Layer 1 (desktop): input.**
//!
//! Pick a PDF, a language and the output formats, then hand a [`Config`] to
//! layer 2. No conversion logic lives here.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod job;

use job::{Job, Msg, Summary};
use pdf_to_ebook_core::{
    Config, Defaults, Event, LlmMode, OcrMode, OutputFormat, PageBreakMode, Stage, LANGUAGES,
};
use std::path::PathBuf;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([720.0, 660.0])
            .with_min_inner_size([560.0, 480.0])
            .with_title("PDF to EPUB / Kindle"),
        ..Default::default()
    };
    eframe::run_native(
        "pdf-to-ebook",
        options,
        Box::new(|_cc| Ok(Box::new(App::default()))),
    )
}

struct App {
    input: Option<PathBuf>,
    out_dir: Option<PathBuf>,
    lang_index: usize,

    want_md: bool,
    want_epub: bool,
    want_mobi: bool,
    want_azw3: bool,

    ocr: OcrMode,
    llm: LlmMode,
    page_breaks: PageBreakMode,
    llm_model: String,
    ollama_url: String,
    title: String,
    author: String,
    show_advanced: bool,

    job: Option<Job>,
    stage: Option<Stage>,
    progress: f32,
    log: Vec<String>,
    result: Option<Result<Summary, String>>,
}

impl Default for App {
    /// The model and the ollama server come from the environment and `.env`,
    /// so the advanced boxes open on whatever this machine is set up for
    /// rather than on a localhost that may not be running anything.
    fn default() -> Self {
        let env = Defaults::from_env();
        App {
            input: None,
            out_dir: None,
            lang_index: LANGUAGES
                .iter()
                .position(|l| l.tesseract == env.lang)
                .unwrap_or(0),
            want_md: true,
            want_epub: true,
            want_mobi: false,
            want_azw3: false,
            ocr: OcrMode::Auto,
            llm: LlmMode::Auto,
            page_breaks: PageBreakMode::Anchors,
            llm_model: env.llm_model,
            ollama_url: env.ollama_url,
            title: String::new(),
            author: String::new(),
            show_advanced: false,
            job: None,
            stage: None,
            progress: 0.0,
            log: Vec::new(),
            result: None,
        }
    }
}

impl App {
    fn running(&self) -> bool {
        self.job.is_some()
    }

    fn formats(&self) -> Vec<OutputFormat> {
        let mut v = Vec::new();
        if self.want_md {
            v.push(OutputFormat::Markdown);
        }
        if self.want_epub {
            v.push(OutputFormat::Epub);
        }
        if self.want_mobi {
            v.push(OutputFormat::Mobi);
        }
        if self.want_azw3 {
            v.push(OutputFormat::Azw3);
        }
        v
    }

    fn build_config(&self) -> Option<Config> {
        let input = self.input.clone()?;
        let mut cfg = Config::with_defaults(input, &Defaults::from_env());
        if let Some(d) = &self.out_dir {
            cfg.out_dir = d.clone();
        }
        cfg.formats = self.formats();
        cfg.lang = LANGUAGES[self.lang_index].tesseract.to_string();
        cfg.ocr = self.ocr;
        cfg.llm = self.llm;
        cfg.page_breaks = self.page_breaks;
        cfg.llm_model = self.llm_model.clone();
        // A bad list is not worth refusing a conversion over: fall back to
        // the field as typed and let preflight report it.
        cfg.ollama_urls = pdf_to_ebook_core::parse_ollama_urls(&self.ollama_url)
            .unwrap_or_else(|_| vec![self.ollama_url.clone()]);
        cfg.title = (!self.title.trim().is_empty()).then(|| self.title.trim().to_string());
        cfg.author = (!self.author.trim().is_empty()).then(|| self.author.trim().to_string());
        Some(cfg)
    }

    fn start(&mut self, ctx: &egui::Context) {
        let Some(cfg) = self.build_config() else {
            return;
        };
        self.log.clear();
        self.result = None;
        self.progress = 0.0;
        self.stage = Some(Stage::Opening);
        self.job = Some(Job::spawn(cfg, ctx.clone()));
    }

    fn pump(&mut self) {
        let Some(j) = &self.job else { return };
        // Drain everything queued first, then act on it, so the borrow of the
        // job ends before `self` is mutated.
        let mut queued = Vec::new();
        while let Ok(msg) = j.rx.try_recv() {
            queued.push(msg);
        }
        let mut finished = None;
        for msg in queued {
            match msg {
                Msg::Event(e) => self.on_event(e),
                Msg::Finished(r) => finished = Some(r),
            }
        }
        if let Some(r) = finished {
            self.job = None;
            self.stage = Some(Stage::Done);
            self.progress = 1.0;
            self.result = Some(r);
        }
    }

    fn on_event(&mut self, e: Event) {
        match e {
            Event::Stage(s) => {
                self.stage = Some(s);
                self.progress = job::stage_weight(s).0;
                self.log.push(format!("{}…", s.label()));
            }
            Event::Classified {
                text_pages,
                ocr_pages,
            } => self.log.push(format!(
                "{text_pages} page(s) have a text layer, {ocr_pages} need OCR"
            )),
            Event::Progress { done, total } => {
                if total > 0 {
                    let (a, b) = job::stage_weight(self.stage.unwrap_or(Stage::Extracting));
                    self.progress = a + (b - a) * (done as f32 / total as f32);
                }
            }
            Event::Wrote(p) => self.log.push(format!(
                "wrote {}",
                p.file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default()
            )),
            Event::Warning(w) => self.log.push(format!("⚠ {w}")),
            Event::Info(i) => self.log.push(i),
        }
        // Keep the log bounded; a 419-page book is chatty.
        if self.log.len() > 400 {
            self.log.drain(..self.log.len() - 400);
        }
    }
}

fn reveal(path: &std::path::Path) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer")
        .arg(format!("/select,{}", path.display()))
        .spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    if let Some(dir) = path.parent() {
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.pump();

        // Accept a PDF dropped anywhere on the window.
        ctx.input(|i| {
            if let Some(f) = i.raw.dropped_files.iter().find_map(|f| f.path.clone()) {
                if f.extension()
                    .map(|e| e.eq_ignore_ascii_case("pdf"))
                    .unwrap_or(false)
                {
                    self.input = Some(f);
                }
            }
        });

        {
            let ui = &mut *ui;
            ui.add_space(4.0);
            ui.heading("PDF to EPUB / Kindle");
            ui.label(
                egui::RichText::new(
                    "Extracts the text, keeps paragraphs and page breaks, and ignores images.",
                )
                .weak(),
            );
            ui.add_space(10.0);

            let busy = self.running();

            // ---- input ------------------------------------------------------
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.strong("PDF file");
                    if ui
                        .add_enabled(!busy, egui::Button::new("Choose…"))
                        .clicked()
                    {
                        if let Some(p) = rfd::FileDialog::new()
                            .add_filter("PDF", &["pdf"])
                            .pick_file()
                        {
                            self.input = Some(p);
                        }
                    }
                });
                match &self.input {
                    Some(p) => {
                        ui.label(egui::RichText::new(p.display().to_string()).monospace());
                    }
                    None => {
                        ui.label(egui::RichText::new("No file chosen — or drop one here.").weak());
                    }
                }
            });

            ui.add_space(8.0);

            // ---- language and formats ---------------------------------------
            ui.horizontal(|ui| {
                ui.strong("Language");
                egui::ComboBox::from_id_salt("lang")
                    .selected_text(LANGUAGES[self.lang_index].label)
                    .show_ui(ui, |ui| {
                        for (i, l) in LANGUAGES.iter().enumerate() {
                            ui.selectable_value(&mut self.lang_index, i, l.label);
                        }
                    });
                ui.label(egui::RichText::new("used for OCR and set in the ebook metadata").weak());
            });

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.strong("Output");
                ui.add_enabled(!busy, egui::Checkbox::new(&mut self.want_md, "Markdown"));
                ui.add_enabled(!busy, egui::Checkbox::new(&mut self.want_epub, "EPUB"));
                ui.add_enabled(!busy, egui::Checkbox::new(&mut self.want_mobi, "MOBI"));
                ui.add_enabled(!busy, egui::Checkbox::new(&mut self.want_azw3, "AZW3"));
            });
            if (self.want_mobi || self.want_azw3) && !kindle_converter_available() {
                ui.label(
                    egui::RichText::new(
                        "⚠ Kindle formats need calibre installed (brew install --cask calibre).",
                    )
                    .color(egui::Color32::from_rgb(200, 140, 0)),
                );
            }
            if self.want_mobi || self.want_azw3 {
                ui.label(
                    egui::RichText::new(
                        "Note: Kindle formats do not keep page markers. The EPUB does.",
                    )
                    .weak(),
                );
            }

            ui.add_space(8.0);
            egui::CollapsingHeader::new("More options")
                .default_open(self.show_advanced)
                .show(ui, |ui| {
                    egui::Grid::new("adv").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                        ui.label("Scanned pages");
                        egui::ComboBox::from_id_salt("ocr")
                            .selected_text(match self.ocr {
                                OcrMode::Auto => "OCR only pages with no text",
                                OcrMode::Never => "Never OCR",
                                OcrMode::Always => "OCR every page",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.ocr, OcrMode::Auto, "OCR only pages with no text");
                                ui.selectable_value(&mut self.ocr, OcrMode::Never, "Never OCR");
                                ui.selectable_value(&mut self.ocr, OcrMode::Always, "OCR every page");
                            });
                        ui.end_row();

                        ui.label("Fix typos with local model");
                        egui::ComboBox::from_id_salt("llm")
                            .selected_text(match self.llm {
                                LlmMode::Auto => "Only pages we OCR'd",
                                LlmMode::Always => "Every paragraph (slow)",
                                LlmMode::Suspicious => "Only doubtful paragraphs",
                                LlmMode::Never => "Off",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.llm, LlmMode::Auto, "Only pages we OCR'd");
                                ui.selectable_value(&mut self.llm, LlmMode::Suspicious, "Only doubtful paragraphs");
                                ui.selectable_value(&mut self.llm, LlmMode::Always, "Every paragraph (slow)");
                                ui.selectable_value(&mut self.llm, LlmMode::Never, "Off");
                            });
                        ui.end_row();

                        ui.label("Page breaks");
                        egui::ComboBox::from_id_salt("pb")
                            .selected_text(match self.page_breaks {
                                PageBreakMode::Anchors => "Invisible markers (keeps reflow)",
                                PageBreakMode::Hard => "Force a real break",
                                PageBreakMode::None => "Discard",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.page_breaks, PageBreakMode::Anchors, "Invisible markers (keeps reflow)");
                                ui.selectable_value(&mut self.page_breaks, PageBreakMode::Hard, "Force a real break");
                                ui.selectable_value(&mut self.page_breaks, PageBreakMode::None, "Discard");
                            });
                        ui.end_row();

                        ui.label("Title (optional)");
                        ui.text_edit_singleline(&mut self.title);
                        ui.end_row();
                        ui.label("Author (optional)");
                        ui.text_edit_singleline(&mut self.author);
                        ui.end_row();
                        ui.label("Model");
                        ui.text_edit_singleline(&mut self.llm_model);
                        ui.end_row();
                        ui.label("Ollama URL(s)")
                            .on_hover_text("Comma-separated for several servers; the batches are shared out over them.");
                        ui.text_edit_singleline(&mut self.ollama_url);
                        ui.end_row();
                        ui.label("Save to");
                        ui.horizontal(|ui| {
                            if ui.button("Folder…").clicked() {
                                if let Some(d) = rfd::FileDialog::new().pick_folder() {
                                    self.out_dir = Some(d);
                                }
                            }
                            match &self.out_dir {
                                Some(d) => ui.label(egui::RichText::new(d.display().to_string()).monospace()),
                                None => ui.label(egui::RichText::new("next to the PDF").weak()),
                            };
                        });
                        ui.end_row();
                    });
                });

            ui.add_space(12.0);

            // ---- action -----------------------------------------------------
            let can_start = self.input.is_some() && !self.formats().is_empty() && !busy;
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(can_start, egui::Button::new("  Convert  "))
                    .clicked()
                {
                    self.start(&ctx);
                }
                if busy && ui.button("Stop").clicked() {
                    if let Some(j) = &self.job {
                        j.cancel();
                    }
                }
                if self.input.is_none() {
                    ui.label(egui::RichText::new("Choose a PDF first.").weak());
                } else if self.formats().is_empty() {
                    ui.label(egui::RichText::new("Pick at least one output format.").weak());
                }
            });

            if busy || self.result.is_some() {
                ui.add_space(8.0);
                let label = self
                    .stage
                    .map(|s| s.label().to_string())
                    .unwrap_or_default();
                ui.add(
                    egui::ProgressBar::new(self.progress.clamp(0.0, 1.0))
                        .text(label)
                        .desired_width(ui.available_width()),
                );
            }

            // ---- result -----------------------------------------------------
            if let Some(res) = &self.result {
                ui.add_space(10.0);
                match res {
                    Ok(s) => {
                        ui.group(|ui| {
                            ui.set_width(ui.available_width());
                            ui.strong(format!("Done in {:.1}s", s.elapsed.as_secs_f32()));
                            ui.label(format!(
                                "{} paragraphs · {} headings · {} page markers · {} characters",
                                s.paragraphs, s.headings, s.page_markers, s.characters
                            ));
                            ui.label(format!(
                                "{} page(s) from the text layer, {} from OCR",
                                s.text_pages, s.ocr_pages
                            ));
                            if s.llm_changed + s.llm_rejected > 0 {
                                ui.label(format!(
                                    "{} typo fixes applied, {} refused as too different",
                                    s.llm_changed, s.llm_rejected
                                ));
                            }
                            ui.add_space(4.0);
                            for p in &s.written {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(
                                            p.file_name()
                                                .map(|s| s.to_string_lossy().to_string())
                                                .unwrap_or_default(),
                                        )
                                        .monospace(),
                                    );
                                    if ui.small_button("Show").clicked() {
                                        reveal(p);
                                    }
                                });
                            }
                        });
                    }
                    Err(e) => {
                        ui.group(|ui| {
                            ui.set_width(ui.available_width());
                            ui.colored_label(egui::Color32::from_rgb(200, 60, 60), "Failed");
                            // Errors carry their own hint line, so show it whole.
                            ui.label(e.clone());
                        });
                    }
                }
            }

            // ---- log --------------------------------------------------------
            if !self.log.is_empty() {
                ui.add_space(10.0);
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(150.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for l in &self.log {
                            ui.label(egui::RichText::new(l).small().monospace());
                        }
                    });
            }
        }
    }
}

/// Whether calibre is present, so the UI can warn before a long run rather
/// than after it.
fn kindle_converter_available() -> bool {
    // Checked lazily and cached: probing the filesystem every frame is wasteful.
    use std::sync::OnceLock;
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(pdf_to_ebook_orchestrator::converter_available)
}
