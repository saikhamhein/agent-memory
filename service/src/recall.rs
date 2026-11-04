//! The only recall renderer (design §3.3). Pure functions, no I/O.
//!
//! An empty query is the start view: the pinned core, this project's and the
//! requested memories in full, and one index line for everything else. A
//! non-empty query is the task view: only memories that match it. Anything
//! over a bound is listed, never dropped.

use crate::model::{Kind, Memory, absolute_path, block_len};
use chrono::Utc;
use regex::Regex;
use std::{collections::BTreeSet, path::Path, sync::LazyLock};

/// Characters of the whole start view, under Claude Code's 10,000-character hook cap.
pub const START_MAX: usize = 9500;
/// `block_len` shared by "This project" and "Requested" in the start view.
pub const START_BODY_BUDGET: usize = 3000;
/// `block_len` of the memories the task view shows in full.
pub const TASK_BODY_BUDGET: usize = 6000;
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
    let memories = in_scope(memories, &request.project);
    if request.query.trim().is_empty() {
        start(&memories, revision, mirror_dir, request)
    } else {
        task(&memories, revision, &request.query)
    }
}

/// INDEX.md in the mirror: the pinned global memories in full, then one line
/// for every other memory (every project's included), with no cap.
pub fn index_md(memories: &[Memory], revision: i64, mirror_dir: &str) -> String {
    let today = Utc::now().date_naive().to_string();
    let live: Vec<&Memory> = memories.iter().filter(|m| !excluded(m, &today)).collect();
    let core = |m: &&&Memory| m.scope.kind == Kind::Global && is_pinned(m);
    let mut text = header(revision, mirror_dir);
    let pinned = sorted(live.iter().filter(core));
    if !pinned.is_empty() {
        text.push_str("## Always apply\n");
        text.extend(pinned.iter().map(|m| full(m)));
    }
    let index: Vec<&Memory> = [Kind::Global, Kind::Project, Kind::Topic, Kind::Role]
        .into_iter()
        .flat_map(|kind| sorted(live.iter().filter(|m| m.scope.kind == kind && !core(m))))
        .collect();
    if !index.is_empty() {
        text.push_str("## Index\n");
        text.extend(index.iter().map(|m| line(m)));
    }
    text.trim_end().to_owned() + "\n"
}

/// Neither excluded nor another project's.
fn in_scope<'a>(memories: &'a [Memory], project: &str) -> Vec<&'a Memory> {
    let today = Utc::now().date_naive().to_string();
    let project = absolute_path(project).ok();
    memories
        .iter()
        .filter(|m| {
            !excluded(m, &today)
                && (m.scope.kind != Kind::Project || applies(m, project.as_deref()))
        })
        .collect()
}

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
        let n = block_len(m);
        let ok = used + n <= START_BODY_BUDGET;
        used += if ok { n } else { 0 };
        ok
    };
    let project: Vec<_> = project.into_iter().filter(|m| fits(m)).collect();
    let requested: Vec<_> = requested.into_iter().filter(|m| fits(m)).collect();

    let mut text = header(revision, mirror_dir);
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
        let lines: Vec<String> = index.iter().map(|m| line(m)).collect();
        let room = START_MAX.saturating_sub(text.chars().count());
        let (listed, collapsed) = listed(&index, &lines, room, mirror_dir);
        text.extend(lines[..listed].iter().map(String::as_str));
        let rest = &index[listed..];
        if collapsed {
            text.push_str(&collapse_line(rest.len(), mirror_dir));
        } else {
            text.push_str(&count_lines(rest));
        }
    }
    Recalled {
        view: "start",
        pinned: pinned.iter().map(|m| m.path.clone()).collect(),
        returned,
        text: text.trim_end().to_owned() + "\n",
    }
}

/// How many index lines fit in `room` characters beside the one collapse line,
/// stopping at the first that doesn't, and whether the rest stays collapsed
/// (its per-directory count lines don't fit in what is left).
fn listed(index: &[&Memory], lines: &[String], room: usize, mirror_dir: &str) -> (usize, bool) {
    let len = |s: &str| s.chars().count();
    let mut used = 0;
    let mut k = 0;
    while k < index.len() {
        let rest = index.len() - k - 1;
        let collapse = if rest > 0 {
            len(&collapse_line(rest, mirror_dir))
        } else {
            0
        };
        if used + len(&lines[k]) + collapse > room {
            break;
        }
        used += len(&lines[k]);
        k += 1;
    }
    let collapsed = k < index.len() && used + len(&count_lines(&index[k..])) > room;
    (k, collapsed)
}

/// One count line per directory, in order of first appearance.
fn count_lines(rest: &[&Memory]) -> String {
    let mut dirs: Vec<(&str, usize)> = Vec::new();
    for m in rest {
        match dirs.iter_mut().find(|(d, _)| *d == dir(m)) {
            Some((_, n)) => *n += 1,
            None => dirs.push((dir(m), 1)),
        }
    }
    dirs.iter().map(|(d, n)| count_line(d, *n)).collect()
}

fn count_line(dir: &str, n: usize) -> String {
    format!("- {dir} — {n} more not listed; search or list the directory\n")
}

fn collapse_line(n: usize, mirror_dir: &str) -> String {
    format!("- {n} more memories not listed; full index: {mirror_dir}/INDEX.md\n")
}

/// The parent directory, with its trailing slash.
fn dir(m: &Memory) -> &str {
    &m.path[..=m.path.rfind('/').unwrap_or(0)]
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
            // A project's path words would match all of its memories.
            let scope = if m.scope.kind == Kind::Project {
                ""
            } else {
                &m.scope.value
            };
            let head = terms(&format!("{} {tags} {scope}", title(m)));
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
            text: format!("No memory matches \"{query}\".\n"),
        };
    }
    matches.sort_by(|(a, x), (b, y)| b.cmp(a).then_with(|| order(x, y)));

    let mut text = format!("# Memories matching \"{query}\" — revision {revision}\n\n");
    let mut returned = Vec::new();
    let mut lines = Vec::new();
    let mut used = 0;
    for (rank, (_, m)) in matches.iter().enumerate() {
        let n = block_len(m);
        if rank < SEARCH_RESULTS && used + n <= TASK_BODY_BUDGET {
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
            text.push_str(&line(m));
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

fn header(revision: i64, mirror_dir: &str) -> String {
    format!(
        "# Shared agent memory — revision {revision}\n\n{HEADER}\nRead-only files: {mirror_dir}/ (index: INDEX.md). Read a memory there when its index line applies; search with agent-memory-client recall --project ROOT --query TEXT; suggest a change with agent-memory-client observe PATH - (a memory file on stdin); mark one that helped with agent-memory-client useful PATH --why TEXT.\n\n"
    )
}

fn line(m: &Memory) -> String {
    format!("- {} — {}\n", m.path, title(m))
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

/// A project memory applies at its root or below it, by whole components.
fn applies(m: &Memory, project: Option<&Path>) -> bool {
    project.is_some_and(|p| p.starts_with(&m.scope.value))
}
