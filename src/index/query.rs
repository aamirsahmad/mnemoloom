//! Reading the index: session rows from fast fields, the filter pipeline
//! (`--after`, `--agent`, …), full-text ranking and lazy snippets.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use tantivy::collector::TopDocs;
use tantivy::columnar::StrColumn;
use tantivy::query::{
    BooleanQuery, BoostQuery, EnableScoring, Occur, PhraseQuery, Query, TermQuery,
};
use tantivy::schema::{IndexRecordOption, Term, Value};
use tantivy::{DocAddress, Index, IndexReader, ReloadPolicy, Searcher, TantivyDocument};

use super::dates::DateBound;
use super::schema::Fields;
use super::snippet::{Snippet, keywords, make_snippet};
use crate::session::discover::codex_thread_names;
use crate::session::homes::normalize_path;
use crate::session::{Agent, Homes};

/// Full-text hits considered before filtering (later filters may leave
/// fewer results than exist).
pub const TOP_HITS: usize = 2000;
/// Recency half-weight scale: 7 days.
const RECENCY_SECS: f64 = 7.0 * 86_400.0;
/// Phrase boost for multi-word queries.
const PHRASE_BOOST: f32 = 5.0;

/// One indexed session (all stored metadata except `content`).
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub doc: DocAddress,
    pub session_id: String,
    pub agent: Agent,
    pub project: String,
    pub branch: String,
    pub cwd: String,
    pub created: String,
    pub modified: String,
    pub modified_ts: u64,
    pub lines: i64,
    pub file_path: String,
    pub first_msg_role: String,
    pub first_msg_content: String,
    pub last_msg_role: String,
    pub last_msg_content: String,
    pub first_user_msg_content: String,
    pub derivation_type: String,
    pub is_sidechain: bool,
    pub is_exec_run: bool,
    pub home: String,
    pub custom_title: String,
}

impl Row {
    /// `first_user_msg_content`, else `first_msg_content`.
    pub fn first_msg(&self) -> &str {
        if self.first_user_msg_content.is_empty() {
            &self.first_msg_content
        } else {
            &self.first_user_msg_content
        }
    }

    /// Project name, else cwd basename, else `unknown`.
    pub fn project_name(&self) -> &str {
        if !self.project.is_empty() {
            return &self.project;
        }
        Path::new(&self.cwd)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
    }
}

/// An open index plus every session row.
pub struct Catalog {
    pub index: Index,
    pub reader: IndexReader,
    pub fields: Fields,
    pub rows: Vec<Row>,
    /// Codex thread names (`session_index.jsonl`), overlaid on rows whose
    /// transcript carries no title.
    codex_names: HashMap<String, String>,
}

/// A dictionary-encoded fast column materialized for random access.
struct StrCol {
    terms: Vec<String>,
    col: StrColumn,
}

impl StrCol {
    fn load(seg: &tantivy::SegmentReader, name: &str) -> anyhow::Result<Option<StrCol>> {
        let Some(col) = seg.fast_fields().str(name)? else {
            return Ok(None);
        };
        let mut terms = Vec::with_capacity(col.num_terms());
        let mut stream = col.dictionary().stream()?;
        while stream.advance() {
            terms.push(String::from_utf8_lossy(stream.key()).into_owned());
        }
        Ok(Some(StrCol { terms, col }))
    }

    fn get(&self, doc: u32) -> String {
        self.col
            .term_ords(doc)
            .next()
            .and_then(|o| self.terms.get(o as usize))
            .cloned()
            .unwrap_or_default()
    }
}

impl Catalog {
    /// Open `dir` and load all rows (`None` when there is no index).
    pub fn open(dir: &Path, homes: &Homes) -> anyhow::Result<Option<Catalog>> {
        let Some((index, fields)) = super::store::open_existing(dir)? else {
            return Ok(None);
        };
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .context("Failed to open index. Run 'mnemoloom build-index' first.")?;
        let mut cat = Catalog {
            index,
            reader,
            fields,
            rows: Vec::new(),
            codex_names: codex_thread_names(&homes.codex),
        };
        cat.load_rows()?;
        Ok(Some(cat))
    }

    /// Reload after a commit and re-read the rows.
    pub fn refresh(&mut self) -> anyhow::Result<()> {
        self.reader.reload()?;
        self.load_rows()
    }

