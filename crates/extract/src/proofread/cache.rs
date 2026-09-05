//! Content-addressed disk cache for model replies.
//!
//! Keyed by (model, prompt version, paragraph), so a re-run of a 419-page book
//! costs nothing and an interrupted run resumes where it stopped.

use std::path::PathBuf;

pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    pub fn open(model: &str) -> Option<Cache> {
        let base = dirs::cache_dir()?.join("pdftomobi").join("llm");
        let safe: String = model
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '_' })
            .collect();
        let dir = base.join(safe);
        std::fs::create_dir_all(&dir).ok()?;
        Some(Cache { dir })
    }

    fn path(&self, version: u32, text: &str) -> PathBuf {
        let mut h = blake3::Hasher::new();
        h.update(&version.to_le_bytes());
        h.update(text.as_bytes());
        let hex = h.finalize().to_hex();
        // Shard by the first two hex chars to keep directories small.
        let s = hex.to_string();
        let (a, rest) = s.split_at(2);
        self.dir.join(a).join(format!("{rest}.txt"))
    }

    pub fn get(&self, version: u32, text: &str) -> Option<String> {
        std::fs::read_to_string(self.path(version, text)).ok()
    }

    pub fn put(&self, version: u32, text: &str, reply: &str) {
        let p = self.path(version, text);
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(p, reply);
    }
}
