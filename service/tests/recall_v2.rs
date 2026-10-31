//! Acceptance tests for workstream R (design `docs/design/managed-agents-alignment.md`
//! §3.3 and §4.3 "R"). They are written against the frozen `recall.rs` interface of §4.1
//! and judge the output by the rules of §3.3, not by any implementation.
//!
//! Memories are built with `model::parse` from real file content. The shared fixture
//! (`tests/fixtures/recall/store/`, one file per memory, plus the golden `*.txt`
//! outputs) is synthetic and shaped like the live set; `render_golden.py` next to it is the
//! independent spec-derived renderer that produced the goldens.
//!
//! Bounds are literals on purpose: they are the frozen constants of §3.3
//! (`START_MAX` 9,500, `START_BODY_BUDGET` 3,000, `TASK_BODY_BUDGET` 6,000, pinned budget 4,000).

use agent_memory_server::{
    model::{self, Memory},
    recall::{self, Recalled, Request},
};
use regex::Regex;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

const MIRROR: &str = "/srv/memory/mnt/shared-agent-memory";
const REV: i64 = 42;
const AM: &str = "/home/sai/Projects/agent-memory";
const LORA: &str = "/home/sai/Projects/custom-lora-mesh";
const KEEP: &str = "/global/keep-it-simple.md";
const START_MAX: usize = 9500;
const START_BODY_BUDGET: usize = 3000;
const TASK_BODY_BUDGET: usize = 6000;

// ---------------------------------------------------------------- helpers

/// One memory plus the facts the tests need about it, read from the file content itself.
struct Item {
    path: String,
    title: String,
    tags: Vec<String>,
    body: String,
    pinned: bool,
    tentative: bool,
    expires: Option<String>,
    priority: i64,
}

impl Item {
    fn expired(&self) -> bool {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        self.expires
            .as_deref()
            .is_some_and(|e| !e.is_empty() && e < today.as_str())
    }
    fn live(&self) -> bool {
        !self.tentative && !self.expired()
    }
    /// `/projects/a/b/x.md` applies to project `/a/b` and below, by whole components.
    fn applies(&self, project: &str) -> bool {
        match self.path.strip_prefix("/projects/") {
            None => true,
            Some(rest) => {
                let root = format!("/{}", rest.rsplit_once('/').map_or("", |(d, _)| d));
                let project = project.trim_end_matches('/');
                !project.is_empty() && (project == root || project.starts_with(&format!("{root}/")))
            }
        }
    }
    fn dir(&self) -> String {
        format!("{}/", self.path.rsplit_once('/').unwrap().0)
    }
    /// Value of the scope: the topic, the role, or the project root.
    fn scope_value(&self) -> String {
        let segs: Vec<&str> = self.path.trim_matches('/').split('/').collect();
        match segs[0] {
            "topics" | "roles" => segs[1].to_string(),
            "projects" => format!("/{}", segs[1..segs.len() - 1].join("/")),
            _ => String::new(),
        }
    }
    fn sort_key(&self) -> (i64, String) {
        (
            -self.priority,
            self.path.rsplit('/').next().unwrap().to_string(),
        )
    }
}

struct Store {
    items: Vec<Item>,
    mems: Vec<Memory>,
}

fn content(title: &str, body: &str, extra: Value) -> String {
    let mut meta = json!({"title": title, "confidence": "explicit", "tags": ["misc"],
        "evidence": [{"source": "test", "locator": "recall_v2", "note": "Synthetic evidence"}]});
    for (k, v) in extra.as_object().into_iter().flatten() {
        meta[k] = v.clone();
    }
    format!("---\n{meta}\n---\n{body}\n")
}

impl Store {
    fn new() -> Self {
        Store {
            items: Vec::new(),
            mems: Vec::new(),
        }
    }
    /// Add a memory from file content (frontmatter + body).
    fn add_raw(&mut self, path: &str, text: &str) {
        let mut parts = text.splitn(3, "---\n");
        parts.next();
        let meta: Value = serde_json::from_str(parts.next().unwrap().trim()).unwrap();
        let body = parts.next().unwrap().trim_end_matches('\n').to_string();
        self.items.push(Item {
            path: path.to_string(),
            title: meta["title"].as_str().unwrap().to_string(),
            tags: meta["tags"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t.as_str().unwrap().to_string())
                .collect(),
            body,
            pinned: meta["pinned"].as_bool().unwrap_or(false),
            tentative: meta["confidence"] == "tentative",
            expires: meta["expires"].as_str().map(str::to_string),
            priority: meta["priority"].as_i64().unwrap_or(0),
        });
        self.mems.push(
            model::parse(path, text).unwrap_or_else(|e| panic!("{path} does not parse: {e}")),
        );
    }
    fn add(&mut self, path: &str, title: &str, body: &str, extra: Value) -> &mut Self {
        self.add_raw(path, &content(title, body, extra));
        self
    }
    /// The shared fixture: ~30 synthetic memories (global, topics, roles, projects).
    fn fixture() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/recall/store");
        let mut files = Vec::new();
        walk(&root, &mut files);
        files.sort();
        let mut s = Store::new();
        for f in files {
            let path = format!("/{}", f.strip_prefix(&root).unwrap().to_string_lossy());
            s.add_raw(&path, &fs::read_to_string(&f).unwrap());
        }
        s
    }
    /// `n` unrelated memories in scopes other than the fixture's requested ones: global
    /// (unpinned), topics `topic-N`, roles `role-N` and other projects `extra-N`.
    fn extras(&mut self, n: usize) -> &mut Self {
        for i in 0..n {
            let path = match i % 4 {
                0 => format!("/global/extra-{i}.md"),
                1 => format!("/topics/topic-{}/extra-{i}.md", i % 20),
                2 => format!("/roles/role-{}/extra-{i}.md", i % 7),
                _ => format!("/projects/home/sai/Projects/extra-{}/extra-{i}.md", i % 40),
            };
            self.add(
                &path,
                &format!("Extra note about zq{i} handling"),
                &format!("Filler body for synthetic entry zq{i}."),
                json!({"priority": i % 9}),
            );
        }
        self
    }
    fn item(&self, path: &str) -> &Item {
        self.items
            .iter()
            .find(|i| i.path == path)
            .unwrap_or_else(|| panic!("no fixture memory {path}"))
    }
    /// Memories the start view must account for: live and applicable to the project.
    fn in_scope(&self, project: &str) -> Vec<&Item> {
        self.items
            .iter()
            .filter(|i| i.live() && i.applies(project))
            .collect()
    }
    fn recall(&self, project: &str, role: &str, topics: &[&str], query: &str) -> Recalled {
        recall::recall(&self.mems, REV, MIRROR, &req(project, role, topics, query))
    }
    fn start(&self, project: &str, role: &str, topics: &[&str]) -> Recalled {
        self.recall(project, role, topics, "")
    }
    fn task(&self, query: &str) -> Recalled {
        self.recall(AM, "", &[], query)
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "md") {
            out.push(p);
        }
    }
}