    fn load_rows(&mut self) -> anyhow::Result<()> {
        let searcher = self.reader.searcher();
        let mut rows = Vec::new();
        for (ord, seg) in searcher.segment_readers().iter().enumerate() {
            rows.extend(segment_rows(ord as u32, seg)?);
        }
        for r in rows
            .iter_mut()
            .filter(|r| r.agent == Agent::Codex && r.custom_title.is_empty())
        {
            if let Some(n) = self.codex_names.get(&r.session_id.to_lowercase()) {
                r.custom_title.clone_from(n);
            }
        }
        rows.sort_by_key(|r| std::cmp::Reverse(r.modified_ts));
        self.rows = rows;
        Ok(())
    }

    pub fn searcher(&self) -> Searcher {
        self.reader.searcher()
    }
}

fn segment_rows(ord: u32, seg: &tantivy::SegmentReader) -> anyhow::Result<Vec<Row>> {
    let col = |n: &str| StrCol::load(seg, n);
    let names = [
        "session_id",
        "agent",
        "project",
        "branch",
        "cwd",
        "created",
        "modified",
        "export_path",
        "first_msg_role",
        "first_msg_content",
        "last_msg_role",
        "last_msg_content",
        "first_user_msg_content",
        "derivation_type",
        "is_sidechain",
        "is_exec_run",
        "claude_home",
        "custom_title",
    ];
    let cols: Vec<Option<StrCol>> = names.iter().map(|n| col(n)).collect::<Result<_, _>>()?;
    let ts = seg.fast_fields().u64("modified_ts")?;
    let lines = seg.fast_fields().i64("lines")?;
    let mut out = Vec::new();
    for doc in seg.doc_ids_alive() {
        let s: Vec<String> = cols
            .iter()
            .map(|c| c.as_ref().map(|c| c.get(doc)).unwrap_or_default())
            .collect();
        let mut it = s.into_iter();
        let mut next = || it.next().unwrap_or_default();
        out.push(Row {
            doc: DocAddress::new(ord, doc),
            session_id: next(),
            agent: Agent::parse(&next()).unwrap_or(Agent::Claude),
            project: next(),
            branch: next(),
            cwd: next(),
            created: next(),
            modified: next(),
            file_path: next(),
            first_msg_role: next(),
            first_msg_content: next(),
            last_msg_role: next(),
            last_msg_content: next(),
            first_user_msg_content: next(),
            derivation_type: next(),
            is_sidechain: next() == "true",
            is_exec_run: next() == "true",
            home: next(),
            custom_title: next(),
            modified_ts: ts.first(doc).unwrap_or(0),
            lines: lines.first(doc).unwrap_or(0),
        });
    }
    Ok(out)
}

/// The agents a search shows: a non-empty set (all by default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Agents(u8);

impl Agents {
    pub const ALL: Agents = Agents((1 << Agent::ALL.len()) - 1);

    fn bit(a: Agent) -> u8 {
        1 << a as u8
    }

    /// Just `a`.
    pub fn only(a: Agent) -> Agents {
        Agents(Self::bit(a))
    }

    pub fn contains(self, a: Agent) -> bool {
        self.0 & Self::bit(a) != 0
    }

    pub fn is_all(self) -> bool {
        self == Agents::ALL
    }

    /// The single enabled agent, if exactly one is.
    pub fn single(self) -> Option<Agent> {
        let mut on = Agent::ALL.into_iter().filter(|a| self.contains(*a));
        match (on.next(), on.next()) {
            (Some(a), None) => Some(a),
            _ => None,
        }
    }

    /// Enable / disable `a`; disabling the last enabled agent is a no-op.
    pub fn toggle(&mut self, a: Agent) {
        let next = self.0 ^ Self::bit(a);
        if next != 0 {
            self.0 = next;
        }
    }
}

impl Default for Agents {
    fn default() -> Agents {
        Agents::ALL
    }
}

/// Every filter of the search CLI / TUI.
#[derive(Debug, Clone, PartialEq)]
pub struct Filters {
    pub global: bool,
    /// Directory prefix scope (overrides `global`).
    pub dir: Option<String>,
    /// Process cwd: the default (exact) scope.
    pub launch_cwd: String,
    /// Exact branch; ignored when global.
    pub branch: Option<String>,
    pub include_original: bool,
    pub include_sub: bool,
    pub include_exec: bool,
    pub include_trimmed: bool,
    pub include_continued: bool,
    pub agents: Agents,
    pub min_lines: Option<i64>,
    pub after: Option<DateBound>,
    pub before: Option<DateBound>,
    pub live_only: bool,
    pub claude_home: String,
    pub codex_home: String,
    /// Empty when the agent's home is not configured.
    pub pi_home: String,
    pub opencode_home: String,
}

