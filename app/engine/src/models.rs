//! 识别模型：首次启动时下载（约 330 MB），并把 FunASR 官方 ONNX 转成 sherpa-onnx 能加载的样子。
//! 另外两个可选的识别模型（SenseVoice、Fun-ASR-Nano）在设置里按需下载，见 [`catalog`]。
//!
//! - 识别：FunASR Paraformer-large int8（ModelScope，国内直连）。sherpa-onnx 要求模型里带
//!   lfr_window_size / neg_mean / inv_stddev / vocab_size 等元数据，官方文件没有，下载后补上。
//! - 标点、声纹、VAD：sherpa-onnx 的发布文件（GitHub）。
//!
//! 补元数据不解析整个模型：protobuf 的消息在末尾追加字段等于给它加字段，
//! 所以直接在文件末尾追加 ModelProto.metadata_props（字段 14）即可，238 MB 的模型不用整个读进内存。

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::asr::{AsrKind, ModelPaths};
use crate::i18n::tr;

const MODELSCOPE: &str = "https://modelscope.cn/models/damo/speech_paraformer-large_asr_nat-zh-cn-16k-common-vocab8404-onnx/resolve/master";
const SHERPA: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download";
/// 国内镜像：sherpa-onnx 作者在 ModelScope、HuggingFace 上传的同一批模型
const MS_PUNCT: &str =
    "https://modelscope.cn/models/csukuangfj/sherpa-onnx-punct-ct-transformer-zh-en-vocab272727-2024-04-12/resolve/master";
const HF_MIRROR: &str = "https://hf-mirror.com/csukuangfj";
const PUNCT_TAR: &str = "sherpa-onnx-punct-ct-transformer-zh-en-vocab272727-2024-04-12-int8";

/// 模型从哪儿下：界面里让用户选。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// 国内镜像：识别模型、标点走 ModelScope，声纹、VAD 走 hf-mirror。标点模型镜像上只有完整版，比量化版大 230 MB
    Cn,
    /// GitHub 官方发布：体积最小，国内经常很慢
    Github,
}

impl Source {
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "cn" => Some(Self::Cn),
            "github" => Some(Self::Github),
            _ => None,
        }
    }
}

struct Download {
    url: String,
    /// 下载到模型目录下的这个相对路径
    to: &'static str,
    /// 大致大小，用来算总进度
    size: u64,
}

/// 设置页里列出的识别模型
pub struct ModelInfo {
    pub kind: AsrKind,
    pub name: &'static str,
    pub desc: &'static str,
    /// 下载大小（字节）
    pub size: u64,
}

pub fn catalog() -> [ModelInfo; 4] {
    [
        ModelInfo { kind: AsrKind::AppleSpeech, name: "Apple Speech",
            desc: tr("macOS 26+ 系统本地识别；请明确选择中文或英文。系统管理语言模型，仍需基础声纹模型", "On-device system speech on macOS 26+. Select Chinese or English. Language assets are system-managed; shared speaker models are still needed"), size: 0 },
        ModelInfo {
            kind: AsrKind::Paraformer,
            name: "Paraformer-large",
            desc: tr(
                "默认。中文准、速度快、内存占用小；热词交给大模型在纪要里纠正",
                "Default. Accurate for Chinese, fast, light on memory; hotwords are corrected by the language model in the minutes. Chinese only",
            ),
            size: paraformer_downloads().iter().map(|d| d.size).sum(),
        },
        ModelInfo {
            kind: AsrKind::SenseVoice,
            name: "SenseVoice",
            desc: tr(
                "多语种：中、英、日、韩、粤语混着说也能认；纯中文会议的错字比默认模型多一些",
                "Multilingual: Chinese, English, Japanese, Korean and Cantonese, even mixed; a few more errors than the default on all-Chinese meetings",
            ),
            size: 165_783_878,
        },
        ModelInfo {
            kind: AsrKind::FunAsrNano,
            name: "Fun-ASR-Nano",
            desc: tr(
                "最准、断句最自然，识别时直接用上热词（人名、项目代号）；速度约为默认的 1/4，内存约 2 GB",
                "Most accurate with the most natural sentence breaks, and uses hotwords (names, project code names) while recognizing; about 1/4 the speed of the default, about 2 GB of memory",
            ),
            size: 841_730_611,
        },
    ]
}

