//! 会议文件夹的约定，和 Python 版 meetings.py 一致：一场会议一个文件夹。
//!
//! <根目录>/2026-11-02_1430_周会/{录音.m4a, 转写.md, transcript.json, 纪要.md}

use chrono::{Local, NaiveDate, NaiveDateTime};
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use crate::transcript::{fmt_ts, Transcript};

pub const AUDIO: &str = "录音.m4a";
pub const MINUTES: &str = "纪要.md";
pub const TRANSCRIPT_MD: &str = "转写.md";
pub const TRANSCRIPT_JSON: &str = "transcript.json";
/// 用户改的标题、标签、归档（没有这个文件 = 全部默认）
pub const META: &str = "meta.json";
pub const MAX_TAGS: usize = 10;
pub const MAX_TAG_CHARS: usize = 20;
const RAW_TRACKS: [&str; 3] = ["system.wav", "mic.wav", "mixed.wav"];
pub const AUDIO_EXTS: [&str; 10] = [
    "m4a", "mp3", "wav", "mp4", "aac", "flac", "ogg", "webm", "mov", "mkv",
];

static FOLDER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d{4}-\d{2}-\d{2})_(\d{2})(\d{2})_(.+)$").unwrap());

pub fn default_root() -> PathBuf {
    if let Some(p) = crate::config::env_var("ROOT") {
        return PathBuf::from(p);
    }
    let cfg = crate::config::load(&crate::config::default_path());
    if !cfg.storage_root.is_empty() {
        return PathBuf::from(cfg.storage_root);
    }
    let docs = crate::platform::home_dir().unwrap_or_default().join("Documents");
    crate::config::with_legacy(docs.join("Tingo"), docs.join("jiyao"))
}

/// 会议文件夹名里的标题部分：去掉文件名里不能用的字符和空白
pub fn folder_title(title: &str) -> String {
    static UNSAFE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"[\\/:*?"<>|\s]+"#).unwrap());
    let safe = UNSAFE.replace_all(title.trim(), "_");
    // Windows 会悄悄去掉文件夹名末尾的点，建出来的文件夹就和记下的名字对不上了
    let safe = safe.trim_end_matches('.');
    if safe.is_empty() {
        "会议".into()
    } else {
        safe.to_string()
    }
}

pub fn new_folder(root: &Path, title: &str, when: NaiveDateTime) -> PathBuf {
    let safe = folder_title(title);
    let base = format!("{}_{safe}", when.format("%Y-%m-%d_%H%M"));
    let mut path = root.join(&base);
    let mut n = 2;
    while path.exists() {
        path = root.join(format!("{base}_{n}"));
        n += 1;
    }
    path
}

pub fn save_transcript(tr: &Transcript, dir: &Path) -> std::io::Result<()> {
    tr.save(&dir.join(TRANSCRIPT_JSON))?;
    std::fs::write(dir.join(TRANSCRIPT_MD), tr.to_markdown())
}

pub fn find_audio(dir: &Path) -> Option<PathBuf> {
    let main = dir.join(AUDIO);
    if main.is_file() {
        return Some(main);
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    files.sort();
    files.into_iter().find(|p| {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        AUDIO_EXTS.contains(&ext.as_str()) && !RAW_TRACKS.contains(&name)
    })
}

fn modified_date(p: &Path) -> Option<NaiveDate> {
    let t = std::fs::metadata(p).ok()?.modified().ok()?;
    Some(chrono::DateTime::<Local>::from(t).date_naive())
}

/// 会议日期：文件夹名里的优先，否则用录音或转写文件的修改时间。
pub fn meeting_date(dir: &Path) -> NaiveDate {
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if let Some(c) = FOLDER.captures(name) {
        if let Ok(d) = NaiveDate::parse_from_str(&c[1], "%Y-%m-%d") {
            return d;
        }
    }
    find_audio(dir)
        .and_then(|a| modified_date(&a))
        .or_else(|| modified_date(&dir.join(TRANSCRIPT_JSON)))
        .unwrap_or_else(|| Local::now().date_naive())
}

/// 会议开始的时间：文件夹名里的日期和时分；不按约定起名的文件夹只知道日期，算当天零点。
pub fn meeting_start(dir: &Path) -> NaiveDateTime {
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    FOLDER
        .captures(name)
        .and_then(|c| NaiveDateTime::parse_from_str(&format!("{} {}:{}", &c[1], &c[2], &c[3]), "%Y-%m-%d %H:%M").ok())
        .unwrap_or_else(|| meeting_date(dir).and_hms_opt(0, 0, 0).unwrap_or_default())
}

pub fn minutes_title(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.strip_prefix("# ").map(|t| t.trim().to_string()))
}

