//! Type-to-filter syntax and metadata matching.
//!
//! Whitespace-separated terms, all of which must match (AND):
//!
//! - `word`: exact substring.
//! - `"a phrase"`: exact substring, spaces included (an unclosed quote runs
//!   to the end).
//! - `!word`, `!"a phrase"`: exclude agents that contain it.
//! - `~word`: fzf-style fuzzy match: its letters in order within one field.
//!
//! Smart case per term: a term with an uppercase letter is case-sensitive.
//! Terms match the full, untruncated fields (see [`Haystack::of`]), never
//! the rendered row. A lone `!` or `~` is ignored while it is being typed.

use crate::mux::model::Agent;

/// How a term matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum How {
    Exact,
    Fuzzy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Term {
    /// Lowercased unless `case_sensitive`.
    needle: String,
    /// As typed (for the conversation query).
    text: String,
    how: How,
    exclude: bool,
    phrase: bool,
    case_sensitive: bool,
}

impl Term {
    fn hit(&self, h: &Haystack) -> bool {
        let hay = if self.case_sensitive {
            &h.raw
        } else {
            &h.lower
        };
        match self.how {
            How::Exact => hay.contains(&self.needle),
            How::Fuzzy => hay.split('\n').any(|f| is_subsequence(&self.needle, f)),
        }
    }

    /// The term as a unit of the index query: phrases stay quoted.
    fn unit(&self) -> String {
        if self.phrase {
            format!("\"{}\"", self.text)
        } else {
            self.text.clone()
        }
    }
}

/// A parsed filter query.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    terms: Vec<Term>,
}

/// How an agent's metadata fares against a [`Filter`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Meta {
    /// No exclusion hit and every `~fuzzy` term matched: the agent may be
    /// shown.
    pub allowed: bool,
    /// `allowed`, and every plain term and phrase matched too.
    pub matched: bool,
}

/// The conversation side of a filter, in `mnemoloom search` syntax.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conversation {
    /// Plain terms and quoted phrases; every one must match.
    pub query: String,
    /// Excluded terms and phrases.
    pub exclude: String,
}

impl Filter {
    pub fn parse(query: &str) -> Filter {
        let mut terms = Vec::new();
        let mut rest = query.trim_start();
        while !rest.is_empty() {
            let (how, exclude) = match rest.chars().next() {
                Some('!') => (How::Exact, true),
                Some('~') => (How::Fuzzy, false),
                _ => (How::Exact, false),
            };
            if how == How::Fuzzy || exclude {
                rest = &rest[1..];
            }
            let (text, phrase, after) = match rest.strip_prefix('"') {
                Some(quoted) => {
                    let end = quoted.find('"').unwrap_or(quoted.len());
                    (&quoted[..end], true, quoted.get(end + 1..).unwrap_or(""))
                }
                None => {
                    let end = rest
                        .find(|c: char| c.is_whitespace() || c == '"')
                        .unwrap_or(rest.len());
                    (&rest[..end], false, &rest[end..])
                }
            };
            if !text.trim().is_empty() {
                let case_sensitive = text.chars().any(char::is_uppercase);
                terms.push(Term {
                    needle: if case_sensitive {
                        text.to_owned()
                    } else {
                        text.to_lowercase()
                    },
                    text: text.to_owned(),
                    how,
                    exclude,
                    phrase,
                    case_sensitive,
                });
            }
            rest = after.trim_start();
        }
        Filter { terms }
    }

    pub fn meta(&self, h: &Haystack) -> Meta {
        let allowed = self
            .terms
            .iter()
            .filter(|t| t.exclude || t.how == How::Fuzzy)
            .all(|t| t.hit(h) != t.exclude);
        let matched = allowed && self.plain().all(|t| t.hit(h));
        Meta { allowed, matched }
    }

    /// Plain (not excluded, not fuzzy) terms and phrases.
    fn plain(&self) -> impl Iterator<Item = &Term> {
        self.terms
            .iter()
            .filter(|t| !t.exclude && t.how == How::Exact)
    }

    /// The query to run against conversations: `None` without plain terms
    /// (fuzzy terms are metadata-only).
    pub fn conversation(&self) -> Option<Conversation> {
        let units = |exclude: bool| {
            self.terms
                .iter()
                .filter(|t| t.exclude == exclude && t.how == How::Exact)
                .map(Term::unit)
                .collect::<Vec<_>>()
                .join(" ")
        };
        let query = units(false);
        (!query.is_empty()).then(|| Conversation {
            query,
            exclude: units(true),
        })
    }
}

/// An agent's searchable metadata, one field per line so a phrase never
/// spans two fields.
#[derive(Clone, Debug, Default)]
pub struct Haystack {
    raw: String,
    lower: String,
}

