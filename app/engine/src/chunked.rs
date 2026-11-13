//! 分段转写：把音频按停顿切成 10～30 秒逐段识别，最后统一聚类说话人。
//! 录音中（边录边转）和处理已有文件共用；Python 版 live.py 的移植。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::asr::{Engine, Segment, SR};
use sherpa_onnx::VoiceActivityDetector;
use crate::audio::{
    duck_mix_default, find_cut, find_live_cut, GrowingWav, LIVE_MAX_CHUNK_S, LIVE_MIN_CHUNK_S, MAX_CHUNK_S, MIN_CHUNK_S, SEARCH_S,
};
use crate::cluster::cluster;
use crate::transcript::{Sentence, Transcript};

const MIX_MARGIN_S: f64 = 1.0; // 混音时往前多读一点，让"让路"的保持时间跨片段连续
const LOOKAHEAD_S: f64 = 0.2; // 文件末尾先不处理，留给"提前关麦克风"看后面的数据
const LIVE_STEP_S: f64 = 0.25; // 边录边转：攒够这么多新音频就混一次、看看能不能切
const LIVE_POLL: Duration = Duration::from_millis(250);
/// 录音中给说话人临时编号：和已有说话人的声纹余弦相似度够高就归进去，否则算新的一位。
/// 一句话很短、声纹不稳，光靠这个会越分越多（实测 15 分钟 3 人的会编出 66 位），所以每出新句子就用 [`cluster`]
/// 整场重新聚类一次（见 [`LIVE_RECLUSTER`]），临时编号通常不到一秒就被改正。
const LIVE_SAME_SPEAKER: f32 = 0.7;
/// 录音中整场重新聚类说话人的最短间隔；聚类耗时（15 分钟的会约 0.13 秒）长了就按耗时的 5 倍拉开，不拖慢出字
const LIVE_RECLUSTER: Duration = Duration::from_secs(1);

#[derive(Default)]
pub struct Timings {
    pub tail_s: f64,
    pub cluster_s: f64,
    pub segments: usize,
    /// 边录边转：一句话说完到识别出来的平均、最长延迟（秒）
    pub live_lag_avg_s: f64,
    pub live_lag_max_s: f64,
}

#[derive(Default)]
pub struct Chunker {
    pending: Vec<f32>,
    pending_start: f64, // pending 第一个样本对应的秒数
    pub segments: Vec<Segment>,
    pub timings: Timings,
    /// 边录边转：遇到停顿就切（见 [`find_live_cut`]），而不是攒 10～30 秒
    live: bool,
    /// 已经切走的样本数（pending 第一个样本的绝对位置），和静音检测的位置对齐用
    drained: usize,
}

impl Chunker {
    pub fn feed(&mut self, audio: &[f32]) {
        self.pending.extend_from_slice(audio);
    }

    pub fn processed_s(&self) -> f64 {
        self.pending_start
    }

    /// 从待处理音频里切出下一段；不够长或找不到合适的切点时返回 None。
    pub fn next_chunk(&mut self, last: bool) -> Option<(Vec<f32>, f64)> {
        let min_len = if last {
            (0.3 * SR as f64) as usize
        } else {
            (MIN_CHUNK_S * SR as f64) as usize
        };
        if self.pending.len() < min_len {
            return None;
        }
        if self.live && !last {
            let max = (LIVE_MAX_CHUNK_S * SR as f64) as usize;
            let window = &self.pending[..self.pending.len().min(max)];
            let cut = find_live_cut(window, LIVE_MIN_CHUNK_S, LIVE_MAX_CHUNK_S)?;
            return Some(self.take(cut));
        }
        // 每段最长 MAX_CHUNK_S：处理落后时也不会攒出一个巨大的段
        let max = (MAX_CHUNK_S * SR as f64) as usize;
        let window = &self.pending[..self.pending.len().min(max)];
        let over = self.pending.len() > window.len();
        let cut = if last && !over {
            self.pending.len()
        } else {
            find_cut(
                window,
                last || over || window.len() >= max,
                MIN_CHUNK_S,
                SEARCH_S,
            )?
        };
        Some(self.take(cut))
    }