/// 可选模型的发布包（sherpa-onnx 打包好的，解压即用）
fn archive(kind: AsrKind) -> Option<&'static str> {
    match kind {
        AsrKind::Paraformer | AsrKind::AppleSpeech => None,
        AsrKind::SenseVoice => Some("sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09"),
        AsrKind::FunAsrNano => Some("sherpa-onnx-funasr-nano-int8-2025-12-30"),
    }
}

fn paraformer_downloads() -> Vec<Download> {
    vec![
        Download { url: format!("{MODELSCOPE}/model_quant.onnx"), to: "download/paraformer.onnx", size: 238_380_216 },
        Download { url: format!("{MODELSCOPE}/am.mvn"), to: "download/am.mvn", size: 11_203 },
        Download { url: format!("{MODELSCOPE}/tokens.json"), to: "download/tokens.json", size: 93_676 },
    ]
}

/// VAD、标点、声纹：所有识别模型共用
fn base_downloads(source: Source) -> Vec<Download> {
    if source == Source::Cn {
        // 和 GitHub 上的结果实测一致：分段、说话人完全相同，只是标点偶尔差一个逗号（完整版 vs 量化版）
        return vec![
            Download { url: format!("{MS_PUNCT}/model.onnx"), to: "punct/model.onnx", size: 294_372_519 },
            Download {
                url: format!("{HF_MIRROR}/speaker-embedding-models/resolve/main/3dspeaker_speech_campplus_sv_zh-cn_16k-common.onnx"),
                to: "campplus_zh.onnx",
                size: 28_281_138,
            },
            Download { url: format!("{HF_MIRROR}/vad/resolve/main/silero_vad.onnx"), to: "silero_vad.onnx", size: 1_807_522 },
        ];
    }
    vec![
        Download { url: format!("{SHERPA}/punctuation-models/{PUNCT_TAR}.tar.bz2"), to: "download/punct.tar.bz2", size: 64_000_000 },
        Download {
            url: format!("{SHERPA}/speaker-recongition-models/3dspeaker_speech_campplus_sv_zh-cn_16k-common.onnx"),
            to: "campplus_zh.onnx",
            size: 28_281_138,
        },
        Download { url: format!("{SHERPA}/asr-models/silero_vad.onnx"), to: "silero_vad.onnx", size: 643_854 },
    ]
}

fn downloads(source: Source) -> Vec<Download> {
    let mut v = paraformer_downloads();
    v.extend(base_downloads(source));
    v
}

pub fn total_size(source: Source) -> u64 {
    downloads(source).iter().map(|d| d.size).sum()
}

/// 下载一个识别模型（连同还没下载的基础模型）。`progress(已下载字节, 总字节)`。
pub fn ensure_model(
    paths: &ModelPaths,
    kind: AsrKind,
    source: Source,
    mut progress: impl FnMut(u64, u64),
) -> Result<(), String> {
    if kind == AsrKind::AppleSpeech {
        let list = base_downloads(source);
        fetch_all(paths, &list, &mut progress)?;
        if !paths.punct().is_file() { extract_punct(&paths.dir.join("download/punct.tar.bz2"), paths.punct().parent().unwrap())?; }
        cleanup(paths, &list);
        return if paths.base_ready() { Ok(()) } else { Err("Shared speech models are incomplete".into()) };
    }
    let Some(name) = archive(kind) else { return ensure(paths, source, progress) };
    if paths.base_ready() && paths.asr_ready(kind) {
        return Ok(());
    }
    // SenseVoice、Fun-ASR-Nano 的模型包只在 GitHub 上有
    let mut list = if paths.base_ready() { Vec::new() } else { base_downloads(source) };
    let size = catalog().iter().find(|m| m.kind == kind).map(|m| m.size).unwrap_or(0);
    let tar_to: &'static str = match kind {
        AsrKind::SenseVoice => "download/sensevoice.tar.bz2",
        _ => "download/funasr-nano.tar.bz2",
    };
    list.push(Download { url: format!("{SHERPA}/asr-models/{name}.tar.bz2"), to: tar_to, size });
    fetch_all(paths, &list, &mut progress)?;
    if !paths.punct().is_file() {
        extract_punct(&paths.dir.join("download/punct.tar.bz2"), paths.punct().parent().unwrap())?;
    }
    extract_dir(&paths.dir.join(tar_to), &paths.asr_dir(kind))?;
    cleanup(paths, &list);
    if paths.base_ready() && paths.asr_ready(kind) {
        Ok(())
    } else {
        Err(tr(
            "模型解压完了但文件不全，删掉后重新下载试试",
            "The model was unpacked but some files are missing; delete it and download again",
        )
        .into())
    }
}

