//! 识别引擎：sherpa-onnx 运行识别模型 + Silero VAD + CT-Transformer 标点 + CAM++ 声纹。
//! 识别模型默认 FunASR Paraformer-large（int8），可以换成 SenseVoice（更快、多语种）或
//! Fun-ASR-Nano（更准、支持热词，慢几倍），见 [`AsrKind`]。
//!
//! 这条路线的选型依据见 spike/sherpa/RESULTS.md。识别结果没有逐字时间戳，
//! 所以按 VAD 切出的语音段识别、每段提一个声纹，说话人以语音段为单位判断。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sherpa_onnx::{
    OfflinePunctuation, OfflinePunctuationConfig, OfflinePunctuationModelConfig, OfflineRecognizer,
    OfflineRecognizerConfig, SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig,
    VadModelConfig, VoiceActivityDetector,
};

use crate::i18n::tr;

pub const SR: usize = 16000;

/// 可选的识别模型。VAD、标点、声纹三个基础模型大家共用。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AsrKind {
    Paraformer,
    SenseVoice,
    FunAsrNano,
    AppleSpeech,
}

impl AsrKind {
    pub const ALL: [AsrKind; 4] = [AsrKind::Paraformer, AsrKind::SenseVoice, AsrKind::FunAsrNano, AsrKind::AppleSpeech];

    pub fn id(self) -> &'static str {
        match self {
            AsrKind::Paraformer => "paraformer",
            AsrKind::SenseVoice => "sensevoice",
            AsrKind::FunAsrNano => "funasr-nano",
            AsrKind::AppleSpeech => "apple-speech",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.id() == id)
    }

    /// 能指定会议语言（[asr] language）；Paraformer 只认中文
    pub fn uses_language(self) -> bool {
        matches!(self, AsrKind::SenseVoice | AsrKind::FunAsrNano | AsrKind::AppleSpeech)
    }

    /// 模型自己会加标点（不用再过 CT-Transformer）。SenseVoice 按 VAD 切出的短段识别时基本不出标点，照样要过
    fn punctuates(self) -> bool {
        matches!(self, AsrKind::FunAsrNano | AsrKind::AppleSpeech)
    }
}

/// 模型文件布局（相对模型目录）。
pub struct ModelPaths {
    pub dir: PathBuf,
}

impl ModelPaths {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
    pub fn paraformer(&self) -> PathBuf {
        self.dir.join("paraformer/model.int8.onnx")
    }
    pub fn tokens(&self) -> PathBuf {
        self.dir.join("paraformer/tokens.txt")
    }
    pub fn vad(&self) -> PathBuf {
        self.dir.join("silero_vad.onnx")
    }
    /// GitHub 上下的是量化版，国内镜像只有完整版（models::Source）
    pub fn punct(&self) -> PathBuf {
        let int8 = self.dir.join("punct/model.int8.onnx");
        if int8.is_file() {
            int8
        } else {
            self.dir.join("punct/model.onnx")
        }
    }
    pub fn speaker(&self) -> PathBuf {
        self.dir.join("campplus_zh.onnx")
    }
    /// 识别模型所在目录（Paraformer 沿用最早的位置，老用户不用重新下载）
    pub fn asr_dir(&self, kind: AsrKind) -> PathBuf {
        self.dir.join(match kind {
            AsrKind::Paraformer => "paraformer",
            AsrKind::SenseVoice => "sensevoice",
            AsrKind::FunAsrNano => "funasr-nano",
            AsrKind::AppleSpeech => "apple-speech",
        })
    }
    pub fn asr_files(&self, kind: AsrKind) -> Vec<PathBuf> {
        let d = self.asr_dir(kind);
        match kind {
            AsrKind::AppleSpeech => vec![],
            AsrKind::Paraformer => vec![self.paraformer(), self.tokens()],
            AsrKind::SenseVoice => vec![d.join("model.int8.onnx"), d.join("tokens.txt")],
            AsrKind::FunAsrNano => vec![
                d.join("encoder_adaptor.int8.onnx"),
                d.join("llm.int8.onnx"),
                d.join("embedding.int8.onnx"),
                d.join("Qwen3-0.6B/tokenizer.json"),
            ],
        }
    }
    pub fn base(&self) -> [PathBuf; 3] {
        [self.vad(), self.punct(), self.speaker()]
    }
    pub fn base_ready(&self) -> bool {
        self.base().iter().all(|p| p.is_file())
    }
    pub fn asr_ready(&self, kind: AsrKind) -> bool {
        self.asr_files(kind).iter().all(|p| p.is_file())
    }
    /// 默认的一套（基础模型 + Paraformer）是否齐全：首次启动下载的就是这一套
    pub fn ready(&self) -> bool {
        self.base_ready() && self.asr_ready(AsrKind::Paraformer)
    }
    fn active_file(&self) -> PathBuf {
        self.dir.join("active-asr.txt")
    }
    /// 当前选用的识别模型；没选过或者选的那个文件不全时用 Paraformer
    pub fn active(&self) -> AsrKind {
        std::fs::read_to_string(self.active_file())
            .ok()
            .and_then(|s| AsrKind::from_id(s.trim()))
            .filter(|&k| self.asr_ready(k))
            .unwrap_or(AsrKind::Paraformer)
    }
    pub fn set_active(&self, kind: AsrKind) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        std::fs::write(self.active_file(), kind.id())
    }
}