    /// 切走 pending 的前 `cut` 个样本，返回这段和它的起始秒数
    fn take(&mut self, cut: usize) -> (Vec<f32>, f64) {
        let chunk: Vec<f32> = self.pending.drain(..cut).collect();
        let offset = self.pending_start;
        self.pending_start += cut as f64 / SR as f64;
        self.drained += cut;
        (chunk, offset)
    }

    pub fn transcribe(&mut self, engine: &Engine, chunk: &[f32], offset_s: f64) -> Result<Vec<Segment>, String> {
        let segs = engine.transcribe(chunk, (offset_s * 1000.0).round() as u64)?;
        self.segments.extend(segs.iter().cloned());
        Ok(segs)
    }

    pub fn duration_ms(&self) -> u64 {
        ((self.pending_start + self.pending.len() as f64 / SR as f64) * 1000.0).round() as u64
    }

    /// 全部声纹统一聚类，得到说话人编号全场一致的转写。
    pub fn assemble(&mut self, source: &Path) -> Transcript {
        let t = Instant::now();
        let speaker = segment_speakers(&self.segments);
        let first = speaker.iter().flatten().next().copied().unwrap_or(0);
        self.timings.cluster_s = t.elapsed().as_secs_f64();
        self.timings.segments = self.segments.len();
        Transcript {
            source: source.to_string_lossy().into_owned(),
            duration_ms: self.duration_ms(),
            speaker_names: Default::default(),
            sentences: self
                .segments
                .iter()
                .zip(speaker)
                .map(|(seg, spk)| Sentence {
                    speaker: spk.unwrap_or(first),
                    start_ms: seg.start_ms,
                    end_ms: seg.end_ms,
                    text: seg.text.clone(),
                })
                .collect(),
        }
    }
}

/// 全部声纹统一聚类，给每段编说话人：每段取段内小窗的多数，按首次开口的先后编号（第一个开口的是 0）。
/// 太短提不出声纹的段归给前一段；开头就没有的是 None。
fn segment_speakers(segments: &[Segment]) -> Vec<Option<usize>> {
    let mut owner = Vec::new(); // 每个小窗属于哪一段
    let mut emb = Vec::new();
    for (i, seg) in segments.iter().enumerate() {
        for e in &seg.embeddings {
            owner.push(i);
            emb.push(e.clone());
        }
    }
    let labels = cluster(&emb);
    let mut votes: Vec<std::collections::BTreeMap<usize, usize>> = vec![Default::default(); segments.len()];
    for (&i, &l) in owner.iter().zip(&labels) {
        *votes[i].entry(l).or_default() += 1;
    }
    let mut speaker: Vec<Option<usize>> = votes
        .iter()
        .map(|v| v.iter().max_by_key(|(l, n)| (**n, std::cmp::Reverse(**l))).map(|(l, _)| *l))
        .collect();
    let mut last = None;
    for s in speaker.iter_mut() {
        if s.is_none() {
            *s = last;
        }
        last = *s;
    }
    // 按首次出现的先后重新编号（第一个开口的是说话人1），和 FunASR 一致
    let mut order: Vec<usize> = Vec::new();
    for s in speaker.iter().flatten() {
        if !order.contains(s) {
            order.push(*s);
        }
    }
    speaker.iter().map(|s| s.map(|l| order.iter().position(|&o| o == l).unwrap())).collect()
}

/// 分段转写一段完整音频（16 kHz 单声道）。
pub fn transcribe_samples(
    engine: &Engine,
    audio: &[f32],
    source: &Path,
    mut progress: impl FnMut(f64),
) -> Result<Transcript, String> {
    let total = audio.len() as f64 / SR as f64;
    let mut c = Chunker::default();
    c.feed(audio);
    while let Some((chunk, offset)) = c.next_chunk(false).or_else(|| c.next_chunk(true)) {
        c.transcribe(engine, &chunk, offset)?;
        if total > 0.0 {
            progress((c.processed_s() / total).min(1.0));
        }
    }
    Ok(c.assemble(source))
}

