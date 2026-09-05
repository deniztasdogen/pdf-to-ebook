//! The proofreading prompt, as files rather than string constants.
//!
//! The prompt is the single largest lever on proofreading quality — every
//! number in `docs/findings.md` §4.5 is a number about a *prompt*, not about
//! the code around it — so it lives in `prompts/`, where it can be re-tuned,
//! diffed and reviewed on its own terms.
//!
//! It is in two parts, and the split is the point. `proofread.md` holds what
//! is true of any Latin-script scan: the fragment rule, the mangled quotation
//! marks, the letter-shape confusions, the limits. `languages/<code>.md` holds
//! what is true of exactly one language — `ğ`, dotted `İ`, the apostrophe
//! before a Turkish case suffix — and is spliced into the middle of the list.
//! Which pack is used comes from the language the user already picks, `--lang`
//! or the GUI's language box, so adding a language is adding a file.
//!
//! **The built-in files are compiled in.** An installed binary has no
//! `prompts/` directory next to it and must still work, so `include_str!` is
//! the default and a directory is the override:
//!
//! ```text
//! PDF_TO_EBOOK_PROMPT_DIR=/somewhere/prompts pdf-to-ebook book.pdf --llm always
//! ```
//!
//! The override is per file, not all-or-nothing: a directory holding only
//! `languages/nld.md` adds a Dutch pack and keeps the built-in general prompt.
//! A file that is there but unreadable, or that is missing a section the
//! renderer needs, falls back to the built-in one with a warning rather than
//! failing the run — a bad prompt file must not cost you an extraction.

use std::collections::HashMap;
use std::path::PathBuf;

const BUILTIN_PROOFREAD: &str = include_str!("../../../../prompts/proofread.md");
const BUILTIN_TURKISH: &str = include_str!("../../../../prompts/languages/tur.md");

/// A built-in language pack, so that a checkout and an installed binary agree.
/// Adding a file to `prompts/languages/` means adding a line here; a pack that
/// is only ever loaded from `PDF_TO_EBOOK_PROMPT_DIR` needs no line at all.
const BUILTIN_LANGUAGES: &[(&str, &str)] = &[("tur", BUILTIN_TURKISH)];

/// One `## name` section: the prose before its first `-`, then its rules.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Section {
    /// Printed as written, above the numbers. Empty when there is none.
    pub lead_in: String,
    /// One per `-`, rejoined onto a single line.
    pub rules: Vec<String>,
}

/// A parsed prompt file, general or language pack. Unknown sections are kept
/// rather than rejected, so a file can carry notes the renderer ignores.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct PromptFile {
    sections: HashMap<String, Section>,
}

impl PromptFile {
    /// `## name` opens a section; anything above the first one is the file's
    /// own documentation and is dropped. Inside a section, the lines before
    /// the first `- ` are the lead-in and the rest are one rule per `- `,
    /// each rejoined with single spaces however far it wrapped.
    ///
    /// Only exactly two hashes open a section, so a file is free to use `#`
    /// for its title and `###` for prose headings of its own — those are
    /// dropped wherever they appear, rather than joined into a rule.
    pub fn parse(text: &str) -> PromptFile {
        let mut sections: HashMap<String, Section> = HashMap::new();
        let mut name: Option<String> = None;
        let mut lead: Vec<&str> = Vec::new();
        let mut rules: Vec<Vec<&str>> = Vec::new();

        // A section ends at the next header and at end of file, and both must
        // finish it the same way. A macro rather than a closure because it
        // moves out of `name` and clears the two buffers.
        macro_rules! flush {
            () => {
                if let Some(n) = name.take() {
                    sections.insert(
                        n,
                        Section {
                            lead_in: join(&lead),
                            rules: rules.iter().map(|r| join(r)).collect(),
                        },
                    );
                }
                lead.clear();
                rules.clear();
            };
        }

        for line in text.lines() {
            if let Some(header) = section_header(line) {
                flush!();
                name = Some(header.to_string());
                continue;
            }
            if name.is_none() {
                continue;
            }
            // A heading of any other level is the file's own prose. Skipped
            // outright rather than treated as text, or a `### Notes` between
            // two rules would be rejoined onto the end of the first one.
            if line.trim_start().starts_with('#') {
                continue;
            }
            match line.trim_start().strip_prefix("- ") {
                Some(first) => rules.push(vec![first]),
                None => match rules.last_mut() {
                    Some(current) => current.push(line),
                    None => lead.push(line),
                },
            }
        }
        flush!();
        PromptFile { sections }
    }

