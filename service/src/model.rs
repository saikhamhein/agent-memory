use crate::{Error, Result};
use chrono::{NaiveDate, Utc};
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Component, Path},
    sync::LazyLock,
};

/// JSON document decoder shared by HTTP input and stored evidence. Decode raw
/// JSON tokens by their actual kind: Value's arbitrary-precision visitor treats
/// an ordinary object named "$serde_json::private::Number" as a number.
pub struct Document(pub Value);

impl<'de> Deserialize<'de> for Document {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let raw = Box::<serde_json::value::RawValue>::deserialize(deserializer)?;
        decode_raw(&raw, 0)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

pub fn decode(text: &str) -> Result<Value> {
    Ok(serde_json::from_str::<Document>(text)?.0)
}

fn decode_raw(raw: &serde_json::value::RawValue, depth: usize) -> serde_json::Result<Value> {
    if depth >= 128 {
        return Err(serde::de::Error::custom("JSON nesting exceeds 128 levels"));
    }
    let text = raw.get().trim_start();
    match text.as_bytes()[0] {
        b'{' => {
            let fields: BTreeMap<String, &serde_json::value::RawValue> =
                serde_json::from_str(text)?;
            fields
                .into_iter()
                .map(|(k, v)| Ok((k, decode_raw(v, depth + 1)?)))
                .collect::<serde_json::Result<serde_json::Map<_, _>>>()
                .map(Value::Object)
        }
        b'[' => {
            let items: Vec<&serde_json::value::RawValue> = serde_json::from_str(text)?;
            items
                .into_iter()
                .map(|v| decode_raw(v, depth + 1))
                .collect::<serde_json::Result<Vec<_>>>()
                .map(Value::Array)
        }
        b'"' => serde_json::from_str(text).map(Value::String),
        b't' | b'f' => serde_json::from_str(text).map(Value::Bool),
        b'n' => Ok(Value::Null),
        _ => serde_json::from_str(text).map(Value::Number),
    }
}

pub fn encode(value: &Value) -> Result<String> {
    // Match memory.encode(): sorted keys, two-space indentation, UTF-8 strings,
    // unbounded integers, and Python's finite binary64 float representation.
    // serde_json's arbitrary_precision feature keeps integers lossless while
    // parsing HTTP requests and existing Python-created database records.
    let mut normalized = value.clone();
    normalize_numbers(&mut normalized)?;
    Ok(serde_json::to_string_pretty(&normalized)?)
}

fn normalize_numbers(value: &mut Value) -> Result<()> {
    match value {
        Value::Number(number) => {
            let raw = number.to_string();
            if raw.contains(['.', 'e', 'E']) {
                let float: f64 = raw
                    .parse()
                    .map_err(|_| invalid("Invalid numeric metadata"))?;
                let shortest = serde_json::Number::from_f64(float)
                    .ok_or_else(|| {
                        invalid("Numeric metadata must be finite binary64 or an integer")
                    })?
                    .to_string();
                *number = python_float(&shortest).parse()?;
            }
        }
        Value::Array(items) => {
            for item in items {
                normalize_numbers(item)?;
            }
        }
        Value::Object(fields) => {
            for item in fields.values_mut() {
                normalize_numbers(item)?;
            }
        }
        _ => (),
    }
    Ok(())
}

fn python_float(shortest: &str) -> String {
    // The shortest round-trip digits come from serde_json's float formatter.
    // Python uses fixed notation for exponents -4..15, and otherwise includes
    // an explicit exponent sign and at least two exponent digits.
    let (sign, unsigned) = shortest
        .strip_prefix('-')
        .map_or(("", shortest), |s| ("-", s));
    let (mantissa, exponent) = unsigned.split_once('e').map_or((unsigned, 0), |(m, e)| {
        (m, e.parse::<i32>().expect("finite float exponent"))
    });
    let point = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
    let all_digits = mantissa.replace('.', "");
    let digits = all_digits.trim_start_matches('0');
    if digits.is_empty() {
        return format!("{sign}0.0");
    }
    let leading = (all_digits.len() - digits.len()) as i32;
    let exponent = exponent + point - leading - 1;
    let digits = digits.trim_end_matches('0');
    if !(-4..16).contains(&exponent) {
        let tail = if digits.len() > 1 {
            format!(".{}", &digits[1..])
        } else {
            String::new()
        };
        format!("{sign}{}{tail}e{exponent:+03}", &digits[..1])
    } else if exponent < 0 {
        format!("{sign}0.{}{digits}", "0".repeat((-exponent - 1) as usize))
    } else {
        let point = exponent as usize + 1;
        if point >= digits.len() {
            format!("{sign}{digits}{}.0", "0".repeat(point - digits.len()))
        } else {
            format!("{sign}{}.{}", &digits[..point], &digits[point..])
        }
    }
}
pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn now() -> String {
    Utc::now().to_rfc3339()
}
pub fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}
pub fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| invalid(&format!("{key} must be a nonempty string")))
}