/// 录音进行中读取录音组件正在写的音轨，边录边转。
pub struct LiveTranscriber {
    inner: Arc<Mutex<LiveState>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

struct LiveState {
    tracks: Vec<(String, GrowingWav)>,
    mixed_until: f64,
    chunker: Chunker,
    /// 静音检测：找出每句话在哪结束，说完就切；加载失败时退回按音量找停顿
    vad: Option<VoiceActivityDetector>,
    vad_fed: usize,
    speech_ends: Vec<usize>,
    lines: Vec<LiveLine>,
    speakers: LiveSpeakers,
    /// 整场重新聚类改过前面句子的说话人就加一，界面据此整场重取
    speaker_rev: u64,
    /// 上次整场聚类：当时有几段、什么时候、花了多久
    clustered: usize,
    clustered_at: Option<Instant>,
    cluster_cost: Duration,
    /// 每句从说完到识别出来用了几秒
    lags: Vec<f64>,
    error: Option<String>,
}

/// 录音中实时显示的一句
#[derive(Clone, Debug, PartialEq)]
pub struct LiveLine {
    pub start_ms: u64,
    /// 临时说话人编号（第一个开口的是 0）；太短提不出声纹时沿用上一句
    pub speaker: Option<usize>,
    pub text: String,
}

/// 录音中逐句给说话人编号：每位说话人记下声纹（单位向量）之和，方向就是平均声纹
#[derive(Default)]
struct LiveSpeakers {
    centroids: Vec<Vec<f32>>,
}

impl LiveSpeakers {
    fn assign(&mut self, embeddings: &[Vec<f32>]) -> Option<usize> {
        let sum = unit_sum(embeddings.iter())?;
        let best = self
            .centroids
            .iter()
            .enumerate()
            .map(|(i, c)| (i, cosine(c, &sum)))
            .max_by(|a, b| a.1.total_cmp(&b.1));
        match best {
            Some((i, sim)) if sim >= LIVE_SAME_SPEAKER => {
                for (c, v) in self.centroids[i].iter_mut().zip(&sum) {
                    *c += v;
                }
                Some(i)
            }
            _ => {
                self.centroids.push(sum);
                Some(self.centroids.len() - 1)
            }
        }
    }

    /// 按整场聚类的结果重建每位说话人的声纹
    fn reset(&mut self, segments: &[Segment], speaker: &[Option<usize>]) {
        let k = speaker.iter().flatten().max().map_or(0, |m| m + 1);
        self.centroids = (0..k)
            .map(|i| {
                let of_i = segments.iter().zip(speaker).filter(|(_, s)| **s == Some(i)).flat_map(|(g, _)| &g.embeddings);
                unit_sum(of_i).unwrap_or_default()
            })
            .collect();
    }
}

/// 一组声纹各自归一化后相加；没有声纹时 None
fn unit_sum<'a>(embeddings: impl Iterator<Item = &'a Vec<f32>>) -> Option<Vec<f32>> {
    let mut sum: Option<Vec<f32>> = None;
    for e in embeddings {
        let n = norm(e);
        let acc = sum.get_or_insert_with(|| vec![0.0; e.len()]);
        for (a, v) in acc.iter_mut().zip(e) {
            *a += v / n;
        }
    }
    sum
}

fn norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9)
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>() / (norm(a) * norm(b))
}

