//! Local-LLM typo repair via Ollama.
//!
//! Five things here are load-bearing, each one a response to a measured
//! failure rather than a precaution:
//!
//! 1. `"think": false`. `gemma4:e4b` is a thinking model. Left on, the answer
//!    lands in the `thinking` field and `message.content` comes back **empty**
//!    - 2 of 5 test paragraphs returned nothing, at 6-17s each. Off, it is
//!    ~1s with no empties.
//! 2. **Batch length validation.** Ten paragraphs per request is 2.4x faster,
//!    but a test batch returned **9 items for 10 inputs**, silently dropping
//!    the shortest. A mismatch re-runs the batch one at a time.
//! 3. **A drift guard.** One observed "correction" turned `gec. saatiere` into
//!    `gec saate`, dropping a suffix and changing the meaning. Anything that
//!    strays too far from the original is rejected in favour of the original.
//! 4. **A disk cache**, so a re-run costs nothing and a long job is resumable.
//! 5. **A prompt that names the artefact classes.** A generic "fix OCR errors"
//!    instruction left the largest class — mangled quotation marks, a third of
//!    all repairs — entirely unaddressed. See `prompts/proofread.md`.
//!
//! The sixth thing is not a safeguard but the shape of the run: the batches are
//! handed out from **one queue to one worker per configured server**, so a book
//! can be proofread on several ollama boxes at once. Nothing is dealt out in
//! advance — a worker takes the next batch when it is free — so a fast server
//! simply takes more of them, and a server that goes away mid-run gives its
//! batch back to the others instead of taking it down with it.

mod cache;
mod pass;
mod prompt;

pub use cache::Cache;
pub use pass::{proofread_document, wants_llm, Pass};
pub use prompt::{PromptLanguage, Prompts};

use pdf_to_ebook_core::{Error, Result};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex;

/// Bump when the **request shape** changes — the JSON body, the options, the
/// way a reply is read — so old cache entries are not reused for a different
/// question.
///
/// The prompt itself no longer needs a bump. It is a file now, and a file can
/// be edited between two runs, so the cache key carries a hash of the rendered
/// prompt as well (see `Proofreader::fingerprint`). Editing `prompts/` costs a
/// full re-run and cannot silently serve an answer to the old question.
///
/// 2: ranked artefact-class prompt, plus the fragment and no-substitution rules.
/// 3: fragment rule promoted above the quote rules, which were overriding it.
/// 4: the colon in `:'` named as a misread full stop; lost-`ğ`, apostrophe and
///    sentence-initial `İ` classes added, which v3 had left out entirely.
/// 5: `�` no longer claimed to be a lost `ğ`. That held on the pages the prompt
///    was tuned on and was wrong on held-out pages, where `�` stood for a full
///    stop, a `d`, an `l`, an `m` and an `ıkar` — it made v4 worse than v1 there.
/// 6: split into a general prompt and a per-language one. A Turkish run is
///    meant to carry the same rules and the same examples v5 did, reordered and
///    renumbered; every other language stops being told about `ğ` and `İ`.
///    Not re-scored — the CER tables in `findings.md` §4.5 are v5's.
const PROMPT_VERSION: u32 = 6;

const BATCH_SIZE: usize = 10;

/// Reject a correction whose normalised edit similarity to the original falls
/// below this. Chosen to admit ordinary typo fixes and refuse rewrites.
const MIN_SIMILARITY: f64 = 0.90;

/// Reject a correction that changes the length by more than this fraction.
const MAX_LENGTH_DRIFT: f32 = 0.15;

/// Edits always allowed, however short the span, unless the span only grew at
/// one end — see `drift_reason`.
///
/// The ratio tests above are meaningless on a short string: `"Emin misin?''`
/// -> `"Emin misin?"` is a correct two-character repair, but two characters of
/// fourteen is 14% drift and 86% similarity, so both ratios refuse it. Measured
/// on the 419-page Turkish book, the median refused span was 36 characters
/// against 131 for accepted ones — the guard was rejecting short spans as a
/// class, and half the refusals sampled were correct fixes.
///
/// Four is the width of the recurring quote artefacts (`.,,` and `:'` -> `."`)
/// plus one. It is an absolute floor, not a replacement: a long span is still
/// held to the ratios, so this cannot licence a rewrite.
const ALWAYS_ALLOWED_EDITS: usize = 4;

#[derive(Debug, Clone)]
pub struct Correction {
    pub original: String,
    pub corrected: String,
    pub accepted: bool,
    pub reason: Option<String>,
}

