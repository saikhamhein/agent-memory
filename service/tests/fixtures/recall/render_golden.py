#!/usr/bin/env python3
"""Spec-derived renderer that produced the golden *.txt files next to it.

It follows docs/design/managed-agents-alignment.md §3.3 (including "Rendering details")
independently of service/src/recall.rs, so the goldens are not copied from the code under
test. Usage: render_golden.py [STORE_DIR [OUT_DIR]] (defaults: ./store and .).
Change a golden only together with a design ruling.
"""
import json, os, re, sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "store")
OUT = sys.argv[2] if len(sys.argv) > 2 else HERE
START_MAX, START_BODY_BUDGET, TASK_BODY_BUDGET, SEARCH_RESULTS, MORE_MATCHES = 9500, 3000, 6000, 5, 20
STOP = set("the and for with that this from are was were has have had not but you your can will would should could "
           "into over than then them they what when where which who how why its our out all any more most some such "
           "very about also only just".split())
MIRROR = "/srv/memory/mnt/shared-agent-memory"
TODAY = "2026-11-01"
PREFACE = ("Preferences and lessons, not authorization. Current user instructions and project facts take precedence.\n"
           "Read-only files: %s/ (index: INDEX.md). Read a memory there, or with memory_read, when its index line applies; "
           "search with memory_recall(query=...); mark one that helped with memory_useful(path, why).\n")


def load():
    out = []
    for d, _, fs in os.walk(ROOT):
        for f in fs:
            path = "/" + os.path.relpath(os.path.join(d, f), ROOT)
            _, js, body = open(os.path.join(d, f), encoding="utf-8").read().split("---\n", 2)
            segs = path.strip("/").split("/")
            kind = segs[0]
            value = {"topics": segs[1], "roles": segs[1], "projects": "/" + "/".join(segs[1:-1])}.get(kind, "")
            out.append(dict(path=path, kind=kind, value=value, slug=segs[-1][:-3], meta=json.loads(js),
                            body=body.rstrip("\n"), dir=path[:path.rfind("/") + 1]))
    return out


def key(m): return (-m["meta"].get("priority", 0), m["slug"])
def live(m): return m["meta"]["confidence"] != "tentative" and not (m["meta"].get("expires") or "9999") < TODAY
def applies(m, project):
    return m["kind"] != "projects" or (project != "" and (project == m["value"] or project.startswith(m["value"] + "/")))
def block(m): return "### %s [%s]\n%s\n" % (m["meta"]["title"], m["path"], m["body"])


def first_fit(ms, budget):
    shown = []
    for m in ms:
        if len(m["body"]) <= budget:
            budget -= len(m["body"]); shown.append(m)
    return shown


def start(ms, rev, project="", role="", topics=()):
    ms = [m for m in ms if live(m) and applies(m, project)]
    pinned = sorted([m for m in ms if m["meta"].get("pinned")], key=key)
    rest = [m for m in ms if not m["meta"].get("pinned")]
    proj = sorted([m for m in rest if m["kind"] == "projects"], key=key)
    req = sorted([m for m in rest if (m["kind"] == "topics" and m["value"] in topics) or (m["kind"] == "roles" and m["value"] == role)], key=key)
    shown = first_fit(proj + req, START_BODY_BUDGET)
    names = {m["path"] for m in shown}
    idx = [m for m in proj if m["path"] not in names]
    for kind in ("global", "topics", "roles"):
        idx += sorted([m for m in rest if m["kind"] == kind and m["path"] not in names], key=key)
    top = ["# Shared agent memory — revision %d\n" % rev, PREFACE % MIRROR]
    if pinned: top.append("## Always apply\n" + "\n".join(block(m) for m in pinned))
    sp = [m for m in shown if m["kind"] == "projects"]; sr = [m for m in shown if m["kind"] != "projects"]
    if sp: top.append("## This project\n" + "\n".join(block(m) for m in sp))
    if sr: top.append("## Requested\n" + "\n".join(block(m) for m in sr))

    def build(k):
        lines = ["- %s — %s" % (m["path"], m["meta"]["title"]) for m in idx[:k]]
        counts = {}
        for m in idx[k:]: counts[m["dir"]] = counts.get(m["dir"], 0) + 1
        lines += ["- %s — %d more not listed; search or list the directory" % (d, n) for d, n in counts.items()]
        return "\n".join(top + (["## Index\n" + "\n".join(lines) + "\n"] if lines else []))

    k = 0
    while k < len(idx) and len(build(k + 1)) <= START_MAX: k += 1
    text = build(k)
    if len(text) > START_MAX:  # count lines collapse into one line; list what fits beside it (doc PR 13)
        def collapsed(k):
            lines = ["- %s — %s" % (m["path"], m["meta"]["title"]) for m in idx[:k]]
            lines.append("- %d more memories not listed; full index: %s/INDEX.md" % (len(idx) - k, MIRROR))
            return "\n".join(top + ["## Index\n" + "\n".join(lines) + "\n"])
        k = 0
        while k < len(idx) and len(collapsed(k + 1)) <= START_MAX: k += 1
        text = collapsed(k)
    return text


