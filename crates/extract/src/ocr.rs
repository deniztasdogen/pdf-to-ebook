//! OCR behind a trait, so the engine can be swapped without touching layout.
//!
//! v1 shells out to the `tesseract` CLI and parses its TSV output. That gives
//! word boxes, per-word confidence and tesseract's own block/line segmentation
//! with no build-time linkage to leptonica.
//!
//! `ocrs`, the pure-Rust alternative, was evaluated and rejected: its
//! recognition model's alphabet is hardcoded ASCII, with no `ı ğ ş ç` and no
//! `ë ï`, so it cannot read Turkish or Dutch at all.

use crate::geom::Rect;
use crate::model::Word;
use pdftomobi_core::{Error, Result};
use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

pub trait OcrEngine: Send + Sync {
    /// Recognise a rendered page. `dpi` is the resolution the image was
    /// rendered at, needed to convert pixels back to points.
    fn recognise(&self, img: &image::GrayImage, lang: &str, dpi: f32) -> Result<Vec<Word>>;

    /// Check the engine works before a long run starts.
    fn preflight(&self, lang: &str) -> Result<()>;
}

pub struct TesseractCli {
    exe: String,
}

impl Default for TesseractCli {
    fn default() -> Self {
        TesseractCli {
            exe: std::env::var("TESSERACT_BIN").unwrap_or_else(|_| "tesseract".to_string()),
        }
    }
}

const INSTALL_HINT: &str = "install it with `brew install tesseract tesseract-lang` \
    (macOS) or `apt install tesseract-ocr tesseract-ocr-all` (Debian/Ubuntu)";

impl TesseractCli {
    pub fn available_languages(&self) -> Result<Vec<String>> {
        let out = Command::new(&self.exe)
            .arg("--list-langs")
            .output()
            .map_err(|e| Error::OcrUnavailable {
                reason: format!("could not run `{}`: {e}", self.exe),
                hint: INSTALL_HINT.to_string(),
            })?;
        // tesseract prints the list on stdout with a header line.
        let text = String::from_utf8_lossy(&out.stdout);
        Ok(text
            .lines()
            .skip(1)
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }
}

impl OcrEngine for TesseractCli {
    fn preflight(&self, lang: &str) -> Result<()> {
        let langs = self.available_languages()?;
        if langs.is_empty() {
            return Err(Error::OcrUnavailable {
                reason: "tesseract reported no installed languages".to_string(),
                hint: INSTALL_HINT.to_string(),
            });
        }
        // A language spec can be a `+`-joined list, e.g. `eng+tur`.
        for part in lang.split('+') {
            if !langs.iter().any(|l| l == part) {
                return Err(Error::OcrUnavailable {
                    reason: format!(
                        "tesseract has no language data for {part:?} (installed: {})",
                        langs.join(", ")
                    ),
                    hint: INSTALL_HINT.to_string(),
                });
            }
        }
        Ok(())
    }