    fn get(&self, name: &str) -> Option<&Section> {
        self.sections.get(name)
    }

    /// The lead-in of a section that carries only a value — `## label`.
    fn value(&self, name: &str) -> Option<&str> {
        self.get(name)
            .map(|s| s.lead_in.as_str())
            .filter(|s| !s.is_empty())
    }

    /// Whether this file can be rendered at all. A file missing `intro` or
    /// `rules` is a typo, not a prompt, and the caller falls back rather than
    /// asking the model half a question.
    fn is_usable(&self) -> bool {
        self.value("intro").is_some() && self.get("rules").is_some_and(|s| !s.rules.is_empty())
    }
}

/// `## name`, and nothing else. `#` and `###` are prose: neither has a space
/// as its third character, so neither gets past the prefix.
fn section_header(line: &str) -> Option<&str> {
    Some(line.strip_prefix("## ")?.trim())
}

/// Rejoin wrapped lines. Blank lines vanish, which is what makes a rule free
/// to be laid out however it reads best in the file.
fn join(lines: &[&str]) -> String {
    lines
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// What the prompt knows about the language of the book: its name, and the
/// fault classes only it has. Both may be absent — a language with no pack
/// gets its name and the general rules, which is every language's baseline.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PromptLanguage {
    /// Human-readable name, e.g. `Turkish`, for the prompt to say out loud.
    pub label: String,
    /// Fault classes that occur only in this language. Empty is normal.
    pub rules: Vec<String>,
}

/// The general prompt plus whatever language packs have been asked for.
#[derive(Debug, Clone)]
pub struct Prompts {
    general: PromptFile,
    /// Where an override was loaded from, for the run report. `None` when
    /// everything came from the built-in files.
    dir: Option<PathBuf>,
}

impl Prompts {
    /// The compiled-in prompt, ignoring `PDF_TO_EBOOK_PROMPT_DIR` entirely.
    /// This is what the tests assert against, so that a developer who has
    /// pointed the variable at their own prompts still gets an honest
    /// `cargo test`.
    pub fn builtin() -> Prompts {
        Prompts {
            general: PromptFile::parse(BUILTIN_PROOFREAD),
            dir: None,
        }
    }

    /// The prompt as this run will actually use it: the built-in one, or the
    /// one in `PDF_TO_EBOOK_PROMPT_DIR` when that names a readable, usable file.
    pub fn load() -> Prompts {
        let Some(dir) = pdf_to_ebook_core::env::path("PDF_TO_EBOOK_PROMPT_DIR") else {
            return Prompts::builtin();
        };
        let path = dir.join("proofread.md");
        if !path.is_file() {
            // Not an error: the directory may exist only to add a language.
            return Prompts {
                dir: Some(dir),
                ..Prompts::builtin()
            };
        }
        let general = match std::fs::read_to_string(&path) {
            Ok(text) => {
                let parsed = PromptFile::parse(&text);
                if parsed.is_usable() {
                    parsed
                } else {
                    tracing::warn!(
                        "{} has no usable `## intro` and `## rules`; using the built-in prompt",
                        path.display()
                    );
                    PromptFile::parse(BUILTIN_PROOFREAD)
                }
            }
            Err(e) => {
                tracing::warn!("could not read {} ({e}); using the built-in prompt", path.display());
                PromptFile::parse(BUILTIN_PROOFREAD)
            }
        };
        Prompts {
            general,
            dir: Some(dir),
        }
    }