pub struct Proofreader {
    /// One entry per request that may be in flight at once. `preflight`
    /// narrows it to the servers that answered; a URL listed twice stays
    /// twice, which is how two batches are run against one server.
    endpoints: Vec<String>,
    model: String,
    /// The system prompt, rendered once. Building it means reading files, and
    /// it is the same string for every batch of the run.
    system: String,
    /// `blake3(request shape + rendered prompt)`, the other half of the cache
    /// key. An edit to anything in `prompts/` changes it, so a stale answer to
    /// a prompt that no longer exists is never served.
    fingerprint: String,
    /// Where the prompt came from, when it was not the built-in one. Reported,
    /// because a run whose prompt is not the one in the repo should say so.
    prompt_source: Option<PathBuf>,
    cache: Option<Cache>,
    /// One agent for every worker. ureq's agent is a connection pool keyed by
    /// host and is meant to be shared, so there is nothing to gain from one
    /// per server and a pool per server to lose.
    agent: ureq::Agent,
}

#[derive(Default, Debug, Clone)]
pub struct Stats {
    pub considered: usize,
    pub changed: usize,
    pub rejected: usize,
    pub cache_hits: usize,
    pub batches_split: usize,
    /// Batches completed per server, in the order the servers were given. An
    /// uneven split is the point, not a fault: work is pulled, so a faster box
    /// takes more of it.
    pub per_server: Vec<(String, usize)>,
    /// Servers that stopped answering mid-run. Their unfinished batches went
    /// back to the others.
    pub servers_lost: Vec<String>,
    /// Paragraphs no server ever got to, because every one of them went away.
    /// Their original text is kept.
    pub unproofread: usize,
}

