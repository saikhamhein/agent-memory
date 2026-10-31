//! Acceptance tests for recall (design §4.3 "R").

use agent_memory_server::model::{self, Memory};
use agent_memory_server::recall::{
    self, BODY_BUDGET, INDEX_BUDGET, MORE_MATCHES, Request, SEARCH_RESULTS, STOPWORDS,
};
use std::{collections::BTreeSet, fs, path::Path};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/recall");
const MIRROR: &str = "/srv/demo/mnt/shared-agent-memory";
const PROJECT: &str = "/home/demo/work/notes-app";
const OTHER_PROJECTS: [&str; 3] = [
    "/projects/home/demo/archive/",
    "/projects/home/demo/work/radio-mesh/",
    "/projects/home/demo/work/notes/",
];

fn fixture() -> Vec<Memory> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<Memory>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                let logical = format!("/{}", path.strip_prefix(root).unwrap().display());
                let content = fs::read_to_string(&path).unwrap();
                out.push(model::parse(&logical, &content).unwrap());
            }
        }
    }
    let root = Path::new(FIXTURES).join("store");
    let mut out = Vec::new();
    walk(&root, &root, &mut out);
    out.sort_by(|a, b| a.path.cmp(&b.path));
    assert!(out.len() >= 30, "fixture has {} memories", out.len());
    out
}

fn memory(path: &str, title: &str, body: &str, extra: &str) -> Memory {
    let content = format!(
        "---\n{{\"confidence\": \"explicit\", \"evidence\": [{{\"source\": \"test\", \"locator\": \"recall_v2\", \"note\": \"generated\"}}], \"tags\": [], \"title\": {}{extra}}}\n---\n{body}\n",
        serde_json::to_string(title).unwrap()
    );
    model::parse(path, &content).unwrap()
}

/// `n` filler memories spread over scopes a notes-app session never requests.
fn others(n: usize) -> Vec<Memory> {
    (0..n)
        .map(|i| {
            let path = match i % 4 {
                0 => format!("/global/filler-{i}.md"),
                1 => format!("/topics/extra/filler-{i}.md"),
                2 => format!("/roles/archivist/filler-{i}.md"),
                _ => format!("/projects/home/demo/archive/filler-{i}.md"),
            };
            memory(
                &path,
                &format!("Filler note number {i}"),
                "Filler body about nothing in particular.",
                "",
            )
        })
        .collect()
}

fn request(project: &str, role: &str, topics: &[&str], query: &str) -> Request {
    Request {
        project: project.into(),
        role: role.into(),
        topics: topics.iter().map(|t| t.to_string()).collect(),
        query: query.into(),
    }
}

fn run(memories: &[Memory], r: &Request) -> recall::Recalled {
    recall::recall(memories, 42, MIRROR, r)
}

fn title(m: &Memory) -> &str {
    m.meta["title"].as_str().unwrap()
}

fn block(m: &Memory) -> String {
    format!("### {} [{}]\n{}\n", title(m), m.path, m.body.trim_end())
}

fn above_index(text: &str) -> &str {
    text.split("## Index\n").next().unwrap()
}

fn index_part(text: &str) -> &str {
    text.split_once("## Index\n").map_or("", |(_, i)| i)
}

fn pinned_in_scope<'a>(memories: &'a [Memory], project: &str) -> Vec<&'a Memory> {
    memories
        .iter()
        .filter(|m| m.meta["pinned"] == true)
        .filter(|m| {
            m.path.starts_with("/global/")
                || (!project.is_empty()
                    && Path::new(project).starts_with(&m.scope.value)
                    && m.path.starts_with("/projects/"))
        })
        .collect()
}

fn excluded(m: &Memory) -> bool {
    m.meta["confidence"] == "tentative" || m.meta["expires"] == "2020-01-31"
}

fn requests() -> Vec<Request> {
    let mut out = Vec::new();
    for project in [
        "",
        PROJECT,
        "/home/demo/work/notes-app/web/src",
        "/home/demo/work/radio-mesh",
    ] {
        for role in ["", "coordinator"] {
            for topics in [&[][..], &["hardware"], &["hardware", "burmese", "ui"]] {
                out.push(request(project, role, topics, ""));
            }
        }
    }
    out
}

#[test]
fn core_never_cut() {
    let base = fixture();
    let mut extra = base.clone();
    extra.extend(others(5000));
    let mut large = base.clone();
    large.extend((0..30).map(|i| {
        memory(
            &format!("/projects/home/demo/work/notes-app/large-{i:02}.md"),
            &format!("Large project note {i}"),
            &"Long project detail. ".repeat(60),
            "",
        )
    }));
    assert!(
        pinned_in_scope(&base, "")
            .iter()
            .any(|m| m.path == "/global/keep-it-simple.md")
    );
    for memories in [&base, &extra, &large] {
        for r in requests() {
            let out = run(memories, &r);
            let pinned = pinned_in_scope(memories, &r.project);
            assert!(!pinned.is_empty());
            for m in pinned {
                assert!(
                    out.text.contains(&block(m)),
                    "{} cut for {:?}",
                    m.path,
                    r.project
                );
                assert!(out.pinned.contains(&m.path));
            }
        }
    }
}