/// 删除一个可选的识别模型（默认的 Paraformer 不删）
pub fn remove_model(paths: &ModelPaths, kind: AsrKind) -> Result<(), String> {
    if matches!(kind, AsrKind::Paraformer | AsrKind::AppleSpeech) {
        return Err(tr("默认模型不能删除", "The default model can't be deleted").into());
    }
    fs::remove_dir_all(paths.asr_dir(kind)).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e.to_string()) })
}

fn ok_marker(dest: &Path) -> PathBuf {
    dest.with_extension(format!("{}.ok", dest.extension().and_then(|e| e.to_str()).unwrap_or("")))
}

fn fetch_all(paths: &ModelPaths, list: &[Download], progress: &mut impl FnMut(u64, u64)) -> Result<(), String> {
    let total: u64 = list.iter().map(|d| d.size).sum();
    let mut done_before = 0u64;
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        // 阻塞客户端的超时按每次等待算（等响应头、每读一块），不是整个下载：
        // 连着但不来数据（国内连 GitHub 常见）30 秒就断开，从断点重连
        .timeout(Duration::from_secs(30))
        .user_agent("tingo")
        .build()
        .map_err(|e| e.to_string())?;
    for d in list {
        let dest = paths.dir.join(d.to);
        let finished = ok_marker(&dest);
        if !finished.exists() && !final_exists(paths, d.to) {
            fetch(&client, &d.url, &dest, |n| progress(done_before + n, total))?;
            File::create(&finished).map_err(|e| e.to_string())?;
        }
        done_before += d.size;
        progress(done_before, total);
    }
    Ok(())
}

/// 断点续传用的完成标记和下载的压缩包，全部就绪后就不需要了
fn cleanup(paths: &ModelPaths, list: &[Download]) {
    fs::remove_dir_all(paths.dir.join("download")).ok();
    for d in list {
        let _ = fs::remove_file(ok_marker(&paths.dir.join(d.to)));
    }
}

/// 下载并准备好全部模型。`progress(已下载字节, 总字节)`。可以重复调用：已完成的文件跳过，断点续传。
pub fn ensure(paths: &ModelPaths, source: Source, mut progress: impl FnMut(u64, u64)) -> Result<(), String> {
    if paths.ready() {
        return Ok(());
    }
    let dir = &paths.dir;
    let list = downloads(source);
    fetch_all(paths, &list, &mut progress)?;
    prepare_paraformer(&dir.join("download"), &dir.join("paraformer"))?;
    // 国内镜像直接下的是 punct/model.onnx，没有压缩包
    if !paths.punct().is_file() {
        extract_punct(&dir.join("download/punct.tar.bz2"), &dir.join("punct"))?;
    }
    cleanup(paths, &list);
    if paths.ready() {
        Ok(())
    } else {
        Err(tr(
            "模型准备完了但文件不全，请删除模型目录后重试",
            "The models were prepared but some files are missing; delete the models folder and try again",
        )
        .into())
    }
}

/// 中间文件（download/ 下的）在最终产物已就绪时不用再下。
fn final_exists(paths: &ModelPaths, to: &str) -> bool {
    match to {
        "download/paraformer.onnx" | "download/am.mvn" | "download/tokens.json" => {
            paths.paraformer().is_file() && paths.tokens().is_file()
        }
        "download/punct.tar.bz2" => paths.punct().is_file(),
        "download/sensevoice.tar.bz2" => paths.asr_ready(AsrKind::SenseVoice),
        "download/funasr-nano.tar.bz2" => paths.asr_ready(AsrKind::FunAsrNano),
        other => paths.dir.join(other).is_file(),
    }
}