fn req(project: &str, role: &str, topics: &[&str], query: &str) -> Request {
    Request {
        project: project.into(),
        role: role.into(),
        topics: topics.iter().map(|t| t.to_string()).collect(),
        query: query.into(),
    }
}

fn no_match(query: &str) -> String {
    format!("No memory matches \"{query}\".\n")
}

/// Every recall text ends with exactly one newline.
fn assert_one_newline(text: &str) {
    assert!(
        text.ends_with('\n') && !text.ends_with("\n\n"),
        "text must end with exactly one newline: {:?}",
        &text[text.len().saturating_sub(40)..]
    );
}

fn block(i: &Item) -> String {
    format!("### {} [{}]\n{}", i.title, i.path, i.body)
}

/// `model::block_len`: the chars of `### {title} [{path}]\n{body}\n\n`, the cost every budget counts.
fn block_len(i: &Item) -> usize {
    block(i).chars().count() + 2
}

/// A body that makes the block of a memory with this title and path exactly `n` characters.
fn body_for_block(n: usize, title: &str, path: &str) -> String {
    let overhead = format!("### {title} [{path}]\n").chars().count() + 2;
    "b".repeat(n - overhead)
}

/// The start view above `## Index`, without the revision line.
fn above_index(text: &str) -> &str {
    let head = text.split("\n## Index\n").next().unwrap();
    head.split_once('\n').unwrap().1
}

fn headings(text: &str) -> Vec<String> {
    let re = Regex::new(r"(?m)^### .* \[(/[^\[\]]+)\]$").unwrap();
    re.captures_iter(text).map(|c| c[1].to_string()).collect()
}

/// Paths under the `## name` heading, in order.
fn section(text: &str, name: &str) -> Vec<String> {
    let marker = format!("\n## {name}\n");
    match text.split_once(&marker) {
        None => Vec::new(),
        Some((_, rest)) => headings(rest.split("\n## ").next().unwrap()),
    }
}

struct Index {
    listed: Vec<String>,
    counts: Vec<(String, usize)>,
    /// `- N more memories not listed; full index: DIR/INDEX.md`, N
    collapsed: Option<usize>,
}

fn index(text: &str) -> Index {
    let listed_re = Regex::new(r"^- (/\S+\.md) — ").unwrap();
    let count_re =
        Regex::new(r"^- (/\S*/) — (\d+) more not listed; search or list the directory$").unwrap();
    let collapse_re =
        Regex::new(r"^- (\d+) more memories not listed; full index: (.+)/INDEX\.md$").unwrap();
    let mut ix = Index {
        listed: vec![],
        counts: vec![],
        collapsed: None,
    };
    let Some((_, rest)) = text.split_once("\n## Index\n") else {
        return ix;
    };
    for line in rest.lines().filter(|l| !l.is_empty()) {
        if let Some(c) = listed_re.captures(line) {
            ix.listed.push(c[1].to_string());
        } else if let Some(c) = count_re.captures(line) {
            ix.counts.push((c[1].to_string(), c[2].parse().unwrap()));
        } else if let Some(c) = collapse_re.captures(line) {
            assert!(ix.collapsed.is_none(), "two collapse lines");
            ix.collapsed = Some(c[1].parse().unwrap());
        } else {
            panic!("index line is neither a memory nor a count: {line:?}");
        }
    }
    ix
}

fn set<I: IntoIterator<Item = S>, S: Into<String>>(it: I) -> BTreeSet<String> {
    it.into_iter().map(Into::into).collect()
}

/// Index order of §3.3: this project's overflow, then global, topics, roles; each by priority then slug.
fn index_rank(i: &Item) -> u8 {
    match i.path.split('/').nth(1).unwrap() {
        "projects" => 0,
        "global" => 1,
        "topics" => 2,
        _ => 3,
    }
}

/// Every live, applicable memory is accounted for exactly once: in full, listed in the index
/// (a prefix of the index order), or counted by a count line (or the one collapse line), and
/// nothing else appears (other projects, tentative, expired). The whole view is within START_MAX.
fn assert_accounted(store: &Store, project: &str, text: &str) {
    assert_one_newline(text);
    assert!(
        text.chars().count() <= START_MAX,
        "start view has {} characters",
        text.chars().count()
    );
    let want: BTreeSet<String> = store
        .in_scope(project)
        .iter()
        .map(|i| i.path.clone())
        .collect();
    let full = headings(text);
    let full_set = set(full.iter().cloned());
    let ix = index(text);
    assert_eq!(full.len(), full_set.len(), "a memory appears in full twice");
    assert!(
        full_set.is_subset(&want),
        "out-of-scope memories in full: {:?}",
        full_set.difference(&want).collect::<Vec<_>>()
    );
    let mut order: Vec<&Item> = store
        .in_scope(project)
        .into_iter()
        .filter(|i| !full_set.contains(&i.path))
        .collect();
    order.sort_by_key(|i| (index_rank(i), i.sort_key()));
    let k = ix.listed.len();
    assert!(k <= order.len(), "more index lines than memories");
    assert_eq!(
        ix.listed,
        order[..k]
            .iter()
            .map(|i| i.path.clone())
            .collect::<Vec<_>>(),
        "listed lines are not the start of the index order"
    );
    let rest = &order[k..];
    match ix.collapsed {
        Some(n) => {
            assert!(
                ix.counts.is_empty(),
                "count lines next to the collapse line"
            );
            assert_eq!(n, rest.len(), "collapse line counts the wrong number");
            // as many lines are listed as fit beside the collapse line, so the next one does not
            // (the count in the collapse line may lose one digit when a line is listed)
            if let Some(next) = rest.first() {
                let line = format!("- {} — {}\n", next.path, next.title)
                    .chars()
                    .count();
                assert!(
                    text.chars().count() + line - 1 > START_MAX,
                    "index stops early: {} characters, the next line has {line}",
                    text.chars().count()
                );
            }
        }
        None => {
            // one line per directory, in the order each directory first appears among the unlisted
            let mut want_counts: Vec<(String, usize)> = Vec::new();
            for i in rest {
                match want_counts.iter_mut().find(|(d, _)| *d == i.dir()) {
                    Some((_, n)) => *n += 1,
                    None => want_counts.push((i.dir(), 1)),
                }
            }
            assert_eq!(
                ix.counts, want_counts,
                "count lines do not match the unlisted memories"
            );
        }
    }
}

