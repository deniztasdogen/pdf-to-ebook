//! MOBI / AZW3 output, by handing the EPUB to calibre's `ebook-convert`.
//!
//! There is no usable native Rust MOBI writer, so this is the one place the
//! pipeline depends on an external tool. Everything upstream is pure Rust.

use pdf_to_ebook_core::{Error, OutputFormat, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

const HINT: &str = "install calibre from https://calibre-ebook.com (macOS: \
`brew install --cask calibre`), or ask only for EPUB output";

/// Locate `ebook-convert`.
///
/// `EBOOK_CONVERT`, from the environment or `.env`, wins when it names a file
/// that exists. Otherwise `PATH`, and then the usual install locations: on
/// macOS the binary lives inside the app bundle and is not on `PATH` by
/// default, so `which ebook-convert` failing does not mean calibre is missing.
pub fn find_converter() -> Result<PathBuf> {
    if let Some(p) = pdf_to_ebook_core::env::path("EBOOK_CONVERT") {
        if p.is_file() {
            return Ok(p);
        }
        tracing::warn!("EBOOK_CONVERT names {}, which is not a file", p.display());
    }
    if let Ok(p) = which::which("ebook-convert") {
        return Ok(p);
    }
    const CANDIDATES: &[&str] = &[
        "/Applications/calibre.app/Contents/MacOS/ebook-convert",
        "/opt/homebrew/bin/ebook-convert",
        "/usr/local/bin/ebook-convert",
        "/usr/bin/ebook-convert",
    ];
    for c in CANDIDATES {
        let p = PathBuf::from(c);
        if p.is_file() {
            return Ok(p);
        }
    }
    Err(Error::ConverterUnavailable {
        reason: "could not find `ebook-convert`".to_string(),
        hint: HINT.to_string(),
    })
}

pub fn available() -> bool {
    find_converter().is_ok()
}

/// Convert an EPUB to MOBI or AZW3.
pub fn convert(epub: &Path, out: &Path, format: OutputFormat) -> Result<()> {
    if !matches!(format, OutputFormat::Mobi | OutputFormat::Azw3) {
        return Err(Error::ConverterFailed(format!(
            "{format:?} is not a converter target"
        )));
    }
    let exe = find_converter()?;
    let mut cmd = Command::new(&exe);
    cmd.arg(epub).arg(out);
    if format == OutputFormat::Mobi {
        // `both` writes the old MOBI6 index alongside KF8, which is what makes
        // the file usable on older Kindles as well as new ones.
        cmd.args(["--mobi-file-type", "both"]);
    }
    let output = cmd.output().map_err(|e| Error::ConverterUnavailable {
        reason: format!("could not run {}: {e}", exe.display()),
        hint: HINT.to_string(),
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        // calibre reports real problems on stdout, so include both.
        return Err(Error::ConverterFailed(
            format!("{stderr}\n{stdout}").trim().to_string(),
        ));
    }
    if !out.exists() {
        return Err(Error::ConverterFailed(format!(
            "{} reported success but wrote no file",
            exe.display()
        )));
    }
    Ok(())
}