/// 纪要里"一句话总结"（英文纪要是 Summary）下面的那句。
pub fn one_liner(text: &str) -> String {
    let mut lines = text.lines();
    while let Some(l) = lines.next() {
        if matches!(l.trim(), "## 一句话总结" | "## Summary") {
            return lines
                .find(|n| !n.trim().is_empty())
                .map(|n| n.trim().to_string())
                .unwrap_or_default();
        }
    }
    String::new()
}

/// 会议元数据 meta.json 里由用户编辑的三项：title（为空时不写）、tags、archived。其他字段写入时原样保留。
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Meta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub tags: Vec<String>,
    pub archived: bool,
}

/// 标题：去首尾空白，空的当没有
pub fn normalize_title(t: Option<&str>) -> Option<String> {
    t.map(str::trim).filter(|t| !t.is_empty()).map(String::from)
}

/// 一个标签：去首尾空白，最长 20 个字符
pub fn normalize_tag(t: &str) -> String {
    t.trim().chars().take(MAX_TAG_CHARS).collect::<String>().trim_end().to_string()
}

/// 标签：去首尾空白、空的丢掉、每个最长 20 个字符、去重（保持顺序）、最多 10 个
pub fn normalize_tags<S: AsRef<str>>(tags: &[S]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in tags {
        let t = normalize_tag(t.as_ref());
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
        if out.len() >= MAX_TAGS {
            break;
        }
    }
    out
}

/// 录音中标记的重点时刻（毫秒，升序）。和 meta.json 分开存：meta 只写标题、标签、归档三项。
pub const MARKS: &str = "marks.json";

pub fn read_marks(dir: &Path) -> Vec<u64> {
    std::fs::read_to_string(dir.join(MARKS))
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<u64>>(&s).ok())
        .unwrap_or_default()
}

pub fn write_marks(dir: &Path, marks: &[u64]) -> std::io::Result<()> {
    let mut marks = marks.to_vec();
    marks.sort_unstable();
    marks.dedup();
    std::fs::write(dir.join(MARKS), serde_json::to_vec(&marks).map_err(std::io::Error::other)?)
}