impl LiveState {
    /// 边录边转的下一段：在刚说完的那句话后面一点切（句尾留 0.15 秒静音），这样说完一两秒就能出字；
    /// 一直没人说话就把静音切掉；一直在说、没有停顿时攒到 12 秒强制切。
    fn next_live_chunk(&mut self) -> Option<(Vec<f32>, f64)> {
        let Some(vad) = &self.vad else { return self.chunker.next_chunk(false) };
        let c = &mut self.chunker;
        let total = c.drained + c.pending.len();
        // 静音检测也看放大后的声音：新来的音频连同前面约 1 秒一起算增益，只喂新的部分
        let new_from = self.vad_fed.saturating_sub(c.drained).min(c.pending.len());
        let ctx = new_from.saturating_sub(SR);
        let leveled = crate::audio::agc(&c.pending[ctx..]);
        for part in leveled[new_from - ctx..].chunks(512) {
            vad.accept_waveform(part);
        }
        self.vad_fed = total;
        while let Some(seg) = vad.front() {
            self.speech_ends.push(seg.start().max(0) as usize + seg.n().max(0) as usize);
            vad.pop();
        }
        let tail = (0.15 * SR as f64) as usize;
        let min = SR / 2;
        if let Some(cut) = self.speech_ends.iter().rev().map(|e| (e + tail).min(total)).find(|&e| e >= c.drained + min) {
            self.speech_ends.retain(|&e| e > cut);
            return Some(c.take(cut - c.drained));
        }
        let pending = c.pending.len();
        if !vad.detected() && self.speech_ends.is_empty() && pending >= 3 * SR {
            // 没人说话：留最后半秒（可能是刚开口），前面的静音直接切走
            return Some(c.take(pending - SR / 2));
        }
        let max = (LIVE_MAX_CHUNK_S * SR as f64) as usize;
        if pending >= max {
            let cut = find_cut(&c.pending[..max], true, LIVE_MAX_CHUNK_S, 3.0)?;
            self.speech_ends.retain(|&e| e > c.drained + cut);
            return Some(c.take(cut));
        }
        None
    }

    /// 刚识别出的几段：编上临时说话人，记下说完到识别出来的延迟
    fn add_lines(&mut self, segs: &[Segment]) {
        let now = self.tracks.iter_mut().map(|(_, t)| t.seconds()).fold(f64::INFINITY, f64::min);
        for g in segs {
            if now.is_finite() {
                self.lags.push((now - g.end_ms as f64 / 1000.0).max(0.0));
            }
            let speaker = self
                .speakers
                .assign(&g.embeddings)
                .or_else(|| self.lines.last().and_then(|l| l.speaker));
            self.lines.push(LiveLine { start_ms: g.start_ms, speaker, text: g.text.clone() });
        }
    }

    /// 该整场重新聚类了吗：有新的句子、声纹够聚类（太少时 [`cluster`] 只会归成一人），离上次够久
    fn recluster_due(&self) -> bool {
        let segs = &self.chunker.segments;
        segs.len() > self.clustered
            && segs.iter().map(|g| g.embeddings.len()).sum::<usize>() >= 20
            && self.clustered_at.is_none_or(|t| t.elapsed() >= LIVE_RECLUSTER.max(self.cluster_cost * 5))
    }

    /// 用整场聚类的结果（`speaker` 对应前 `speaker.len()` 段）改正各句的说话人；聚类期间新出的几句按新声纹重新归类
    fn apply_speakers(&mut self, speaker: &[Option<usize>], cost: Duration) {
        let segs = &self.chunker.segments;
        self.speakers.reset(&segs[..speaker.len()], speaker);
        let (mut changed, mut prev) = (false, None);
        for (i, line) in self.lines.iter_mut().enumerate() {
            let spk = match speaker.get(i) {
                Some(s) => *s,
                None => self.speakers.assign(&segs[i].embeddings).or(prev),
            };
            changed |= line.speaker != spk;
            line.speaker = spk;
            prev = spk;
        }
        self.speaker_rev += changed as u64;
        self.clustered = speaker.len();
        self.clustered_at = Some(Instant::now());
        self.cluster_cost = cost;
    }
}

impl LiveState {
    fn mix_new(&mut self, last: bool) {
        let avail = self
            .tracks
            .iter_mut()
            .map(|(_, t)| t.seconds())
            .fold(f64::INFINITY, f64::min);
        if !avail.is_finite() {
            return;
        }
        let commit_end = if last { avail } else { avail - LOOKAHEAD_S };
        if commit_end - self.mixed_until < if last { 0.05 } else { LIVE_STEP_S } {
            return;
        }
        let margin = MIX_MARGIN_S.min(self.mixed_until);
        let start = self.mixed_until - margin;
        let mut parts: Vec<(String, Vec<f32>)> = self
            .tracks
            .iter_mut()
            .map(|(name, t)| (name.clone(), t.read(start, avail)))
            .collect();
        let mixed = if parts.len() == 2 {
            let mic = parts.iter().position(|(n, _)| n == "mic").unwrap_or(1);
            let mic = parts.remove(mic).1;
            duck_mix_default(&parts[0].1, &mic)
        } else {
            parts.pop().map(|p| p.1).unwrap_or_default()
        };
        let a = (margin * SR as f64).round() as usize;
        let b = (((commit_end - start) * SR as f64).round() as usize).min(mixed.len());
        if b <= a {
            return;
        }
        self.chunker.feed(&mixed[a..b]);
        // 按实际拿到的样本数推进，两路长度差几个样本也不会累积漂移
        self.mixed_until = start + b as f64 / SR as f64;
    }
}