/// 一段语音的识别结果（时间是整段音频里的绝对毫秒）。
#[derive(Clone, Debug)]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    /// 声纹：段内每 1.5 秒一个小窗（滑动 0.75 秒），和 FunASR 的做法一致；太短提不出时为空
    pub embeddings: Vec<Vec<f32>>,
}

pub struct Engine {
    pub kind: AsrKind,
    /// 加载时交给模型的热词（只有 Fun-ASR-Nano 用；热词变了要重新加载）
    pub hotwords: Vec<String>,
    /// 加载时定下的会议语言（zh | en | auto；Paraformer 不用，为空。语言变了要重新加载）
    pub language: String,
    rec: Option<OfflineRecognizer>,
    apple_bin: Option<PathBuf>,
    punct: Option<OfflinePunctuation>,
    spk: SpeakerEmbeddingExtractor,
    vad: VadModelConfig,
    // sherpa 的对象单线程使用是安全的，这里统一串行化
    lock: Mutex<()>,
}

// SAFETY: 所有使用都经过 `lock` 串行化；sherpa-onnx 的 C 对象在单线程使用时是安全的。
unsafe impl Sync for Engine {}
unsafe impl Send for Engine {}

fn path_str(p: &Path) -> Option<String> {
    Some(p.to_string_lossy().into_owned())
}

/// SenseVoice 的 language：zh / en / auto 原样给
fn sense_voice_language(lang: &str) -> &'static str {
    match lang {
        "en" => "en",
        "auto" => "auto",
        _ => "zh", // auto 在中文会议上错字明显多；中英混说照样能认英文
    }
}

/// Fun-ASR-Nano 的 language 会拼进提示词「语音转写成{language}」；auto 不设，让模型自己判断
fn funasr_nano_language(lang: &str) -> Option<String> {
    match lang {
        "en" => Some("英文".into()),
        "auto" => None,
        _ => Some("中文".into()),
    }
}