/// 读 meta.json；没有、读不出或格式不对的字段都按默认值
pub fn read_meta(dir: &Path) -> Meta {
    let Some(v) = std::fs::read_to_string(dir.join(META))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
    else {
        return Meta::default();
    };
    let tags: Vec<&str> = v["tags"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    Meta {
        title: normalize_title(v["title"].as_str()),
        tags: normalize_tags(&tags),
        archived: v["archived"].as_bool().unwrap_or(false),
    }
}

/// 写 meta.json：UTF-8、缩进 2、中文原样。只改 title、tags、archived 三项，
/// 其他字段（Watch 录音的 source / watch_recording_id、中断恢复的 interrupted 等）原样保留；
/// 现有文件读不出或不是 JSON 对象时不覆盖，返回错误。先写临时文件再改名，写一半不会留下坏文件。
pub fn write_meta(dir: &Path, meta: &Meta) -> std::io::Result<()> {
    #[derive(Serialize)]
    struct Out<'a> {
        #[serde(flatten)]
        meta: &'a Meta,
        #[serde(flatten)]
        extra: serde_json::Map<String, Value>,
    }
    let path = dir.join(META);
    let mut extra = match std::fs::read(&path) {
        Ok(data) => match serde_json::from_slice::<Value>(&data) {
            Ok(Value::Object(map)) => map,
            _ => return Err(std::io::Error::other("meta.json 格式不对，没有覆盖")),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(e) => return Err(e),
    };
    for k in ["title", "tags", "archived"] {
        extra.remove(k);
    }
    let meta = Meta {
        title: normalize_title(meta.title.as_deref()),
        tags: normalize_tags(&meta.tags),
        archived: meta.archived,
    };
    let text = serde_json::to_string_pretty(&Out { meta: &meta, extra }).map_err(std::io::Error::other)?;
    let temp = dir.join(".meta.json.tmp");
    std::fs::write(&temp, text)?;
    std::fs::rename(temp, path)
}

/// 会议文件夹里的录音：录音.*（所有扩展名）、system.wav、mic.wav
pub fn audio_files(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = top_files(dir)
        .into_iter()
        .filter(|p| is_recording(p) || matches!(file_name(p), "system.wav" | "mic.wav"))
        .collect();
    out.sort();
    out
}

/// 残留文件：已经有 录音.* 时还留着的 system.wav / mic.wav（合成录音失败前的原始音轨），以及 *.part、*.tmp
pub fn leftover_files(dir: &Path) -> Vec<PathBuf> {
    let files = top_files(dir);
    let has_recording = files.iter().any(|p| is_recording(p));
    let mut out: Vec<PathBuf> = files
        .into_iter()
        .filter(|p| {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            (has_recording && matches!(file_name(p), "system.wav" | "mic.wav")) || ext == "part" || ext == "tmp"
        })
        .collect();
    out.sort();
    out
}

/// 有没有 录音.* 文件
pub fn has_recording(dir: &Path) -> bool {
    top_files(dir).iter().any(|p| is_recording(p))
}

fn file_name(p: &Path) -> &str {
    p.file_name().and_then(|n| n.to_str()).unwrap_or("")
}

fn is_recording(p: &Path) -> bool {
    file_name(p).strip_prefix("录音.").is_some_and(|ext| !ext.is_empty())
}

/// 文件夹里直接放着的文件（不进子文件夹）
fn top_files(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default()
}

/// 文件（夹）的大小：文件夹按里面所有文件算，符号链接不跟进去
pub fn size_of(path: &Path) -> u64 {
    let Ok(md) = std::fs::symlink_metadata(path) else { return 0 };
    if !md.is_dir() {
        return md.len();
    }
    std::fs::read_dir(path)
        .map(|rd| rd.flatten().map(|e| size_of(&e.path())).sum())
        .unwrap_or(0)
}

pub fn files_size(files: &[PathBuf]) -> u64 {
    files.iter().map(|p| size_of(p)).sum()
}

#[derive(Clone, Debug, Serialize)]
pub struct Summary {
    pub id: String,
    pub title: String,
    pub date: String,
    pub time: String,
    pub summary: String,
    pub has_minutes: bool,
    pub has_transcript: bool,
    pub duration: String,
    pub speakers: Vec<String>,
    pub tags: Vec<String>,
    pub archived: bool,
    /// 整个会议文件夹的大小
    pub size_bytes: u64,
    /// 有没有 录音.* 文件
    pub has_audio: bool,
    /// 录音时 App 被系统关掉、下次启动恢复出来的会议。界面据此提示可以「接着录」
    pub interrupted: bool,
}

/// meta.json 里的一个布尔记号（interrupted 之类），没有或读不出按 false
fn meta_flag(dir: &Path, key: &str) -> bool {
    let v: Option<serde_json::Value> = std::fs::read(dir.join(META)).ok().and_then(|d| serde_json::from_slice(&d).ok());
    v.is_some_and(|m| m[key] == true)
}

/// 录音时 App 被系统关掉、恢复出来的会议做个记号（记在 meta.json 里，其余字段不动）。
pub fn mark_interrupted(dir: &Path) -> std::io::Result<()> {
    let path = dir.join("meta.json");
    let mut meta = match std::fs::read(&path) {
        Ok(data) => serde_json::from_slice(&data).map_err(std::io::Error::other)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(e),
    };
    meta.as_object_mut()
        .ok_or_else(|| std::io::Error::other("Invalid meeting metadata"))?
        .insert("interrupted".into(), true.into());
    let temp = dir.join(".meta.json.tmp");
    std::fs::write(&temp, serde_json::to_vec_pretty(&meta).map_err(std::io::Error::other)?)?;
    std::fs::rename(temp, path)
}

/// 与 Mac 版共用 meta.json 的 title 字段，保留其他字段。
pub fn display_title(dir: &Path) -> Option<String> {
    let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("meta.json")).ok()?).ok()?;
    meta.get("title")?.as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

pub fn set_title(dir: &Path, title: Option<&str>) -> std::io::Result<()> {
    let path = dir.join("meta.json");
    let mut meta: serde_json::Value = match std::fs::read(&path) {
        Ok(data) => serde_json::from_slice(&data).map_err(std::io::Error::other)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(e),
    };
    let object = meta.as_object_mut().ok_or_else(|| std::io::Error::other("Invalid meeting metadata"))?;
    if let Some(title) = title.map(str::trim).filter(|t| !t.is_empty()) {
        object.insert("title".into(), title.chars().take(80).collect::<String>().into());
    } else { object.remove("title"); }
    let temp = dir.join(".meta.json.tmp");
    std::fs::write(&temp, serde_json::to_vec_pretty(&meta).map_err(std::io::Error::other)?)?;
    std::fs::rename(temp, path)
}

