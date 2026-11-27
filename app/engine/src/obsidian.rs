//! 把纪要存进 Obsidian 库：一场会议一篇笔记（带属性），鱼骨图另存为 Excalidraw 画板嵌进笔记，
//! 转写全文放在折叠的引用块里。重复导出覆盖同名笔记。
//! 笔记里程序加的文字（标签、转写一节、画板文件名）跟纪要的语言走；报错跟界面语言走。

use crate::config::ObsidianConfig;
use crate::fishbone::{self, fmt_ms};
use crate::i18n::{doc_lang, tr, Lang};
use crate::meetings::{self, MINUTES, TRANSCRIPT_JSON};
use crate::transcript::Transcript;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// 本机 Obsidian 打开过的库（读 Obsidian 自己的配置），设置页里直接选。
pub fn vaults() -> Vec<(String, String)> {
    let Some(p) = obsidian_json() else { return vec![] };
    let Ok(v) = std::fs::read_to_string(p).map(|s| serde_json::from_str::<Value>(&s).unwrap_or_default()) else {
        return vec![];
    };
    let mut out: Vec<(u64, String, String)> = v["vaults"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(_, x)| {
            let path = x["path"].as_str()?;
            Path::new(path).is_dir().then(|| {
                let name = Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                (x["ts"].as_u64().unwrap_or(0), name, path.to_string())
            })
        })
        .collect();
    out.sort_by_key(|b| std::cmp::Reverse(b.0)); // 最近打开的在前
    out.into_iter().map(|(_, n, p)| (n, p)).collect()
}

/// Obsidian 自己的配置文件（记着打开过哪些库）
fn obsidian_json() -> Option<PathBuf> {
    let dir = if cfg!(target_os = "macos") {
        crate::platform::home_dir()?.join("Library/Application Support")
    } else if cfg!(windows) {
        PathBuf::from(std::env::var_os("APPDATA")?)
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| Some(crate::platform::home_dir()?.join(".config")))?
    };
    Some(dir.join("obsidian").join("obsidian.json"))
}

/// 文件名里不能出现的字符（Obsidian 链接也不认 # ^ [ ] |）
pub fn safe_name(s: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| if "/\\:*?\"<>|#^[]".contains(c) || c.is_control() { ' ' } else { c })
        .collect();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn yaml_str(s: &str) -> String {
    serde_json::to_string(s).unwrap() // JSON 字符串也是合法的 YAML 字符串
}

