//! The read-only file copy of the live store. Each write builds a complete
//! generation directory, then renames a symlink over `shared-agent-memory`:
//! rename(2) replaces a symlink atomically, but cannot replace a non-empty
//! directory. Readers therefore always see one whole generation.
//! The service is the only writer, so generations are not locked.

use serde_json::{Value, json};
use std::{
    fs, io,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
};
use uuid::Uuid;

const LINK: &str = "shared-agent-memory";
const GENERATIONS: &str = ".generations";
const MARKER: &str = ".agent-memory-store";

pub fn dir(mnt: &Path) -> PathBuf {
    mnt.join(LINK)
}

/// Whether the mirror already holds this revision of this store with this index.
pub fn is_current(mnt: &Path, store_id: &str, revision: i64, index: &str) -> bool {
    let dir = dir(mnt);
    let marker = fs::read_to_string(dir.join(MARKER))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    marker == Some(json!({"store_id": store_id, "revision": revision}))
        && fs::read_to_string(dir.join("INDEX.md")).is_ok_and(|t| t == index)
}

/// Publishes `files` (path, content, …) as the new generation and keeps only
/// it and the one it replaced.
pub fn write(
    mnt: &Path,
    store_id: &str,
    revision: i64,
    files: &[(String, String, String, String)],
    index: &str,
) -> io::Result<()> {
    let generations = mnt.join(GENERATIONS);
    fs::create_dir_all(&generations)?;
    let build = generations.join(format!(".build-{}", Uuid::new_v4().simple()));
    fs::create_dir(&build)?;
    let built = (|| {
        for (_, path, content, _) in files {
            let file = build.join(path.trim_start_matches('/'));
            fs::create_dir_all(file.parent().expect("memory paths have a directory"))?;
            create(&file, content)?;
        }
        create(&build.join("INDEX.md"), index)?;
        let marker = json!({"store_id": store_id, "revision": revision});
        create(&build.join(MARKER), &marker.to_string())?;
        let name = revision.to_string();
        let target = generations.join(&name);
        if target.exists() {
            remove(&target)?;
        }
        // Rename before sealing: rename(2) may need write access to a moved directory.
        fs::rename(&build, &target)?;
        seal(&target)?;
        Ok(name)
    })();
    let name = match built {
        Ok(name) => name,
        Err(e) => {
            let _ = remove(&build);
            return Err(e);
        }
    };
    let link = dir(mnt);
    let previous = fs::read_link(&link).ok();
    let next = mnt.join(format!(".{LINK}.new"));
    match fs::remove_file(&next) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => (),
    }
    symlink(Path::new(GENERATIONS).join(&name), &next)?;
    fs::rename(&next, &link)?;
    let previous = previous.as_deref().and_then(Path::file_name);
    for entry in fs::read_dir(&generations)? {
        let entry = entry?;
        let old = entry.file_name();
        if old != name.as_str() && Some(old.as_os_str()) != previous {
            remove(&entry.path())?;
        }
    }
    Ok(())
}

fn create(path: &Path, content: &str) -> io::Result<()> {
    fs::write(path, content)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o444))
}

/// Makes every directory read-only (0555), deepest first.
fn seal(dir: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            seal(&entry.path())?;
        }
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o555))
}

fn remove(dir: &Path) -> io::Result<()> {
    fn unseal(dir: &Path) -> io::Result<()> {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755))?;
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                unseal(&entry.path())?;
            }
        }
        Ok(())
    }
    unseal(dir)?;
    fs::remove_dir_all(dir)
}