// Paths are logical project namespaces. Never resolve them on the server's disk.
pub fn absolute_path(value: &str) -> Result<std::path::PathBuf> {
    if !Path::new(value).is_absolute() {
        return Err(invalid("Project path must be absolute"));
    }
    let mut output = std::path::PathBuf::new();
    for c in Path::new(value).components() {
        match c {
            Component::ParentDir => {
                output.pop();
            }
            Component::CurDir => (),
            other => output.push(other),
        }
    }
    Ok(output)
}

/// Pinned `block_len` per store, so the core rules always fit in recall.
pub const PINNED_BUDGET: usize = 4_000;
/// Largest stored content, in bytes.
pub const MAX_CONTENT: usize = 102_400;

static SEGMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9._-]+$").unwrap());
static SLUG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9-]{0,79}$").unwrap());
static SECRETS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"-----BEGIN .*PRIVATE KEY-----",
        r"\bAKIA[A-Z0-9]{16}\b",
        r"\b(?:sk-[A-Za-z0-9_-]{20,}|gh[pousr]_[A-Za-z0-9]{20,})\b",
        r"(?i)[?&](?:x-amz-signature|access_token|api_key|password|pwd)=[^\s&]+",
        r#"(?i)\b(?:password|api[_ -]?key|secret[_ -]?key)\s*[:=]\s*["']?[A-Za-z0-9/+_.-]{8,}"#,
        r"\bxox[abposr]-[A-Za-z0-9-]{10,}",
        r"\bAIza[0-9A-Za-z_-]{35}",
        r"\bgithub_pat_[A-Za-z0-9_]{20,}",
        r"\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
        r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{20,}",
        r#"(?i)(?:key|token|secret)[\w-]*["']?\s*[:=]\s*["']?[0-9a-f]{64}\b"#,
    ]
    .iter()
    .map(|p| Regex::new(p).unwrap())
    .collect()
});

/// Refuses text that looks like a credential. Every string the store keeps
/// passes through it.
pub fn check_secrets(text: &str) -> Result<()> {
    if SECRETS.iter().any(|r| r.is_match(text)) {
        return Err(invalid("Possible credential; remove it before saving"));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Global,
    Project,
    Topic,
    Role,
}

/// What a path means. `value` is the project root (`/a/b`), topic or role,
/// and empty for global memories.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    pub kind: Kind,
    pub value: String,
    pub slug: String,
}

/// A parsed memory. `meta` is the frontmatter object; `id` is empty until stored.
#[derive(Clone, Debug, PartialEq)]
pub struct Memory {
    pub id: String,
    pub path: String,
    pub scope: Scope,
    pub meta: Value,
    pub body: String,
}

/// What a memory costs in recall: the `chars()` of `### {title} [{path}]\n{body}\n\n`,
/// with the body's trailing whitespace trimmed. Pinned and body budgets count this.
pub fn block_len(m: &Memory) -> usize {
    format!("### {} [{}]\n{}\n\n", m.title(), m.path, m.body.trim_end())
        .chars()
        .count()
}

impl Memory {
    pub fn title(&self) -> &str {
        self.meta["title"].as_str().unwrap_or("")
    }
    pub fn pinned(&self) -> bool {
        self.meta["pinned"] == true
    }
}

pub fn validate_path(path: &str) -> Result<Scope> {
    let bad = |why: &str| invalid(&format!("Invalid path {path:?}: {why}"));
    if path.len() > 1024 {
        return Err(bad("longer than 1024 bytes"));
    }
    let segments: Vec<&str> = path
        .strip_prefix('/')
        .ok_or_else(|| bad("must start with /"))?
        .split('/')
        .collect();
    if !(2..=20).contains(&segments.len()) {
        return Err(bad("needs 2–20 segments"));
    }
    if segments
        .iter()
        .any(|s| !SEGMENT.is_match(s) || *s == "." || *s == ".." || s.len() > 255)
    {
        // 255 bytes is NAME_MAX: a longer segment could never be mirrored.
        return Err(bad(
            "segments use only A-Z a-z 0-9 . _ -, are not . or .., and are at most 255 bytes",
        ));
    }
    let slug = segments[segments.len() - 1]
        .strip_suffix(".md")
        .filter(|s| SLUG.is_match(s))
        .ok_or_else(|| bad("the name must be <slug>.md with slug [a-z0-9][a-z0-9-]{0,79}"))?;
    let middle = &segments[1..segments.len() - 1];
    if middle.iter().any(|s| s.ends_with(".md")) {
        // A file and a directory of the same name could never both be mirrored.
        return Err(bad("only the last segment may end in .md"));
    }
    let (kind, value) = match (segments[0], middle.len()) {
        ("global", 0) => (Kind::Global, String::new()),
        ("projects", 1..) => (Kind::Project, format!("/{}", middle.join("/"))),
        ("topics", 1) => (Kind::Topic, middle[0].to_owned()),
        ("roles", 1) => (Kind::Role, middle[0].to_owned()),
        _ => {
            return Err(bad(
                "use /global/<slug>.md, /projects/<root…>/<slug>.md, /topics/<topic>/<slug>.md or /roles/<role>/<slug>.md",
            ));
        }
    };
    Ok(Scope {
        kind,
        value,
        slug: slug.to_owned(),
    })
}