impl Engine {
    /// `language` 是配置里的会议语言（zh | en | auto），只有 SenseVoice、Fun-ASR-Nano 用
    pub fn load(models: &ModelPaths, kind: AsrKind, hotwords: &[String], language: &str, threads: i32) -> Result<Self, String> {
        let files = models.base().into_iter().chain(models.asr_files(kind));
        if let Some(missing) = files.into_iter().find(|p| !p.is_file()) {
            let m = missing.display();
            return Err(tr(&format!("缺少模型文件：{m}"), &format!("Missing model file: {m}")).into());
        }
        let mut cfg = OfflineRecognizerConfig::default();
        cfg.model_config.num_threads = threads;
        let rec = if kind == AsrKind::AppleSpeech { None } else {
        let [first, second, ..] = &models.asr_files(kind)[..] else { unreachable!() };
        let dir = models.asr_dir(kind);
        match kind {
            AsrKind::AppleSpeech => unreachable!(),
            AsrKind::Paraformer => {
                cfg.model_config.paraformer.model = path_str(first);
                cfg.model_config.tokens = path_str(second);
            }
            AsrKind::SenseVoice => {
                cfg.model_config.sense_voice.model = path_str(first);
                cfg.model_config.sense_voice.language = Some(sense_voice_language(language).into());
                cfg.model_config.sense_voice.use_itn = true;
                cfg.model_config.tokens = path_str(second);
            }
            AsrKind::FunAsrNano => {
                let n = &mut cfg.model_config.funasr_nano;
                n.encoder_adaptor = path_str(&dir.join("encoder_adaptor.int8.onnx"));
                n.llm = path_str(&dir.join("llm.int8.onnx"));
                n.embedding = path_str(&dir.join("embedding.int8.onnx"));
                n.tokenizer = path_str(&dir.join("Qwen3-0.6B"));
                // Rust 绑定的默认值是随机采样（temperature 1.0），改成和官方 Python 接口一样的近似贪心
                n.max_new_tokens = 512;
                n.temperature = 1e-6;
                n.top_p = 0.8;
                n.itn = 1;
                n.language = funasr_nano_language(language);
                if !hotwords.is_empty() {
                    n.hotwords = Some(hotwords.join(","));
                }
            }
        }
        Some(OfflineRecognizer::create(&cfg).ok_or(tr("识别模型加载失败", "Couldn't load the speech recognition model"))?)
        };

        let punct = if kind.punctuates() {
            None
        } else {
            Some(
                OfflinePunctuation::create(&OfflinePunctuationConfig {
                    model: OfflinePunctuationModelConfig {
                        ct_transformer: path_str(&models.punct()),
                        num_threads: 2,
                        ..Default::default()
                    },
                })
                .ok_or(tr("标点模型加载失败", "Couldn't load the punctuation model"))?,
            )
        };

        let spk = SpeakerEmbeddingExtractor::create(&SpeakerEmbeddingExtractorConfig {
            model: path_str(&models.speaker()),
            num_threads: 2,
            ..Default::default()
        })
        .ok_or(tr("声纹模型加载失败", "Couldn't load the speaker model"))?;

        let mut vad = VadModelConfig::default();
        vad.silero_vad.model = path_str(&models.vad());
        vad.silero_vad.threshold = 0.5;
        vad.silero_vad.min_silence_duration = 0.3;
        vad.silero_vad.min_speech_duration = 0.25;
        vad.silero_vad.window_size = 512;
        vad.silero_vad.max_speech_duration = 10.0;
        vad.sample_rate = SR as i32;
        vad.num_threads = 1;
        Ok(Self {
            kind,
            hotwords: if kind == AsrKind::FunAsrNano { hotwords.to_vec() } else { Vec::new() },
            language: if kind.uses_language() { language.to_string() } else { String::new() },
            rec,
            apple_bin: None,
            punct,
            spk,
            vad,
            lock: Mutex::new(()),
        })
    }

    /// 识别一段 16 kHz 单声道音频。`offset_ms` 是这段在整场会议里的起点。
    /// 除了 Fun-ASR-Nano，识别模型都不支持热词；热词在生成纪要时作为术语表交给大模型纠正。
    pub fn with_apple_helper(mut self, bin: &Path) -> Result<Self, String> {
        let status = crate::apple_speech::status(bin, &self.language)?;
        if !status.available || !status.ready { return Err("请先检查／安装系统语音模型 / Install the system speech model first".into()); }
        self.apple_bin = Some(bin.to_owned());
        Ok(self)
    }

    /// 边录边转用的静音检测：持续喂音频，一句话说完（停顿约 0.3 秒）就能知道它在哪结束。
    /// 和识别用的是同一个 Silero 模型和参数。
    pub fn live_vad(&self) -> Option<VoiceActivityDetector> {
        VoiceActivityDetector::create(&self.vad, 60.0)
    }