    /// The name and rule pack for a tesseract code.
    ///
    /// `None` for a code with neither a pack nor an entry in `LANGUAGES` —
    /// better to say nothing than to name the wrong language, and a pack we
    /// cannot name is a pack we cannot trust to be about the right script.
    pub fn language(&self, tesseract_code: &str) -> Option<PromptLanguage> {
        let pack = self.language_file(tesseract_code);
        let label = pack
            .as_ref()
            .and_then(|f| f.value("label"))
            .map(|s| s.to_string())
            .or_else(|| pdf_to_ebook_core::label_for(tesseract_code).map(|s| s.to_string()))?;
        let rules = pack
            .as_ref()
            .and_then(|f| f.get("rules"))
            .map(|s| s.rules.clone())
            .unwrap_or_default();
        Some(PromptLanguage { label, rules })
    }

    /// A pack from the override directory first, then the compiled-in ones. A
    /// code with no pack anywhere is the ordinary case, not a failure.
    fn language_file(&self, code: &str) -> Option<PromptFile> {
        if code.is_empty() || !code.chars().all(|c| c.is_ascii_alphanumeric()) {
            // The code becomes a filename, so anything that could climb out of
            // the directory is refused rather than sanitised.
            return None;
        }
        if let Some(dir) = &self.dir {
            let path = dir.join("languages").join(format!("{code}.md"));
            if path.is_file() {
                match std::fs::read_to_string(&path) {
                    Ok(text) => return Some(PromptFile::parse(&text)),
                    Err(e) => tracing::warn!("could not read {} ({e})", path.display()),
                }
            }
        }
        BUILTIN_LANGUAGES
            .iter()
            .find(|(c, _)| *c == code)
            .map(|(_, text)| PromptFile::parse(text))
    }

    /// The system prompt, assembled.
    ///
    /// The numbering is produced here rather than written in the files, so a
    /// language pack can be spliced into the middle of the list without any
    /// number being written by hand — and be left out again, for a language
    /// that has no pack, without leaving a gap. A section with no rules is
    /// skipped lead-in and all, which is what stops a pack-less language being
    /// handed an empty "These faults are specific to English:" heading.
    ///
    /// What was measured about the ordering is that rule 0 must precede the
    /// quote rules and the limits must come last. Where the language pack sits
    /// among the fault classes was not measured; it goes after the general
    /// ones, which keeps both of those orderings intact.
    pub fn system_prompt(&self, language: Option<&PromptLanguage>) -> String {
        let g = &self.general;
        let name = language.map(|l| l.label.as_str()).unwrap_or("");
        // `{language_sentence}` is the file's own sentence naming the
        // language, and nothing at all when there is no language to name — so
        // the one piece of English that varies stays in the file too.
        let sentence = match language {
            Some(_) => match g.value("language-sentence") {
                Some(s) => format!("{} ", s.replace("{language}", name).trim()),
                None => String::new(),
            },
            None => String::new(),
        };
        let mut out = g
            .value("intro")
            .unwrap_or_default()
            .replace("{language_sentence}", &sentence)
            .replace("{language}", name);

        let language_rules: &[String] = language.map(|l| &l.rules[..]).unwrap_or(&[]);
        let mut n = 0;
        for (section, extra) in [
            ("rule-zero", &[][..]),
            ("rules", &[]),
            ("language-rules", language_rules),
            ("limits", &[]),
        ] {
            let Some(s) = g.get(section) else { continue };
            let rules: Vec<&String> = s.rules.iter().chain(extra).collect();
            if rules.is_empty() {
                continue;
            }
            out.push_str("\n\n");
            out.push_str(&s.lead_in.replace("{language}", name));
            for rule in rules {
                out.push_str(&format!("\n{n}. {rule}"));
                n += 1;
            }
        }

        if let Some(trailer) = g.value("trailer") {
            out.push_str("\n\n");
            out.push_str(trailer);
        }
        out
    }

