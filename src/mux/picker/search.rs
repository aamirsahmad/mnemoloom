//! Conversation search for the picker: the `mnemoloom search` index,
//! queried for the live agents' sessions only, off the UI thread.
//!
//! When the picker opens, one thread brings the index up to date (without
//! waiting for another process's build) and another opens it and answers
//! queries. Agents without a session id, or whose session is not indexed,
//! match on metadata only.

use std::collections::{HashMap, HashSet};
use std::iter;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::SystemTime;

use mnemoloom::index::query::{self, Catalog};
use mnemoloom::index::snippet::Snippet;
use mnemoloom::index::store::{self, LockWait};
use mnemoloom::index::{build, build::UpdateStats};
use mnemoloom::session::Homes;
use tantivy::DocAddress;

use super::matcher::Conversation;
use super::{guarded, spawn_worker};

/// A live session's best conversation match.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    /// BM25 with the phrase boost and recency weight, as `search` ranks.
    pub score: f64,
    pub snippet: Snippet,
}

/// Hits by lowercase session id.
pub type Hits = HashMap<String, Hit>;

/// One conversation search; `seq` ties the answer to the query state that
/// asked for it.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub seq: u64,
    pub conversation: Conversation,
    pub sessions: Vec<String>,
}

/// What the search thread reports.
#[derive(Debug)]
pub enum Reply {
    /// The index was (re)opened; `false` when there is none yet.
    Ready(bool),
    Hits(u64, Hits),
    Failed(String),
}

/// The best-scoring document of each session in `sessions` (any case) that
/// matches `conversation`, with its snippet.
pub fn hits(
    cat: &Catalog,
    conversation: &Conversation,
    sessions: &[String],
    now: SystemTime,
) -> Hits {
    let wanted: HashSet<String> = sessions.iter().map(|s| s.to_lowercase()).collect();
    let session_of: HashMap<DocAddress, String> = cat
        .rows
        .iter()
        .filter_map(|r| {
            let id = r.session_id.to_lowercase();
            wanted.contains(&id).then_some((r.doc, id))
        })
        .collect();
    if session_of.is_empty() {
        return Hits::new();
    }
    let docs: Vec<DocAddress> = session_of.keys().copied().collect();
    let q = query::run_query_docs(cat, &conversation.query, &conversation.exclude, &docs, now);
    let searcher = cat.searcher();
    let mut out = Hits::new();
    // Best first, so the first document seen per session wins.
    for (doc, &score) in q.ranked.iter().zip(&q.scores) {
        let id = &session_of[doc];
        if !out.contains_key(id) {
            let snippet = q.snippet(cat, &searcher, *doc);
            out.insert(id.clone(), Hit { score, snippet });
        }
    }
    out
}

/// The default index and homes, as `mnemoloom search` uses them.
pub fn default_index() -> Option<(PathBuf, Homes)> {
    let homes = Homes::resolve_all(None, None, None, None).ok()?;
    Some((store::default_index_dir(), homes.canonical()))
}

/// Incremental index update on a worker thread. An existing index being
/// updated by another process is used as is; with no index yet, wait for
/// that build instead (this thread is background either way).
pub fn spawn_update(dir: PathBuf, homes: Homes) -> Receiver<Result<UpdateStats, String>> {
    let (tx, rx) = mpsc::channel();
    spawn_worker("index", move || {
        let wait = if store::index_exists(&dir) {
            LockWait::Try
        } else {
            LockWait::Block
        };
        let result = guarded(|| build::update(&dir, &homes, None, wait))
            .and_then(|r| r.map_err(|e| format!("{e:#}")));
        let _ = tx.send(result);
    });
    rx
}

enum Job {
    Query(Request),
    Reload,
}

/// Owns the open index and answers [`Request`]s, newest first: queries
/// queued behind a running one are dropped for the last of them.
pub struct Searcher {
    jobs: Sender<Job>,
    pub replies: Receiver<Reply>,
}