/// 打开某篇笔记的 obsidian:// 链接
pub fn uri(path: &Path) -> String {
    let mut out = String::from("obsidian://open?path=");
    for b in path.to_string_lossy().bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub struct Exported {
    pub note: PathBuf,
    pub drawings: Vec<PathBuf>,
}

pub fn export(cfg: &ObsidianConfig, dir: &Path) -> Result<Exported, String> {
    export_version(cfg, dir, None)
}

pub fn export_version(cfg: &ObsidianConfig, dir: &Path, version: Option<&str>) -> Result<Exported, String> {
    let vault = PathBuf::from(cfg.vault.trim());
    if cfg.vault.trim().is_empty() || !vault.is_dir() {
        return Err(tr(
            "还没有设置 Obsidian 库，或者库的文件夹不存在，去设置里选一下",
            "No Obsidian vault is set, or its folder doesn't exist. Pick one in Settings",
        )
        .into());
    }
    let minutes = if let Some(id) = version { crate::minutes_history::read(dir, id).map(|v| v.text) } else { std::fs::read_to_string(dir.join(MINUTES)) }
        .map_err(|_| tr("这场会议还没有纪要", "This meeting has no minutes yet").to_string())?;
    // 术语表只给搜索用，笔记里不要
    let minutes = crate::terms::strip(&minutes);
    let lang = doc_lang(&minutes);
    // 英文纪要里没改名的说话人叫 Speaker N
    let transcript = Transcript::load(&dir.join(TRANSCRIPT_JSON)).ok().map(|t| t.localized(lang));
    let id = dir.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let date = meetings::meeting_date(dir);
    // 用会议文件夹的标题命名（重新生成纪要标题会变，文件名不跟着变，避免同一场会出现多篇）
    let topic = id.splitn(3, '_').nth(2).unwrap_or(&id);
    let suffix = version.map(|v| format!("-history-{v}")).unwrap_or_default();
    let name = safe_name(&format!("{} {}{suffix}", date.format("%Y-%m-%d"), topic));
    let folder = vault.join(cfg.folder.trim().trim_matches('/'));
    std::fs::create_dir_all(&folder).map_err(|e| {
        let f = folder.display();
        tr(&format!("建不了文件夹 {f}：{e}"), &format!("Couldn't create folder {f}: {e}")).to_string()
    })?;

    let turns = transcript.as_ref().map(|t| t.turns()).unwrap_or_default();
    let duration = transcript.as_ref().map(|t| t.duration_ms).unwrap_or(u64::MAX);
    let mut body = String::new();
    let mut last = 0;
    let mut drawings = Vec::new();
    for (k, (range, spec)) in fishbone::blocks(&minutes).into_iter().enumerate() {
        body.push_str(&minutes[last..range.start]);
        last = range.end;
        let Some(spec) = spec else { continue };
        let grounds: Vec<_> = spec.branches.iter().map(|b| fishbone::ground(b, &turns, duration)).collect();
        let draw_name = format!(
            "{name} {}{}",
            lang.pick("鱼骨图", "fishbone"),
            if k == 0 { String::new() } else { (k + 1).to_string() }
        );
        let path = folder.join(format!("{draw_name}.excalidraw.md"));
        std::fs::write(&path, fishbone::excalidraw_md(&fishbone::excalidraw(&spec, &grounds, lang))).map_err(|e| e.to_string())?;
        drawings.push(path);
        body.push_str(&format!("![[{draw_name}.excalidraw]]\n\n**{}**\n", spec.problem));
        let colon = lang.pick("：", ": ");
        for b in &spec.branches {
            let detail = if b.detail.is_empty() { String::new() } else { format!("{colon}{}", b.detail) };
            body.push_str(&format!("- **{}**{detail}\n", b.title));
        }
    }
    body.push_str(&minutes[last..]);

    let title = meetings::minutes_title(&minutes).unwrap_or_else(|| topic.to_string());
    let mut fm = vec![
        "---".to_string(),
        format!("title: {}", yaml_str(&title)),
        format!("date: {}", date.format("%Y-%m-%d")),
    ];
    if let Some(t) = &transcript {
        fm.push(format!("duration: {}", yaml_str(&fmt_ms(t.duration_ms))));
        let names: Vec<String> = t.speakers_in_order().iter().map(|&s| yaml_str(&t.name_of(s))).collect();
        fm.push(format!("speakers: [{}]", names.join(", ")));
    }
    fm.push(lang.pick("tags: [会议纪要]", "tags: [meeting-notes]").into());
    fm.push(format!("tingo: {}", yaml_str(&id)));
    fm.push("---\n".into());
    let mut note = fm.join("\n") + body.trim_end() + "\n";

    if cfg.transcript {
        if let Some(t) = &transcript {
            let (dur, n) = (fmt_ms(t.duration_ms), t.speakers_in_order().len());
            note.push_str(&match lang {
                Lang::Zh => format!("\n## 转写\n\n> [!quote]- 转写全文（{dur}，{n} 人）\n"),
                Lang::En => format!("\n## Transcript\n\n> [!quote]- Full transcript ({dur}, {n} speakers)\n"),
            });
            for turn in &turns {
                note.push_str(&format!(">\n> **{}** `{}`\n> {}\n", turn.speaker, fmt_ms(turn.start_ms), turn.text));
            }
        }
    }
    let path = folder.join(format!("{name}.md"));
    std::fs::write(&path, note).map_err(|e| {
        let p = path.display();
        tr(&format!("写不进 {p}：{e}"), &format!("Couldn't write {p}: {e}")).to_string()
    })?;
    Ok(Exported { note: path, drawings })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_note_with_drawing_and_transcript() {
        let base = std::env::temp_dir().join(format!("tingo-obs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("meetings/2026-11-02_1030_发布评审");
        let vault = base.join("vault");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&vault).unwrap();
        let tr: Transcript = serde_json::from_str(
            r#"{"source":"a.wav","duration_ms":52000,"speaker_names":{"0":"李经理","1":"小张"},
                "sentences":[{"speaker":0,"start_ms":0,"end_ms":8000,"text":"今天讨论发布。"},
                             {"speaker":1,"start_ms":9000,"end_ms":18000,"text":"支付模块还有两个问题没修，十五号才能提测。"}]}"#,
        )
        .unwrap();
        tr.save(&dir.join(TRANSCRIPT_JSON)).unwrap();
        std::fs::write(
            dir.join(MINUTES),
            "# 发布评审会\n\n## 原因分析\n```fishbone\n{\"problem\": \"为什么延期？\", \"branches\": [{\"title\": \"支付有问题\", \"refs\": [\"00:09\"]}, {\"title\": \"猜的\", \"refs\": []}]}\n```\n\n```terms\n{\"terms\": [{\"term\": \"支付\", \"variants\": []}]}\n```\n",
        )
        .unwrap();
        let cfg = ObsidianConfig { vault: vault.to_string_lossy().into(), folder: "会议纪要".into(), auto: true, transcript: true };
        let out = export(&cfg, &dir).unwrap();
        assert_eq!(out.note, vault.join("会议纪要/2026-11-02 发布评审.md"));
        let note = std::fs::read_to_string(&out.note).unwrap();
        assert!(note.starts_with("---\ntitle: \"发布评审会\"\ndate: 2026-11-02\n"));
        assert!(note.contains("speakers: [\"李经理\", \"小张\"]"));
        assert!(note.contains("![[2026-11-02 发布评审 鱼骨图.excalidraw]]"));
        assert!(note.contains("- **猜的**"));
        assert!(!note.contains("    - `00:09` 小张：支付模块"));
        assert!(note.contains("> [!quote]- 转写全文（00:52，2 人）"));
        assert!(!note.contains("```fishbone") && !note.contains("```terms")); // 术语表不进笔记
        assert!(out.drawings[0].ends_with("2026-11-02 发布评审 鱼骨图.excalidraw.md"));
        assert_eq!(uri(Path::new("/a b/会")), "obsidian://open?path=/a%20b/%E4%BC%9A");
        let old_note = note.clone();
        crate::minutes_history::save(&dir, "# 最新纪要\n新的内容").unwrap();
        let versions = crate::minutes_history::list(&dir).unwrap();
        let current = export(&cfg, &dir).unwrap();
        let historical = export_version(&cfg, &dir, Some(&versions[0].id)).unwrap();
        assert_ne!(current.note, historical.note);
        assert!(std::fs::read_to_string(&current.note).unwrap().contains("最新纪要"));
        assert_eq!(std::fs::read_to_string(&historical.note).unwrap().replace(&format!("-history-{}", versions[0].id), ""), old_note);
        assert!(export_version(&cfg, &dir, Some("999")).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn english_minutes_export_in_english() {
        let base = std::env::temp_dir().join(format!("tingo-obs-en-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("meetings/2026-11-02_1030_Release_review");
        let vault = base.join("vault");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&vault).unwrap();
        let tr: Transcript = serde_json::from_str(
            r#"{"source":"a.wav","duration_ms":52000,"speaker_names":{"0":"Lin"},
                "sentences":[{"speaker":0,"start_ms":0,"end_ms":8000,"text":"Let's talk about the release."},
                             {"speaker":1,"start_ms":9000,"end_ms":18000,"text":"Payments still has two open bugs."}]}"#,
        )
        .unwrap();
        tr.save(&dir.join(TRANSCRIPT_JSON)).unwrap();
        std::fs::write(
            dir.join(MINUTES),
            "# Release review\n\n## Summary\nSlipping.\n\n## Root Cause Analysis\n```fishbone\n{\"problem\": \"Why late?\", \"branches\": [{\"title\": \"Payment bugs\", \"detail\": \"two left\", \"refs\": [\"00:09\"]}, {\"title\": \"Guess\", \"refs\": []}]}\n```\n",
        )
        .unwrap();
        let cfg = ObsidianConfig { vault: vault.to_string_lossy().into(), folder: "Meetings".into(), auto: true, transcript: true };
        let out = export(&cfg, &dir).unwrap();
        let note = std::fs::read_to_string(&out.note).unwrap();
        assert!(note.contains("tags: [meeting-notes]"));
        assert!(note.contains("speakers: [\"Lin\", \"Speaker 2\"]"));
        assert!(note.contains("![[2026-11-02 Release_review fishbone.excalidraw]]"));
        assert!(note.contains("- **Payment bugs**: two left\n"));
        assert!(note.contains("- **Guess**"));
        assert!(note.contains("\n## Transcript\n\n> [!quote]- Full transcript (00:52, 2 speakers)\n"));
        assert!(note.contains("> **Speaker 2** `00:09`"));
        assert!(out.drawings[0].ends_with("2026-11-02 Release_review fishbone.excalidraw.md"));
        // transcript.json 里的名字不动
        assert_eq!(Transcript::load(&dir.join(TRANSCRIPT_JSON)).unwrap().name_of(1), "说话人2");
        let _ = std::fs::remove_dir_all(&base);
    }
}