#[test]
fn start_is_constant_beyond_index() {
    let base = fixture();
    for r in requests() {
        let before = run(&base, &r).text;
        for n in [10, 500, 5000] {
            let mut grown = base.clone();
            grown.extend(others(n));
            let after = run(&grown, &r).text;
            assert_eq!(above_index(&before), above_index(&after), "n={n}");
        }
    }
}

#[test]
fn index_bounded_and_named() {
    let mut memories = fixture();
    memories.extend(others(5000));
    for r in requests() {
        let out = run(&memories, &r);
        let index = index_part(&out.text);
        let (listed, counts): (Vec<&str>, Vec<&str>) = index
            .lines()
            .partition(|l| !l.ends_with("search or list the directory"));
        let listed_chars: usize = listed.iter().map(|l| l.chars().count() + 1).sum();
        assert!(listed_chars <= INDEX_BUDGET, "{listed_chars}");
        let unlisted: usize = counts
            .iter()
            .map(|l| l.split(" — ").nth(1).unwrap().split(' ').next().unwrap())
            .map(|n| n.parse::<usize>().unwrap())
            .sum();
        // Every in-scope memory is in full, listed or counted: exactly once.
        let in_scope = memories
            .iter()
            .filter(|m| !excluded(m))
            .filter(|m| {
                !m.path.starts_with("/projects/")
                    || (!r.project.is_empty() && Path::new(&r.project).starts_with(&m.scope.value))
            })
            .count();
        assert_eq!(out.returned.len() + listed.len() + unlisted, in_scope);
        for other in OTHER_PROJECTS {
            if !Path::new(&r.project).starts_with(other.trim_start_matches("/projects")) {
                assert!(
                    !out.text.contains(other),
                    "{other} leaked for {:?}",
                    r.project
                );
            }
        }
    }
}

#[test]
fn project_and_requested_in_full() {
    let mut memories = fixture();
    let r = request(PROJECT, "coordinator", &["hardware"], "");
    let wanted = |m: &&Memory| {
        !excluded(m)
            && m.meta["pinned"] != true
            && (m.path.starts_with("/projects/home/demo/work/notes-app/")
                && !m.path.contains("/web/")
                || m.path.starts_with("/topics/hardware/")
                || m.path.starts_with("/roles/coordinator/"))
    };
    let out = run(&memories, &r);
    let expected: Vec<_> = memories.iter().filter(wanted).collect();
    assert_eq!(expected.len(), 9);
    for m in &expected {
        assert!(out.text.contains(&block(m)), "{} not in full", m.path);
    }
    assert!(above_index(&out.text).contains("## This project\n"));
    assert!(above_index(&out.text).contains("## Requested\n"));

    // Overflow beyond BODY_BUDGET becomes index lines, never silent drops.
    memories.extend((0..30).map(|i| {
        memory(
            &format!("/projects/home/demo/work/notes-app/large-{i:02}.md"),
            &format!("Large project note {i}"),
            &"Long project detail. ".repeat(60),
            "",
        )
    }));
    let out = run(&memories, &r);
    let full_chars: usize = memories
        .iter()
        .filter(|m| out.returned.contains(&m.path) && m.meta["pinned"] != true)
        .map(|m| m.body.trim_end().chars().count())
        .sum();
    assert!(full_chars <= BODY_BUDGET);
    for m in memories.iter().filter(wanted) {
        let line = format!("- {} — {}\n", m.path, title(m));
        assert!(
            out.text.contains(&block(m)) ^ index_part(&out.text).contains(&line),
            "{} dropped or doubled",
            m.path
        );
    }
}

#[test]
fn unrelated_query_returns_nothing() {
    let out = run(
        &fixture(),
        &request(PROJECT, "", &[], "kubernetes ingress yaml"),
    );
    assert_eq!(out.text, "No memory matches \"kubernetes ingress yaml\".");
    assert_eq!(out.view, "task");
    assert!(out.returned.is_empty());
}

fn terms(text: &str) -> BTreeSet<String> {
    regex::Regex::new(r"\w+")
        .unwrap()
        .find_iter(&text.to_lowercase())
        .map(|w| w.as_str().to_owned())
        .filter(|w| w.chars().count() >= 3 && !STOPWORDS.contains(&w.as_str()))
        .collect()
}

#[test]
fn no_unrelated_bodies() {
    let memories = fixture();
    for query in [
        "raspberry usb power",
        "sqlite schema migration",
        "burmese unicode font",
        "release changelog",
        "merge approval coordinator",
    ] {
        let out = run(&memories, &request(PROJECT, "", &[], query));
        assert!(!out.returned.is_empty(), "{query}");
        let q = terms(query);
        for path in &out.returned {
            let m = memories.iter().find(|m| &m.path == path).unwrap();
            let tags = m.meta["tags"].to_string();
            let head = terms(&format!("{} {tags} {}", title(m), m.scope.value));
            let body = terms(&m.body);
            assert!(
                q.intersection(&head).count() >= 1 || q.intersection(&body).count() >= 2,
                "{query} returned unrelated {path}"
            );
        }
    }
}