// ---------------------------------------------------------------- 1. core_never_cut

fn assert_core(store: &Store, projects: &[&str], roles: &[&str], topic_sets: &[&[&str]]) {
    for project in projects {
        for role in roles {
            for topics in topic_sets {
                let r = store.start(project, role, topics);
                let want: Vec<&Item> = {
                    let mut p: Vec<&Item> = store
                        .in_scope(project)
                        .into_iter()
                        .filter(|i| i.pinned)
                        .collect();
                    p.sort_by_key(|i| i.sort_key());
                    p
                };
                assert_eq!(r.view, "start");
                assert!(want.iter().any(|i| i.path == KEEP));
                assert_eq!(
                    set(r.pinned.iter().cloned()),
                    set(want.iter().map(|i| i.path.clone())),
                    "pinned list for {project:?} {role:?} {topics:?}"
                );
                assert_eq!(r.pinned.len(), want.len(), "a pinned path is listed twice");
                assert_eq!(
                    r.pinned,
                    section(&r.text, "Always apply"),
                    "`pinned` is the Always apply section, in output order"
                );
                assert_eq!(
                    r.returned,
                    headings(&r.text),
                    "`returned` is every body in the text, pinned included, in output order"
                );
                for i in &want {
                    assert!(
                        r.text.contains(&block(i)),
                        "pinned {} cut or altered for {project:?} {role:?} {topics:?}",
                        i.path
                    );
                    assert!(!index(&r.text).listed.contains(&i.path));
                }
                assert!(section(&r.text, "Always apply").len() == want.len());
            }
        }
    }
}

const PROJECTS: [&str; 4] = ["", AM, LORA, "/nowhere/at/all"];
const ROLES: [&str; 3] = ["", "coordinator", "nobody"];
const TOPIC_SETS: [&[&str]; 3] = [
    &[],
    &["hardware"],
    &["hardware", "media", "ui", "burmese", "documentation"],
];

#[test]
fn core_never_cut() {
    // fixture alone
    assert_core(&Store::fixture(), &PROJECTS, &ROLES, &TOPIC_SETS);
    // 5,000 extra memories
    let mut big = Store::fixture();
    big.extras(5000);
    assert_core(
        &big,
        &[AM, ""],
        &["coordinator"],
        &[&["hardware"][..], &[][..]],
    );
    // 30 large unpinned project memories overflow BODY_BUDGET
    let mut over = Store::fixture();
    for i in 0..30 {
        over.add(
            &format!("/projects/home/sai/Projects/agent-memory/big-{i:02}.md"),
            &format!("Large project note {i}"),
            &"x".repeat(1500),
            json!({"priority": 100 - i}),
        );
    }
    assert_core(
        &over,
        &PROJECTS,
        &["coordinator"],
        &[&["hardware"][..], &[][..]],
    );
    let r = over.start(AM, "", &[]);
    assert!(
        r.text.contains(&block(over.item(
            "/projects/home/sai/Projects/agent-memory/run-tests-before-merge.md"
        ))),
        "project-pinned memory cut by overflow"
    );
}

// ---------------------------------------------------------------- 2. start_is_constant_beyond_index

#[test]
fn start_is_constant_beyond_index() {
    let base = Store::fixture();
    let a = base.start(AM, "coordinator", &["hardware"]);
    assert!(a.text.contains("\n## Index\n"));
    for n in [10usize, 500, 5000] {
        let mut grown = Store::fixture();
        grown.extras(n);
        let b = recall::recall(
            &grown.mems,
            REV + n as i64,
            MIRROR,
            &req(AM, "coordinator", &["hardware"], ""),
        );
        assert!(b.text.contains("\n## Index\n"));
        assert_eq!(
            above_index(&a.text),
            above_index(&b.text),
            "start view above the index changed with {n} extra memories"
        );
        assert_eq!(a.pinned, b.pinned);
        assert_eq!(a.returned, b.returned);
        assert!(b.text.starts_with(&format!(
            "# Shared agent memory — revision {}\n",
            REV + n as i64
        )));
    }
}

// ---------------------------------------------------------------- 3. index_bounded_and_named

#[test]
fn index_bounded_and_named() {
    // small store: everything fits, so the index lists every other memory and has no count line
    let small = Store::fixture();
    let r = small.start(AM, "", &[]);
    assert_accounted(&small, AM, &r.text);
    let ix = index(&r.text);
    assert!(
        ix.counts.is_empty() && ix.collapsed.is_none(),
        "nothing needs counting in a 32-memory store"
    );
    assert!(
        !r.text.contains("/custom-lora-mesh/") && !r.text.contains("/burmese-reader/"),
        "another project appears"
    );

    // large store: bounded by START_MAX, exact counts, other projects never appear
    let mut big = Store::fixture();
    big.extras(2000);
    let cases: [(&str, &str, &[&str]); 3] = [
        (AM, "", &[]),
        ("", "coordinator", &["hardware"]),
        (LORA, "", &["ui"]),
    ];
    for (project, role, topics) in cases {
        let r = big.start(project, role, topics);
        assert_accounted(&big, project, &r.text);
        let ix = index(&r.text);
        assert!(
            ix.collapsed.is_some() || !ix.counts.is_empty(),
            "2,000 extra memories cannot all be listed"
        );
        assert!(ix.counts.iter().all(|(_, n)| *n > 0));
        // Listing stops only when the next line no longer fits. One index line is at most ~130
        // characters and the count lines it would shrink or replace at most ~300 (measured on
        // this fixture), so a view far below START_MAX listed too little.
        assert!(
            r.text.chars().count() > START_MAX - 600,
            "index stops early: the view has only {} characters",
            r.text.chars().count()
        );
        for other in ["/projects/home/sai/Projects/extra-", "/burmese-reader/"] {
            assert!(
                !r.text.contains(other),
                "another project's memory appears: {other}"
            );
        }
        if project != AM {
            assert!(
                !r.text.contains("/agent-memory/"),
                "another project's memory appears for {project:?}"
            );
        }
        if project.is_empty() {
            assert!(
                !r.text.contains("/projects/"),
                "with no project no project memory applies"
            );
        }
    }

    // this project's overflow comes first in the index (assert_accounted checks the full order)
    let mut s = Store::fixture();
    for i in 0..8 {
        s.add(
            &format!("/projects/home/sai/Projects/agent-memory/wide-{i}.md"),
            &format!("Wide note {i}"),
            &"y".repeat(900),
            json!({"priority": 50 - i}),
        );
    }
    let r = s.start(AM, "", &[]);
    assert_accounted(&s, AM, &r.text);
    assert!(
        index(&r.text).listed[0].contains("/agent-memory/wide-"),
        "overflowing project memory is listed first"
    );
}