/// iOS 没有系统废纸篓：整场会议移到文稿中的「已删除会议」（「最近删除」和 iCloud 同步都认这个目录），
/// 每次删除放进带时间戳的新批次目录，绝不覆盖同名旧会议。
pub fn move_to_deleted(root: &Path, dir: &Path) -> std::io::Result<()> {
    let root = root.canonicalize()?;
    if dir.canonicalize()?.parent() != Some(root.as_path()) {
        return Err(std::io::Error::other("Invalid meeting path"));
    }
    move_into(&root, dir, DELETED_MEETINGS)
}

/// iOS 上「只删录音」：录音文件移到「已删除录音」，保留会议文件夹名，在「文件」App 里能找回。
/// 不放进「已删除会议」，免得在「最近删除」里显示成一场只有录音的会议、被当成删除的会议同步。
pub fn move_audio_to_deleted(root: &Path, file: &Path) -> std::io::Result<()> {
    let root = root.canonicalize()?;
    let meeting = file.canonicalize()?.parent().map(Path::to_path_buf);
    if !file.is_file() || meeting.as_ref().and_then(|m| m.parent()) != Some(root.as_path()) {
        return Err(std::io::Error::other("Invalid recording path"));
    }
    move_into(&root, file, DELETED_AUDIO)
}

pub const DELETED_MEETINGS: &str = "已删除会议";
pub const DELETED_AUDIO: &str = "已删除录音";

