//! Settings that describe the **machine**, not the book: where the external
//! tools live, which ollama servers to use, which model to ask.
//!
//! They are read from the process environment, and from a `.env` file when
//! there is one, so that a checkout can be pointed at a particular setup
//! without editing any source. `.env.example` is the documented list.
//!
//! Two properties this deliberately has:
//!
//! * **The process environment always wins.** A `.env` supplies a default, so
//!   `PDF_TO_EBOOK_LLM_MODEL=x pdf-to-ebook book.pdf` still overrides one — and a
//!   command-line flag overrides both, because layer 1 only ever asks for a
//!   *default* to hand `clap`.
//! * **Nothing is ever written back into the process environment.** The file
//!   is parsed into a map and read from there. `std::env::set_var` mutates
//!   global state that other threads may be reading, and this program runs
//!   OCR and proofreading on thread pools.
//!
//! This is layer 0, so the accessors here are plain strings and numbers. What
//! they mean is layer 1's business: [`Defaults`] is what the CLI and the GUI
//! start a [`Config`](crate::Config) from.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The compiled-in fallbacks. Also what `Config::new` uses, so that a `Config`
/// built in a test does not depend on the developer's `.env`.
pub mod defaults {
    pub const OLLAMA_URL: &str = "http://localhost:11434";
    pub const LLM_MODEL: &str = "gemma4:e4b";
    pub const OCR_LANG: &str = "eng";
    pub const DPI: f32 = 300.0;
    pub const TESSERACT_BIN: &str = "tesseract";
}

/// Parsed `.env`, loaded once. Empty when there is no file, which is the
/// ordinary case for someone who has not needed to override anything.
fn file() -> &'static HashMap<String, String> {
    static FILE: OnceLock<HashMap<String, String>> = OnceLock::new();
    FILE.get_or_init(|| match locate() {
        Some(p) => match std::fs::read_to_string(&p) {
            Ok(s) => parse(&s),
            Err(e) => {
                // A `.env` that cannot be read is worth saying out loud: the
                // run will otherwise silently use compiled-in defaults.
                eprintln!("!!  could not read {}: {e}", p.display());
                HashMap::new()
            }
        },
        None => HashMap::new(),
    })
}

/// Where the `.env` is: named outright, or the nearest one at or above the
/// working directory.
///
/// Walking up is what makes `cargo run` from inside `crates/cli` behave the
/// same as from the repo root.
fn locate() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PDF_TO_EBOOK_ENV_FILE") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let mut dir: &Path = &std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join(".env");
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
}

/// `KEY=value` a line at a time, with `#` comments, an optional `export`
/// prefix, and quoted values for when the value has a `#` or trailing spaces
/// in it. Deliberately small: this is a settings file, not a shell.
fn parse(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let value = value.trim();
        let value = match value.chars().next() {
            // Quoted: everything up to the closing quote, verbatim.
            Some(q @ ('"' | '\'')) => match value[1..].find(q) {
                Some(end) => value[1..1 + end].to_string(),
                None => value[1..].to_string(),
            },
            // Bare: a `#` after the value starts a comment.
            _ => match value.split_once(" #") {
                Some((v, _)) => v.trim_end().to_string(),
                None => value.to_string(),
            },
        };
        out.insert(key.to_string(), value);
    }
    out
}

