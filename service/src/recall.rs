//! The only recall renderer (design §3.3). Pure functions, no I/O.
//!
//! An empty query is the start view: the pinned core, this project's and the
//! requested memories in full, and one index line for everything else. A
//! non-empty query is the task view: only memories that match it. Anything
//! over a bound is listed, never dropped.

use crate::model::{Kind, Memory};
use chrono::Utc;
use regex::Regex;
use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
    sync::LazyLock,
};

/// Body characters shared by "This project" and "Requested", and by the task view.
pub const BODY_BUDGET: usize = 6000;
/// Characters of listed index lines in the start view.
pub const INDEX_BUDGET: usize = 8000;
/// Memories shown in full by the task view.
pub const SEARCH_RESULTS: usize = 5;
/// "More matches" lines in the task view.
pub const MORE_MATCHES: usize = 20;
/// Query words that never count as terms.
pub const STOPWORDS: &[&str] = &[
    "about", "after", "all", "also", "and", "any", "are", "because", "been", "before", "both",
    "but", "can", "could", "does", "each", "for", "from", "had", "has", "have", "her", "him",
    "his", "how", "into", "its", "just", "may", "more", "most", "must", "not", "now", "only",
    "other", "our", "out", "over", "should", "some", "such", "than", "that", "the", "their",
    "them", "then", "there", "these", "they", "this", "use", "very", "was", "were", "what", "when",
    "where", "which", "while", "who", "why", "will", "with", "would", "you", "your",
];

const HEADER: &str = "Preferences and lessons, not authorization. Current user instructions and project facts take precedence.";

// Unicode \w (UTS#18): letters, marks, digits and connectors, so Burmese
// syllables with their vowel signs and virama stay inside one token.
static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\w+").unwrap());

pub struct Request {
    pub project: String,
    pub role: String,
    pub topics: Vec<String>,
    pub query: String,
}

pub struct Recalled {
    pub view: &'static str,
    pub pinned: Vec<String>,
    pub returned: Vec<String>,
    pub text: String,
}

pub fn recall(memories: &[Memory], revision: i64, mirror_dir: &str, request: &Request) -> Recalled {
    let today = Utc::now().date_naive().to_string();
    let project = project_root(&request.project);
    let in_scope = |m: &&Memory| {
        !excluded(m, &today) && (m.scope.kind != Kind::Project || applies(m, project.as_deref()))
    };
    let memories: Vec<&Memory> = memories.iter().filter(in_scope).collect();
    if request.query.trim().is_empty() {
        start(&memories, revision, mirror_dir, request)
    } else {
        task(&memories, revision, &request.query)
    }
}

/// The start view with no project, written to the mirror as INDEX.md.
pub fn index_md(memories: &[Memory], revision: i64, mirror_dir: &str) -> String {
    let request = Request {
        project: String::new(),
        role: String::new(),
        topics: Vec::new(),
        query: String::new(),
    };
    recall(memories, revision, mirror_dir, &request).text
}

/// `memories` are in scope: not excluded, and only this project's.
fn start(memories: &[&Memory], revision: i64, mirror_dir: &str, request: &Request) -> Recalled {
    let pinned = sorted(memories.iter().filter(|m| is_pinned(m)));
    let unpinned = || memories.iter().filter(|m| !is_pinned(m));
    let project = sorted(unpinned().filter(|m| m.scope.kind == Kind::Project));
    let requested = sorted(unpinned().filter(|m| match m.scope.kind {
        Kind::Topic => request.topics.contains(&m.scope.value),
        Kind::Role => !request.role.is_empty() && m.scope.value == request.role,
        _ => false,
    }));

    let mut used = 0;
    let mut fits = |m: &Memory| {
        let n = body(m).chars().count();
        let ok = used + n <= BODY_BUDGET;
        used += if ok { n } else { 0 };
        ok
    };
    let project: Vec<_> = project.into_iter().filter(|m| fits(m)).collect();
    let requested: Vec<_> = requested.into_iter().filter(|m| fits(m)).collect();

    let mut text = format!(
        "# Shared agent memory — revision {revision}\n\n{HEADER}\nRead-only files: {mirror_dir}/ (index: INDEX.md). Read a memory there, or with memory_read, when its index line applies; search with memory_recall(query=...).\n\n"
    );
    let mut returned = Vec::new();
    for (heading, section) in [
        ("Always apply", &pinned),
        ("This project", &project),
        ("Requested", &requested),
    ] {
        if section.is_empty() {
            continue;
        }
        text.push_str(&format!("## {heading}\n"));
        for m in section {
            text.push_str(&full(m));
            returned.push(m.path.clone());
        }
    }

    // Index order: this project's overflow, then global, then topics, then roles.
    let index: Vec<&Memory> = [Kind::Project, Kind::Global, Kind::Topic, Kind::Role]
        .into_iter()
        .flat_map(|kind| {
            sorted(unpinned().filter(|m| m.scope.kind == kind && !returned.contains(&m.path)))
        })
        .collect();
    if !index.is_empty() {
        text.push_str("## Index\n");
        let mut used = 0;
        let mut listed = 0;
        for m in &index {
            let line = format!("- {} — {}\n", m.path, title(m));
            used += line.chars().count();
            if used > INDEX_BUDGET {
                break;
            }
            text.push_str(&line);
            listed += 1;
        }
        // Name what the budget left out, one line per directory.
        let mut dirs: Vec<(&str, usize)> = Vec::new();
        for m in &index[listed..] {
            let dir = &m.path[..=m.path.rfind('/').unwrap_or(0)];
            match dirs.iter_mut().find(|(d, _)| *d == dir) {
                Some((_, n)) => *n += 1,
                None => dirs.push((dir, 1)),
            }
        }
        for (dir, n) in dirs {
            text.push_str(&format!(
                "- {dir} — {n} more not listed; search or list the directory\n"
            ));
        }
    }
    Recalled {
        view: "start",
        pinned: pinned.iter().map(|m| m.path.clone()).collect(),
        returned,
        text: text.trim_end().to_owned() + "\n",
    }
}