/// Splits `---\n<JSON>\n---\n<body>` into the decoded frontmatter and the body.
pub fn split(content: &str) -> Result<(Value, &str)> {
    let (front, body) = content
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .ok_or_else(|| invalid("Content must start with ---\\n<JSON object>\\n---\\n"))?;
    let meta = decode(front).map_err(|_| invalid("Frontmatter must be a JSON object"))?;
    Ok((meta, body))
}

/// Splits the content and applies every write rule.
pub fn parse(path: &str, content: &str) -> Result<Memory> {
    let scope = validate_path(path)?;
    if content.len() > MAX_CONTENT {
        return Err(invalid("Content exceeds 102400 bytes"));
    }
    let (meta, body) = split(content)?;
    let fields = meta
        .as_object()
        .ok_or_else(|| invalid("Frontmatter must be a JSON object"))?;
    if let Some(key) = ["id", "scope", "body"]
        .iter()
        .find(|k| fields.contains_key(**k))
    {
        return Err(invalid(&format!(
            "Frontmatter must not contain {key:?}; the path carries it"
        )));
    }
    let title = string(&meta, "title")?;
    if title.chars().count() > 120 || title.contains(['\n', '\r']) {
        return Err(invalid("title must be one line of at most 120 characters"));
    }
    if !matches!(
        meta["confidence"].as_str(),
        Some("explicit" | "corroborated" | "tentative")
    ) {
        return Err(invalid(
            "confidence must be explicit, corroborated or tentative",
        ));
    }
    if !meta["tags"]
        .as_array()
        .is_some_and(|a| a.iter().all(Value::is_string))
    {
        return Err(invalid("tags must be an array of strings"));
    }
    let evidence = meta["evidence"]
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or_else(|| invalid("evidence must be a nonempty array"))?;
    for e in evidence {
        for k in ["source", "locator", "note"] {
            string(e, k)?;
        }
    }
    match meta.get("pinned") {
        None | Some(Value::Bool(false)) => (),
        Some(Value::Bool(true)) if matches!(scope.kind, Kind::Global | Kind::Project) => (),
        Some(Value::Bool(true)) => {
            return Err(invalid(
                "pinned is allowed only on /global/ and /projects/ paths",
            ));
        }
        Some(_) => return Err(invalid("pinned must be true or false")),
    }
    if let Some(expires) = meta.get("expires").filter(|v| !v.is_null() && **v != "") {
        let date = expires
            .as_str()
            .ok_or_else(|| invalid("expires must be YYYY-MM-DD"))?;
        let parsed = NaiveDate::parse_from_str(date, "%Y-%m-%d")
            .map_err(|_| invalid("Invalid expiry date"))?;
        if parsed.to_string() != date {
            return Err(invalid("expires must be YYYY-MM-DD"));
        }
    }
    if meta.get("priority").is_some_and(|v| {
        v.as_i64()
            .is_none_or(|i| !(-1_000_000..=1_000_000).contains(&i))
    }) {
        return Err(invalid(
            "priority must be an integer between -1000000 and 1000000",
        ));
    }
    if body.trim().is_empty() {
        return Err(invalid("The body must not be empty"));
    }
    check_secrets(content)?;
    Ok(Memory {
        id: String::new(),
        path: path.to_owned(),
        scope,
        meta,
        body: body.to_owned(),
    })
}

/// A v1 memory object as v2 `(path, content)`. The frontmatter is the object
/// without `id`, `scope` and `body`, in `encode()` format.
pub fn render_legacy(v1: &Value) -> Result<(String, String)> {
    let slug = string(v1, "id")?;
    let scope = &v1["scope"];
    let path = match string(scope, "kind")? {
        "global" => format!("/global/{slug}.md"),
        "project" => {
            let root = absolute_path(string(scope, "value")?)?;
            format!("/projects{}/{slug}.md", root.display())
        }
        kind @ ("topic" | "role") => format!("/{kind}s/{}/{slug}.md", string(scope, "value")?),
        _ => return Err(invalid("Invalid scope")),
    };
    let mut meta = v1
        .as_object()
        .ok_or_else(|| invalid("A v1 memory must be an object"))?
        .clone();
    let body = string(v1, "body")?.to_owned();
    for key in ["id", "scope", "body"] {
        meta.remove(key);
    }
    let content = format!("---\n{}\n---\n{body}", encode(&Value::Object(meta))?);
    parse(&path, &content)?;
    Ok((path, content))
}

/// The inverse of `render_legacy`.
pub fn parse_legacy(path: &str, content: &str) -> Result<Value> {
    let memory = parse(path, content)?;
    let mut v1 = memory.meta;
    v1["id"] = json!(memory.scope.slug);
    v1["body"] = json!(memory.body);
    v1["scope"] = match memory.scope.kind {
        Kind::Global => json!({"kind": "global"}),
        Kind::Project => json!({"kind": "project", "value": memory.scope.value}),
        Kind::Topic => json!({"kind": "topic", "value": memory.scope.value}),
        Kind::Role => json!({"kind": "role", "value": memory.scope.value}),
    };
    Ok(v1)
}