/// A setting, from the process environment first and the `.env` second.
///
/// An empty value counts as unset, so `PDF_TO_EBOOK_LLM_MODEL=` in a `.env` falls
/// back to the default rather than asking ollama for the model named "".
pub fn var(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .or_else(|| file().get(key).cloned())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// A setting, or the given fallback.
pub fn var_or(key: &str, fallback: &str) -> String {
    var(key).unwrap_or_else(|| fallback.to_string())
}

/// A setting that names a directory or a file. `None` when unset, so the
/// caller keeps whatever discovery it does on its own.
pub fn path(key: &str) -> Option<PathBuf> {
    var(key).map(PathBuf::from)
}

/// The starting point for a [`Config`](crate::Config), before the command line
/// or the GUI has its say.
///
/// Only the settings that are about the machine are here. `--format`,
/// `--pages` and the crop are about the book in front of you and have no
/// business in an environment file.
#[derive(Debug, Clone, PartialEq)]
pub struct Defaults {
    /// Unparsed, exactly as `--ollama-url` takes it: a comma-separated list.
    pub ollama_url: String,
    pub llm_model: String,
    pub lang: String,
    pub dpi: f32,
}

impl Default for Defaults {
    /// The compiled-in values, with no file and no environment consulted.
    fn default() -> Self {
        Defaults {
            ollama_url: defaults::OLLAMA_URL.to_string(),
            llm_model: defaults::LLM_MODEL.to_string(),
            lang: defaults::OCR_LANG.to_string(),
            dpi: defaults::DPI,
        }
    }
}

impl Defaults {
    /// What layer 1 builds its flag defaults from.
    ///
    /// `OLLAMA_HOST` is ollama's own variable and is honoured as a fallback,
    /// so a machine that already points at a remote ollama needs no `.env` at
    /// all. A value it cannot use — a `dpi` that is not a number — is reported
    /// and ignored rather than taken as zero.
    ///
    /// Read once and kept. The CLI asks for this separately for each flag it
    /// defaults, and a `.env` with one bad line should complain once, not five
    /// times.
    pub fn from_env() -> Defaults {
        static ENV: OnceLock<Defaults> = OnceLock::new();
        ENV.get_or_init(Defaults::read_env).clone()
    }

    fn read_env() -> Defaults {
        let d = Defaults::default();
        Defaults {
            ollama_url: var("PDF_TO_EBOOK_OLLAMA_URL")
                .or_else(|| var("OLLAMA_HOST"))
                .unwrap_or(d.ollama_url),
            llm_model: var("PDF_TO_EBOOK_LLM_MODEL").unwrap_or(d.llm_model),
            lang: var("PDF_TO_EBOOK_OCR_LANG").unwrap_or(d.lang),
            dpi: match var("PDF_TO_EBOOK_DPI") {
                Some(v) => match v.parse::<f32>() {
                    Ok(n) if n > 0.0 => n,
                    _ => {
                        eprintln!("!!  PDF_TO_EBOOK_DPI is not a positive number ({v:?}); ignored");
                        d.dpi
                    }
                },
                None => d.dpi,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_settings_file_is_key_equals_value_with_comments_and_quotes() {
        let m = parse(
            "# a comment\n\
             \n\
             PDF_TO_EBOOK_LLM_MODEL=gemma4:e4b\n\
             export TESSERACT_BIN=/opt/homebrew/bin/tesseract\n\
             SPACED = value with spaces \n\
             QUOTED=\"has # a hash in it\"\n\
             SINGLE='keeps  everything'\n\
             TRAILING=value # and a comment\n\
             URLS=http://a:11434,http://b:11434\n\
             not a setting at all\n",
        );
        assert_eq!(m["PDF_TO_EBOOK_LLM_MODEL"], "gemma4:e4b");
        assert_eq!(m["TESSERACT_BIN"], "/opt/homebrew/bin/tesseract");
        assert_eq!(m["SPACED"], "value with spaces");
        assert_eq!(m["QUOTED"], "has # a hash in it");
        assert_eq!(m["SINGLE"], "keeps  everything");
        assert_eq!(m["TRAILING"], "value");
        // A `#` inside a URL fragment is not a comment; only ` #` starts one.
        assert_eq!(m["URLS"], "http://a:11434,http://b:11434");
        // Seven settings: the line that is not `key=value` is skipped.
        assert_eq!(m.len(), 7);
    }

    #[test]
    fn the_compiled_defaults_do_not_depend_on_the_environment() {
        // `Config::new` builds on these, so a test that never mentions ollama
        // must not start failing because the developer has a `.env`.
        let d = Defaults::default();
        assert_eq!(d.ollama_url, "http://localhost:11434");
        assert_eq!(d.llm_model, "gemma4:e4b");
        assert_eq!(d.lang, "eng");
        assert_eq!(d.dpi, 300.0);
    }
}