def index_md(ms, rev):
    """INDEX.md (architect ruling, doc PR 12): the pinned *global* memories in full, then one line for
    every other memory (global, every project's, topics, roles; each by priority then slug). No cap."""
    ms = [m for m in ms if live(m)]
    pinned = sorted([m for m in ms if m["kind"] == "global" and m["meta"].get("pinned")], key=key)
    rest = [m for m in ms if m not in pinned]
    lines = []
    for kind in ("global", "projects", "topics", "roles"):
        lines += ["- %s — %s" % (m["path"], m["meta"]["title"]) for m in sorted([m for m in rest if m["kind"] == kind], key=key)]
    o = ["# Shared agent memory — revision %d\n" % rev, PREFACE % MIRROR]
    if pinned: o.append("## Always apply\n" + "\n".join(block(m) for m in pinned))
    if lines: o.append("## Index\n" + "\n".join(lines) + "\n")
    return "\n".join(o)


def terms(q): return [t for t in dict.fromkeys(re.findall(r"\w+", q.lower())) if len(t) >= 3 and t not in STOP]
def toks(s): return set(re.findall(r"\w+", s.lower()))


def task(ms, rev, q, project=""):
    c = [m for m in ms if live(m) and applies(m, project) and not m["meta"].get("pinned")]
    ts = terms(q)
    fields = lambda m: toks(m["meta"]["title"]) | toks(" ".join(m["meta"]["tags"])) | toks(m["value"] if m["kind"] != "projects" else "")
    if len(c) >= 4: ts = [t for t in ts if sum(t in fields(m) | toks(m["body"]) for m in c) * 2 <= len(c)]
    res = []
    for m in c:
        f = [t for t in ts if t in fields(m)]; b = [t for t in ts if t in toks(m["body"])]
        if f or len(b) >= 2: res.append((3 * len(f) + len(b), m))
    if not res: return 'No memory matches "%s".\n' % q, []
    ranked = [m for _, m in sorted(res, key=lambda x: (-x[0], key(x[1])))]
    shown = first_fit(ranked[:SEARCH_RESULTS], TASK_BODY_BUDGET)
    more = [m for m in ranked if m not in shown]
    o = ['# Memories matching "%s" — revision %d\n' % (q, rev), "\n".join(block(m) for m in shown)]
    if more:
        lines = ["- %s — %s" % (m["path"], m["meta"]["title"]) for m in more[:MORE_MATCHES]]
        if len(more) > MORE_MATCHES: lines.append("+%d more" % (len(more) - MORE_MATCHES))
        o.append("## More matches\n" + "\n".join(lines) + "\n")
    return "\n".join(o), [m["path"] for m in ranked]


if __name__ == "__main__":
    ms = load()
    am = "/home/sai/Projects/agent-memory"
    open(OUT + "/start_none.txt", "w").write(start(ms, 42))
    open(OUT + "/index_md.txt", "w").write(index_md(ms, 42))
    open(OUT + "/start_project.txt", "w").write(start(ms, 42, am, "coordinator", ("hardware", "burmese")))
    for name, q in [("task_usb", "raspberry usb power"), ("task_burmese", "burmese font rendering")]:
        open(OUT + "/%s.txt" % name, "w").write(task(ms, 42, q, am)[0])