// ---------------------------------------------------------------- 13. start_within_hook_cap

/// A title of exactly `n` characters.
fn title_of(n: usize, tag: &str) -> String {
    format!("{tag} {}", "t".repeat(n - tag.len() - 1))
}

/// Pinned blocks totalling exactly 4,000 characters (the pinned budget), as 4 large memories
/// or as 60 tiny ones with long-ish titles; `tiny` selects the second shape.
fn hook_cap_store(tiny: bool) -> Store {
    let mut s = Store::new();
    if tiny {
        // 60 blocks of 66 characters, 40 of them stretched by one title character
        for i in 0..60 {
            let title = title_of(if i < 40 { 39 } else { 38 }, &format!("Pin {i:02}"));
            s.add(
                &format!("/global/pin-{i:02}.md"),
                &title,
                "x",
                json!({"pinned": true, "priority": 100 - i}),
            );
        }
    } else {
        for i in 0..4 {
            let (title, path) = (
                title_of(120, &format!("Core rule {i}")),
                format!("/global/core-{i}.md"),
            );
            let body = body_for_block(1000, &title, &path);
            s.add(
                &path,
                &title,
                &body,
                json!({"pinned": true, "priority": 10 - i}),
            );
        }
    }
    // project memories filling START_BODY_BUDGET: 3 blocks of 990 characters
    for i in 0..3 {
        let (title, path) = (
            title_of(120, &format!("Project rule {i}")),
            format!("/projects/work/app/proj-{i}.md"),
        );
        let body = body_for_block(990, &title, &path);
        s.add(&path, &title, &body, json!({"priority": 9 - i}));
    }
    // a requested memory that no longer fits
    s.add(
        "/topics/hardware/bench.md",
        &title_of(120, "Bench"),
        &"b".repeat(1000),
        json!({}),
    );
    s.extras(5000);
    s
}

#[test]
fn start_within_hook_cap() {
    // a mirror path of 130 characters makes the header and the collapse line long
    let mirror = format!("/srv/{}/mnt/shared-agent-memory", "m".repeat(100));
    for tiny in [false, true] {
        let s = hook_cap_store(tiny);
        let pinned: Vec<&Item> = s.items.iter().filter(|i| i.pinned).collect();
        assert_eq!(pinned.len(), if tiny { 60 } else { 4 });
        assert_eq!(
            pinned.iter().map(|i| block_len(i)).sum::<usize>(),
            4000,
            "pinned blocks must total the pinned budget"
        );
        for (project, topics) in [
            ("/work/app", &["hardware"][..]),
            ("/work/app/sub", &[][..]),
            ("", &["hardware"][..]),
            ("/nowhere", &[][..]),
        ] {
            let r = recall::recall(&s.mems, REV, &mirror, &req(project, "", topics, ""));
            let n = r.text.chars().count();
            assert!(
                n <= START_MAX,
                "start view has {n} characters (project {project:?}, tiny {tiny})"
            );
            assert_one_newline(&r.text);
            for i in &pinned {
                assert!(
                    r.text.contains(&block(i)),
                    "pinned {} cut (project {project:?}, tiny {tiny})",
                    i.path
                );
            }
            assert_eq!(r.pinned.len(), pinned.len());
            if project.starts_with("/work/app") {
                assert_eq!(
                    section(&r.text, "This project").len(),
                    3,
                    "3 x 990 fit in 3,000"
                );
            }
            let ix = index(&r.text);
            if let Some(n) = ix.collapsed {
                assert!(n > 0 && r.text.contains(&format!("full index: {mirror}/INDEX.md")));
            }
            assert!(
                ix.collapsed.is_some() || !ix.counts.is_empty(),
                "5,000 extra memories cannot all be listed"
            );
            // whatever is listed is the start of the index order, and the counts are exact
            let want: Vec<&Item> = s.in_scope(project);
            let full = set(headings(&r.text));
            let mut order: Vec<&Item> = want
                .into_iter()
                .filter(|i| !full.contains(&i.path))
                .collect();
            order.sort_by_key(|i| (index_rank(i), i.sort_key()));
            let k = ix.listed.len();
            assert_eq!(
                ix.listed,
                order[..k]
                    .iter()
                    .map(|i| i.path.clone())
                    .collect::<Vec<_>>()
            );
            let counted: usize =
                ix.collapsed.unwrap_or(0) + ix.counts.iter().map(|(_, n)| n).sum::<usize>();
            assert_eq!(k + counted, order.len(), "memories are dropped or invented");
        }

        // index_md is the full index, with no cap and no count lines
        let md = recall::index_md(&s.mems, REV, &mirror);
        assert!(
            md.chars().count() > START_MAX,
            "index_md was cut to {} characters",
            md.chars().count()
        );
        assert!(!md.contains("truncated"));
        assert_index_md(&s, &md);
    }
}