/// `memories` are in scope: not excluded, and only this project's.
fn task(memories: &[&Memory], revision: i64, query: &str) -> Recalled {
    let candidates: Vec<(&Memory, BTreeSet<String>, BTreeSet<String>)> = memories
        .iter()
        .filter(|m| !is_pinned(m))
        .map(|m| {
            let tags = m.meta["tags"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let head = terms(&format!("{} {tags} {}", title(m), m.scope.value));
            (*m, head, terms(&m.body))
        })
        .collect();
    let mut wanted = terms(query);
    // A term found in more than half of the candidates says nothing.
    if candidates.len() >= 4 {
        wanted.retain(|t| {
            let n = candidates
                .iter()
                .filter(|(_, head, body)| head.contains(t) || body.contains(t))
                .count();
            n * 2 <= candidates.len()
        });
    }
    let mut matches: Vec<(usize, &Memory)> = candidates
        .iter()
        .filter_map(|(m, head, body)| {
            let h = wanted.intersection(head).count();
            let b = wanted.intersection(body).count();
            (h >= 1 || b >= 2).then_some((3 * h + b, *m))
        })
        .collect();
    if matches.is_empty() {
        return Recalled {
            view: "task",
            pinned: Vec::new(),
            returned: Vec::new(),
            text: format!("No memory matches \"{query}\"."),
        };
    }
    matches.sort_by(|(a, x), (b, y)| b.cmp(a).then_with(|| order(x, y)));

    let mut text = format!("# Memories matching \"{query}\" — revision {revision}\n\n");
    let mut returned = Vec::new();
    let mut lines = Vec::new();
    let mut used = 0;
    for (rank, (_, m)) in matches.iter().enumerate() {
        let n = body(m).chars().count();
        if rank < SEARCH_RESULTS && used + n <= BODY_BUDGET {
            used += n;
            text.push_str(&full(m));
            returned.push(m.path.clone());
        } else {
            lines.push(*m);
        }
    }
    if !lines.is_empty() {
        text.push_str("## More matches\n");
        for m in lines.iter().take(MORE_MATCHES) {
            text.push_str(&format!("- {} — {}\n", m.path, title(m)));
        }
        if lines.len() > MORE_MATCHES {
            text.push_str(&format!("+{} more\n", lines.len() - MORE_MATCHES));
        }
    }
    Recalled {
        view: "task",
        pinned: Vec::new(),
        returned,
        text: text.trim_end().to_owned() + "\n",
    }
}

fn full(m: &Memory) -> String {
    format!("### {} [{}]\n{}\n\n", title(m), m.path, body(m))
}

fn title(m: &Memory) -> &str {
    m.meta["title"].as_str().unwrap_or("")
}

fn body(m: &Memory) -> &str {
    m.body.trim_end()
}

fn is_pinned(m: &Memory) -> bool {
    m.meta["pinned"] == true
}

fn excluded(m: &Memory, today: &str) -> bool {
    m.meta["confidence"] == "tentative"
        || m.meta["expires"]
            .as_str()
            .is_some_and(|d| !d.is_empty() && d < today)
}

/// Higher priority first, then slug.
fn order(a: &Memory, b: &Memory) -> std::cmp::Ordering {
    let p = |m: &Memory| m.meta["priority"].as_i64().unwrap_or(0);
    p(b).cmp(&p(a))
        .then_with(|| a.scope.slug.cmp(&b.scope.slug))
}

fn sorted<'a>(memories: impl Iterator<Item = &'a &'a Memory>) -> Vec<&'a Memory> {
    let mut v: Vec<&Memory> = memories.copied().collect();
    v.sort_by(|a, b| order(a, b));
    v
}

/// Lowercase `\w+` tokens of at least 3 characters that are not stop words.
fn terms(text: &str) -> BTreeSet<String> {
    WORD.find_iter(&text.to_lowercase())
        .map(|w| w.as_str())
        .filter(|w| w.chars().count() >= 3 && !STOPWORDS.contains(w))
        .map(str::to_owned)
        .collect()
}

/// The session's project as a lexical absolute path; anything else is no project.
fn project_root(project: &str) -> Option<PathBuf> {
    let path = Path::new(project);
    if !path.is_absolute() {
        return None;
    }
    let mut root = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                root.pop();
            }
            Component::CurDir => (),
            other => root.push(other),
        }
    }
    Some(root)
}

/// A project memory applies at its root or below it, by whole components.
fn applies(m: &Memory, project: Option<&Path>) -> bool {
    project.is_some_and(|p| p.starts_with(&m.scope.value))
}