impl Searcher {
    pub fn spawn(dir: PathBuf, homes: Homes) -> Searcher {
        let (jobs, inbox) = mpsc::channel::<Job>();
        let (out, replies) = mpsc::channel();
        spawn_worker("search", move || {
            let mut cat = open(&dir, &homes, &out);
            while let Ok(first) = inbox.recv() {
                let mut reload = false;
                let mut request = None;
                for job in iter::once(first).chain(inbox.try_iter()) {
                    match job {
                        Job::Reload => reload = true,
                        Job::Query(r) => request = Some(r),
                    }
                }
                if reload {
                    cat = match cat.take() {
                        Some(mut c) => match guarded(|| c.refresh()) {
                            Ok(Ok(())) => {
                                let _ = out.send(Reply::Ready(true));
                                Some(c)
                            }
                            Ok(Err(e)) => fail(&out, format!("{e:#}")),
                            Err(e) => fail(&out, e),
                        },
                        None => open(&dir, &homes, &out),
                    };
                }
                if let (Some(r), Some(c)) = (request, &cat) {
                    let reply = match guarded(|| {
                        hits(c, &r.conversation, &r.sessions, SystemTime::now())
                    }) {
                        Ok(h) => Reply::Hits(r.seq, h),
                        Err(e) => Reply::Failed(format!("search failed: {e}")),
                    };
                    if out.send(reply).is_err() {
                        return;
                    }
                }
            }
        });
        Searcher { jobs, replies }
    }

    pub fn query(&self, request: Request) {
        let _ = self.jobs.send(Job::Query(request));
    }

    /// Re-read the index (after an update).
    pub fn reload(&self) {
        let _ = self.jobs.send(Job::Reload);
    }
}

/// Open the index, reporting whether there is one.
fn open(dir: &std::path::Path, homes: &Homes, out: &Sender<Reply>) -> Option<Catalog> {
    match guarded(|| Catalog::open(dir, homes)) {
        Ok(Ok(cat)) => {
            let _ = out.send(Reply::Ready(cat.is_some()));
            cat
        }
        Ok(Err(e)) => fail(out, format!("{e:#}")),
        Err(e) => fail(out, e),
    }
}