/// INDEX.md (architect ruling, doc PR 12): the header, `## Always apply` with the pinned
/// *global* memories in full, then `## Index` with one line for every other live memory:
/// global, then every project's (pinned project memories included), topics, roles; each group
/// by priority then slug. No cap, no count lines.
fn assert_index_md(store: &Store, md: &str) {
    assert_one_newline(md);
    let pinned_global: Vec<&Item> = {
        let mut p: Vec<&Item> = store
            .items
            .iter()
            .filter(|i| i.live() && i.pinned && i.path.starts_with("/global/"))
            .collect();
        p.sort_by_key(|i| i.sort_key());
        p
    };
    for i in &pinned_global {
        assert!(md.contains(&block(i)), "pinned {} not in full", i.path);
    }
    assert_eq!(
        section(md, "Always apply"),
        pinned_global
            .iter()
            .map(|i| i.path.clone())
            .collect::<Vec<_>>(),
        "Always apply holds the pinned global memories, in priority order"
    );
    assert_eq!(
        headings(md).len(),
        pinned_global.len(),
        "no other memory in full"
    );
    let mut rest: Vec<&Item> = store
        .items
        .iter()
        .filter(|i| i.live() && !pinned_global.iter().any(|p| p.path == i.path))
        .collect();
    let rank = |i: &Item| match i.path.split('/').nth(1).unwrap() {
        "global" => 0,
        "projects" => 1,
        "topics" => 2,
        _ => 3,
    };
    rest.sort_by_key(|i| (rank(i), i.sort_key()));
    let ix = index(md);
    assert!(
        ix.counts.is_empty() && ix.collapsed.is_none(),
        "index_md has no count lines"
    );
    assert_eq!(
        ix.listed,
        rest.iter().map(|i| i.path.clone()).collect::<Vec<_>>(),
        "one line for every other memory, in index order"
    );
    for i in &rest {
        assert!(
            md.contains(&format!("- {} — {}\n", i.path, i.title)),
            "index line of {}",
            i.path
        );
    }
}

#[test]
fn budget_equal_to_remaining_fits() {
    // start: two blocks of exactly 1,500 fill START_BODY_BUDGET (<=), a third does not fit
    let mut s = Store::new();
    for i in 0..3 {
        let (title, path) = (
            format!("Edge note {i}"),
            format!("/projects/work/app/e-{i}.md"),
        );
        let body = body_for_block(1500, &title, &path);
        s.add(&path, &title, &body, json!({"priority": 5 - i}));
    }
    let r = s.start("/work/app", "", &[]);
    assert_eq!(
        section(&r.text, "This project"),
        ["/projects/work/app/e-0.md", "/projects/work/app/e-1.md"]
    );
    // task: four blocks of exactly 1,500 fill TASK_BODY_BUDGET, the fifth moves to More matches
    let mut t = Store::new();
    for i in 0..5 {
        let (title, path) = (format!("Zebra edge {i}"), format!("/global/zebra-{i}.md"));
        let body = body_for_block(1500, &title, &path);
        t.add(&path, &title, &body, json!({"priority": 9 - i}));
    }
    for i in 0..10 {
        t.add(
            &format!("/global/filler-{i}.md"),
            &format!("Filler note {i}"),
            "Plain filler.",
            json!({}),
        );
    }
    let (shown, listed, _) = task_parts(&t.task("zebra").text);
    assert_eq!(shown.len(), 4);
    assert_eq!(listed, ["/global/zebra-4.md"]);
}

#[test]
fn budgets_count_the_heading() {
    // start view: two bodies of 1,480 fit in 3,000 (2,960) but their blocks (~1,530 each) do not
    let mut s = Store::new();
    for i in 0..2 {
        s.add(
            &format!("/projects/work/app/h-{i}.md"),
            &format!("Heading note {i}"),
            &"h".repeat(1480),
            json!({"priority": 5 - i}),
        );
    }
    let r = s.start("/work/app", "", &[]);
    assert_eq!(
        section(&r.text, "This project"),
        ["/projects/work/app/h-0.md"]
    );
    assert!(
        index(&r.text)
            .listed
            .contains(&"/projects/work/app/h-1.md".to_string())
    );
    assert_accounted(&s, "/work/app", &r.text);

    // task view: five bodies of 1,170 (5,850) fit in 6,000 but their blocks (~1,210 each) do not
    let mut t = Store::new();
    for i in 0..5 {
        t.add(
            &format!("/global/zebra-{i}.md"),
            &format!("Zebra rule {i}"),
            &"z".repeat(1170),
            json!({"priority": 10 - i}),
        );
    }
    for i in 0..10 {
        t.add(
            &format!("/global/filler-{i}.md"),
            &format!("Filler note {i}"),
            "Plain filler.",
            json!({}),
        );
    }
    let (shown, listed, remainder) = task_parts(&t.task("zebra").text);
    assert_eq!(shown.len(), 4);
    assert_eq!(listed, ["/global/zebra-4.md"]);
    assert_eq!(remainder, None);
}

// ---------------------------------------------------------------- 4. project_and_requested_in_full

#[test]
fn project_and_requested_in_full() {
    let s = Store::fixture();
    let r = s.start(AM, "coordinator", &["hardware", "ui"]);
    let am = "/projects/home/sai/Projects/agent-memory";
    // "This project": the unpinned project memories, ordered by priority then slug
    assert_eq!(
        section(&r.text, "This project"),
        [
            format!("{am}/sqlite-journal-delete-mode.md"),
            format!("{am}/service-port-8787.md"),
            format!("{am}/mirror-is-read-only.md")
        ]
    );
    // "Requested": topic and role memories named in the request, same order
    assert_eq!(
        section(&r.text, "Requested"),
        [
            "/roles/coordinator/merge-with-head-match.md",
            "/topics/hardware/pi-usb-port-power.md",
            "/topics/ui/empty-states.md",
            "/roles/coordinator/state-file-checkpoints.md",
            "/topics/ui/dark-mode-contrast.md",
            "/topics/hardware/serial-console-wiring.md",
        ]
    );
    for p in section(&r.text, "This project")
        .iter()
        .chain(section(&r.text, "Requested").iter())
    {
        assert!(r.text.contains(&block(s.item(p))), "{p} not in full");
    }
    // `returned` is every body in the text in output order: Always apply, This project, Requested
    assert_eq!(r.returned, headings(&r.text));
    assert_eq!(r.returned.len(), 4 + 3 + 6);
    assert_eq!(r.returned[..4], r.pinned[..]);
    assert_accounted(&s, AM, &r.text);
    // a topic that is not requested is only in the index
    assert!(
        !r.text
            .contains(&block(s.item("/topics/media/render-loudness-target.md")))
    );
    assert!(
        index(&r.text)
            .listed
            .contains(&"/topics/media/render-loudness-target.md".to_string())
    );
    // unknown topics and roles are ignored
    let r2 = s.start(AM, "nobody", &["nonexistent"]);
    assert!(section(&r2.text, "Requested").is_empty());
    assert_accounted(&s, AM, &r2.text);

    // overflow beyond BODY_BUDGET becomes index lines, never silent drops
    let mut o = Store::new();
    o.add(
        "/global/rule.md",
        "The one rule",
        "Pinned rule.",
        json!({"pinned": true}),
    );
    for i in 0..30 {
        o.add(
            &format!("/projects/work/app/note-{i:02}.md"),
            &format!("Project note {i}"),
            &"p".repeat(1400),
            json!({"priority": 100 - i}),
        );
    }
    o.add(
        "/topics/hardware/bench.md",
        "Bench notes",
        &"t".repeat(800),
        json!({"priority": 1}),
    );
    let r = o.start("/work/app", "", &["hardware"]);
    let shown = section(&r.text, "This project");
    assert_eq!(
        shown,
        (0..2)
            .map(|i| format!("/projects/work/app/note-{i:02}.md"))
            .collect::<Vec<_>>(),
        "2 x 1400 fit in 3000, the 3rd does not"
    );
    assert!(shown.iter().map(|p| block_len(o.item(p))).sum::<usize>() <= START_BODY_BUDGET);
    assert!(
        section(&r.text, "Requested").is_empty(),
        "800 more characters do not fit in the 200 left"
    );
    assert_accounted(&o, "/work/app", &r.text);
    let listed = index(&r.text).listed;
    assert_eq!(
        &listed[..28],
        &(2..30)
            .map(|i| format!("/projects/work/app/note-{i:02}.md"))
            .collect::<Vec<_>>()[..],
        "overflow comes first in the index, in order"
    );
    assert!(
        listed.contains(&"/topics/hardware/bench.md".to_string()),
        "the requested memory that did not fit is named in the index"
    );
    assert_eq!(r.pinned, ["/global/rule.md"]);
}