impl Filters {
    /// Defaults: current-dir scope, originals/trimmed/rollovers shown,
    /// sub-agents and exec runs hidden.
    pub fn new(homes: &Homes, launch_cwd: String) -> Filters {
        Filters {
            global: false,
            dir: None,
            launch_cwd,
            branch: None,
            include_original: true,
            include_sub: false,
            include_exec: false,
            include_trimmed: true,
            include_continued: true,
            agents: Agents::ALL,
            min_lines: None,
            after: None,
            before: None,
            live_only: false,
            claude_home: home_key(homes, Agent::Claude),
            codex_home: home_key(homes, Agent::Codex),
            pi_home: home_key(homes, Agent::Pi),
            opencode_home: home_key(homes, Agent::OpenCode),
        }
    }

    /// Whether any non-scope filter differs from the defaults.
    pub fn has_active(&self) -> bool {
        self.min_lines.is_some()
            || self.after.is_some()
            || self.before.is_some()
            || !self.agents.is_all()
            || self.branch.is_some()
            || !self.include_original
            || self.include_sub
            || self.include_exec
            || !self.include_trimmed
            || !self.include_continued
            || self.live_only
    }

    /// Reset type/agent/min-lines filters; dates too when `dates`.
    pub fn reset(&mut self, dates: bool) {
        self.include_original = true;
        self.include_sub = false;
        self.include_exec = false;
        self.include_trimmed = true;
        self.include_continued = true;
        self.agents = Agents::ALL;
        self.min_lines = None;
        self.live_only = false;
        if dates {
            self.after = None;
            self.before = None;
        }
    }

    /// The (canonical) home rows of `agent` must come from.
    pub fn home(&self, agent: Agent) -> &str {
        match agent {
            Agent::Claude => &self.claude_home,
            Agent::Codex => &self.codex_home,
            Agent::Pi => &self.pi_home,
            Agent::OpenCode => &self.opencode_home,
        }
    }

    /// The per-session predicate (everything except the query and live).
    pub fn keep(&self, r: &Row) -> bool {
        let home_filter = self.home(r.agent);
        // An unconfigured pi / opencode home shows none of its rows.
        if home_filter.is_empty() || (!r.home.is_empty() && r.home != home_filter) {
            return false;
        }
        let in_scope = if let Some(dir) = &self.dir {
            let dir = dir.trim_end_matches('/');
            r.cwd.is_empty() || r.cwd == dir || r.cwd.starts_with(&format!("{dir}/"))
        } else {
            self.global || r.cwd.is_empty() || r.cwd == self.launch_cwd
        };
        if !in_scope {
            return false;
        }
        let type_ok = if r.is_sidechain {
            self.include_sub
        } else if r.is_exec_run {
            self.include_exec
        } else {
            match r.derivation_type.as_str() {
                "" => self.include_original,
                "trimmed" => self.include_trimmed,
                "continued" => self.include_continued,
                _ => true,
            }
        };
        type_ok
            && self.agents.contains(r.agent)
            && (self.global && self.dir.is_none()
                || self.branch.as_ref().is_none_or(|b| *b == r.branch))
            && self.min_lines.is_none_or(|n| r.lines >= n)
            && self.after.is_none_or(|d| d.allows_after(&r.modified))
            && self.before.is_none_or(|d| d.allows_before(&r.modified))
    }
}

/// Canonical home of `agent` as stored in the index (empty when unset).
fn home_key(homes: &Homes, agent: Agent) -> String {
    if homes.has(agent) {
        normalize_path(homes.home(agent))
            .to_string_lossy()
            .into_owned()
    } else {
        String::new()
    }
}

/// A ranked full-text query.
pub struct TextQuery {
    pub raw: String,
    /// Parsed query without the phrase boost (snippets).
    base: Box<dyn Query>,
    /// Ranked documents (best first).
    pub ranked: Vec<DocAddress>,
    /// Recency-weighted score of each `ranked` document.
    pub scores: Vec<f64>,
}