    fn recognise(&self, img: &image::GrayImage, lang: &str, dpi: f32) -> Result<Vec<Word>> {
        // Write a PNG to a temp file. Piping via stdin is possible but the CLI
        // wants a seekable input for some formats, and a temp file keeps the
        // failure modes obvious.
        //
        // The name comes from a counter, not a timestamp. Pages are recognised
        // in parallel, and a nanosecond clock is not guaranteed to differ
        // between threads that start together — two pages then share a
        // filename, one overwrites the other's image, and the book silently
        // ends up with a duplicated page and a missing one.
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir();
        let stamp = format!(
            "pdftomobi-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let img_path = dir.join(format!("{stamp}.png"));
        {
            let mut f = std::fs::File::create(&img_path).map_err(|e| Error::io(&img_path, e))?;
            let mut buf = Vec::new();
            image::codecs::png::PngEncoder::new(&mut buf)
                .write_image(
                    img.as_raw(),
                    img.width(),
                    img.height(),
                    image::ExtendedColorType::L8,
                )
                .map_err(|e| Error::Pdf(format!("png encode: {e}")))?;
            f.write_all(&buf).map_err(|e| Error::io(&img_path, e))?;
        }
        let _guard = TempFile(img_path.clone());

        let out = Command::new(&self.exe)
            .arg(&img_path)
            .arg("stdout")
            .args(["-l", lang])
            // psm 1 = automatic page segmentation with orientation detection.
            // This is what correctly ordered the two columns of the Dutch
            // e-reader screenshot; the default psm 3 does not run OSD.
            .args(["--psm", "1"])
            .args(["--dpi", &format!("{}", dpi.round() as i32)])
            .arg("tsv")
            .output()
            .map_err(|e| Error::OcrUnavailable {
                reason: format!("could not run `{}`: {e}", self.exe),
                hint: INSTALL_HINT.to_string(),
            })?;

        if !out.status.success() {
            return Err(Error::OcrFailed {
                page: 0,
                reason: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            });
        }
        Ok(parse_tsv(&String::from_utf8_lossy(&out.stdout), dpi))
    }
}

struct TempFile(std::path::PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Parse tesseract's TSV. Columns:
/// `level page_num block_num par_num line_num word_num left top width height conf text`
///
/// Only `level == 5` rows (words) are kept. Pixel coordinates are scaled back
/// to points so that everything downstream shares one unit.
pub fn parse_tsv(tsv: &str, dpi: f32) -> Vec<Word> {
    let scale = 72.0 / dpi.max(1.0);
    let mut words = Vec::new();
    for line in tsv.lines().skip(1) {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 12 {
            continue;
        }
        if f[0] != "5" {
            continue;
        }
        let text = f[11].trim();
        if text.is_empty() {
            continue;
        }
        let (Ok(left), Ok(top), Ok(w), Ok(h)) = (
            f[6].parse::<f32>(),
            f[7].parse::<f32>(),
            f[8].parse::<f32>(),
            f[9].parse::<f32>(),
        ) else {
            continue;
        };
        let conf = f[10].parse::<f32>().ok();
        // tesseract sometimes emits conf -1 for structural rows.
        if conf.map(|c| c < 0.0).unwrap_or(false) {
            continue;
        }
        words.push(Word {
            text: text.to_string(),
            bbox: Rect::new(
                left * scale,
                top * scale,
                (left + w) * scale,
                (top + h) * scale,
            ),
            font_size: h * scale,
            conf,
            // tesseract's TSV has no font information.
            bold: false,
        });
    }
    words
}

use image::ImageEncoder;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_word_rows_and_scales_to_points() {
        // Real rows captured from `tesseract booksample_300.png ... tsv`.
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
                   1\t1\t0\t0\t0\t0\t0\t0\t3507\t2480\t-1\t\n\
                   5\t1\t6\t1\t3\t1\t854\t1355\t99\t36\t96.5\tdoor\n\
                   5\t1\t6\t1\t3\t2\t970\t1366\t74\t25\t93.3\teen\n";
        let words = parse_tsv(tsv, 300.0);
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].text, "door");
        // 854 px at 300 dpi = 204.96 pt
        assert!((words[0].bbox.x0 - 204.96).abs() < 0.01, "{:?}", words[0].bbox);
        assert_eq!(words[0].conf, Some(96.5));
        assert_eq!(words[1].text, "een");
    }

    #[test]
    fn skips_structural_and_empty_rows() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
                   2\t1\t1\t0\t0\t0\t59\t125\t3389\t6\t-1\t\n\
                   5\t1\t1\t1\t1\t1\t59\t125\t3389\t6\t95.0\t \n\
                   5\t1\t2\t1\t1\t1\t112\t180\t23\t42\t49.2\t<\n";
        let words = parse_tsv(tsv, 300.0);
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].text, "<");
    }
}