#[test]
fn project_matches_by_whole_components() {
    let mut s = Store::new();
    s.add(
        "/projects/work/app/rule.md",
        "App rule",
        "Applies below the app root.",
        json!({}),
    );
    let cases = [
        ("/work/app", true),
        ("/work/app/service", true),
        ("/work/app/", true),
        ("/work/other/../app/sub", true),
        ("/work/app/service/..", true),
        ("/work/application", false),
        ("/work/app/..", false),
        ("/work", false),
        ("/other/work/app", false),
        ("work/app", false), // not absolute: no project
        ("./work/app", false),
        ("", false),
    ];
    for (project, applies) in cases {
        let r = s.start(project, "", &[]);
        let headed = section(&r.text, "This project");
        assert_eq!(!headed.is_empty(), applies, "project {project:?}");
        if !applies {
            assert!(
                !r.text.contains("/projects/work/app/rule.md"),
                "project {project:?} sees another project's memory"
            );
        }
    }
}

#[test]
fn budgets_count_characters_not_bytes() {
    // Burmese letters take 3 bytes each: 2 x 1,400 characters fit in 3,000, but 2 x 4,200 bytes do not
    let burmese = |n: usize| "မြန်မာ".chars().cycle().take(n).collect::<String>();
    let mut s = Store::new();
    for i in 0..3 {
        s.add(
            &format!("/projects/work/app/my-{i}.md"),
            &format!("မြန်မာ မှတ်စု {i}"),
            &burmese(1400),
            json!({"priority": 10 - i}),
        );
    }
    let r = s.start("/work/app", "", &[]);
    assert_eq!(
        section(&r.text, "This project"),
        ["/projects/work/app/my-0.md", "/projects/work/app/my-1.md"]
    );
    assert!(
        index(&r.text)
            .listed
            .contains(&"/projects/work/app/my-2.md".to_string())
    );
    assert_accounted(&s, "/work/app", &r.text);
}

// ---------------------------------------------------------------- 5-7. query rules on the fixture

#[test]
fn unrelated_query_returns_nothing() {
    let s = Store::fixture();
    let r = s.task("kubernetes ingress yaml");
    assert_eq!(r.text, no_match("kubernetes ingress yaml"));
    assert_eq!(r.view, "task");
    assert!(r.returned.is_empty() && r.pinned.is_empty());
    let mut big = Store::fixture();
    big.extras(2000);
    for project in ["", AM, LORA] {
        let r = big.recall(
            project,
            "coordinator",
            &["hardware"],
            "kubernetes ingress yaml",
        );
        assert_eq!(r.text, no_match("kubernetes ingress yaml"));
    }
}

/// Fixture queries with the memories each must return (exactly, best match first).
const QUERIES: [(&str, &[&str]); 5] = [
    (
        "raspberry usb power",
        &["/topics/hardware/pi-usb-port-power.md"],
    ),
    ("small commits", &["/global/small-commits.md"]),
    (
        "burmese font rendering",
        &[
            "/topics/burmese/font-fallback-order.md",
            "/global/burmese-reply-script.md",
        ],
    ),
    (
        "mirror directory permissions",
        &["/projects/home/sai/Projects/agent-memory/mirror-is-read-only.md"],
    ),
    (
        "merge approved",
        &["/roles/coordinator/merge-with-head-match.md"],
    ),
];

fn tokens(s: &str) -> BTreeSet<String> {
    Regex::new(r"\w+")
        .unwrap()
        .find_iter(&s.to_lowercase())
        .map(|m| m.as_str().to_string())
        .collect()
}

/// The match rule of §3.3 step 3: one term in title, tags or scope, or two distinct terms in the body.
fn related(i: &Item, terms: &BTreeSet<String>) -> bool {
    let head = tokens(&format!(
        "{} {} {}",
        i.title,
        i.tags.join(" "),
        i.scope_value()
    ));
    let body = tokens(&i.body);
    terms.iter().any(|t| head.contains(t))
        || terms.iter().filter(|t| body.contains(*t)).count() >= 2
}

#[test]
fn no_unrelated_bodies() {
    let s = Store::fixture();
    for (query, want) in QUERIES {
        let r = s.task(query);
        let terms = tokens(query); // the queries hold only content words
        assert_eq!(r.view, "task");
        assert_one_newline(&r.text);
        assert!(
            r.pinned.is_empty(),
            "the task view carries no pinned memories"
        );
        assert_eq!(r.returned, want, "query {query:?}");
        for i in &s.items {
            let shown = r.text.contains(&i.body);
            if r.returned.contains(&i.path) {
                assert!(shown, "{} missing for {query:?}", i.path);
                assert!(
                    related(i, &terms),
                    "{} returned for {query:?} but does not match",
                    i.path
                );
            } else {
                assert!(!shown, "unrelated body of {} appears for {query:?}", i.path);
            }
        }
    }
}