/// Split input into units: `"quoted"` segments and whitespace words.
/// There is no query syntax: `:`, `(`, `+`, `-`, `*` ... are plain text.
pub fn query_units(raw: &str) -> Vec<&str> {
    raw.split('"')
        .enumerate()
        .flat_map(|(i, seg)| {
            if i % 2 == 1 {
                vec![seg]
            } else {
                seg.split_whitespace().collect()
            }
        })
        .filter(|u| !u.trim().is_empty())
        .collect()
}

fn tokens(index: &Index, field: tantivy::schema::Field, text: &str) -> Vec<Term> {
    let Ok(mut analyzer) = index.tokenizer_for_field(field) else {
        return Vec::new();
    };
    let mut ts = analyzer.token_stream(text);
    let mut out = Vec::new();
    while ts.advance() {
        out.push(Term::from_field_text(field, &ts.token().text));
    }
    out
}

fn terms_query(mut terms: Vec<Term>) -> Box<dyn Query> {
    if terms.len() == 1 {
        let t = terms.remove(0);
        Box::new(TermQuery::new(t, IndexRecordOption::WithFreqs))
    } else {
        Box::new(PhraseQuery::new(terms))
    }
}

/// Text query over `content`: each unit matches as a term, or as a phrase
/// when it tokenizes to several terms (`"a b"`, `std::fs`,
/// `localhost:8080`); units are OR-ed (`Occur::Should`) or AND-ed
/// (`Occur::Must`). Returns (base, boosted): the boosted form adds the
/// whole input's tokens as a 5x phrase.
fn content_query(
    index: &Index,
    field: tantivy::schema::Field,
    raw: &str,
    unit: Occur,
) -> (Box<dyn Query>, Box<dyn Query>) {
    let mut all = Vec::new();
    let mut units: Vec<(Occur, Box<dyn Query>)> = Vec::new();
    for u in query_units(raw) {
        let t = tokens(index, field, u);
        if t.is_empty() {
            continue;
        }
        all.extend(t.iter().cloned());
        units.push((unit, terms_query(t)));
    }
    let base: Box<dyn Query> = Box::new(BooleanQuery::new(units));
    if all.len() < 2 {
        return (base.box_clone(), base);
    }
    let phrase = BoostQuery::new(Box::new(PhraseQuery::new(all)), PHRASE_BOOST);
    let boosted = BooleanQuery::new(vec![
        (Occur::Should, Box::new(phrase) as Box<dyn Query>),
        (Occur::Should, base.box_clone()),
    ]);
    (base, Box::new(boosted))
}

/// Run `raw` against the index: plain-text units (see [`content_query`]),
/// phrase boost, home and exec-run filters, top [`TOP_HITS`],
/// recency-weighted re-rank.
pub fn run_query(cat: &Catalog, raw: &str, f: &Filters, now: SystemTime) -> TextQuery {
    let fields = cat.fields;
    let (base, content) = content_query(&cat.index, fields.content, raw, Occur::Should);
    let term = |field, v: &str| -> Box<dyn Query> {
        Box::new(TermQuery::new(
            Term::from_field_text(field, v),
            IndexRecordOption::Basic,
        ))
    };
    let homes = BooleanQuery::new(
        Agent::ALL
            .into_iter()
            .map(|a| f.home(a))
            .filter(|h| !h.is_empty())
            .map(|h| (Occur::Should, term(fields.claude_home, h)))
            .collect(),
    );
    let mut clauses = vec![
        (Occur::Must, content),
        (Occur::Must, Box::new(homes) as Box<dyn Query>),
    ];
    if !f.include_exec {
        clauses.push((Occur::Must, term(fields.is_exec_run, "false")));
    }
    let query = BooleanQuery::new(clauses);
    let searcher = cat.searcher();
    let hits = searcher
        .search(&query, &TopDocs::with_limit(TOP_HITS).order_by_score())
        .unwrap_or_default();
    ranked(cat, raw, base, hits, now)
}