/// 断点续传下载，失败自动重试几次（GitHub 在国内经常中途断开）。
/// 下载到 dest（先写 .part，断点续传、失败重试）。`progress(已写字节)`。
pub fn fetch(
    client: &reqwest::blocking::Client,
    url: &str,
    dest: &Path,
    mut progress: impl FnMut(u64),
) -> Result<(), String> {
    fs::create_dir_all(dest.parent().unwrap()).map_err(|e| e.to_string())?;
    let part = dest.with_extension("part");
    let mut last_err = String::new();
    for attempt in 0..8 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_secs(2));
        }
        let have = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        let mut req = client.get(url);
        if have > 0 {
            req = req.header("Range", format!("bytes={have}-"));
        }
        let mut resp = match req.send() {
            Ok(r) if r.status().is_success() => r,
            // 已经下完（上次写完了 .part 但没来得及改名）
            Ok(r) if r.status().as_u16() == 416 && have > 0 => {
                fs::rename(&part, dest).map_err(|e| e.to_string())?;
                return Ok(());
            }
            Ok(r) => {
                last_err = format!("HTTP {}", r.status());
                continue;
            }
            Err(e) => {
                last_err = e.to_string();
                continue;
            }
        };
        // 服务器不支持续传时从头写
        let resumed = resp.status().as_u16() == 206;
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(resumed)
            .truncate(!resumed)
            .open(&part)
            .map_err(|e| e.to_string())?;
        let mut written = if resumed { have } else { 0 };
        let mut buf = vec![0u8; 1 << 16];
        let result = loop {
            match resp.read(&mut buf) {
                Ok(0) => break Ok(()),
                Ok(n) => {
                    file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
                    written += n as u64;
                    progress(written);
                }
                Err(e) => break Err(e.to_string()),
            }
        };
        match result {
            Ok(()) => {
                fs::rename(&part, dest).map_err(|e| e.to_string())?;
                return Ok(());
            }
            Err(e) => last_err = e,
        }
    }
    // 下了一半的 .part 留着，下次接着下
    Err(tr(
        &format!("下载失败（{url}）：{last_err}"),
        &format!("Download failed ({url}): {last_err}"),
    )
    .into())
}

/// FunASR 官方 Paraformer ONNX → sherpa-onnx：补元数据、词表改成 "符号 编号" 一行一个。
pub fn prepare_paraformer(src: &Path, dst: &Path) -> Result<(), String> {
    let model = dst.join("model.int8.onnx");
    let tokens_txt = dst.join("tokens.txt");
    if model.is_file() && tokens_txt.is_file() {
        return Ok(());
    }
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    let mvn = fs::read_to_string(src.join("am.mvn"))
        .map_err(|e| tr(&format!("读 am.mvn：{e}"), &format!("Reading am.mvn: {e}")).to_string())?;
    let tokens: Vec<String> = serde_json::from_str(
        &fs::read_to_string(src.join("tokens.json"))
            .map_err(|e| tr(&format!("读 tokens.json：{e}"), &format!("Reading tokens.json: {e}")).to_string())?,
    )
    .map_err(|e| tr(&format!("tokens.json 格式不对：{e}"), &format!("tokens.json is malformed: {e}")).to_string())?;
    let lines: String = tokens
        .iter()
        .enumerate()
        .map(|(i, t)| format!("{t} {i}\n"))
        .collect();
    fs::write(&tokens_txt, lines).map_err(|e| e.to_string())?;

    let meta = [
        ("lfr_window_size", "7".to_string()),
        ("lfr_window_shift", "6".to_string()),
        ("neg_mean", join(&mvn_vector(&mvn, "<AddShift>")?)),
        ("inv_stddev", join(&mvn_vector(&mvn, "<Rescale>")?)),
        ("vocab_size", tokens.len().to_string()),
        ("model_type", "paraformer".to_string()),
        ("version", "1".to_string()),
    ];
    let tmp = dst.join("model.int8.onnx.tmp");
    fs::copy(src.join("paraformer.onnx"), &tmp)
        .map_err(|e| tr(&format!("复制模型：{e}"), &format!("Copying the model: {e}")).to_string())?;
    let mut f = OpenOptions::new()
        .append(true)
        .open(&tmp)
        .map_err(|e| e.to_string())?;
    for (k, v) in &meta {
        f.write_all(&metadata_entry(k, v))
            .map_err(|e| e.to_string())?;
    }
    drop(f);
    fs::rename(&tmp, &model).map_err(|e| e.to_string())
}