/// Report an unusable index; the picker falls back to metadata only.
fn fail(out: &Sender<Reply>, error: String) -> Option<Catalog> {
    let _ = out.send(Reply::Ready(false));
    let _ = out.send(Reply::Failed(format!("index: {error}")));
    None
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::time::{Duration, Instant};

    use super::*;

    const A: &str = "c1a0de00-aaaa-4abc-8def-012345678901";
    const B: &str = "c1a0de00-bbbb-4abc-8def-012345678902";
    const C: &str = "c1a0de00-cccc-4abc-8def-012345678903";

    /// A one-exchange Claude session in `/w` saying `text`.
    fn write_session(claude: &Path, id: &str, text: &str) {
        let env = format!(
            r#""isSidechain":false,"userType":"external","cwd":"/w","sessionId":"{id}","version":"2.1.311","gitBranch":"main""#
        );
        let lines = [
            format!(
                r#"{{"parentUuid":null,{env},"type":"user","message":{{"role":"user","content":"{text}"}},"uuid":"u1","timestamp":"2026-01-03T03:28:26.000Z"}}"#
            ),
            format!(
                r#"{{"parentUuid":"u1",{env},"message":{{"model":"claude-opus-5-5","id":"msg_1","type":"message","role":"assistant","content":[{{"type":"text","text":"done"}}]}},"type":"assistant","uuid":"a1","timestamp":"2026-01-03T03:28:30.000Z"}}"#
            ),
        ];
        let dir = claude.join("projects").join("-w");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{id}.jsonl")), lines.join("\n") + "\n").unwrap();
    }

    struct Env {
        _root: tempfile::TempDir,
        homes: Homes,
        idx: PathBuf,
    }

    fn env() -> Env {
        let root = tempfile::tempdir().unwrap();
        let homes = Homes::new(root.path().join("claude"), root.path().join("codex")).canonical();
        write_session(&homes.claude, A, "deploy the rocket to staging");
        write_session(&homes.claude, B, "polish the rocket fins");
        write_session(&homes.claude, C, "rocket fuel for staging");
        let idx = root.path().join("idx");
        Env {
            _root: root,
            homes,
            idx,
        }
    }

    fn conv(query: &str, exclude: &str) -> Conversation {
        Conversation {
            query: query.into(),
            exclude: exclude.into(),
        }
    }

    fn keys(h: &Hits) -> Vec<&str> {
        let mut k: Vec<&str> = h.keys().map(String::as_str).collect();
        k.sort();
        k
    }

    #[test]
    fn hits_only_for_the_given_sessions() {
        let e = env();
        build::update(&e.idx, &e.homes, None, LockWait::Block).unwrap();
        let cat = Catalog::open(&e.idx, &e.homes).unwrap().unwrap();
        // Any case; C is indexed and matches but is not live.
        let live = [A.to_uppercase(), B.to_owned(), "not-indexed".to_owned()];
        let now = SystemTime::now();
        let h = hits(&cat, &conv("rocket", ""), &live, now);
        assert_eq!(keys(&h), vec![A, B]);
        let s = &h[A].snippet;
        assert!(
            s.text.contains("deploy the rocket to staging"),
            "{}",
            s.text
        );
        assert_eq!(&s.text[s.highlights[0].clone()], "rocket");
        assert!(h[A].score > 0.0);
        // Every term must match.
        assert_eq!(
            keys(&hits(&cat, &conv("rocket staging", ""), &live, now)),
            vec![A]
        );
        assert_eq!(
            keys(&hits(&cat, &conv("\"rocket fins\"", ""), &live, now)),
            vec![B]
        );
        assert!(hits(&cat, &conv("\"staging rocket\"", ""), &live, now).is_empty());
        // Exclusions drop sessions that mention them.
        assert_eq!(
            keys(&hits(&cat, &conv("rocket", "staging"), &live, now)),
            vec![B]
        );
        assert!(hits(&cat, &conv("rocket", ""), &[], now).is_empty());
    }

    /// The next reply, or a panic after a generous wait.
    fn reply(s: &Searcher) -> Reply {
        s.replies.recv_timeout(Duration::from_secs(20)).unwrap()
    }

    #[test]
    fn worker_without_an_index_builds_one_in_the_background() {
        let e = env();
        let s = Searcher::spawn(e.idx.clone(), e.homes.clone());
        assert!(matches!(reply(&s), Reply::Ready(false)));
        // No index: a query gets no answer (the picker never sends one).
        let request = Request {
            seq: 1,
            conversation: conv("rocket", ""),
            sessions: vec![A.into()],
        };
        s.query(request.clone());
        let stats = spawn_update(e.idx.clone(), e.homes.clone())
            .recv_timeout(Duration::from_secs(20))
            .unwrap()
            .unwrap();
        assert_eq!(stats.indexed, 3);
        s.reload();
        assert!(matches!(reply(&s), Reply::Ready(true)));
        s.query(Request { seq: 2, ..request });
        match reply(&s) {
            Reply::Hits(2, h) => assert_eq!(keys(&h), vec![A]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn update_uses_a_busy_index_as_is() {
        let e = env();
        build::update(&e.idx, &e.homes, None, LockWait::Block).unwrap();
        let _held = store::lock(&e.idx, LockWait::Block).unwrap().unwrap();
        let started = Instant::now();
        let stats = spawn_update(e.idx.clone(), e.homes.clone())
            .recv_timeout(Duration::from_secs(20))
            .unwrap()
            .unwrap();
        assert!(stats.busy && !stats.changed());
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