/// A batch of paragraphs and, for each, where in the document it came from.
type Batch<'a> = &'a [(usize, &'a String)];

/// What one server's worker did. Merged into [`Stats`] when the run ends.
#[derive(Default)]
struct Worker {
    endpoint: String,
    batches: usize,
    split: usize,
    /// The server stopped answering, so the worker retired and handed its
    /// batch back to whoever was left.
    lost: bool,
}

/// What the workers share: the queue they pull from, the replies they write
/// back, and the progress count. One mutex, because there is no ordering to
/// get wrong with one.
struct Shared<F> {
    queue: VecDeque<usize>,
    replies: Vec<Option<Vec<String>>>,
    done: usize,
    progress: F,
}

impl Proofreader {
    /// `urls` is the list of servers to spread the work over. One is the
    /// ordinary case and behaves exactly as it always did.
    pub fn new(urls: &[String], model: &str, lang: &str, use_cache: bool) -> Self {
        let prompts = Prompts::load();
        let system = prompts.system_prompt(prompts.language(lang).as_ref());
        Proofreader {
            endpoints: urls
                .iter()
                .map(|u| u.trim_end_matches('/').to_string())
                .collect(),
            model: model.to_string(),
            fingerprint: fingerprint(&system),
            system,
            prompt_source: prompts.source().cloned(),
            cache: if use_cache { Cache::open(model) } else { None },
            agent: ureq::AgentBuilder::new()
                // A machine in the list that is asleep must cost one short
                // connect timeout, not the read timeout below.
                .timeout_connect(std::time::Duration::from_secs(10))
                .timeout_read(std::time::Duration::from_secs(300))
                .timeout_write(std::time::Duration::from_secs(60))
                .build(),
        }
    }

    /// The servers still in play — after `preflight`, the ones that answered.
    pub fn endpoints(&self) -> &[String] {
        &self.endpoints
    }

    /// The directory the prompt was loaded from, or `None` for the built-in
    /// one. Layer 2 reports it, so a run that was not using the repo's prompt
    /// cannot be mistaken for one that was.
    pub fn prompt_source(&self) -> Option<&PathBuf> {
        self.prompt_source.as_ref()
    }

    /// Fail fast with an actionable message rather than after 400 pages.
    ///
    /// Every server is asked at once, so a laptop that is asleep costs one
    /// connect timeout for the whole list rather than one each. A server that
    /// does not answer, or that has not pulled the model, is dropped from the
    /// list and the reason comes back as a warning: the run goes ahead on the
    /// servers that are there. It is an error only when none is left, which
    /// for a single server is the behaviour unchanged.
    pub fn preflight(&mut self) -> Result<Vec<String>> {
        let checked: Vec<(String, Option<Error>)> = {
            let me: &Proofreader = self;
            std::thread::scope(|scope| {
                let handles: Vec<_> = me
                    .endpoints
                    .iter()
                    .map(|url| scope.spawn(move || (url.clone(), me.check(url).err())))
                    .collect();
                handles.into_iter().map(join).collect()
            })
        };

        let mut live = Vec::new();
        let mut warnings = Vec::new();
        let mut first: Option<Error> = None;
        for (url, err) in checked {
            match err {
                None => live.push(url),
                Some(e) => {
                    warnings.push(format!("not using {url} — {}", one_line(&e)));
                    if first.is_none() {
                        first = Some(e);
                    }
                }
            }
        }
        if live.is_empty() {
            return Err(first.unwrap_or_else(|| Error::OllamaUnreachable {
                url: String::new(),
                reason: "no ollama server was given".to_string(),
                hint: "pass one with --ollama-url".to_string(),
            }));
        }
        self.endpoints = live;
        Ok(warnings)
    }

    /// Is this one server up, and does it have the model?
    fn check(&self, url: &str) -> Result<()> {
        let tags = format!("{url}/api/tags");
        let resp = self
            .agent
            .get(&tags)
            .call()
            .map_err(|e| Error::OllamaUnreachable {
                url: url.to_string(),
                reason: e.to_string(),
                hint: "start it with `ollama serve`, or set proofreading to Never".to_string(),
            })?;
        let body: serde_json::Value = resp.into_json().map_err(|e| Error::OllamaUnreachable {
            url: url.to_string(),
            reason: format!("unexpected reply: {e}"),
            hint: "is something else listening on that port?".to_string(),
        })?;
        let names: Vec<String> = body["models"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|m| m["name"].as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        // Ollama accepts `foo` for `foo:latest`, so compare on both forms.
        let wanted = &self.model;
        let ok = names.iter().any(|n| {
            n == wanted
                || n.split(':').next() == Some(wanted.as_str())
                || format!("{n}:latest") == *wanted
        });
        if !ok {
            return Err(Error::OllamaUnreachable {
                url: url.to_string(),
                reason: format!(
                    "model {wanted:?} is not installed (available: {})",
                    names.join(", ")
                ),
                hint: format!("pull it with `ollama pull {wanted}`"),
            });
        }
        Ok(())
    }

    /// Proofread paragraphs, returning one entry per input in the same order.
    ///
    /// Batches are handed out from one queue, one at a time, to one worker per
    /// endpoint. Nothing is dealt out in advance: a server that answers in half
    /// the time simply comes back for the next batch sooner, so a fast desktop
    /// and a slow laptop still finish together instead of the desktop idling on
    /// a half it finished long ago.
    ///
    /// The cache is read before any of that and written after all of it, on
    /// this thread — so a paragraph is stored once, in input order, however
    /// many servers were racing over it.
    pub fn run(
        &self,
        paragraphs: &[String],
        stats: &mut Stats,
        on_progress: impl FnMut(usize, usize) + Send,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Vec<Correction>> {
        let total = paragraphs.len();
        let mut out: Vec<Correction> = paragraphs
            .iter()
            .map(|p| Correction {
                original: p.clone(),
                corrected: p.clone(),
                accepted: false,
                reason: None,
            })
            .collect();

        let mut pending: Vec<(usize, &String)> = Vec::new();
        for (i, p) in paragraphs.iter().enumerate() {
            if let Some(c) = self.cache.as_ref().and_then(|c| c.get(&self.fingerprint, p)) {
                stats.cache_hits += 1;
                out[i] = self.judge(p, &c);
                continue;
            }
            pending.push((i, p));
        }

        let batches: Vec<Batch> = pending.chunks(BATCH_SIZE).collect();
        let shared = Mutex::new(Shared {
            queue: (0..batches.len()).collect(),
            replies: vec![None; batches.len()],
            done: total - pending.len(),
            progress: on_progress,
        });
        report_progress(&shared, total);

        let mut workers: Vec<Worker> = Vec::new();
        std::thread::scope(|scope| {
            let handles: Vec<_> = self
                .endpoints
                .iter()
                .map(|url| {
                    let (shared, batches) = (&shared, &batches);
                    scope.spawn(move || self.work(url, batches, shared, total, cancelled))
                })
                .collect();
            for h in handles {
                workers.push(join(h));
            }
        });
        if cancelled() {
            return Err(Error::Cancelled);
        }

        let shared = shared.into_inner().unwrap();
        let mut unproofread = 0;
        for (b, batch) in batches.iter().enumerate() {
            let Some(replies) = &shared.replies[b] else {
                // Every server went away before anyone picked this batch up.
                // `out` already holds the original text.
                unproofread += batch.len();
                continue;
            };
            for ((idx, orig), reply) in batch.iter().zip(replies) {
                if let Some(c) = &self.cache {
                    c.put(&self.fingerprint, orig, reply);
                }
                out[*idx] = self.judge(orig, reply);
            }
        }

        stats.batches_split += workers.iter().map(|w| w.split).sum::<usize>();
        stats.per_server = workers
            .iter()
            .map(|w| (w.endpoint.clone(), w.batches))
            .collect();
        stats.servers_lost = workers
            .iter()
            .filter(|w| w.lost)
            .map(|w| w.endpoint.clone())
            .collect();
        stats.unproofread += unproofread;
        stats.considered += total;
        stats.changed += out.iter().filter(|c| c.accepted).count();
        stats.rejected += out.iter().filter(|c| c.reason.is_some()).count();
        Ok(out)
    }

    /// One server's worker: take the next batch, ask, write the replies back,
    /// repeat until the queue is empty.
    ///
    /// A batch that comes back the wrong length, or that fails outright, is
    /// redone one paragraph at a time against this same server — what a single
    /// server has always done. Only when not one of those singles gets through
    /// is the server presumed gone: the batch goes back on the queue for
    /// someone else and this worker retires. The distinction is the point. One
    /// batch hitting the read timeout must not disqualify a server that is
    /// merely busy, and a server that has been unplugged must not swallow
    /// batch after batch at ten failed requests each.
    fn work<F: FnMut(usize, usize) + Send>(
        &self,
        endpoint: &str,
        batches: &[Batch],
        shared: &Mutex<Shared<F>>,
        total: usize,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Worker {
        let mut w = Worker {
            endpoint: endpoint.to_string(),
            ..Worker::default()
        };
        loop {
            if cancelled() {
                return w;
            }
            let next = shared.lock().unwrap().queue.pop_front();
            let Some(b) = next else { return w };
            let inputs: Vec<String> = batches[b].iter().map(|(_, s)| (*s).clone()).collect();

            let answer = match self.ask_batch(endpoint, &inputs) {
                Ok(r) if r.len() == inputs.len() => Some(r),
                Ok(_) => {
                    // The verified failure mode: fewer items back than sent.
                    // Positional alignment is now meaningless, so redo singly.
                    w.split += 1;
                    self.ask_one_by_one(endpoint, &inputs)
                }
                Err(e) => {
                    tracing::warn!("batch failed on {endpoint} ({e}), retrying one at a time");
                    w.split += 1;
                    self.ask_one_by_one(endpoint, &inputs)
                }
            };
            let Some(replies) = answer else {
                shared.lock().unwrap().queue.push_back(b);
                w.lost = true;
                return w;
            };

            let mut sh = shared.lock().unwrap();
            sh.replies[b] = Some(replies);
            sh.done += inputs.len();
            let done = sh.done;
            (sh.progress)(done, total);
            drop(sh);
            w.batches += 1;
        }
    }

    /// Decide whether a model reply is a typo fix or a rewrite.
    fn judge(&self, original: &str, reply: &str) -> Correction {
        let cleaned = reply.trim();
        if cleaned.is_empty() {
            return Correction {
                original: original.to_string(),
                corrected: original.to_string(),
                accepted: false,
                reason: Some("model returned nothing".to_string()),
            };
        }
        if cleaned == original.trim() {
            return Correction {
                original: original.to_string(),
                corrected: original.to_string(),
                accepted: false,
                reason: None,
            };
        }
        if let Some(reason) = drift_reason(original, cleaned) {
            return Correction {
                original: original.to_string(),
                corrected: original.to_string(),
                accepted: false,
                reason: Some(reason),
            };
        }
        Correction {
            original: original.to_string(),
            corrected: cleaned.to_string(),
            accepted: true,
            reason: None,
        }
    }

    /// Ask for one paragraph at a time, keeping the original for any that
    /// fails. `None` when not one of them got through, which the caller reads
    /// as "this server is gone" rather than "these paragraphs are hard".
    fn ask_one_by_one(&self, endpoint: &str, inputs: &[String]) -> Option<Vec<String>> {
        let mut out = Vec::with_capacity(inputs.len());
        let mut answered = 0;
        for s in inputs {
            match self.ask_batch(endpoint, std::slice::from_ref(s)) {
                Ok(r) if r.len() == 1 => {
                    answered += 1;
                    out.push(r.into_iter().next().unwrap());
                }
                // Keep the original rather than aborting the whole run.
                _ => out.push(s.clone()),
            }
        }
        (answered > 0).then_some(out)
    }

    fn ask_batch(&self, endpoint: &str, inputs: &[String]) -> Result<Vec<String>> {
        let user = serde_json::json!({ "paragraphs": inputs }).to_string();
        let body = serde_json::json!({
            "model": self.model,
            "stream": false,
            // Without this the reply lands in `thinking` and content is empty.
            "think": false,
            "format": "json",
            "options": { "temperature": 0, "num_predict": 4096 },
            "messages": [
                { "role": "system", "content": self.system },
                { "role": "user", "content": user }
            ]
        });
        let url = format!("{endpoint}/api/chat");
        let resp = self
            .agent
            .post(&url)
            .send_json(body)
            .map_err(|e| Error::OllamaUnreachable {
                url: endpoint.to_string(),
                reason: e.to_string(),
                hint: "is `ollama serve` still running?".to_string(),
            })?;
        let v: serde_json::Value = resp
            .into_json()
            .map_err(|e| Error::Other(anyhow::anyhow!("ollama reply not json: {e}")))?;
        let content = v["message"]["content"].as_str().unwrap_or("").trim().to_string();
        if content.is_empty() {
            return Err(Error::Other(anyhow::anyhow!(
                "ollama returned empty content (is `think` being honoured?)"
            )));
        }
        Ok(parse_reply(&content))
    }
}

fn report_progress<F: FnMut(usize, usize)>(shared: &Mutex<Shared<F>>, total: usize) {
    let mut sh = shared.lock().unwrap();
    let done = sh.done;
    (sh.progress)(done, total);
}

/// The half of the cache key that is about the question rather than the text.
fn fingerprint(system: &str) -> String {
    let mut h = blake3::Hasher::new();
    h.update(&PROMPT_VERSION.to_le_bytes());
    h.update(system.as_bytes());
    h.finalize().to_hex().to_string()
}

/// A worker panicking is a bug, not a failed batch: carry the panic out to the
/// caller rather than lose it in a `Result` nobody reads.
fn join<T>(h: std::thread::ScopedJoinHandle<'_, T>) -> T {
    match h.join() {
        Ok(v) => v,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// `Error`'s display puts the hint on a second line, which reads badly in a
/// list of servers that were skipped.
fn one_line(e: &Error) -> String {
    e.to_string().replace('\n', " — ")
}

/// Pull the paragraph array out of the model's JSON, tolerating the shapes it
/// actually produces.
pub fn parse_reply(content: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(content) else {
        // Not JSON at all: treat the whole thing as a single paragraph — the
        // model sometimes answers with the bare corrected text.
        //
        // Unless it is *broken* JSON, which is what a reply truncated by
        // `num_predict` looks like. Ten replies in the 419-page book came back
        // cut off mid-array, and returning them verbatim spliced a literal
        // `{"paragraphs":[` into the prose. The drift guard caught all ten, but
        // only by accident of their length. Refuse them here instead: an empty
        // string is read as "model returned nothing" and keeps the original.
        let t = content.trim();
        if t.starts_with('{') || t.starts_with('[') {
            return vec![String::new()];
        }
        return vec![t.to_string()];
    };
    let arr = match &v {
        serde_json::Value::Array(a) => Some(a.clone()),
        serde_json::Value::Object(o) => o
            .get("paragraphs")
            .or_else(|| o.get("corrected"))
            .or_else(|| o.get("result"))
            .or_else(|| o.values().next())
            .and_then(|x| x.as_array().cloned()),
        _ => None,
    };
    match arr {
        Some(a) => a
            .iter()
            .map(|x| x.as_str().unwrap_or("").trim().to_string())
            .collect(),
        None => vec![v.as_str().unwrap_or(content).trim().to_string()],
    }
}

/// Why a correction should be refused, or `None` if it looks like a typo fix.
pub fn drift_reason(original: &str, corrected: &str) -> Option<String> {
    let a = original.trim();
    let b = corrected.trim();
    if b.is_empty() {
        return Some("empty".to_string());
    }
    // Text glued onto an untouched span is the model completing a fragment,
    // never a repair. Checked before the edit floor below, because the damage
    // is usually one or two characters and would otherwise slip straight
    // through it.
    if b.len() > a.len() && (b.starts_with(a) || b.ends_with(a)) {
        return Some("only adds text at the start or end".to_string());
    }
    // A handful of edits is a typo fix at any length, so let the short spans
    // through before the ratios get a chance to refuse them.
    if strsim::levenshtein(a, b) <= ALWAYS_ALLOWED_EDITS {
        return None;
    }
    let la = a.chars().count() as f32;
    let lb = b.chars().count() as f32;
    if la > 0.0 {
        let drift = (lb - la).abs() / la;
        if drift > MAX_LENGTH_DRIFT {
            return Some(format!("length changed by {:.0}%", drift * 100.0));
        }
    }
    let sim = strsim::normalized_levenshtein(a, b);
    if sim < MIN_SIMILARITY {
        return Some(format!("only {:.0}% similar to the original", sim * 100.0));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Condvar};
    use std::time::Duration;

    const FAKE_MODEL: &str = "fake-proofreader";

    /// What a fake server does with `/api/chat`. `/api/tags` always answers, so
    /// a `Dead` one gets past preflight and only falls over under load — which
    /// is the case worth testing.
    #[derive(PartialEq)]
    enum Chat {
        /// Repair the paragraph, one letter, so the drift guard accepts it.
        Fix,
        /// Accept the connection and hang up without answering.
        Dead,
    }

    /// How many `/api/chat` requests were being served at the same moment.
    /// Shared by every fake in a test, so the count is across servers.
    #[derive(Default)]
    struct Overlap {
        /// (in flight now, high-water mark)
        counts: Mutex<(usize, usize)>,
        wake: Condvar,
    }

    impl Overlap {
        /// Enter a request and wait, briefly, for `want` of them to be in
        /// flight together. A serial dispatcher waits this out and then fails
        /// on the high-water mark below, rather than hanging the test.
        fn enter(&self, want: usize) {
            let mut c = self.counts.lock().unwrap();
            c.0 += 1;
            c.1 = c.1.max(c.0);
            self.wake.notify_all();
            // Once the mark has been reached the point is proved; later
            // requests must not sit here waiting for company.
            while c.0 < want && c.1 < want {
                let (next, t) = self.wake.wait_timeout(c, Duration::from_secs(5)).unwrap();
                c = next;
                if t.timed_out() {
                    break;
                }
            }
        }

        fn leave(&self) {
            self.counts.lock().unwrap().0 -= 1;
        }

        fn most_at_once(&self) -> usize {
            self.counts.lock().unwrap().1
        }
    }

    struct Behaviour {
        chat: Chat,
        delay: Duration,
        overlap: Option<Arc<Overlap>>,
        want: usize,
    }

    /// Just enough of ollama to drive the dispatcher: `/api/tags` for
    /// preflight and `/api/chat` for the work. One thread per connection,
    /// because the whole point here is that several are open at once.
    ///
    /// The listener is left running; the test binary exiting is all the
    /// cleanup a socket needs.
    fn fake(b: Behaviour) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let b = Arc::new(b);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { break };
                let b = Arc::clone(&b);
                std::thread::spawn(move || serve(&mut s, &b));
            }
        });
        url
    }

    fn serve(stream: &mut TcpStream, b: &Behaviour) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request = String::new();
        if reader.read_line(&mut request).unwrap_or(0) == 0 {
            return;
        }
        let mut length = 0usize;
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                break;
            }
            if let Some(v) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                length = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; length];
        if length > 0 && reader.read_exact(&mut body).is_err() {
            return;
        }

        let reply = if request.contains("/api/chat") {
            if b.chat == Chat::Dead {
                return;
            }
            if let Some(o) = &b.overlap {
                o.enter(b.want);
            }
            std::thread::sleep(b.delay);
            let sent: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let user = sent["messages"].as_array().unwrap().last().unwrap()["content"]
                .as_str()
                .unwrap()
                .to_string();
            let asked: serde_json::Value = serde_json::from_str(&user).unwrap();
            let fixed: Vec<String> = asked["paragraphs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p.as_str().unwrap().replace("teh", "the"))
                .collect();
            if let Some(o) = &b.overlap {
                o.leave();
            }
            let content = serde_json::json!({ "paragraphs": fixed }).to_string();
            serde_json::json!({ "message": { "content": content } }).to_string()
        } else {
            serde_json::json!({ "models": [ { "name": FAKE_MODEL } ] }).to_string()
        };
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{reply}",
            reply.len()
        );
    }

    fn damaged(n: usize) -> Vec<String> {
        (0..n)
            .map(|i| format!("Paragraph number {i} has teh word wrong in it."))
            .collect()
    }

    fn reader(urls: &[String]) -> Proofreader {
        // Never touch the real reply cache from a test.
        Proofreader::new(urls, FAKE_MODEL, "eng", false)
    }

    fn proofread(pr: &Proofreader, texts: &[String]) -> (Vec<Correction>, Stats) {
        let mut stats = Stats::default();
        let out = pr.run(texts, &mut stats, |_, _| {}, &|| false).unwrap();
        (out, stats)
    }

    #[test]
    fn two_servers_are_asked_at_the_same_time_and_share_the_batches() {
        let overlap = Arc::new(Overlap::default());
        let urls: Vec<String> = (0..2)
            .map(|_| {
                fake(Behaviour {
                    chat: Chat::Fix,
                    delay: Duration::from_millis(20),
                    overlap: Some(Arc::clone(&overlap)),
                    want: 2,
                })
            })
            .collect();

        // 25 paragraphs is three batches for two servers, so both must be busy.
        let texts = damaged(25);
        let mut pr = reader(&urls);
        assert_eq!(pr.preflight().unwrap(), Vec::<String>::new());
        let (out, stats) = proofread(&pr, &texts);

        assert_eq!(overlap.most_at_once(), 2, "the servers were asked in turn");
        assert!(out.iter().all(|c| c.accepted), "every paragraph was repaired");
        assert!(out[7].corrected.contains("the word wrong"));
        assert_eq!(stats.per_server.len(), 2);
        assert_eq!(
            stats.per_server.iter().map(|(_, n)| n).sum::<usize>(),
            3,
            "three batches, however they were shared out"
        );
        assert_eq!(stats.unproofread, 0);
    }

    #[test]
    fn a_server_that_stops_answering_hands_its_batch_to_the_others() {
        // Slow enough that the dead one is certain to have taken a batch.
        let good = fake(Behaviour {
            chat: Chat::Fix,
            delay: Duration::from_millis(150),
            overlap: None,
            want: 0,
        });
        let dead = fake(Behaviour {
            chat: Chat::Dead,
            delay: Duration::ZERO,
            overlap: None,
            want: 0,
        });

        let texts = damaged(35);
        let mut pr = reader(&[good.clone(), dead.clone()]);
        // Both answer `/api/tags`, so preflight has no reason to drop either.
        assert!(pr.preflight().unwrap().is_empty());
        let (out, stats) = proofread(&pr, &texts);

        assert!(
            out.iter().all(|c| c.accepted),
            "the batch the dead server took must still have been proofread"
        );
        assert_eq!(stats.servers_lost, vec![dead.clone()]);
        assert_eq!(stats.unproofread, 0);
        assert_eq!(stats.per_server, vec![(good, 4), (dead, 0)]);
    }

    #[test]
    fn when_every_server_goes_away_the_text_is_kept_and_counted() {
        let dead = fake(Behaviour {
            chat: Chat::Dead,
            delay: Duration::ZERO,
            overlap: None,
            want: 0,
        });
        let texts = damaged(15);
        let mut pr = reader(std::slice::from_ref(&dead));
        assert!(pr.preflight().unwrap().is_empty());
        let (out, stats) = proofread(&pr, &texts);

        // A run that reached no server must lose nothing but the repairs.
        assert!(out.iter().all(|c| !c.accepted));
        assert_eq!(out[3].corrected, texts[3]);
        assert_eq!(stats.servers_lost, vec![dead]);
        assert_eq!(stats.unproofread, 15);
    }

    #[test]
    fn preflight_drops_the_servers_that_are_not_there_and_keeps_the_rest() {
        let good = fake(Behaviour {
            chat: Chat::Fix,
            delay: Duration::ZERO,
            overlap: None,
            want: 0,
        });
        // Port 1 is reserved and nothing can be listening on it.
        let mut pr = reader(&[good.clone(), "http://127.0.0.1:1".to_string()]);
        let warnings = pr.preflight().unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].starts_with("not using http://127.0.0.1:1"));
        assert_eq!(pr.endpoints(), [good]);
    }


    #[test]
    fn preflight_fails_only_when_no_server_is_left() {
        let mut pr = reader(&[
            "http://127.0.0.1:1".to_string(),
            "http://127.0.0.1:2".to_string(),
        ]);
        assert!(pr.preflight().is_err());
    }


    #[test]
    fn accepts_a_real_typo_fix() {
        // Verified good correction from the Turkish book.
        assert_eq!(
            drift_reason(
                "Sinirim bozuluyor ama bekliyorum. Sessiziilde karsilik vermeyi de kardesim ogretti bana.",
                "Sinirim bozuluyor ama bekliyorum. Sessizlikle karsilik vermeyi de kardesim ogretti bana."
            ),
            None
        );
    }

    #[test]
    fn accepts_stray_punctuation_removal() {
        assert_eq!(
            drift_reason(
                "dugunu hayal edemiyorum. Onu sudan. cikarmaya calisirken ben de yandim.",
                "dugunu hayal edemiyorum. Onu sudan cikarmaya calisirken ben de yandim."
            ),
            None
        );
    }

    #[test]
    fn rejects_a_rewrite() {
        let r = drift_reason(
            "Aslinda hakli.",
            "Aslinda o kisi tamamen hakli ve ben bunu kabul ediyorum.",
        );
        assert!(r.is_some(), "a rewrite must be refused");
    }

    #[test]
    fn rejects_an_empty_reply() {
        assert_eq!(drift_reason("some text", "  "), Some("empty".to_string()));
    }

    #[test]
    fn parses_the_object_shape_ollama_returns() {
        let c = r#"{"paragraphs": ["one", "two", "three"]}"#;
        assert_eq!(parse_reply(c), vec!["one", "two", "three"]);
    }

    #[test]
    fn parses_a_bare_array() {
        assert_eq!(parse_reply(r#"["a","b"]"#), vec!["a", "b"]);
    }

    #[test]
    fn short_batch_reply_is_detectable_by_length() {
        // The measured failure: 9 back for 10 sent.
        let c = r#"{"paragraphs": ["1","2","3","4","5","6","7","8","9"]}"#;
        assert_eq!(parse_reply(c).len(), 9);
    }

    #[test]
    fn non_json_reply_degrades_to_one_paragraph() {
        assert_eq!(parse_reply("just some text"), vec!["just some text"]);
    }

    #[test]
    fn a_truncated_json_reply_does_not_leak_its_wrapper_into_the_prose() {
        // Measured: 10 replies in the 419-page book were cut off by
        // `num_predict` and the raw text was spliced into the document.
        let cut = r#"{"paragraphs":[":IIIIIHIIIIIIIIIIIIIIIIIIIIIIII 11111111111"#;
        assert_eq!(parse_reply(cut), vec![""], "broken JSON must be refused");
    }

    #[test]
    fn a_short_span_keeps_its_two_character_quote_fix() {
        // 2 edits in 14 chars is 86% similar, which the ratio alone refuses.
        assert_eq!(drift_reason("\"Emin misin?''", "\"Emin misin?\""), None);
    }

    #[test]
    fn a_short_span_keeps_a_fix_that_shifts_its_length_by_a_fifth() {
        // "m al ıyım." -> "malıyım." is 20% shorter and entirely correct.
        assert_eq!(drift_reason("m al ıyım.", "malıyım."), None);
    }

    #[test]
    fn the_absolute_edit_floor_does_not_licence_a_rewrite() {
        // Still refused: a short original replaced by a much longer sentence is
        // far more than ALWAYS_ALLOWED_EDITS away.
        assert!(drift_reason(
            "Aslinda hakli.",
            "Aslinda o kisi tamamen hakli ve ben bunu kabul ediyorum."
        )
        .is_some());
    }





    #[test]
    fn a_quote_appended_to_an_already_closed_span_is_refused() {
        // Measured: the model turned `mi?"` into `mi?"?` on a span that was
        // already correct, once the prompt told it to close quotations.
        assert!(drift_reason("\"Sarhoş olduğu için mi?\"", "\"Sarhoş olduğu için mi?\"?").is_some());
        assert!(drift_reason("Sustuğu için memnunum. \"Kayıp kü", "Sustuğu için memnunum. \"Kayıp kü\"").is_some());
    }

    #[test]
    fn a_word_cut_off_by_a_page_break_is_not_completed() {
        // `yere sür` continues on the next printed page; finishing it here
        // invents text and leaves the other half orphaned.
        assert!(drift_reason("sandalyesi gürültüyle yere sür", "sandalyesi gürültüyle yere sürüld.").is_some());
    }

    #[test]
    fn an_opening_quote_is_not_prepended_to_a_continuation() {
        assert!(drift_reason("Üzerinde UMUT yazan poster", "\"Üzerinde UMUT yazan poster").is_some());
    }

    #[test]
    fn replacing_junk_at_the_end_is_still_allowed() {
        // The distinction that matters: rule 1 replaces the junk, it does not
        // add to it. `girdim:'` -> `girdim."` must survive.
        assert_eq!(drift_reason("Ben de içeri girdim:'", "Ben de içeri girdim.\""), None);
    }

}