fn move_into(root: &Path, path: &Path, bin_name: &str) -> std::io::Result<()> {
    let path = path.canonicalize()?;
    let rel = path.strip_prefix(root).map_err(|_| std::io::Error::other("Invalid meeting path"))?.to_path_buf();
    if rel.as_os_str().is_empty() || rel.starts_with(DELETED_MEETINGS) || rel.starts_with(DELETED_AUDIO) {
        return Err(std::io::Error::other("Invalid meeting path"));
    }
    let bin = root.join(bin_name);
    std::fs::create_dir_all(&bin)?;
    if bin.canonicalize()? != bin { return Err(std::io::Error::other("Invalid deleted meetings path")); }
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let stamp = Local::now().format("%Y%m%d-%H%M%S-%f");
    let batch = bin.join(format!("{}-{}", stamp, NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    let target = batch.join(&rel);
    std::fs::create_dir_all(target.parent().unwrap_or(&batch))?;
    std::fs::rename(&path, target)
}

#[derive(Serialize)]
pub struct DeletedMeeting {
    pub batch: String,
    pub deleted_at: String,
    #[serde(flatten)]
    pub meeting: Summary,
}

pub fn deleted_folder(root: &Path, batch: &str, id: &str) -> std::io::Result<PathBuf> {
    fn component(s: &str) -> bool {
        !s.is_empty() && s != "." && s != ".." && !s.contains(['/', '\\'])
    }
    if !component(batch) || !component(id) { return Err(std::io::Error::other("Invalid deleted meeting")); }
    let bin = root.canonicalize()?.join("已删除会议");
    let path = bin.join(batch).join(id);
    if path.canonicalize()? != path || !path.is_dir() {
        return Err(std::io::Error::other("Invalid deleted meeting path"));
    }
    Ok(path)
}

pub fn deleted(root: &Path) -> Vec<DeletedMeeting> {
    let mut items = Vec::new();
    if let Ok(batches) = std::fs::read_dir(root.join("已删除会议")) {
        for batch in batches.flatten() {
            let b = batch.file_name().to_string_lossy().into_owned();
            if let Ok(entries) = std::fs::read_dir(batch.path()) {
                for entry in entries.flatten() {
                    let id = entry.file_name().to_string_lossy().into_owned();
                    if let Ok(dir) = deleted_folder(root, &b, &id) {
                        let date = b.get(..15).and_then(|s| NaiveDateTime::parse_from_str(s, "%Y%m%d-%H%M%S").ok())
                            .map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default();
                        items.push(DeletedMeeting { batch: b.clone(), deleted_at: date, meeting: summary(&dir) });
                    }
                }
            }
        }
    }
    items.sort_by(|a, b| b.batch.cmp(&a.batch));
    items
}

pub fn restore_deleted(root: &Path, batch: &str, id: &str) -> std::io::Result<PathBuf> {
    let dir = deleted_folder(root, batch, id)?;
    let mut dest = root.join(id);
    let mut n = 2;
    while dest.exists() { dest = root.join(format!("{id}_{n}")); n += 1; }
    std::fs::rename(&dir, &dest)?;
    // Only remove the empty batch, never another deleted meeting.
    let _ = std::fs::remove_dir(dir.parent().unwrap());
    Ok(dest)
}

pub fn summary(dir: &Path) -> Summary {
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    let minutes = std::fs::read_to_string(dir.join(MINUTES)).unwrap_or_default();
    let caps = FOLDER.captures(&name);
    let time = caps
        .as_ref()
        .map(|c| format!("{}:{}", &c[2], &c[3]))
        .unwrap_or_default();
    let fallback = caps
        .as_ref()
        .map(|c| c[4].to_string())
        .unwrap_or_else(|| name.clone());
    let tr = Transcript::load(&dir.join(TRANSCRIPT_JSON)).ok();
    let meta = read_meta(dir);
    Summary {
        // 标题：用户改的 → 纪要第一行 # 标题 → 文件夹名里的会议名
        title: meta.title.or_else(|| minutes_title(&minutes)).unwrap_or(fallback),
        date: meeting_date(dir).format("%Y-%m-%d").to_string(),
        time,
        summary: one_liner(&crate::terms::strip(&minutes)),
        has_minutes: !minutes.is_empty(),
        has_transcript: tr.is_some(),
        duration: tr
            .as_ref()
            .map(|t| fmt_ts(t.duration_ms))
            .unwrap_or_default(),
        speakers: tr
            .as_ref()
            .map(|t| {
                t.speakers_in_order()
                    .into_iter()
                    .map(|s| t.name_of(s))
                    .collect()
            })
            .unwrap_or_default(),
        tags: meta.tags,
        archived: meta.archived,
        size_bytes: size_of(dir),
        has_audio: has_recording(dir),
        interrupted: meta_flag(dir, "interrupted"),
        id: name,
    }
}

pub fn list(root: &Path) -> Vec<Summary> {
    let Ok(rd) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut items: Vec<Summary> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && (p.join(TRANSCRIPT_JSON).is_file() || find_audio(p).is_some()))
        .map(|p| summary(&p))
        .collect();
    items.sort_by(|a, b| (&b.date, &b.time, &b.id).cmp(&(&a.date, &a.time, &a.id)));
    items
}

/// 存储空间统计（不含模型目录和磁盘剩余空间，那两个由调用方补上）。
/// `busy(id)` 为 true 的会议正在处理（比如正在合成录音），不算残留文件。
pub fn storage(root: &Path, busy: impl Fn(&str) -> bool) -> Value {
    let list = list(root);
    let (mut audio_bytes, mut leftover_bytes) = (0u64, 0u64);
    let mut rows: Vec<(u64, Value)> = Vec::new();
    for m in &list {
        let dir = root.join(&m.id);
        let audio = files_size(&audio_files(&dir));
        audio_bytes += audio;
        if !busy(&m.id) {
            leftover_bytes += files_size(&leftover_files(&dir));
        }
        rows.push((
            m.size_bytes,
            serde_json::json!({"id": m.id, "title": m.title, "date": m.date, "bytes": m.size_bytes,
                               "audio_bytes": audio, "archived": m.archived}),
        ));
    }
    // 从大到小；一样大时保持列表顺序（新的在前）
    rows.sort_by_key(|r| std::cmp::Reverse(r.0));
    let largest: Vec<Value> = rows.into_iter().take(10).map(|(_, v)| v).collect();
    serde_json::json!({
        "root": root,
        "count": list.len(),
        "meetings_bytes": list.iter().map(|m| m.size_bytes).sum::<u64>(),
        "audio_bytes": audio_bytes,
        "leftover_bytes": leftover_bytes,
        "largest": largest,
    })
}

pub use crate::platform::free_bytes;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trash_restore_preserves_collision_and_rejects_escape() {
        let root = std::env::temp_dir().join(format!("tingo-trash-{}", uuid::Uuid::new_v4()));
        let dir = root.join("2026-11-07_1200_test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(AUDIO), b"old audio").unwrap();
        std::fs::write(dir.join(MINUTES), "# Archived").unwrap();
        move_to_deleted(&root, &dir).unwrap();
        let items = deleted(&root); assert_eq!(items.len(), 1);
        assert_eq!(items[0].meeting.title, "Archived");
        assert!(deleted_folder(&root, "..", &items[0].meeting.id).is_err());
        assert!(deleted_folder(&root, &items[0].batch, "../outside").is_err());
        std::fs::create_dir_all(&dir).unwrap(); std::fs::write(dir.join(AUDIO), b"new audio").unwrap();
        let restored = restore_deleted(&root, &items[0].batch, &items[0].meeting.id).unwrap();
        assert_ne!(restored, dir);
        assert_eq!(std::fs::read(restored.join(AUDIO)).unwrap(), b"old audio");
        assert_eq!(std::fs::read(dir.join(AUDIO)).unwrap(), b"new audio");
        assert!(deleted(&root).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn meeting_start_comes_from_the_folder_name() {
        let start = meeting_start(Path::new("/会议/2026-11-12_1452_周会"));
        assert_eq!(start.format("%Y-%m-%d %H:%M").to_string(), "2026-11-12 14:52");
        // 同一分钟开始的第二场，文件夹名后面多个 _2，时间照样认得出
        assert_eq!(meeting_start(Path::new("/会议/2026-11-12_1452_周会_2")), start);
    }

    #[test]
    fn interrupted_mark_keeps_the_rest_of_the_metadata() {
        let dir = std::env::temp_dir().join(format!("tingo-interrupted-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!summary(&dir).interrupted);
        set_title(&dir, Some("周会")).unwrap();
        mark_interrupted(&dir).unwrap();
        assert!(summary(&dir).interrupted);
        assert_eq!(summary(&dir).title, "周会");
        // 改标题也不会把记号冲掉
        set_title(&dir, Some("周会（下半场）")).unwrap();
        assert!(summary(&dir).interrupted);
        // 改标签、归档也不会
        write_meta(&dir, &Meta { title: Some("周会（下半场）".into()), tags: vec!["周会".into()], archived: true }).unwrap();
        assert!(summary(&dir).interrupted);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rename_preserves_metadata_and_delete_preserves_files() {
        let root = std::env::temp_dir().join(format!("tingo-manage-{}", std::process::id()));
        let dir = root.join("2026-11-06_1200_test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("meta.json"), r#"{"tags":["work"],"archived":true}"#).unwrap();
        std::fs::write(dir.join(MINUTES), "# Original").unwrap();
        std::fs::write(dir.join("录音.m4a"), b"audio").unwrap();
        set_title(&dir, Some("  New title  ")).unwrap();
        assert_eq!(summary(&dir).title, "New title");
        let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("meta.json")).unwrap()).unwrap();
        assert_eq!(meta["tags"][0], "work");
        assert_eq!(meta["archived"], true);
        set_title(&dir, None).unwrap();
        assert_eq!(summary(&dir).title, "Original");
        std::fs::write(dir.join("meta.json"), "broken").unwrap();
        assert!(set_title(&dir, Some("do not overwrite")).is_err());
        assert_eq!(std::fs::read_to_string(dir.join("meta.json")).unwrap(), "broken");
        assert!(move_to_deleted(&root, &root).is_err());
        for _ in 0..2 {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("录音.m4a"), b"audio").unwrap();
            move_to_deleted(&root, &dir).unwrap();
            assert!(!dir.exists());
            assert!(list(&root).is_empty());
        }
        let batches: Vec<_> = std::fs::read_dir(root.join("已删除会议")).unwrap().flatten().collect();
        assert_eq!(batches.len(), 2);
        for batch in batches {
            assert_eq!(std::fs::read(batch.path().join(dir.file_name().unwrap()).join("录音.m4a")).unwrap(), b"audio");
        }
        // 只删录音：录音进「已删除录音」，会议留在列表里，「最近删除」里不多出一场
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(TRANSCRIPT_JSON), "{}").unwrap();
        std::fs::write(dir.join("录音.m4a"), b"audio").unwrap();
        assert!(move_audio_to_deleted(&root, &dir).is_err());
        move_audio_to_deleted(&root, &dir.join("录音.m4a")).unwrap();
        assert!(!dir.join("录音.m4a").exists() && dir.join(TRANSCRIPT_JSON).exists());
        assert_eq!(list(&root).len(), 1);
        assert_eq!(deleted(&root).len(), 2);
        let audio: Vec<_> = std::fs::read_dir(root.join(DELETED_AUDIO)).unwrap().flatten().collect();
        assert_eq!(std::fs::read(audio[0].path().join(dir.file_name().unwrap()).join("录音.m4a")).unwrap(), b"audio");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn marks_roundtrip_sorted_and_tolerate_missing_or_broken_files() {
        let dir = std::env::temp_dir().join(format!("tingo-marks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(read_marks(&dir).is_empty());
        write_marks(&dir, &[9000, 1200, 9000]).unwrap();
        assert_eq!(read_marks(&dir), vec![1200, 9000]);
        std::fs::write(dir.join(MARKS), "broken").unwrap();
        assert!(read_marks(&dir).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn folder_naming_and_listing() {
        let root = std::env::temp_dir().join(format!("tingo-meet-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let when = NaiveDate::from_ymd_opt(2026, 11, 2)
            .unwrap()
            .and_hms_opt(14, 30, 0)
            .unwrap();
        let a = new_folder(&root, "周 会/评审", when);
        assert_eq!(a.file_name().unwrap(), "2026-11-02_1430_周_会_评审");
        std::fs::create_dir_all(&a).unwrap();
        assert!(new_folder(&root, "周 会/评审", when).ends_with("2026-11-02_1430_周_会_评审_2"));
        std::fs::write(a.join(MINUTES), "# 周会\n\n## 一句话总结\n定了上线时间。\n").unwrap();
        std::fs::write(a.join("录音.m4a"), b"x").unwrap();
        let raw = root.join("2026-11-02_1500_录音中");
        std::fs::create_dir_all(&raw).unwrap();
        std::fs::write(raw.join("system.wav"), b"x").unwrap(); // 录音中的原始轨道不算会议
        let items = list(&root);
        assert_eq!(items.len(), 1);
        assert_eq!(
            (
                items[0].title.as_str(),
                items[0].summary.as_str(),
                items[0].time.as_str()
            ),
            ("周会", "定了上线时间。", "14:30")
        );
        assert_eq!(
            meeting_date(&a),
            NaiveDate::from_ymd_opt(2026, 11, 2).unwrap()
        );
        std::fs::remove_dir_all(root).ok();
    }

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("tingo-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn tags_normalize() {
        let long = "一".repeat(25);
        let tags = normalize_tags(&[" 项目A ", "", "  ", "周会", "项目A", &long, "周会 "]);
        assert_eq!(tags, vec!["项目A".to_string(), "周会".into(), "一".repeat(20)]);
        let many: Vec<String> = (0..15).map(|i| format!("t{i}")).collect();
        let tags = normalize_tags(&many);
        assert_eq!(tags.len(), 10);
        assert_eq!((tags[0].as_str(), tags[9].as_str()), ("t0", "t9"));
        // 截到 20 个字符后末尾的空白也去掉
        assert_eq!(normalize_tag(&format!("{} x", "a".repeat(19))), "a".repeat(19));
        assert_eq!(normalize_title(Some("  ")), None);
        assert_eq!(normalize_title(Some(" 新标题 ")).as_deref(), Some("新标题"));
    }

    #[test]
    fn meta_read_write() {
        let dir = temp_root("meta");
        assert_eq!(read_meta(&dir), Meta::default());
        let meta = Meta { title: Some(" 周会 ".into()), tags: vec!["项目A".into(), "项目A".into(), " 周会".into()], archived: true };
        write_meta(&dir, &meta).unwrap();
        let text = std::fs::read_to_string(dir.join(META)).unwrap();
        assert_eq!(text, "{\n  \"title\": \"周会\",\n  \"tags\": [\n    \"项目A\",\n    \"周会\"\n  ],\n  \"archived\": true\n}");
        assert_eq!(read_meta(&dir), Meta { title: Some("周会".into()), tags: vec!["项目A".into(), "周会".into()], archived: true });
        // 空标题不写这个键
        write_meta(&dir, &Meta { title: Some("".into()), tags: vec![], archived: false }).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(META)).unwrap(), "{\n  \"tags\": [],\n  \"archived\": false\n}");
        // 手改坏的、类型不对的字段按默认值；读的时候也整理标签
        std::fs::write(dir.join(META), "{\"title\": 3, \"tags\": [\" a \", 1, \"a\"], \"archived\": \"yes\"}").unwrap();
        assert_eq!(read_meta(&dir), Meta { title: None, tags: vec!["a".into()], archived: false });
        std::fs::write(dir.join(META), "not json").unwrap();
        assert_eq!(read_meta(&dir), Meta::default());
        // 读不出的文件不覆盖
        assert!(write_meta(&dir, &Meta::default()).is_err());
        assert_eq!(std::fs::read_to_string(dir.join(META)).unwrap(), "not json");
        // 其他字段原样保留，三项写在前面
        std::fs::write(dir.join(META), r#"{"source":"apple_watch","watch_recording_id":"x","interrupted":true,"title":"旧"}"#).unwrap();
        write_meta(&dir, &Meta { title: None, tags: vec!["周会".into()], archived: true }).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(dir.join(META)).unwrap()).unwrap();
        assert_eq!(v, serde_json::json!({"tags":["周会"],"archived":true,"interrupted":true,"source":"apple_watch","watch_recording_id":"x"}));
        assert!(std::fs::read_to_string(dir.join(META)).unwrap().starts_with("{\n  \"tags\""));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn list_fields_and_title_precedence() {
        let root = temp_root("list-fields");
        let a = root.join("2026-10-01_0900_文件夹标题");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join("录音.mp3"), vec![0u8; 100]).unwrap();
        let items = list(&root);
        assert_eq!(items[0].title, "文件夹标题");
        assert_eq!((items[0].tags.len(), items[0].archived, items[0].has_audio, items[0].size_bytes), (0, false, true, 100));
        std::fs::write(a.join(MINUTES), "# 纪要标题\n").unwrap();
        assert_eq!(summary(&a).title, "纪要标题");
        write_meta(&a, &Meta { title: Some("我改的".into()), tags: vec!["周会".into()], archived: true }).unwrap();
        let m = summary(&a);
        assert_eq!((m.title.as_str(), m.tags.clone(), m.archived), ("我改的", vec!["周会".to_string()], true));
        assert_eq!(m.size_bytes, size_of(&a));
        assert!(m.size_bytes > 100);
        // 只有原始音轨（不是 录音.*）时 has_audio 为 false
        let b = root.join("2026-10-02_0900_b");
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(b.join(TRANSCRIPT_JSON), serde_json::to_string(&Transcript::default()).unwrap()).unwrap();
        std::fs::write(b.join("system.wav"), b"x").unwrap();
        assert!(!summary(&b).has_audio);
        let v = serde_json::to_value(summary(&b)).unwrap();
        for k in ["tags", "archived", "size_bytes", "has_audio"] {
            assert!(v.get(k).is_some(), "{k}");
        }
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn audio_and_leftover_files() {
        let dir = temp_root("leftovers");
        for f in ["system.wav", "mic.wav", "mixed.wav", "转写.md", "x.part", "y.TMP"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let names = |v: Vec<PathBuf>| v.iter().map(|p| file_name(p).to_string()).collect::<Vec<_>>();
        assert_eq!(names(audio_files(&dir)), vec!["mic.wav", "system.wav"]);
        // 还没有 录音.*：原始音轨不算残留（可能正在录、或者合成没做）
        assert_eq!(names(leftover_files(&dir)), vec!["x.part", "y.TMP"]);
        std::fs::write(dir.join("录音.m4a"), b"x").unwrap();
        std::fs::write(dir.join("录音.wav"), b"x").unwrap();
        assert_eq!(names(audio_files(&dir)), vec!["mic.wav", "system.wav", "录音.m4a", "录音.wav"]);
        assert_eq!(names(leftover_files(&dir)), vec!["mic.wav", "system.wav", "x.part", "y.TMP"]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn storage_stats() {
        let root = temp_root("storage");
        let mk = |name: &str, files: &[(&str, usize)]| {
            let d = root.join(name);
            std::fs::create_dir_all(&d).unwrap();
            for (f, n) in files {
                std::fs::write(d.join(f), vec![0u8; *n]).unwrap();
            }
            d
        };
        // a：合成好了但原始音轨还在 → 残留；b：只有原始音轨（没有 录音.*）→ 不算残留；c：最大
        mk("2026-10-01_0900_a", &[("录音.m4a", 100), ("system.wav", 30), ("mic.wav", 20), (TRANSCRIPT_JSON, 5)]);
        mk("2026-10-02_0900_b", &[("system.wav", 40), (TRANSCRIPT_JSON, 5), ("dl.part", 7)]);
        let c = mk("2026-10-03_0900_c", &[("录音.mp3", 1000), (MINUTES, 10)]);
        write_meta(&c, &Meta { archived: true, ..Default::default() }).unwrap();
        mk("2026-10-04_0900_not-a-meeting", &[("notes.txt", 5000)]);
        let v = storage(&root, |_| false);
        assert_eq!(v["count"], 3);
        assert_eq!(v["audio_bytes"], 100 + 30 + 20 + 40 + 1000);
        assert_eq!(v["leftover_bytes"], 30 + 20 + 7);
        let meta_len = size_of(&c.join(META));
        assert_eq!(v["meetings_bytes"], 155 + 52 + 1010 + meta_len);
        let largest = v["largest"].as_array().unwrap();
        let ids: Vec<&str> = largest.iter().map(|x| x["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["2026-10-03_0900_c", "2026-10-01_0900_a", "2026-10-02_0900_b"]);
        assert_eq!(largest[0]["audio_bytes"], 1000);
        assert_eq!(largest[0]["archived"], true);
        assert_eq!(largest[0]["date"], "2026-10-03");
        assert_eq!(largest[1]["bytes"], 155);
        // 正在处理的会议不算残留（比如正在合成录音）
        let v = storage(&root, |id| id.ends_with("_a"));
        assert_eq!(v["leftover_bytes"], 7);
        assert!(free_bytes(&root).is_some_and(|n| n > 0));
        assert!(free_bytes(&root.join("还没建/的目录")).is_some());
        std::fs::remove_dir_all(root).ok();
    }
}