#[test]
fn common_and_stop_words_ignored() {
    // "memory" and "agent" are in most fixture memories, so the frequency filter drops them
    let s = Store::fixture();
    for q in [
        "the memory agent",
        "memory agent",
        "the and for with that this from",
    ] {
        assert_eq!(s.task(q).text, no_match(q), "query {q:?}");
    }
    // stop words and short words never match, even when a title and body are made of them
    let mut c = Store::new();
    c.add(
        "/global/words.md",
        "The rules for this team and that project",
        "The rules are for this and that, from you with the rest. Go is it ok to do so.",
        json!({}),
    );
    for i in 0..6 {
        c.add(
            &format!("/global/filler-{i}.md"),
            &format!("Plain filler number {i}"),
            &format!("Nothing to see in filler {i}."),
            json!({}),
        );
    }
    for q in [
        "the and for with that this from",
        "the rules",
        "go is it ok",
        "to do so",
    ] {
        let r = c.task(q);
        if q == "the rules" {
            // only "rules" counts, once, in the title
            assert_eq!(r.returned, ["/global/words.md"]);
        } else {
            assert_eq!(r.text, no_match(q), "query {q:?}");
        }
    }
    // a query is matched case-insensitively
    assert_eq!(c.task("RULES").returned, ["/global/words.md"]);
}

#[test]
fn frequency_filter_boundary() {
    // With fewer than 4 candidates nothing is filtered.
    let mut three = Store::new();
    for i in 0..3 {
        three.add(
            &format!("/global/z-{i}.md"),
            &format!("Zebra stripes {i}"),
            "Plain body text here.",
            json!({}),
        );
    }
    assert_eq!(three.task("zebra").returned.len(), 3);
    // 4 candidates: a term in half of them (2) still counts, in more than half (3) it does not.
    let build = |with: usize| {
        let mut s = Store::new();
        for i in 0..4 {
            let title = if i < with {
                format!("Zebra stripes {i}")
            } else {
                format!("Plain title {i}")
            };
            s.add(
                &format!("/global/z-{i}.md"),
                &title,
                "Plain body text here.",
                json!({}),
            );
        }
        s
    };
    assert_eq!(build(2).task("zebra").returned.len(), 2);
    assert_eq!(build(3).task("zebra").text, no_match("zebra"));
}

// ---------------------------------------------------------------- 8. match_rule