impl LiveTranscriber {
    /// `sources` 是实际在录的音轨名（"system" / "mic"），文件在 `dir/<名字>.wav`。
    pub fn start(engine: Arc<Engine>, dir: &Path, sources: &[String]) -> Self {
        let state = LiveState {
            tracks: sources
                .iter()
                .map(|s| (s.clone(), GrowingWav::new(dir.join(format!("{s}.wav")))))
                .collect(),
            mixed_until: 0.0,
            chunker: Chunker { live: true, ..Default::default() },
            vad: engine.live_vad(),
            vad_fed: 0,
            speech_ends: Vec::new(),
            lines: Vec::new(),
            speakers: LiveSpeakers::default(),
            speaker_rev: 0,
            clustered: 0,
            clustered_at: None,
            cluster_cost: Duration::ZERO,
            lags: Vec::new(),
            error: None,
        };
        let inner = Arc::new(Mutex::new(state));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (inner, stop) = (inner.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    if !step(&inner, &engine, false) & !recluster(&inner) {
                        std::thread::sleep(LIVE_POLL);
                    }
                }
            })
        };
        Self {
            inner,
            stop,
            thread: Some(thread),
        }
    }

    /// 最近识别出的几句，给界面实时显示。
    pub fn recent(&self, n: usize) -> Vec<LiveLine> {
        let s = self.inner.lock().unwrap();
        s.lines[s.lines.len().saturating_sub(n)..].to_vec()
    }

    /// 从第 `from` 句起的全部句子、总句数和说话人版本：界面只取新增的，长会议也不用每次传整场。
    /// 界面手里的说话人版本 `rev` 已经过时（整场重新聚类改过前面的句子）时从头给。
    pub fn lines_from(&self, from: usize, rev: u64) -> (Vec<LiveLine>, usize, u64) {
        let s = self.inner.lock().unwrap();
        let from = if rev == s.speaker_rev { from.min(s.lines.len()) } else { 0 };
        (s.lines[from..].to_vec(), s.lines.len(), s.speaker_rev)
    }

    /// 到目前为止识别出了多少句（有没有新出文字，自动结束录音时用来判断有没有人说话）。
    pub fn line_count(&self) -> usize {
        let s = self.inner.lock().unwrap();
        s.lines.iter().filter(|l| !l.text.trim().is_empty()).count()
    }

    /// 录音停止后调用：处理剩下的几秒，统一聚类，返回完整转写。
    pub fn finish(
        mut self,
        engine: &Engine,
        source: &Path,
    ) -> Result<(Transcript, Timings), String> {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            t.join().map_err(|_| {
                crate::i18n::tr("边录边转线程异常退出", "The live transcription thread crashed").to_string()
            })?;
        }
        let t = Instant::now();
        loop {
            let mut s = self.inner.lock().unwrap();
            if let Some(e) = s.error.take() {
                return Err(e);
            }
            s.mix_new(true);
            let Some((chunk, offset)) = s.chunker.next_chunk(true) else {
                break;
            };
            let segs = s.chunker.transcribe(engine, &chunk, offset)?;
            s.lines.extend(segs.into_iter().map(|g| LiveLine { start_ms: g.start_ms, speaker: None, text: g.text }));
        }
        let mut s = self.inner.lock().unwrap();
        let tail_s = t.elapsed().as_secs_f64();
        let tr = s.chunker.assemble(source);
        let mut timings = std::mem::take(&mut s.chunker.timings);
        timings.tail_s = tail_s;
        if !s.lags.is_empty() {
            timings.live_lag_avg_s = s.lags.iter().sum::<f64>() / s.lags.len() as f64;
            timings.live_lag_max_s = s.lags.iter().copied().fold(0.0, f64::max);
        }
        Ok((tr, timings))
    }
}

