//! Content-addressed disk cache for model replies.
//!
//! Keyed by (model, prompt fingerprint, paragraph), so a re-run of a 419-page
//! book costs nothing and an interrupted run resumes where it stopped.
//!
//! The fingerprint is a hash of the request shape and the *rendered* prompt,
//! not a version number someone has to remember to bump. Since `prompts/` is
//! editable, an entry written under one prompt must never be served under
//! another — the whole value of the cache is that it answers the same question
//! twice, and an edited prompt is a different question.

use std::path::PathBuf;

pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    /// `PDF_TO_EBOOK_CACHE_DIR` overrides the OS cache directory, for a machine
    /// where the replies belong on a particular disk — or a test that must not
    /// touch the real one. `None` when there is no writable directory at all,
    /// which simply means the run is uncached.
    pub fn open(model: &str) -> Option<Cache> {
        let base = match pdf_to_ebook_core::env::path("PDF_TO_EBOOK_CACHE_DIR") {
            Some(d) => d,
            None => dirs::cache_dir()?.join("pdf-to-ebook"),
        }
        .join("llm");
        let safe: String = model
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '_' })
            .collect();
        let dir = base.join(safe);
        std::fs::create_dir_all(&dir).ok()?;
        Some(Cache { dir })
    }

    fn path(&self, fingerprint: &str, text: &str) -> PathBuf {
        let mut h = blake3::Hasher::new();
        h.update(fingerprint.as_bytes());
        h.update(text.as_bytes());
        let hex = h.finalize().to_hex();
        // Shard by the first two hex chars to keep directories small.
        let s = hex.to_string();
        let (a, rest) = s.split_at(2);
        self.dir.join(a).join(format!("{rest}.txt"))
    }

    pub fn get(&self, fingerprint: &str, text: &str) -> Option<String> {
        std::fs::read_to_string(self.path(fingerprint, text)).ok()
    }

    pub fn put(&self, fingerprint: &str, text: &str, reply: &str) {
        let p = self.path(fingerprint, text);
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(p, reply);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_changed_prompt_does_not_hit_an_entry_written_for_the_old_one() {
        let dir = std::env::temp_dir().join(format!("pdf-to-ebook-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let c = Cache { dir: dir.clone() };
        c.put("prompt-v1", "girdim:'", "girdim.\"");
        assert_eq!(c.get("prompt-v1", "girdim:'").as_deref(), Some("girdim.\""));
        // The whole reason the fingerprint is in the key rather than beside it.
        assert_eq!(c.get("prompt-v2", "girdim:'"), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
