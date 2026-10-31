//! Placeholder with the frozen §4.1 signatures so the store and mirror build
//! before workstream R lands. R replaces this file wholesale.

use crate::model::Memory;

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
    let mut text = format!(
        "# Shared agent memory — revision {revision}\n\nRead-only files: {mirror_dir}/\n\n## Index\n"
    );
    for m in memories {
        text.push_str(&format!("- {} — {}\n", m.path, m.title()));
    }
    Recalled {
        view: if request.query.is_empty() {
            "start"
        } else {
            "task"
        },
        pinned: vec![],
        returned: vec![],
        text,
    }
}

pub fn index_md(memories: &[Memory], revision: i64, mirror_dir: &str) -> String {
    let request = Request {
        project: String::new(),
        role: String::new(),
        topics: vec![],
        query: String::new(),
    };
    recall(memories, revision, mirror_dir, &request).text
}
