# Language packs

One file per language, named after its **tesseract code** — `tur.md` for
Turkish, `nld.md` for Dutch. That is the code `--lang` takes and the one the
GUI's language box produces, so nothing else has to be wired up: drop the file
in and a run in that language picks it up.

A pack holds only what is untrue of every other language: which letters the
language has, and what its orthography does. `ğ`, dotted `İ` and the apostrophe
before a case suffix are Turkish facts, and telling an English book about them
is at best noise. Everything about the scanner and the shapes of Latin letters
belongs in `../proofread.md` instead, where every language gets it.

Only Turkish has a pack today, because Turkish is the only language whose
artefacts have been measured here (`docs/findings.md` §4.5). A language with no
pack is not a failure case: it gets the general rules and its own name, which
is what every language got before the split.

## Writing one

```markdown
# Dutch

## label

Dutch

## rules

- The fault, then how to repair it, then two or three real examples
  (misread -> correct) from the scan you measured it on.
```

`## label` is optional when the code is already in `LANGUAGES`
(`crates/core/src/config.rs`) — that table supplies the name. Give it for a
code the table does not know, which is also how you use a pack for a language
the built-in list has never heard of.

Rules are numbered when the prompt is rendered, so never write a number.

Measure before you add. The rules are ranked by how often the fault actually
occurred, and a small model follows a short ranked list far better than a long
one — an unranked guess added to the end costs more than it is worth.