#[test]
fn common_and_stop_words_ignored() {
    for project in ["", PROJECT] {
        let out = run(&fixture(), &request(project, "", &[], "the memory agent"));
        assert_eq!(out.text, "No memory matches \"the memory agent\".");
    }
}

#[test]
fn match_rule() {
    let memories = vec![
        memory("/topics/zoo/a.md", "Zebra crossing", "Stripes.", ""),
        memory("/topics/zoo/b.md", "Marsupials", "The quokka smiles.", ""),
        memory(
            "/topics/zoo/c.md",
            "Sea life",
            "A quokka met a narwhal.",
            "",
        ),
    ];
    let paths = |q: &str| run(&memories, &request("", "", &[], q)).returned;
    assert_eq!(paths("zebra"), ["/topics/zoo/a.md"]);
    assert!(paths("quokka").is_empty());
    assert_eq!(paths("quokka narwhal"), ["/topics/zoo/c.md"]);
}

#[test]
fn task_bounded() {
    let mut memories = fixture();
    // 300 matches, kept under half of the candidates so the term still counts.
    memories.extend((0..300).map(|i| {
        memory(
            &format!("/topics/coast/lighthouse-{i:03}.md"),
            &format!("Lighthouse log {i}"),
            &"Beam rotation notes. ".repeat(20),
            "",
        )
    }));
    memories.extend(others(400));
    let out = run(&memories, &request(PROJECT, "", &[], "lighthouse"));
    assert_eq!(out.returned.len(), SEARCH_RESULTS);
    let bodies: usize = out
        .returned
        .iter()
        .map(|p| {
            memories
                .iter()
                .find(|m| &m.path == p)
                .unwrap()
                .body
                .trim_end()
                .chars()
                .count()
        })
        .sum();
    assert!(bodies <= BODY_BUDGET);
    let more = out.text.split_once("## More matches\n").unwrap().1;
    let lines = more.lines().filter(|l| l.starts_with("- ")).count();
    assert_eq!(lines, MORE_MATCHES);
    let rest = 300 - SEARCH_RESULTS - MORE_MATCHES;
    assert!(more.ends_with(&format!("+{rest} more\n")), "{more}");
}

#[test]
fn task_finds_topic_without_flag() {
    let out = run(
        &fixture(),
        &request(PROJECT, "", &[], "raspberry usb power"),
    );
    assert_eq!(
        out.returned[0],
        "/topics/hardware/raspberry-pi-usb-power.md"
    );
}

#[test]
fn exclusions() {
    let memories = fixture();
    let hidden = [
        "/global/maybe-prefers-tabs.md",
        "/global/old-deploy-window.md",
    ];
    let mut views: Vec<String> = requests().iter().map(|r| run(&memories, r).text).collect();
    for q in [
        "tabs spaces",
        "deploy tuesday",
        "simple remove design",
        "sync conflicts",
    ] {
        let out = run(&memories, &request(PROJECT, "", &[], q));
        for m in memories.iter().filter(|m| m.meta["pinned"] == true) {
            assert!(
                !out.text.contains(&m.path),
                "pinned {} in task view {q}",
                m.path
            );
        }
        views.push(out.text);
    }
    for text in &views {
        for h in hidden {
            assert!(!text.contains(h), "{h} shown");
        }
    }
    // A future expiry is still in force.
    assert!(views[0].contains("/global/name-branches-by-task.md"));
}

#[test]
fn burmese_stays_one_token() {
    // Unicode \w includes the combining marks, so a Burmese run is one term.
    assert_eq!(
        terms("မြန်မာစာ font"),
        BTreeSet::from(["မြန်မာစာ".into(), "font".into()])
    );
    let out = run(&fixture(), &request("", "", &[], "မြန်မာစာ unicode"));
    assert_eq!(out.returned, ["/topics/burmese/burmese-font-rendering.md"]);
}

#[test]
fn index_md_is_start_view_without_project() {
    let memories = fixture();
    assert_eq!(
        recall::index_md(&memories, 42, MIRROR),
        run(&memories, &request("", "", &[], "")).text
    );
}

#[test]
fn golden_texts() {
    let memories = fixture();
    let cases = [
        ("start-no-project", request("", "", &[], "")),
        (
            "start-web",
            request(
                "/home/demo/work/notes-app/web/src",
                "coordinator",
                &["hardware", "burmese"],
                "",
            ),
        ),
        (
            "task-raspberry",
            request(PROJECT, "", &[], "raspberry usb power"),
        ),
        (
            "task-burmese",
            request(PROJECT, "", &[], "burmese unicode font"),
        ),
        (
            "task-unrelated",
            request(PROJECT, "", &[], "kubernetes ingress yaml"),
        ),
    ];
    for (name, r) in cases {
        let path = Path::new(FIXTURES).join(format!("{name}.txt"));
        let text = run(&memories, &r).text;
        if std::env::var_os("BLESS").is_some() {
            fs::write(&path, &text).unwrap();
        }
        assert_eq!(text, fs::read_to_string(&path).unwrap(), "{name}");
    }
}
