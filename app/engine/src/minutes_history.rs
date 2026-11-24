//! Preserve the previous minutes before atomically replacing the current file.
use crate::meetings::MINUTES;
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
static LOCK: Mutex<()> = Mutex::new(());
const HISTORY: &str = "minutes-history";
/// iOS 0.4.2 存历史版本用的隐藏目录，读到时并进 [`HISTORY`]
const LEGACY_HISTORY: &str = ".minutes-history";
#[derive(Serialize, Deserialize)]
pub struct Version {
    pub id: String,
    pub saved_at: u64,
    pub text: String,
}
fn millis(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
/// 把旧 iOS 版的历史版本搬进和 Mac 版共用的目录；搬不动的留在原处，不影响当前纪要
fn migrate_legacy(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir.join(LEGACY_HISTORY)) else { return };
    if fs::create_dir_all(dir.join(HISTORY)).is_err() {
        return;
    }
    for entry in entries.flatten() {
        let target = dir.join(HISTORY).join(entry.file_name());
        if entry.path().extension().is_some_and(|e| e == "json") && !target.exists() {
            let _ = fs::rename(entry.path(), target);
        }
    }
    let _ = fs::remove_dir(dir.join(LEGACY_HISTORY));
}
fn version_path(dir: &Path, id: &str) -> io::Result<std::path::PathBuf> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid version ID",
        ));
    }
    Ok(dir.join(HISTORY).join(format!("{id}.json")))
}
pub fn read(dir: &Path, id: &str) -> io::Result<Version> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    Ok(serde_json::from_slice(&fs::read(version_path(dir, id)?)?)?)
}
pub fn save(dir: &Path, text: &str) -> io::Result<()> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let current = dir.join(MINUTES);
    let old = match fs::read_to_string(&current) {
        Ok(v) => Some(v),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    if old.as_deref() == Some(text) {
        return Ok(());
    }
    // Write the replacement first. A failed archive must never overwrite the current version.
    let temp = dir.join(".minutes-new.tmp");
    fs::write(&temp, text)?;
    let result = (|| {
        if let Some(old) = old.filter(|v| !v.trim().is_empty()) {
            fs::create_dir_all(dir.join(HISTORY))?;
            let id = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
                .to_string();
            let version = Version {
                id: id.clone(),
                saved_at: millis(fs::metadata(&current)?.modified()?),
                text: old,
            };
            let file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(version_path(dir, &id)?)?;
            serde_json::to_writer(file, &version)?;
        }
        fs::rename(&temp, &current)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
pub fn list(dir: &Path) -> io::Result<Vec<Version>> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    migrate_legacy(dir);
    let entries = match fs::read_dir(dir.join(HISTORY)) {
        Ok(v) => v,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e),
    };
    let mut versions = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_file() && entry.path().extension().is_some_and(|e| e == "json") {
            versions.push(serde_json::from_slice::<Version>(&fs::read(entry.path())?)?);
        }
    }
    versions.sort_by(|a, b| b.saved_at.cmp(&a.saved_at).then_with(|| b.id.cmp(&a.id)));
    Ok(versions)
}
pub fn delete(dir: &Path, id: &str) -> io::Result<()> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    fs::remove_file(version_path(dir, id)?)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_preserves_edits_and_deletes_only_selected_version() {
        let dir = std::env::temp_dir().join(format!(
            "tingo-history-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        assert!(list(&dir).unwrap().is_empty());
        save(&dir, "first").unwrap();
        save(&dir, "second").unwrap();
        save(&dir, "second").unwrap();
        let versions = list(&dir).unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].text, "first");
        assert!(delete(&dir, "../纪要.md").is_err());
        delete(&dir, &versions[0].id).unwrap();
        assert!(list(&dir).unwrap().is_empty());
        assert_eq!(fs::read_to_string(dir.join(MINUTES)).unwrap(), "second");
        fs::write(dir.join(HISTORY).join("block"), "").unwrap();
        fs::remove_dir_all(dir.join(HISTORY)).unwrap();
        fs::write(dir.join(HISTORY), "blocked").unwrap();
        assert!(save(&dir, "third").is_err());
        assert_eq!(fs::read_to_string(dir.join(MINUTES)).unwrap(), "second");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn legacy_ios_history_is_listed() {
        let dir = std::env::temp_dir().join(format!(
            "tingo-legacy-history-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(dir.join(LEGACY_HISTORY)).unwrap();
        fs::write(
            dir.join(LEGACY_HISTORY).join("17.json"),
            r#"{"id":"17","saved_at":1793955484000,"text":"old"}"#,
        )
        .unwrap();
        let versions = list(&dir).unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].text, "old");
        assert!(!dir.join(LEGACY_HISTORY).exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