    /// Where the prompt came from, for the run report and for `--help`-ish
    /// output. `None` means the compiled-in files.
    pub fn source(&self) -> Option<&PathBuf> {
        self.dir.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtin() -> Prompts {
        Prompts::builtin()
    }

    #[test]
    fn a_section_is_a_lead_in_and_rules_rejoined_from_however_they_wrapped() {
        let f = PromptFile::parse(
            "# A title, not a section\n\
             \n\
             Documentation above the first header is dropped.\n\
             \n\
             ## intro\n\
             \n\
             One paragraph,\n\
             wrapped over two lines.\n\
             \n\
             ## rules\n\
             \n\
             A lead-in.\n\
             \n\
             - The first rule, which\n  wraps.\n\
             - The second.\n\
             \n\
             ### Not a section\n\
             \n\
             ## limits\n\
             \n\
             - Only a rule, no lead-in.\n",
        );
        assert_eq!(f.value("intro"), Some("One paragraph, wrapped over two lines."));
        let rules = f.get("rules").unwrap();
        assert_eq!(rules.lead_in, "A lead-in.");
        assert_eq!(rules.rules, ["The first rule, which wraps.", "The second."]);
        // `###` is prose, so it neither opens a section nor ends one.
        assert!(f.get("Not a section").is_none());
        assert_eq!(f.get("limits").unwrap().lead_in, "");
    }

    #[test]
    fn the_builtin_prompt_names_the_language_when_it_knows_it() {
        let p = builtin();
        let tur = p.language("tur").unwrap();
        assert!(p.system_prompt(Some(&tur)).contains("The text is in Turkish."));
        assert!(!p.system_prompt(None).contains("The text is in"));
    }

    #[test]
    fn turkish_brings_its_own_rules_and_english_does_not_get_them() {
        let p = builtin();
        let tur = p.language("tur").expect("tur is a known language");
        let eng = p.language("eng").expect("eng is a known language");
        assert_eq!(tur.label, "Turkish");
        assert_eq!(eng.label, "English");
        assert!(tur.rules.iter().any(|r| r.contains("Lost `ğ`")));
        assert!(tur.rules.iter().any(|r| r.contains("İstemsizce")));
        assert!(eng.rules.is_empty());
    }

    #[test]
    fn an_unknown_code_is_not_guessed_at() {
        let p = builtin();
        assert_eq!(p.language("xyz"), None);
        assert_eq!(p.language(""), None);
    }

    #[test]
    fn a_language_code_cannot_name_a_file_outside_the_pack_directory() {
        let p = builtin();
        // Would otherwise be `<dir>/languages/../../secrets.md`.
        assert!(p.language_file("../../secrets").is_none());
        assert!(p.language_file("tur/../..").is_none());
    }

    #[test]
    fn a_prompt_directory_overrides_the_built_in_files_one_at_a_time() {
        let dir = std::env::temp_dir().join(format!("pdf-to-ebook-prompt-dir-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("languages")).unwrap();
        std::fs::write(
            dir.join("languages").join("nld.md"),
            "## label\n\nDutch\n\n## rules\n\n- The Dutch `ij` is one letter.\n",
        )
        .unwrap();

        // Only the pack is overridden, so the general prompt is still ours.
        let p = Prompts {
            general: PromptFile::parse(BUILTIN_PROOFREAD),
            dir: Some(dir.clone()),
        };
        let nld = p.language("nld").expect("the pack supplies Dutch");
        assert_eq!(nld.rules, ["The Dutch `ij` is one letter."]);
        let rendered = p.system_prompt(Some(&nld));
        assert!(rendered.contains("These faults are specific to Dutch"));
        assert!(rendered.contains("The Dutch `ij` is one letter."));
        assert!(rendered.contains("Mangled closing quotation mark"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_pack_can_name_a_language_the_built_in_table_has_never_heard_of() {
        let dir = std::env::temp_dir().join(format!("pdf-to-ebook-prompt-new-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("languages")).unwrap();
        std::fs::write(
            dir.join("languages").join("ell.md"),
            "## label\n\nGreek\n\n## rules\n\n- A final sigma is `ς`, not `σ`.\n",
        )
        .unwrap();
        let p = Prompts {
            general: PromptFile::parse(BUILTIN_PROOFREAD),
            dir: Some(dir.clone()),
        };
        // `ell` is not in LANGUAGES, so without the pack it would be `None`.
        assert_eq!(builtin().language("ell"), None);
        assert_eq!(p.language("ell").unwrap().label, "Greek");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unusable_prompt_file_is_refused_rather_than_half_rendered() {
        assert!(PromptFile::parse(BUILTIN_PROOFREAD).is_usable());
        assert!(!PromptFile::parse("## intro\n\nWords, but no rules.\n").is_usable());
        assert!(!PromptFile::parse("## rules\n\n- A rule, but no intro.\n").is_usable());
        assert!(!PromptFile::parse("").is_usable());
    }
    #[test]
    fn the_rules_are_numbered_without_a_gap_whether_or_not_there_is_a_pack() {
        // The language pack is spliced into the middle of the list, so the
        // numbering has to be produced, not written down.
        let p = builtin();
        for lang in [p.language("tur"), p.language("eng"), None] {
            let rendered = p.system_prompt(lang.as_ref());
            let nums: Vec<usize> = rendered
                .lines()
                .filter_map(|l| l.split_once('.').and_then(|(n, _)| n.parse().ok()))
                .collect();
            let expected: Vec<usize> = (0..nums.len()).collect();
            assert_eq!(nums, expected, "numbering broke for {lang:?}");
        }
    }

    #[test]
    fn a_language_with_no_pack_is_not_handed_an_empty_heading() {
        let p = builtin();
        let eng = p.language("eng").unwrap();
        assert!(!p.system_prompt(Some(&eng)).contains("These faults are specific to"));
        assert!(!p.system_prompt(None).contains("These faults are specific to"));
    }

    #[test]
    fn the_limits_stay_after_every_fault_class() {
        // The one ordering findings.md §4.5 measured, besides rule 0 first: a
        // rule stated after the rule it contradicts does not hold.
        let p = builtin();
        let tur = p.language("tur").unwrap();
        let rendered = p.system_prompt(Some(&tur));
        let limits = rendered.find("Then obey these limits").expect("the limits are in there");
        assert!(rendered.find("Lost `ğ`").unwrap() < limits);
        assert!(rendered.find("Mangled closing").unwrap() < limits);
        assert!(
            rendered.find("NEVER make a string longer").unwrap()
                < rendered.find("Mangled closing").unwrap()
        );
    }

    #[test]
    fn the_prompt_states_the_fragment_rule() {
        // Rule 0 is what stops the model balancing quotes across a page break.
        let p = builtin();
        let tur = p.language("tur").unwrap();
        let rendered = p.system_prompt(Some(&tur));
        assert!(rendered.contains("MIDDLE of a sentence"));
        assert!(rendered.contains("NEVER make a string longer at its start or at its end"));
    }

    #[test]
    fn the_general_half_reaches_every_language_and_the_turkish_half_only_turkish() {
        let p = builtin();
        let tr = p.system_prompt(p.language("tur").as_ref());
        let en = p.system_prompt(p.language("eng").as_ref());
        for turkish_only in ["Lost `ğ`", "İstemsizce", "Clapham'daki", "sarımsaklı"] {
            assert!(tr.contains(turkish_only), "Turkish prompt lost {turkish_only:?}");
            assert!(!en.contains(turkish_only), "English prompt got {turkish_only:?}");
        }
        for general in ["Mangled closing quotation mark", "Tall thin letters", "Do not translate"] {
            assert!(tr.contains(general));
            assert!(en.contains(general));
        }
    }

    /// A dump of the rendered prompt, for eyeballing an edit to `prompts/`.
    /// `cargo test -p pdf-to-ebook-extract show_the_prompt -- --ignored --nocapture`
    #[test]
    #[ignore = "prints the prompt rather than checking anything"]
    fn show_the_prompt() {
        let p = Prompts::load();
        println!("{}", p.system_prompt(p.language("tur").as_ref()));
    }

}


