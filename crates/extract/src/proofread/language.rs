//! The language-specific half of the proofreading prompt.
//!
//! The prompt is in two parts. `system_prompt` owns the part that holds for any
//! Latin-script scan — the fragment rule, the mangled quotation marks, the
//! letter-shape confusions, the limits — and this module owns the part that is
//! only true of one language: which letters that language has, and what its
//! orthography does. `ğ`, dotted `İ` and the apostrophe before a case suffix are
//! Turkish facts, and telling an English book about them is at best noise.
//!
//! Which pack is used comes from the language the user already picks: `--lang`
//! on the CLI, the language box in the GUI. Both end up in `Config::lang` as a
//! tesseract code, and `prompt_language` is what turns that code into a name
//! and a rule set.
//!
//! A language with no pack is not a failure case — it gets the general rules
//! and its name, which is what every language got before the split. Only
//! Turkish has a pack today because Turkish is the only language whose
//! artefacts have been measured here (`docs/findings.md` §4.5).

/// What the prompt knows about the language of the book.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptLanguage {
    /// Human-readable name, e.g. `Turkish`, for the prompt to say out loud.
    pub label: &'static str,
    /// Fault classes that occur only in this language. Empty is normal.
    pub rules: &'static [&'static str],
}

/// Faults that need Turkish to state, measured on the 419-page Turkish book.
///
/// The last entry is not a rule but the measured examples of the *general*
/// classes as this scan renders them. They were part of the tuned prompt and
/// are kept, because they are the evidence the numbers in `findings.md` §4.5
/// were produced with — they simply have no business being shown to an English
/// book.
const TURKISH: &[&str] = &[
    "Lost `ğ`. A capital `A` inside a lowercase word is a `ğ` that failed to \
scan; restore it with the vowels the word needs (girdiAfde -> girdiğinde, \
SoluAum -> Soluğum, canının yandıAı -> canının yandığı).",
    "`m` is also misread as `ın` or `nı` (sarınısaklı -> sarımsaklı), and `rı` \
collapses to `n` (Notlannda -> Notlarında, yanyor -> yarıyor).",
    "A case suffix on a name takes an apostrophe, and the name itself is often \
misread (Carlanm -> Carla'nın, Cariayı -> Carla'yı, Brooke,u -> Brooke'u, \
ClaphamCiaki -> Clapham'daki).",
    "A sentence starting with a lowercase `i` starts with `İ` (istemsizce -> \
İstemsizce, \"insanlar -> \"İnsanlar).",
    "Examples of the faults above, as this scan renders them: girdim:' -> girdim.\", \
temizlerneye -> temizlemeye, Dikatle -> Dikkatle, ayakbı -> ayakkabı, \
müve.kler -> müvekkiller, belieğimi -> belleğimi, bağcıldı -> bağcıklı, \
hanyoda -> banyoda, ikna ebneye -> ikna etmeye, Sabrı m -> Sabrım, tuhafbir -> \
tuhaf bir, oldu�nu -> olduğunu, Halin�e -> Halinde, ç�dığının -> çıkardığının, \
koştu� -> koştu.",
];

/// The name and rule pack for a tesseract code.
///
/// `None` for a code we have no name for — better to say nothing than to name
/// the wrong language, and a pack we cannot name is a pack we cannot trust to
/// be about the right script either.
pub fn prompt_language(tesseract_code: &str) -> Option<PromptLanguage> {
    let label = pdftomobi_core::label_for(tesseract_code)?;
    let rules: &'static [&'static str] = match tesseract_code {
        "tur" => TURKISH,
        _ => &[],
    };
    Some(PromptLanguage { label, rules })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turkish_brings_its_own_rules() {
        let l = prompt_language("tur").expect("tur is a known language");
        assert_eq!(l.label, "Turkish");
        assert!(l.rules.iter().any(|r| r.contains("Lost `ğ`")));
        assert!(l.rules.iter().any(|r| r.contains("İstemsizce")));
    }

    #[test]
    fn a_language_we_have_not_measured_gets_a_name_and_no_rules() {
        let l = prompt_language("eng").expect("eng is a known language");
        assert_eq!(l.label, "English");
        assert!(l.rules.is_empty());
    }

    #[test]
    fn an_unknown_code_is_not_guessed_at() {
        assert_eq!(prompt_language("xyz"), None);
        assert_eq!(prompt_language(""), None);
    }
}