/// [`run_query`] over a chosen set of documents (no filters, no
/// [`TOP_HITS`] cap): every unit of `raw` must match, and no document
/// containing a unit of `exclude` matches. Same scoring and phrase boost.
pub fn run_query_docs(
    cat: &Catalog,
    raw: &str,
    exclude: &str,
    docs: &[DocAddress],
    now: SystemTime,
) -> TextQuery {
    let field = cat.fields.content;
    let (base, content) = content_query(&cat.index, field, raw, Occur::Must);
    let mut clauses = vec![(Occur::Must, content)];
    if !query_units(exclude).is_empty() {
        let (any, _) = content_query(&cat.index, field, exclude, Occur::Should);
        clauses.push((Occur::MustNot, any));
    }
    let searcher = cat.searcher();
    let hits = score_docs(&searcher, &BooleanQuery::new(clauses), docs).unwrap_or_default();
    ranked(cat, raw, base, hits, now)
}

/// Score of each of `docs` that `query` matches.
fn score_docs(
    searcher: &Searcher,
    query: &dyn Query,
    docs: &[DocAddress],
) -> tantivy::Result<Vec<(f32, DocAddress)>> {
    let weight = query.weight(EnableScoring::enabled_from_searcher(searcher))?;
    let mut docs = docs.to_vec();
    docs.sort();
    docs.dedup();
    let mut hits = Vec::new();
    for seg in docs.chunk_by(|a, b| a.segment_ord == b.segment_ord) {
        let ord = seg[0].segment_ord;
        let mut scorer = weight.scorer(searcher.segment_reader(ord), 1.0)?;
        for d in seg {
            // Seeking only moves forward; `docs` are sorted.
            if scorer.doc() < d.doc_id {
                scorer.seek(d.doc_id);
            }
            if scorer.doc() == d.doc_id {
                hits.push((scorer.score(), *d));
            }
        }
    }
    Ok(hits)
}

/// Recency-weighted re-rank of raw hits.
fn ranked(
    cat: &Catalog,
    raw: &str,
    base: Box<dyn Query>,
    hits: Vec<(f32, DocAddress)>,
    now: SystemTime,
) -> TextQuery {
    let ts_by_doc: HashMap<DocAddress, u64> =
        cat.rows.iter().map(|r| (r.doc, r.modified_ts)).collect();
    let now_ms = now
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let mut scored: Vec<(f64, DocAddress)> = hits
        .into_iter()
        .map(|(score, doc)| {
            let ts = ts_by_doc.get(&doc).copied().unwrap_or(0);
            let age = now_ms.saturating_sub(ts) as f64 / 1000.0;
            (f64::from(score) * recency(age), doc)
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    TextQuery {
        raw: raw.to_string(),
        base,
        scores: scored.iter().map(|s| s.0).collect(),
        ranked: scored.into_iter().map(|(_, d)| d).collect(),
    }
}

/// `1 + exp(-age / 7d)`.
pub fn recency(age_secs: f64) -> f64 {
    1.0 + (-age_secs.max(0.0) / RECENCY_SECS).exp()
}

impl TextQuery {
    /// Snippet for one hit (reads `content` from the docstore).
    pub fn snippet(&self, cat: &Catalog, searcher: &Searcher, doc: DocAddress) -> Snippet {
        let content = searcher
            .doc::<TantivyDocument>(doc)
            .ok()
            .and_then(|d| {
                d.get_first(cat.fields.content)
                    .and_then(|v| v.as_str().map(str::to_string))
            })
            .unwrap_or_default();
        make_snippet(
            searcher,
            self.base.as_ref(),
            cat.fields.content,
            &content,
            &keywords(&self.raw),
        )
    }
}

/// Filter, order and cap. Returns indices into `cat.rows`.
///
/// With a query: rows not among the hits are dropped; order is hit rank
/// unless `by_time`. `limit` applies before the live filter.
pub fn select(
    cat: &Catalog,
    f: &Filters,
    query: Option<&TextQuery>,
    by_time: bool,
    limit: Option<usize>,
    live: Option<&HashSet<String>>,
) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..cat.rows.len())
        .filter(|&i| f.keep(&cat.rows[i]))
        .collect();
    if let Some(q) = query {
        let rank: HashMap<DocAddress, usize> =
            q.ranked.iter().enumerate().map(|(i, d)| (*d, i)).collect();
        idx.retain(|&i| rank.contains_key(&cat.rows[i].doc));
        if !by_time {
            idx.sort_by_key(|&i| rank[&cat.rows[i].doc]);
        }
    }
    if let Some(n) = limit {
        idx.truncate(n);
    }
    if f.live_only {
        idx.retain(|&i| live.is_some_and(|l| l.contains(&cat.rows[i].file_path)));
    }
    idx
}
