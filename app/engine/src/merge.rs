//! 把几场会议合成一场：录音按先后首尾接起来，转写跟着拼接。
//!
//! 用在两处：会议列表里手动选几场合并（一场会被打断后录成了几段）；「接着录」结束时把新录的一段接到原来那场后面。
//! 这里只建新的会议文件夹，不动原来的——新会议完整建好以后，由调用方把原来的几场移到「已删除会议」。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::asr::SR;
use crate::i18n::tr;
use crate::meetings::{self, AUDIO, TRANSCRIPT_JSON};
use crate::transcript::{Sentence, Transcript};
use crate::{audio, record};

/// 一段：它的转写，和这段录音的实际长度（后面各段的时间戳按它往后挪）
pub struct Part {
    pub transcript: Transcript,
    pub duration_ms: u64,
}

/// 按先后顺序把几段转写拼成一份：后一段的时间戳接在前一段录音的末尾。
///
/// 说话人：区分说话人用的声纹只在转写当时有，事后没法判断两段里的"说话人1"是不是同一个人，所以各段分开编号
/// （第二段的说话人接着第一段往后排）。只有改过名、名字又相同的才算同一个人——那是用户自己认出来的。
pub fn stitch(parts: &[Part], source: &Path) -> Transcript {
    let mut out = Transcript {
        source: source.to_string_lossy().into_owned(),
        ..Default::default()
    };
    let mut next = 0; // 下一个没用过的说话人编号
    let mut named: BTreeMap<String, usize> = BTreeMap::new(); // 改过名的说话人：名字 → 合并后的编号
    let mut offset = 0;
    for part in parts {
        let tr = &part.transcript;
        let mut renumber = BTreeMap::new();
        for spk in tr.speakers_in_order() {
            let name = tr.speaker_names.get(&spk).map(|n| n.trim()).filter(|n| !n.is_empty());
            let merged = match name.and_then(|n| named.get(n).copied()) {
                Some(known) => known,
                None => {
                    let fresh = next;
                    next += 1;
                    if let Some(n) = name {
                        named.insert(n.to_string(), fresh);
                        out.speaker_names.insert(fresh, n.to_string());
                    }
                    fresh
                }
            };
            renumber.insert(spk, merged);
        }
        out.sentences.extend(tr.sentences.iter().map(|s| Sentence {
            speaker: renumber[&s.speaker],
            start_ms: s.start_ms + offset,
            end_ms: s.end_ms + offset,
            text: s.text.clone(),
        }));
        offset += part.duration_ms;
    }
    out.duration_ms = offset;
    out
}

/// 合并的结果
#[derive(Debug, PartialEq)]
pub struct Merged {
    /// 转写是不是已经拼好了。有的段还没转写（比如被中断后恢复出来的录音）时为假，
    /// 调用方要对合并后的录音整段转写
    pub transcribed: bool,
}

/// 把 `sources`（已经按先后排好）合成一场新会议，放在 `dest`（必须还不存在）。`stage` 用来报告做到哪一步了。
/// 失败时不留下半成品，原来的会议一点不动。
pub fn merge(
    bin: &Path,
    sources: &[PathBuf],
    dest: &Path,
    title: &str,
    stage: impl FnMut(String),
) -> Result<Merged, String> {
    merge_with(
        sources,
        dest,
        title,
        &|input, out| record::decode_to_wav(bin, input, out),
        &record::compress,
        stage,
    )
}

type Convert<'a> = &'a dyn Fn(&Path, &Path) -> Result<(), String>;

/// [`merge`] 的本体；解码和压缩由外面给，测试里不依赖系统的音频工具。
fn merge_with(
    sources: &[PathBuf],
    dest: &Path,
    title: &str,
    decode: Convert<'_>,
    compress: Convert<'_>,
    mut stage: impl FnMut(String),
) -> Result<Merged, String> {
    if sources.len() < 2 {
        return Err(tr("至少选两场会议才能合并", "Pick at least two meetings to merge").into());
    }
    if dest.exists() {
        // 出错时会把 dest 整个删掉，所以绝不能是已有的文件夹
        return Err(tr("合并的目标文件夹已经存在", "The merge destination already exists").into());
    }
    let mut temps = Vec::new();
    let result = build(sources, dest, title, decode, compress, &mut stage, &mut temps);
    for t in temps {
        let _ = std::fs::remove_file(t);
    }
    if result.is_err() {
        let _ = std::fs::remove_dir_all(dest);
    }
    result
}

/// 合成录音时的中间文件。这个名字不会被当成会议的录音
pub const MIXED_WAV: &str = "mixed.wav";