impl Haystack {
    /// Name, pane, kind, state, cwd, repo, branch, `repo@branch`, model,
    /// session id and context line, untruncated.
    pub fn of(agent: &Agent) -> Haystack {
        let repo_branch = format!("{}@{}", agent.repo, agent.branch);
        Haystack::new(&[
            &agent.name,
            &agent.pane,
            agent.kind.as_str(),
            agent.state.as_str(),
            &agent.cwd,
            &agent.repo,
            &agent.branch,
            &repo_branch,
            &agent.model,
            &agent.session_id,
            &agent.info,
        ])
    }

    fn new(fields: &[&str]) -> Haystack {
        let raw = fields.join("\n");
        Haystack {
            lower: raw.to_lowercase(),
            raw,
        }
    }
}

fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut rest = haystack.chars();
    needle.chars().all(|c| rest.any(|h| h == c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::model::{Kind, State};

    fn agent() -> Agent {
        let mut a = Agent::new("work:9.1".into(), "work".into(), Kind::Codex);
        a.state = State::Idle;
        a.name = "Investigate domain website timeouts in the staging cluster".into();
        a.cwd = "/home/alice/src/website".into();
        a.repo = "website".into();
        a.branch = "main".into();
        a.model = "gpt-5.5".into();
        a.session_id = "019c0dec-0001-7000-8000-0000000000c5".into();
        a.info = "⏵⏵ bypass permissions on · 1 shell".into();
        a
    }

    fn matches(query: &str) -> bool {
        Filter::parse(query).meta(&Haystack::of(&agent())).matched
    }

    #[test]
    fn empty_query_matches_everything() {
        assert_eq!(Filter::parse("   "), Filter::default());
        assert!(matches(""));
        assert_eq!(Filter::parse("  ").conversation(), None);
    }

    #[test]
    fn terms_are_and_ed_substrings() {
        assert!(matches("codex"));
        assert!(matches("idle website"));
        assert!(matches("dex"));
        assert!(!matches("busy"));
        assert!(!matches("codex busy"));
        // Subsequences no longer match without `~`.
        assert!(!matches("cdx"));
    }

    #[test]
    fn smart_case() {
        assert!(matches("investigate"));
        assert!(matches("Investigate"));
        assert!(!matches("INVESTIGATE"));
        assert!(!matches("Website"));
    }

    #[test]
    fn phrases() {
        assert!(matches("\"domain website\""));
        assert!(!matches("\"website domain\""));
        // An unclosed quote runs to the end.
        assert!(matches("\"staging cluster"));
        // A phrase never spans two fields.
        assert!(!matches("\"cluster work\""));
        assert!(matches("\"bypass permissions\" codex"));
    }

    #[test]
    fn negation() {
        assert!(matches("!busy"));
        assert!(!matches("!idle"));
        assert!(!matches("codex !\"staging cluster\""));
        assert!(matches("codex !\"cluster staging\""));
        let meta = Filter::parse("busy !idle").meta(&Haystack::of(&agent()));
        assert!(!meta.allowed && !meta.matched);
        // A lone `!` or `~` is ignored while typing.
        assert!(matches("codex !"));
        assert!(matches("codex ~"));
    }

    #[test]
    fn fuzzy_is_opt_in() {
        assert!(matches("~cdx"));
        assert!(matches("~cdx idle"));
        assert!(!matches("~xedoc"));
        assert!(!matches("~CDX"));
        // Within one field: `x` of codex, then `g` of the model.
        assert!(!matches("~xg"));
        assert!(matches("~gpt55"));
        // Fuzzy terms gate the agent but are not conversation terms.
        let meta = Filter::parse("~cdx kubernetes").meta(&Haystack::of(&agent()));
        assert!(meta.allowed && !meta.matched);
        let meta = Filter::parse("~zzz kubernetes").meta(&Haystack::of(&agent()));
        assert!(!meta.allowed);
    }

    #[test]
    fn full_fields_beyond_truncation() {
        // The rendered row cuts the name at 26 cells and drops cwd and model.
        assert!(matches("timeouts"));
        assert!(matches("\"staging cluster\""));
        assert!(matches("/home/alice/src"));
        assert!(matches("gpt-5.5"));
        assert!(matches("0000000000c5"));
        assert!(matches("website@main"));
        assert!(matches("work:9.1"));
        assert!(matches("shell"));
    }

    #[test]
    fn conversation_query() {
        let f = Filter::parse("deploy \"rate limit\" ~cdx !flaky !\"dry run\" Kube");
        assert_eq!(
            f.conversation(),
            Some(Conversation {
                query: "deploy \"rate limit\" Kube".into(),
                exclude: "flaky \"dry run\"".into(),
            })
        );
        assert_eq!(Filter::parse("~cdx !busy").conversation(), None);
    }
}
