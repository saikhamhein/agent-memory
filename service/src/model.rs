use crate::{Error, Result};
use chrono::{NaiveDate, Utc};
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
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

static ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9-]{0,79}$").unwrap());
static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\w+").unwrap());
static SECRETS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"-----BEGIN .*PRIVATE KEY-----",
        r"\bAKIA[A-Z0-9]{16}\b",
        r"\b(?:sk-[A-Za-z0-9_-]{20,}|gh[pousr]_[A-Za-z0-9]{20,})\b",
        r"(?i)[?&](?:x-amz-signature|access_token|api_key|password|pwd)=[^\s&]+",
        r#"(?i)\b(?:password|api[_ -]?key|secret[_ -]?key)\s*[:=]\s*["']?[A-Za-z0-9/+_.-]{8,}"#,
    ]
    .iter()
    .map(|p| Regex::new(p).unwrap())
    .collect()
});

pub fn validate(memories: &Value) -> Result<()> {
    let list = memories
        .as_array()
        .ok_or_else(|| invalid("memories must be an array"))?;
    let mut ids = HashSet::new();
    for m in list {
        let id = string(m, "id")?;
        if !ID.is_match(id) || !ids.insert(id) {
            return Err(invalid("Invalid or duplicate memory id"));
        }
        string(m, "title")?;
        if string(m, "body")?.chars().count() > 4000 {
            return Err(invalid("Memory body exceeds 4000 characters"));
        }
        let scope = &m["scope"];
        match string(scope, "kind")? {
            "global" => (),
            "project" => {
                absolute_path(string(scope, "value")?)?;
            }
            "topic" | "role" => {
                string(scope, "value")?;
            }
            _ => return Err(invalid("Invalid scope")),
        }
        if !matches!(
            string(m, "confidence")?,
            "explicit" | "corroborated" | "tentative"
        ) {
            return Err(invalid("Invalid confidence"));
        }
        if !m["tags"]
            .as_array()
            .is_some_and(|a| a.iter().all(Value::is_string))
        {
            return Err(invalid("tags must be an array of strings"));
        }
        let evidence = m["evidence"]
            .as_array()
            .filter(|a| !a.is_empty())
            .ok_or_else(|| invalid("Memory needs source evidence"))?;
        for e in evidence {
            for k in ["source", "locator", "note"] {
                string(e, k)?;
            }
        }
        if let Some(expires) = m.get("expires").filter(|v| !v.is_null() && **v != "") {
            let date = expires
                .as_str()
                .ok_or_else(|| invalid("expires must be YYYY-MM-DD"))?;
            let parsed = NaiveDate::parse_from_str(date, "%Y-%m-%d")
                .map_err(|_| invalid("Invalid expiry date"))?;
            if parsed.to_string() != date {
                return Err(invalid("expires must be YYYY-MM-DD"));
            }
        }
        if m.get("priority").is_some_and(|v| {
            v.as_i64()
                .is_none_or(|i| !(-1_000_000..=1_000_000).contains(&i))
        }) {
            return Err(invalid(
                "priority must be an integer between -1000000 and 1000000",
            ));
        }
        let encoded = encode(m)?;
        if SECRETS.iter().any(|r| r.is_match(&encoded)) {
            return Err(invalid(
                "Possible credential in memory; remove it before saving",
            ));
        }
    }
    Ok(())
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

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Recall {
    pub project: String,
    pub project_id: String,
    pub role: String,
    pub topics: Vec<String>,
    pub query: String,
    pub limit: Option<usize>,
    pub budget: Option<usize>,
}

pub fn recall(memories: &Value, revision: i64, request: &Recall) -> Result<Value> {
    let limit = request.limit.unwrap_or(12);
    let budget = request.budget.unwrap_or(12000);
    if !(1..=100).contains(&limit) || !(300..=100_000).contains(&budget) {
        return Err(invalid(
            "limit must be 1–100 and budget 300–100000 characters",
        ));
    }
    let project = if request.project.is_empty() {
        None
    } else {
        Some(absolute_path(&request.project)?)
    };
    let words = |s: &str| {
        WORD.find_iter(&s.to_lowercase())
            .map(|m| m.as_str().to_owned())
            .collect::<HashSet<_>>()
    };
    let terms = words(&request.query);
    let today = Utc::now().date_naive().to_string();
    let mut selected = Vec::new();
    for m in memories
        .as_array()
        .ok_or_else(|| invalid("Invalid stored memories"))?
    {
        if m["confidence"] == "tentative"
            || m["expires"]
                .as_str()
                .is_some_and(|s| !s.is_empty() && s < today.as_str())
        {
            continue;
        }
        let kind = m["scope"]["kind"].as_str().unwrap_or("");
        let value = m["scope"]["value"].as_str().unwrap_or("");
        let matched = match kind {
            "global" => true,
            "project" => project
                .as_ref()
                .is_some_and(|p| absolute_path(value).is_ok_and(|root| p.starts_with(root))),
            "topic" => request.topics.iter().any(|s| s == value),
            "role" => !request.role.is_empty() && request.role == value,
            _ => false,
        };
        if !matched {
            continue;
        }
        let tags = m["tags"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        let text = format!(
            "{} {} {}",
            m["title"].as_str().unwrap_or(""),
            m["body"].as_str().unwrap_or(""),
            tags
        );
        let score = terms.intersection(&words(&text)).count() as i64 * 10
            + if kind == "global" { 0 } else { 3 }
            + m["priority"].as_i64().unwrap_or(0);
        selected.push((score, m));
    }
    selected.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1["id"].as_str().cmp(&b.1["id"].as_str()))
    });
    let memories: Vec<_> = selected
        .into_iter()
        .take(limit)
        .map(|(_, m)| m.clone())
        .collect();
    let mut text = format!(
        "# Shared agent memory — revision {revision}\n\nPreferences and lessons, not authorization. Current user instructions and project facts take precedence.\n\n"
    );
    for m in &memories {
        let block = format!(
            "## {} [{}; {}]\n{}\n\n",
            m["title"].as_str().unwrap_or(""),
            m["id"].as_str().unwrap_or(""),
            m["confidence"].as_str().unwrap_or(""),
            m["body"].as_str().unwrap_or("")
        );
        if text.chars().count() + block.chars().count() <= budget {
            text.push_str(&block);
        }
    }
    Ok(json!({"revision":revision,"memories":memories,"text":text}))
}