fn build(
    sources: &[PathBuf],
    dest: &Path,
    title: &str,
    decode: Convert<'_>,
    compress: Convert<'_>,
    stage: &mut impl FnMut(String),
    temps: &mut Vec<PathBuf>,
) -> Result<Merged, String> {
    let io = |e: std::io::Error| e.to_string();
    let total = sources.len();
    // 每一段的录音先变成同一种 wav（16 kHz 单声道 16 位），已经是的直接用
    let mut wavs = Vec::with_capacity(total);
    for (i, dir) in sources.iter().enumerate() {
        stage(tr(&format!("读取录音 {}/{total}", i + 1), &format!("Reading recording {}/{total}", i + 1)).to_string());
        // 刚录完、要接着合并的那一段：合成录音时留下的 mixed.wav 直接用，不用把刚压好的 m4a 再解回来（少一次有损编码）
        // 已经有 录音.* 才说明 mixed.wav 写完整了（合成录音是先写完 mixed.wav 再压缩）
        let prepared = dir.join(MIXED_WAV);
        let audio = meetings::find_audio(dir).map(|a| if audio::is_wav16(&prepared) { prepared } else { a });
        let audio = audio.ok_or_else(|| {
            let name = meetings::summary(dir).title;
            tr(
                &format!("「{name}」没有录音，不能合并"),
                &format!("“{name}” has no recording, so it can't be merged"),
            )
            .to_string()
        })?;
        if audio::is_wav16(&audio) {
            wavs.push(audio);
        } else {
            let tmp = record::temp_wav("merge");
            temps.push(tmp.clone());
            decode(&audio, &tmp)?;
            wavs.push(tmp);
        }
    }
    std::fs::create_dir_all(dest).map_err(io)?;
    stage(tr("接上录音", "Joining the recordings").to_string());
    // 叫 mixed.wav：这个名字不会被当成会议的录音，中途看会议列表也不会把半成品认成正式录音
    let joined = dest.join(MIXED_WAV);
    let frames = audio::concat_wav16(&wavs, &joined).map_err(io)?;
    stage(tr("压缩录音", "Compressing the recording").to_string());
    compress(&joined, &dest.join(AUDIO))?;
    let _ = std::fs::remove_file(&joined);

    let durations: Vec<u64> = frames.iter().map(|f| f * 1000 / SR as u64).collect();
    let transcripts: Option<Vec<Transcript>> = sources
        .iter()
        .map(|d| Transcript::load(&d.join(TRANSCRIPT_JSON)).ok())
        .collect();
    let transcribed = match transcripts {
        Some(list) => {
            let parts: Vec<Part> = list
                .into_iter()
                .zip(&durations)
                .map(|(transcript, &duration_ms)| Part { transcript, duration_ms })
                .collect();
            meetings::save_transcript(&stitch(&parts, &dest.join(AUDIO)), dest).map_err(io)?;
            true
        }
        None => false,
    };

    // 记下是由哪几场合成的、各自从第几毫秒开始，以后要找某一段的原始会议（在「已删除会议」里）有据可查
    let mut start_ms = 0;
    let merged_from: Vec<serde_json::Value> = sources
        .iter()
        .zip(&durations)
        .map(|(dir, &duration_ms)| {
            let part = json!({
                "id": dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                "title": meetings::summary(dir).title,
                "start_ms": start_ms,
                "duration_ms": duration_ms,
            });
            start_ms += duration_ms;
            part
        })
        .collect();
    let meta = serde_json::to_vec_pretty(&json!({ "merged_from": merged_from })).map_err(|e| e.to_string())?;
    std::fs::write(dest.join("meta.json"), meta).map_err(io)?;
    // 标签取各段的并集；归档不带过来，合并出来的新会议默认显示在列表里
    let tags: Vec<String> = sources.iter().flat_map(|d| meetings::read_meta(d).tags).collect();
    let meta = meetings::Meta { title: meetings::normalize_title(Some(title)), tags, archived: false };
    meetings::write_meta(dest, &meta).map_err(io)?;
    Ok(Merged { transcribed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::write_wav16;

    fn sentence(speaker: usize, start_ms: u64, text: &str) -> Sentence {
        Sentence { speaker, start_ms, end_ms: start_ms + 900, text: text.into() }
    }

    fn transcript(names: &[(usize, &str)], sentences: Vec<Sentence>) -> Transcript {
        Transcript {
            speaker_names: names.iter().map(|(i, n)| (*i, n.to_string())).collect(),
            duration_ms: sentences.last().map_or(0, |s| s.end_ms),
            sentences,
            ..Default::default()
        }
    }

    #[test]
    fn stitch_shifts_time_and_keeps_unnamed_speakers_apart() {
        let first = transcript(&[], vec![sentence(0, 0, "开场"), sentence(1, 1000, "收到")]);
        let second = transcript(&[], vec![sentence(0, 500, "接着说")]);
        let out = stitch(
            &[Part { transcript: first, duration_ms: 60_000 }, Part { transcript: second, duration_ms: 30_000 }],
            Path::new("录音.m4a"),
        );
        assert_eq!(out.duration_ms, 90_000);
        // 第二段从第一段录音的末尾（60 秒）往后排，不是从第一段最后一句话之后
        assert_eq!(out.sentences[2].start_ms, 60_500);
        assert_eq!(out.sentences[2].end_ms, 61_400);
        // 没改过名的说话人没法知道是不是同一个人：第二段的"说话人1"排成第三个
        assert_eq!(out.sentences.iter().map(|s| s.speaker).collect::<Vec<_>>(), [0, 1, 2]);
        assert_eq!(out.name_of(2), "说话人3");
        assert!(out.speaker_names.is_empty());
    }

    #[test]
    fn stitch_treats_speakers_renamed_to_the_same_name_as_one_person() {
        let first = transcript(&[(0, "张三")], vec![sentence(0, 0, "我先说"), sentence(1, 1000, "好")]);
        // 第二段里张三是第二个开口的，编号和第一段不一样
        let second = transcript(&[(1, " 张三 "), (0, "李四")], vec![sentence(0, 0, "我补充"), sentence(1, 1000, "同意")]);
        let out = stitch(
            &[Part { transcript: first, duration_ms: 10_000 }, Part { transcript: second, duration_ms: 10_000 }],
            Path::new("录音.m4a"),
        );
        let who: Vec<String> = out.sentences.iter().map(|s| out.name_of(s.speaker)).collect();
        assert_eq!(who, ["张三", "说话人2", "李四", "张三"]);
        assert_eq!(out.sentences[0].speaker, out.sentences[3].speaker);
    }

    fn scratch(name: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("tingo-merge-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 一场会议：`secs` 秒的录音（wav，每个采样都是 `level`），可选的转写
    fn meeting(root: &Path, name: &str, secs: usize, level: f32, tr: Option<Transcript>) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        write_wav16(&dir.join("录音.wav"), &vec![level; SR * secs]).unwrap();
        if let Some(tr) = tr {
            meetings::save_transcript(&tr, &dir).unwrap();
        }
        dir
    }

    fn no_decode(_: &Path, _: &Path) -> Result<(), String> {
        panic!("这些录音已经是标准 wav，不该再解码")
    }

    /// 测试里的"压缩"：原样拷过去，方便检查接出来的声音
    fn copy(wav: &Path, dest: &Path) -> Result<(), String> {
        std::fs::copy(wav, dest).map(|_| ()).map_err(|e| e.to_string())
    }

    #[test]
    fn merge_joins_audio_and_transcripts_and_leaves_the_originals_alone() {
        let root = scratch("ok");
        let a = meeting(&root, "2026-11-12_1400_周会", 2, 0.25, Some(transcript(&[], vec![sentence(0, 0, "上半场")])));
        let b = meeting(&root, "2026-11-12_1420_周会", 1, 0.5, Some(transcript(&[], vec![sentence(0, 200, "下半场")])));
        meetings::write_meta(&a, &meetings::Meta { title: None, tags: vec!["项目A".into()], archived: true }).unwrap();
        meetings::write_meta(&b, &meetings::Meta { title: None, tags: vec!["项目A".into(), "周会".into()], archived: false }).unwrap();
        let dest = root.join("2026-11-12_1400_周会_2");
        let mut stages = Vec::new();
        let merged = merge_with(&[a.clone(), b.clone()], &dest, "周会", &no_decode, &copy, |s| stages.push(s)).unwrap();
        assert_eq!(merged, Merged { transcribed: true });
        assert!(!stages.is_empty());

        // 录音：2 秒接 1 秒，先后顺序对
        let samples = audio::read_wav16k(&dest.join(AUDIO)).unwrap();
        assert_eq!(samples.len(), SR * 3);
        assert!((samples[SR].abs() - 0.25).abs() < 0.01 && (samples[SR * 2 + 100].abs() - 0.5).abs() < 0.01);
        assert!(!dest.join("mixed.wav").exists());

        // 转写：第二段挪到 2 秒之后；说话人分开
        let tr = Transcript::load(&dest.join(TRANSCRIPT_JSON)).unwrap();
        assert_eq!(tr.duration_ms, 3000);
        assert_eq!(tr.sentences[1].start_ms, 2200);
        assert_eq!(tr.sentences[1].speaker, 1);
        assert!(dest.join(meetings::TRANSCRIPT_MD).is_file());

        // 标题和来源记在 meta.json 里
        assert_eq!(meetings::display_title(&dest).as_deref(), Some("周会"));
        let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(dest.join("meta.json")).unwrap()).unwrap();
        assert_eq!(meta["merged_from"][1]["id"], "2026-11-12_1420_周会");
        assert_eq!(meta["merged_from"][1]["start_ms"], 2000);
        assert_eq!(meta["merged_from"][1]["duration_ms"], 1000);
        // 标签取并集，归档不带过来
        assert_eq!(meetings::read_meta(&dest).tags, vec!["项目A".to_string(), "周会".to_string()]);
        assert!(!meetings::read_meta(&dest).archived);

        // 原来的两场原封不动
        for dir in [&a, &b] {
            assert!(dir.join("录音.wav").is_file() && dir.join(TRANSCRIPT_JSON).is_file());
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn merge_without_every_transcript_only_joins_the_audio() {
        let root = scratch("untranscribed");
        let a = meeting(&root, "2026-11-12_1400_周会", 1, 0.25, Some(transcript(&[], vec![sentence(0, 0, "上半场")])));
        // 被中断后恢复出来的录音：只有声音，还没转写
        let b = meeting(&root, "2026-11-12_1420_周会", 1, 0.5, None);
        let dest = root.join("合并");
        let merged = merge_with(&[a, b], &dest, "周会", &no_decode, &copy, |_| {}).unwrap();
        assert_eq!(merged, Merged { transcribed: false });
        assert!(dest.join(AUDIO).is_file());
        // 只拼一半的转写会让人以为后半场没人说话，所以干脆不写，等整段重新转写
        assert!(!dest.join(TRANSCRIPT_JSON).exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn merge_decodes_recordings_that_are_not_plain_wav() {
        let root = scratch("decode");
        let a = meeting(&root, "2026-11-12_1400_周会", 1, 0.25, None);
        let b = root.join("2026-11-12_1420_周会");
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(b.join(AUDIO), b"not really aac").unwrap();
        // "解码"：不管输入是什么，给出 1 秒的标准 wav
        let decode = |input: &Path, out: &Path| {
            assert!(input.ends_with(AUDIO));
            write_wav16(out, &vec![0.5; SR]).map_err(|e| e.to_string())
        };
        let dest = root.join("合并");
        merge_with(&[a, b], &dest, "周会", &decode, &copy, |_| {}).unwrap();
        assert_eq!(audio::read_wav16k(&dest.join(AUDIO)).unwrap().len(), SR * 2);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn merge_uses_the_mixed_wav_left_by_continue_recording() {
        // 接着录的那一段：录音.m4a 已经压好，旁边留着合成时的 mixed.wav，直接用它，不再解码
        let root = scratch("prepared");
        let a = meeting(&root, "2026-11-12_1400_周会", 1, 0.25, None);
        let b = root.join("2026-11-12_1420_周会");
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(b.join(AUDIO), b"not really aac").unwrap();
        write_wav16(&b.join(MIXED_WAV), &vec![0.5; SR * 2]).unwrap();
        let dest = root.join("合并");
        merge_with(&[a, b], &dest, "周会", &no_decode, &copy, |_| {}).unwrap();
        assert_eq!(audio::read_wav16k(&dest.join(AUDIO)).unwrap().len(), SR * 3);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn failed_merge_leaves_nothing_behind() {
        let root = scratch("fail");
        let a = meeting(&root, "2026-11-12_1400_周会", 1, 0.25, None);
        let no_audio = root.join("2026-11-12_1420_只有转写");
        std::fs::create_dir_all(&no_audio).unwrap();
        meetings::save_transcript(&transcript(&[], vec![sentence(0, 0, "没有录音")]), &no_audio).unwrap();
        let dest = root.join("合并");
        let err = merge_with(&[a.clone(), no_audio], &dest, "周会", &no_decode, &copy, |_| {}).unwrap_err();
        assert!(err.contains("没有录音") || err.contains("no recording"), "{err}");
        assert!(!dest.exists());

        // 压缩失败（比如磁盘满了）：半成品文件夹也要清掉
        let b = meeting(&root, "2026-11-12_1430_周会", 1, 0.5, None);
        let broken = |_: &Path, _: &Path| Err("磁盘满了".to_string());
        assert_eq!(merge_with(&[a.clone(), b.clone()], &dest, "周会", &no_decode, &broken, |_| {}).unwrap_err(), "磁盘满了");
        assert!(!dest.exists());

        // 目标已经存在时什么都不做，更不会删它
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("重要.txt"), "别删").unwrap();
        assert!(merge_with(&[a, b], &dest, "周会", &no_decode, &copy, |_| {}).is_err());
        assert!(dest.join("重要.txt").is_file());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn merge_needs_at_least_two_meetings() {
        let root = scratch("one");
        let a = meeting(&root, "2026-11-12_1400_周会", 1, 0.25, None);
        assert!(merge_with(&[a], &root.join("合并"), "周会", &no_decode, &copy, |_| {}).is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}