    pub fn transcribe(&self, samples: &[f32], offset_ms: u64) -> Result<Vec<Segment>, String> {
        let _guard = self.lock.lock().unwrap();
        if self.kind == AsrKind::AppleSpeech {
            let bin = self.apple_bin.as_ref().ok_or("Apple speech helper is not configured")?;
            let spans = crate::apple_speech::transcribe(bin, &self.language, samples)?;
            return Ok(spans.into_iter().filter(|s| !s.text.trim().is_empty()).map(|s| {
                let a = ((s.start * SR as f64) as usize).min(samples.len());
                let b = ((s.end * SR as f64) as usize).min(samples.len());
                Segment { start_ms: offset_ms + (s.start * 1000.0) as u64, end_ms: offset_ms + (s.end * 1000.0) as u64,
                    text: s.text, embeddings: windows(b-a).filter_map(|(x,y)| self.embed(&samples[a+x..a+y])).collect() }
            }).collect());
        }
        let rec = self.rec.as_ref().ok_or("Speech recognizer is not loaded")?;
        // 偏轻的声音先放大，不然静音检测会把轻声说话整句当成安静
        let leveled = crate::audio::agc(samples);
        let samples = leveled.as_slice();
        let Some(vad) = VoiceActivityDetector::create(&self.vad, 120.0) else {
            return Err("Voice activity detector failed to load".into());
        };
        for chunk in samples.chunks(512) {
            vad.accept_waveform(chunk);
        }
        vad.flush();
        let mut out = Vec::new();
        while let Some(seg) = vad.front() {
            let audio = seg.samples();
            let start = offset_ms + seg.start() as u64 * 1000 / SR as u64;
            let end = start + audio.len() as u64 * 1000 / SR as u64;
            let stream = rec.create_stream();
            stream.accept_waveform(SR as i32, audio);
            rec.decode(&stream);
            let raw = stream.get_result().map(|r| r.text).unwrap_or_default();
            let raw = raw.trim();
            if !raw.is_empty() {
                let text = match &self.punct {
                    Some(p) => p.add_punctuation(raw).unwrap_or_else(|| raw.to_string()),
                    None => raw.to_string(),
                };
                let embeddings = windows(audio.len())
                    .filter_map(|(a, b)| self.embed(&audio[a..b]))
                    .collect();
                out.push(Segment {
                    start_ms: start,
                    end_ms: end,
                    text,
                    embeddings,
                });
            }
            vad.pop();
        }
        Ok(out)
    }

    fn embed(&self, audio: &[f32]) -> Option<Vec<f32>> {
        let stream = self.spk.create_stream()?;
        stream.accept_waveform(SR as i32, audio);
        stream.input_finished();
        if !self.spk.is_ready(&stream) {
            return None;
        }
        self.spk.compute(&stream).filter(|e| !e.is_empty())
    }
}

/// 声纹小窗：1.5 秒一窗、滑动 0.75 秒，最后一窗贴着段尾；不足 1.5 秒的段整段一窗（FunASR sv_chunk）。
pub fn windows(len: usize) -> impl Iterator<Item = (usize, usize)> {
    let (win, shift) = (SR * 3 / 2, SR * 3 / 4);
    let mut st = 0;
    let mut done = false;
    std::iter::from_fn(move || {
        if done || len == 0 {
            return None;
        }
        let ed = (st + win).min(len);
        let a = ed.saturating_sub(win);
        done = ed == len;
        st += shift;
        Some((a, ed))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apple_selection_survives_without_onnx_files() {
        let dir = std::env::temp_dir().join(format!("tingo-apple-choice-{}", std::process::id()));
        let paths = ModelPaths::new(&dir);
        let kind = AsrKind::from_id("apple-speech").expect("Apple engine in catalog");
        paths.set_active(kind).unwrap();
        assert_eq!(paths.active(), kind);
        assert!(paths.asr_files(kind).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn windows_cover_segment() {
        assert_eq!(windows(SR).collect::<Vec<_>>(), vec![(0, SR)]);
        let w: Vec<_> = windows(SR * 3).collect();
        assert_eq!(w.first(), Some(&(0, SR * 3 / 2)));
        assert_eq!(w.last().unwrap().1, SR * 3);
        assert!(w.windows(2).all(|p| p[1].0 - p[0].0 <= SR * 3 / 4));
        assert_eq!(windows(0).count(), 0);
    }

    #[test]
    fn language_options() {
        assert_eq!(
            ["zh", "en", "auto"].map(sense_voice_language),
            ["zh", "en", "auto"]
        );
        assert_eq!(funasr_nano_language("zh").as_deref(), Some("中文"));
        assert_eq!(funasr_nano_language("en").as_deref(), Some("英文"));
        assert_eq!(funasr_nano_language("auto"), None);
        assert!(!AsrKind::Paraformer.uses_language() && AsrKind::SenseVoice.uses_language());
    }
}