fn join(v: &[f32]) -> String {
    v.iter()
        .map(|x| x.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// am.mvn 里 `<AddShift>` / `<Rescale>` 后面方括号中的向量。
fn mvn_vector(mvn: &str, tag: &str) -> Result<Vec<f32>, String> {
    let after = mvn
        .split_once(tag)
        .ok_or_else(|| tr(&format!("am.mvn 里没有 {tag}"), &format!("{tag} not found in am.mvn")).to_string())?
        .1;
    let body = after
        .split_once('[')
        .and_then(|(_, b)| b.split_once(']'))
        .ok_or_else(|| tr(&format!("{tag} 格式不对"), &format!("{tag} is malformed")).to_string())?
        .0;
    body.split_whitespace()
        .map(|x| {
            x.parse()
                .map_err(|_| tr(&format!("{tag} 里有非数字：{x}"), &format!("Non-number in {tag}: {x}")).to_string())
        })
        .collect()
}

/// ModelProto.metadata_props（字段 14，length-delimited）的一条 StringStringEntryProto{key=1, value=2}。
fn metadata_entry(key: &str, value: &str) -> Vec<u8> {
    let mut entry = Vec::new();
    for (field, s) in [(1u8, key), (2u8, value)] {
        entry.push((field << 3) | 2);
        varint(&mut entry, s.len() as u64);
        entry.extend_from_slice(s.as_bytes());
    }
    let mut out = vec![(14 << 3) | 2];
    varint(&mut out, entry.len() as u64);
    out.extend(entry);
    out
}

fn varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn extract_punct(tar_bz2: &Path, dst: &Path) -> Result<(), String> {
    let model = dst.join("model.int8.onnx");
    if model.is_file() {
        return Ok(());
    }
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    let f = File::open(tar_bz2).map_err(|e| {
        tr(&format!("打开标点模型包：{e}"), &format!("Opening the punctuation model archive: {e}")).to_string()
    })?;
    let mut archive = tar::Archive::new(bzip2::read::BzDecoder::new(f));
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path: PathBuf = entry.path().map_err(|e| e.to_string())?.into_owned();
        if path.file_name().is_some_and(|n| n == "model.int8.onnx") {
            let tmp = dst.join("model.int8.onnx.tmp");
            entry.unpack(&tmp).map_err(|e| e.to_string())?;
            return fs::rename(&tmp, &model).map_err(|e| e.to_string());
        }
    }
    Err(tr("标点模型包里没有 model.int8.onnx", "model.int8.onnx not found in the punctuation model archive").into())
}

/// 解压 sherpa-onnx 的模型包：去掉最外层目录，跳过示例音频和说明。先解到临时目录，完整了再改名。
fn extract_dir(tar_bz2: &Path, dst: &Path) -> Result<(), String> {
    let tmp = dst.with_extension("tmp");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let f = File::open(tar_bz2)
        .map_err(|e| tr(&format!("打开模型包：{e}"), &format!("Opening the model archive: {e}")).to_string())?;
    let mut archive = tar::Archive::new(bzip2::read::BzDecoder::new(f));
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path: PathBuf = entry.path().map_err(|e| e.to_string())?.into_owned();
        let rel: PathBuf = path.components().skip(1).collect();
        let skip = rel.as_os_str().is_empty()
            || rel.starts_with("test_wavs")
            || rel.extension().is_some_and(|e| e == "md")
            || rel.components().any(|c| matches!(c, std::path::Component::ParentDir));
        if skip || !entry.header().entry_type().is_file() {
            continue;
        }
        let out = tmp.join(&rel);
        fs::create_dir_all(out.parent().unwrap()).map_err(|e| e.to_string())?;
        entry.unpack(&out).map_err(|e| e.to_string())?;
    }
    let _ = fs::remove_dir_all(dst);
    fs::rename(&tmp, dst).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_and_entry_encoding() {
        let mut v = Vec::new();
        varint(&mut v, 300);
        assert_eq!(v, vec![0xAC, 0x02]);
        let e = metadata_entry("k", "vv");
        assert_eq!(e, vec![0x72, 7, 0x0A, 1, b'k', 0x12, 2, b'v', b'v']);
    }

    #[test]
    fn mvn_parse() {
        let mvn = "<Nnet>\n<AddShift> 3 3\n<LearnRateCoef> 0 [ -1.5 2 3e-1 ]\n<Rescale> 3 3\n<LearnRateCoef> 0 [ 0.1 0.2 0.3 ]\n";
        assert_eq!(mvn_vector(mvn, "<AddShift>").unwrap(), vec![-1.5, 2.0, 0.3]);
        assert_eq!(mvn_vector(mvn, "<Rescale>").unwrap(), vec![0.1, 0.2, 0.3]);
        assert!(mvn_vector(mvn, "<Nope>").is_err());
    }
}