#[test]
fn match_rule() {
    let mut s = Store::new();
    s.add(
        "/global/a.md",
        "Quokka habitat notes",
        "Unrelated filler text about weather.",
        json!({}),
    );
    s.add(
        "/global/b.md",
        "Weather notes one",
        "The numbat wanders. quokka quokka quokka quokka",
        json!({}),
    );
    s.add(
        "/global/c.md",
        "Weather notes two",
        "A wombat meets a quokka near the river.",
        json!({}),
    );
    s.add(
        "/global/d.md",
        "Weather notes three",
        "Numbat only here, once.",
        json!({}),
    );
    s.add(
        "/global/e.md",
        "Weather notes four",
        "Tagged entry body.",
        json!({"tags": ["wombat"]}),
    );
    s.add(
        "/topics/capybara/f.md",
        "Plain topic entry",
        "Nothing special in this body.",
        json!({}),
    );
    for i in 0..6 {
        s.add(
            &format!("/global/filler-{i}.md"),
            &format!("Filler note {i}"),
            &format!("Plain filler {i}."),
            json!({}),
        );
    }
    let ids = |q: &str| {
        s.task(q)
            .returned
            .iter()
            .map(|p| p.rsplit('/').next().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    // one term in the title matches; the same word repeated in a body is still one term
    assert_eq!(ids("quokka"), ["a.md"]);
    // one body-only term does not match
    assert_eq!(s.task("numbat").text, no_match("numbat"));
    // two distinct body terms match
    assert_eq!(ids("numbat quokka"), ["a.md", "b.md"]);
    assert_eq!(
        ids("quokka wombat"),
        ["a.md", "e.md", "c.md"],
        "title (score 3) and tag (3) before the two body terms (2)"
    );
    // a tag and the scope value count like a title
    assert_eq!(ids("wombat"), ["e.md"]);
    assert_eq!(ids("capybara"), ["f.md"]);
}

// ---------------------------------------------------------------- 9. task_bounded

fn bounded_store(matches: usize, filler: usize) -> Store {
    let mut s = Store::new();
    for i in 0..matches {
        s.add(
            &format!("/global/zebra-{i:03}.md"),
            &format!("Zebra rule {i}"),
            &format!("Body {i} {}", "z".repeat(1000)),
            json!({"priority": i}),
        );
    }
    for i in 0..filler {
        s.add(
            &format!("/topics/misc-{}/filler-{i:03}.md", i % 10),
            &format!("Filler entry {i}"),
            &format!("Plain filler body {i}."),
            json!({}),
        );
    }
    s
}

/// The bodies shown in full and the "More matches" lines (paths) of a task view.
fn task_parts(text: &str) -> (Vec<String>, Vec<String>, Option<String>) {
    assert_one_newline(text);
    let (head, more) = match text.split_once("\n## More matches\n") {
        Some((h, m)) => (h, Some(m)),
        None => (text, None),
    };
    let lines: Vec<&str> = more
        .map(|m| m.lines().filter(|l| !l.is_empty()).collect())
        .unwrap_or_default();
    let listed = lines
        .iter()
        .filter(|l| l.starts_with("- /"))
        .map(|l| l[2..].split(' ').next().unwrap().to_string())
        .collect();
    let remainder = lines
        .iter()
        .find(|l| l.starts_with('+'))
        .map(|l| l.to_string());
    if let Some(r) = &remainder {
        assert_eq!(
            lines.last().copied(),
            Some(r.as_str()),
            "the remainder line comes last"
        );
    }
    assert_eq!(
        lines.len(),
        lines.iter().filter(|l| l.starts_with("- /")).count() + usize::from(remainder.is_some()),
        "unexpected line in More matches"
    );
    (headings(head), listed, remainder)
}

#[test]
fn task_bounded() {
    let s = bounded_store(300, 400);
    let r = s.task("zebra");
    assert_eq!(r.view, "task");
    assert!(
        r.text
            .starts_with("# Memories matching \"zebra\" — revision 42\n\n### ")
    );
    let (shown, listed, remainder) = task_parts(&r.text);
    assert_eq!(shown.len(), 5);
    assert_eq!(r.returned, shown);
    assert_eq!(
        shown,
        (295..300)
            .rev()
            .map(|i| format!("/global/zebra-{i:03}.md"))
            .collect::<Vec<_>>(),
        "equal scores sort by priority"
    );
    let body_chars: usize = shown.iter().map(|p| block_len(s.item(p))).sum();
    assert!(
        body_chars <= TASK_BODY_BUDGET,
        "{body_chars} body characters"
    );
    assert_eq!(listed.len(), 20, "at most 20 More matches lines");
    assert_eq!(
        listed,
        (275..295)
            .rev()
            .map(|i| format!("/global/zebra-{i:03}.md"))
            .collect::<Vec<_>>()
    );
    assert!(
        r.text
            .contains("\n- /global/zebra-294.md — Zebra rule 294\n")
    );
    assert_eq!(
        remainder.as_deref(),
        Some("+275 more"),
        "300 matches - 5 shown - 20 listed"
    );

    // 12 matches: 5 in full, the other 7 listed, no remainder line
    let (shown, listed, remainder) = task_parts(&bounded_store(12, 30).task("zebra").text);
    assert_eq!((shown.len(), listed.len(), remainder), (5, 7, None));
    // 5 matches: no More matches section
    let r = bounded_store(5, 10).task("zebra");
    let (shown, listed, remainder) = task_parts(&r.text);
    assert_eq!((shown.len(), listed.len(), remainder), (5, 0, None));
    assert!(!r.text.contains("More matches"));
    // 26 matches: 5 + 20 + "+1 more"
    let (_, listed, remainder) = task_parts(&bounded_store(26, 40).task("zebra").text);
    assert_eq!((listed.len(), remainder.as_deref()), (20, Some("+1 more")));
}

#[test]
fn task_body_budget_first_fit() {
    // five matches of 1,400 characters: 4 fit in 6,000, the 5th moves to More matches
    let mut s = Store::new();
    for i in 0..5 {
        s.add(
            &format!("/global/zebra-{i}.md"),
            &format!("Zebra rule {i}"),
            &"z".repeat(1400),
            json!({"priority": 10 - i}),
        );
    }
    for i in 0..10 {
        s.add(
            &format!("/global/filler-{i}.md"),
            &format!("Filler note {i}"),
            "Plain filler.",
            json!({}),
        );
    }
    let (shown, listed, remainder) = task_parts(&s.task("zebra").text);
    assert_eq!(shown.len(), 4);
    assert_eq!(listed, ["/global/zebra-4.md"]);
    assert_eq!(remainder, None);
}

// ---------------------------------------------------------------- 10-11

#[test]
fn task_finds_topic_without_flag() {
    let s = Store::fixture();
    // no topic or role is requested
    let r = s.recall(AM, "", &[], "raspberry usb power");
    assert_eq!(r.returned, ["/topics/hardware/pi-usb-port-power.md"]);
    assert!(
        r.text
            .contains(&block(s.item("/topics/hardware/pi-usb-port-power.md")))
    );
    assert_eq!(
        s.recall("", "", &[], "raspberry usb power").returned,
        r.returned,
        "no project also finds it"
    );
}

#[test]
fn exclusions() {
    let s = Store::fixture();
    let hidden = [
        "/global/maybe-use-tabs.md",
        "/global/expired-freeze-notice.md",
        "/topics/media/draft-codec-choice.md",
    ];
    assert!(hidden.iter().all(|p| !s.item(p).live()));
    assert!(
        s.item("/global/review-date-2099.md").live(),
        "a future expiry is still active"
    );
    // never in the start view, not even as index lines
    let cases: [(&str, &str, &[&str]); 3] = [
        ("", "", &[]),
        (AM, "coordinator", &["hardware", "media"]),
        (LORA, "", &["media"]),
    ];
    for (project, role, topics) in cases {
        let r = s.start(project, role, topics);
        for p in hidden {
            assert!(!r.text.contains(p), "{p} appears in the start view");
            assert!(
                !r.text.contains(&s.item(p).title),
                "the title of {p} appears"
            );
        }
        assert!(
            r.text.contains("/global/review-date-2099.md"),
            "the not-yet-expired memory is present"
        );
        assert_accounted(&s, project, &r.text);
    }
    // never in the task view, even when the query names them
    for q in [
        "maybe indent tabs",
        "winter january",
        "draft codec archived",
    ] {
        assert_eq!(s.task(q).text, no_match(q), "query {q:?}");
    }
    // pinned memories never appear in the task view; the start view already carries them
    for q in [
        "simplicity",
        "outward irreversible",
        "reporting verification",
        "storage",
    ] {
        assert_eq!(s.task(q).text, no_match(q), "a pinned memory matched {q:?}");
    }
    let r = s.task("storage tests merge");
    assert_eq!(r.returned, ["/roles/coordinator/merge-with-head-match.md"]);
    assert!(r.pinned.is_empty());
    for i in s.items.iter().filter(|i| i.pinned) {
        assert!(
            !r.text.contains(&i.path) && !r.text.contains(&i.body),
            "pinned {} in the task view",
            i.path
        );
    }
}

// ---------------------------------------------------------------- 12. golden_texts

fn golden(name: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/recall")
        .join(name);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

#[test]
fn golden_texts() {
    let s = Store::fixture();
    let cases: [(&str, Recalled); 4] = [
        ("start_none.txt", s.start("", "", &[])),
        (
            "start_project.txt",
            s.start(AM, "coordinator", &["hardware", "burmese"]),
        ),
        ("task_usb.txt", s.task("raspberry usb power")),
        ("task_burmese.txt", s.task("burmese font rendering")),
    ];
    for (file, got) in cases {
        let want = golden(file);
        assert_eq!(got.text, want, "output differs from fixtures/recall/{file}");
    }
}

#[test]
fn index_md_is_the_full_index() {
    let s = Store::fixture();
    let md = recall::index_md(&s.mems, REV, MIRROR);
    assert_index_md(&s, &md);
    assert_eq!(md, golden("index_md.txt"));
    // it is not the no-project start view: it lists every project's memories
    let start = s.start("", "", &[]).text;
    assert_ne!(md, start);
    assert_eq!(start, golden("start_none.txt"));
    assert!(!start.contains("/projects/") && md.contains("/projects/"));
    // the pinned project memory is a line in the index, not in full
    assert!(md.contains("- /projects/home/sai/Projects/agent-memory/run-tests-before-merge.md — "));
}