/// 到时候了就整场重新聚类说话人，改正前面各句的临时编号；返回是否聚了类。聚类时不持锁。
fn recluster(inner: &Mutex<LiveState>) -> bool {
    let segs = {
        let s = inner.lock().unwrap();
        if !s.recluster_due() {
            return false;
        }
        s.chunker.segments.clone()
    };
    let t = Instant::now();
    let speaker = segment_speakers(&segs);
    inner.lock().unwrap().apply_speakers(&speaker, t.elapsed());
    true
}

/// 读新数据、必要时切一段识别；返回这一步是否处理了一段。
fn step(inner: &Mutex<LiveState>, engine: &Engine, last: bool) -> bool {
    let job = {
        let mut s = inner.lock().unwrap();
        if s.error.is_some() { return false; }
        s.mix_new(last);
        if last { s.chunker.next_chunk(true) } else { s.next_live_chunk() }
    };
    let Some((chunk, offset)) = job else {
        return false;
    };
    // 识别时不持锁：界面取实时文字不用等
    let segs = engine.transcribe(&chunk, (offset * 1000.0).round() as u64);
    let mut s = inner.lock().unwrap();
    let segs = match segs { Ok(s) => s, Err(e) => { s.error = Some(e); return false; } };
    s.add_lines(&segs);
    s.chunker.segments.extend(segs);
    true
}

pub fn wav_path(dir: &Path, source: &str) -> PathBuf {
    dir.join(format!("{source}.wav"))
}

#[cfg(test)]
mod live_tests {
    use super::*;

    #[test]
    fn live_speakers_reuse_close_voices_and_split_distinct_ones() {
        let mut s = LiveSpeakers::default();
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert_eq!(s.assign(&[a.clone()]), Some(0));
        assert_eq!(s.assign(&[b.clone()]), Some(1));
        // 同一个人的声音略有变化，仍归到第一位
        assert_eq!(s.assign(&[vec![0.95, 0.1, 0.05], a.clone()]), Some(0));
        assert_eq!(s.assign(&[b]), Some(1));
        // 太短提不出声纹：不编号，由调用方沿用上一句
        assert_eq!(s.assign(&[]), None);
    }

    #[test]
    fn recluster_fixes_earlier_lines_and_bumps_revision() {
        let seg = |e: Vec<f32>| Segment { start_ms: 0, end_ms: 0, text: String::new(), embeddings: vec![e] };
        let (a, b) = (vec![1.0, 0.0, 0.0], vec![0.0, 1.0, 0.0]);
        let mut s = LiveState {
            tracks: Vec::new(),
            mixed_until: 0.0,
            chunker: Chunker::default(),
            vad: None,
            vad_fed: 0,
            speech_ends: Vec::new(),
            lines: Vec::new(),
            speakers: LiveSpeakers::default(),
            speaker_rev: 0,
            clustered: 0,
            clustered_at: None,
            cluster_cost: Duration::ZERO,
            lags: Vec::new(),
            error: None,
        };
        // 临时编号分得太碎：两个人被编成了 4 位
        let segs = vec![seg(a.clone()), seg(b.clone()), seg(vec![0.6, 0.0, 0.8]), seg(vec![0.0, 0.8, 0.6]), Segment { embeddings: vec![], ..seg(a.clone()) }];
        s.lines = (0..5).map(|i| LiveLine { start_ms: i, speaker: Some(i as usize), text: String::new() }).collect();
        s.chunker.segments = segs;
        // 整场聚类只覆盖前 3 段，后两段是聚类期间新出的：按新声纹归类，提不出声纹的沿用上一句
        s.apply_speakers(&[Some(0), Some(1), Some(0)], Duration::ZERO);
        assert_eq!(s.lines.iter().map(|l| l.speaker).collect::<Vec<_>>(), [Some(0), Some(1), Some(0), Some(1), Some(1)]);
        assert_eq!((s.speaker_rev, s.clustered), (1, 3));
        // 结果没变就不加版本
        s.apply_speakers(&[Some(0), Some(1), Some(0), Some(1), Some(1)], Duration::ZERO);
        assert_eq!(s.speaker_rev, 1);
    }
}
