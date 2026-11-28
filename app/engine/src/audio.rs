//! 音频小工具：读取正在写入的 wav、重采样、外放回声"让路"、电平、找停顿切点。
//! 都是 Python 版 record.py / live.py 的移植，参数和行为保持一致。

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::asr::SR;

/// 读取正在被写入的 wav：录音组件持续追加数据，文件头里的长度字段不可信，按文件大小算。
pub struct GrowingWav {
    path: PathBuf,
    header: Option<Header>,
}

#[derive(Clone, Copy)]
struct Header {
    float: bool,
    channels: usize,
    rate: usize,
    width: usize,
    data_start: u64,
    /// 文件头里写的 data 块长度（字节）。录音被打断时可能是 0 或者没回填
    data_len: u32,
}

impl GrowingWav {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            header: None,
        }
    }

    fn header(&mut self) -> Option<Header> {
        if self.header.is_none() {
            self.header = parse_header(&self.path);
        }
        self.header
    }

    pub fn seconds(&mut self) -> f64 {
        let Some(h) = self.header() else { return 0.0 };
        let size = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        let frames = size.saturating_sub(h.data_start) / (h.width * h.channels) as u64;
        frames as f64 / h.rate as f64
    }

    /// [start_s, end_s) 这段，单声道 16 kHz。
    pub fn read(&mut self, start_s: f64, end_s: f64) -> Vec<f32> {
        let Some(h) = self.header() else {
            return Vec::new();
        };
        if end_s <= start_s {
            return Vec::new();
        }
        let frame = h.width * h.channels;
        let a = (start_s * h.rate as f64) as u64;
        let b = (end_s * h.rate as f64) as u64;
        let mut raw = vec![0u8; ((b - a) as usize) * frame];
        let Ok(mut f) = File::open(&self.path) else {
            return Vec::new();
        };
        if f.seek(SeekFrom::Start(h.data_start + a * frame as u64))
            .is_err()
        {
            return Vec::new();
        }
        let n = read_up_to(&mut f, &mut raw);
        raw.truncate(n - n % frame);
        let mono = decode_mono(&raw, h);
        resample(&mono, h.rate)
    }
}

fn read_up_to(f: &mut File, buf: &mut [u8]) -> usize {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => n += k,
        }
    }
    n
}

fn parse_header(path: &Path) -> Option<Header> {
    let mut head = vec![0u8; 4096];
    let n = read_up_to(&mut File::open(path).ok()?, &mut head);
    let head = &head[..n];
    let find = |tag: &[u8]| head.windows(4).position(|w| w == tag);
    let (fmt_at, data_at) = (find(b"fmt ")?, find(b"data")?);
    if !head.starts_with(b"RIFF") || head.get(8..12) != Some(b"WAVE")
        || fmt_at + 24 > head.len() || data_at + 8 > head.len()
    {
        return None;
    }
    let u16_at = |i: usize| u16::from_le_bytes([head[i], head[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([head[i], head[i + 1], head[i + 2], head[i + 3]]);
    let mut tag = u16_at(fmt_at + 8);
    let channels = u16_at(fmt_at + 10) as usize;
    let rate = u32_at(fmt_at + 12) as usize;
    let bits = u16_at(fmt_at + 22) as usize;
    if tag == 0xFFFE {
        if fmt_at + 34 > head.len() { return None; }
        tag = u16_at(fmt_at + 8 + 24); // WAVE_FORMAT_EXTENSIBLE：真实格式在子格式 GUID 的前两个字节
    }
    if channels == 0 || rate == 0 { return None; }
    let float = match (tag, bits) {
        (3, 32) => true,
        (1, 16) => false,
        _ => return None,
    };
    Some(Header {
        float,
        channels: channels.max(1),
        rate,
        width: bits / 8,
        data_start: data_at as u64 + 8,
        data_len: u32_at(data_at + 4),
    })
}

/// 进程被杀后 WAV 的长度可能还没回填。按实际数据量流式复制并重建文件头，
/// 不修改原始录音，也不把长录音整个读入内存。仅在录音已停止后调用。
pub fn recover_wav(source: &Path, dest: &Path) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    let invalid = || Error::new(ErrorKind::InvalidData, "No recoverable PCM audio");
    let h = parse_header(source).ok_or_else(invalid)?;
    let mut input = File::open(source)?;
    let frame = (h.width * h.channels) as u64;
    let size = input.metadata()?.len().saturating_sub(h.data_start);
    let size = size - size % frame;
    if size == 0 || size > (u32::MAX - 36) as u64 { return Err(invalid()); }
    input.seek(SeekFrom::Start(h.data_start))?;
    let staging = dest.with_extension("recovering");
    let result = (|| {
        let mut output = File::create(&staging)?;
        output.write_all(b"RIFF")?;
        output.write_all(&(36 + size as u32).to_le_bytes())?;
        output.write_all(b"WAVEfmt ")?;
        output.write_all(&16u32.to_le_bytes())?;
        output.write_all(&(if h.float { 3u16 } else { 1u16 }).to_le_bytes())?;
        output.write_all(&(h.channels as u16).to_le_bytes())?;
        output.write_all(&(h.rate as u32).to_le_bytes())?;
        output.write_all(&(h.rate as u32 * frame as u32).to_le_bytes())?;
        output.write_all(&(frame as u16).to_le_bytes())?;
        output.write_all(&(h.width as u16 * 8).to_le_bytes())?;
        output.write_all(b"data")?;
        output.write_all(&(size as u32).to_le_bytes())?;
        if std::io::copy(&mut input.take(size), &mut output)? != size {
            return Err(Error::new(ErrorKind::UnexpectedEof, "Recording changed during recovery"));
        }
        output.sync_all()?;
        drop(output);
        std::fs::rename(&staging, dest)
    })();
    if result.is_err() { let _ = std::fs::remove_file(staging); }
    result
}

fn decode_mono(raw: &[u8], h: Header) -> Vec<f32> {
    let frame = h.width * h.channels;
    raw.chunks_exact(frame)
        .map(|f| {
            let sum: f32 = f
                .chunks_exact(h.width)
                .map(|s| {
                    if h.float {
                        f32::from_le_bytes([s[0], s[1], s[2], s[3]])
                    } else {
                        i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0
                    }
                })
                .sum();
            sum / h.channels as f32
        })
        .collect()
}

pub fn resample(x: &[f32], rate: usize) -> Vec<f32> {
    if rate == SR || x.is_empty() {
        return x.to_vec();
    }
    match sherpa_onnx::LinearResampler::create(rate as i32, SR as i32) {
        Some(r) => r.resample(x, true),
        None => Vec::new(),
    }
}

/// 读整个 wav 文件为 16 kHz 单声道（只认 16 位整型 / 32 位浮点 PCM）。
/// 16 kHz 单声道 16 位的 wav 的文件头（录音、解码出来的都是这个格式）；不是这个格式返回 None
fn wav16_header(path: &Path) -> Option<Header> {
    parse_header(path).filter(|h| !h.float && h.channels == 1 && h.rate == SR && h.width == 2)
}

/// 是不是 16 kHz 单声道 16 位的 wav，是的话可以直接拿来拼接。
pub fn is_wav16(path: &Path) -> bool {
    wav16_header(path).is_some()
}

/// 把几段 16 kHz 单声道 16 位的 wav 首尾接成一个。流式复制，不把声音读进内存（一小时的录音有一百多 MB）。
/// 返回每一段的采样数。长度按文件头里 data 块的长度算，data 块后面还有别的块（LIST、id3）时不会把它们当成声音；
/// 文件头没写完（被中断的录音，长度是 0 或者比文件还长）时按文件实际大小算。
pub fn concat_wav16(parts: &[PathBuf], dest: &Path) -> std::io::Result<Vec<u64>> {
    use std::io::{Error, ErrorKind};
    let invalid = |p: &Path| Error::new(ErrorKind::InvalidData, format!("not a 16 kHz mono 16-bit wav: {}", p.display()));
    let mut spans = Vec::with_capacity(parts.len()); // 每一段数据的起点和字节数
    for p in parts {
        let h = wav16_header(p).ok_or_else(|| invalid(p))?;
        let actual = std::fs::metadata(p)?.len().saturating_sub(h.data_start);
        let declared = h.data_len as u64;
        // 只有 data 块后面确实接着另一个块时才按文件头截：文件头里的长度过期（录音被打断）时不能截掉真实的声音
        let size = if declared > 0 && declared < actual && next_chunk_at(p, h.data_start + declared + declared % 2) {
            declared
        } else {
            actual
        };
        spans.push((h.data_start, size - size % 2));
    }
    let total: u64 = spans.iter().map(|s| s.1).sum();
    if total > (u32::MAX - 36) as u64 {
        return Err(Error::new(ErrorKind::InvalidData, "merged recording is too long for a wav file"));
    }
    let mut out = File::create(dest)?;
    out.write_all(b"RIFF")?;
    out.write_all(&(36 + total as u32).to_le_bytes())?;
    out.write_all(b"WAVEfmt ")?;
    out.write_all(&16u32.to_le_bytes())?;
    out.write_all(&1u16.to_le_bytes())?;
    out.write_all(&1u16.to_le_bytes())?;
    out.write_all(&(SR as u32).to_le_bytes())?;
    out.write_all(&(SR as u32 * 2).to_le_bytes())?;
    out.write_all(&2u16.to_le_bytes())?;
    out.write_all(&16u16.to_le_bytes())?;
    out.write_all(b"data")?;
    out.write_all(&(total as u32).to_le_bytes())?;
    for (p, &(start, size)) in parts.iter().zip(&spans) {
        let mut input = File::open(p)?;
        input.seek(SeekFrom::Start(start))?;
        if std::io::copy(&mut input.take(size), &mut out)? != size {
            return Err(Error::new(ErrorKind::UnexpectedEof, format!("recording changed while merging: {}", p.display())));
        }
    }
    out.sync_all()?;
    Ok(spans.iter().map(|s| s.1 / 2).collect())
}

/// 文件的这个位置是不是一个 RIFF 块的开头：4 个可打印 ASCII 字符的块名（LIST、id3 ……），
/// 后面写的块长度也装得下（不会超出文件末尾）
fn next_chunk_at(path: &Path, at: u64) -> bool {
    let mut head = [0u8; 8];
    let Ok(len) = std::fs::metadata(path).map(|m| m.len()) else { return false };
    let read = File::open(path).and_then(|mut f| {
        f.seek(SeekFrom::Start(at))?;
        f.read_exact(&mut head)
    });
    let size = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as u64;
    read.is_ok() && head[..4].iter().all(|b| b.is_ascii_graphic() || *b == b' ') && at + 8 + size <= len
}

pub fn read_wav16k(path: &Path) -> Option<Vec<f32>> {
    let mut w = GrowingWav::new(path);
    let secs = w.seconds();
    (secs > 0.0).then(|| w.read(0.0, secs))
}

/// 外放回声"让路"：系统声音里有人说话时只用系统声音，其余时间用麦克风。
///
/// 系统声音一有信号就关麦克风，并多保持 hold_ms 盖住回声尾巴；ScreenCaptureKit 的系统声音轨
/// 比麦克风里的回声还晚 40～55 ms，所以再提前 pre_ms 关。代价是双方同时说话时本地一侧会被盖掉。
pub fn duck_mix(
    system: &[f32],
    mic: &[f32],
    threshold_db: f32,
    hold_ms: usize,
    pre_ms: usize,
    fade_ms: usize,
) -> Vec<f32> {
    let n = system.len().max(mic.len());
    let at = |v: &[f32], i: usize| v.get(i).copied().unwrap_or(0.0);
    let frame = SR / 50; // 20 ms
    let frames = n.div_ceil(frame);
    let active: Vec<bool> = (0..frames)
        .map(|f| {
            let e: f32 = (f * frame..((f + 1) * frame).min(n))
                .map(|i| at(system, i).powi(2))
                .sum::<f32>()
                / frame as f32;
            10.0 * (e + 1e-12).log10() > threshold_db
        })
        .collect();
    let (hold, pre) = ((hold_ms / 20).max(1), pre_ms / 20);
    let open: Vec<f32> = (0..frames)
        .map(|f| {
            let lo = f.saturating_sub(hold);
            let hi = (f + pre).min(frames - 1);
            if active[lo..=hi].iter().any(|&a| a) {
                0.0
            } else {
                1.0
            }
        })
        .collect();
    // 每个样本的麦克风增益，做 fade_ms 的平滑，避免开关时的咔哒声
    let raw_gain: Vec<f32> = (0..n).map(|i| open[i / frame]).collect();
    let fade = (SR * fade_ms / 1000).max(1);
    let half = fade / 2;
    let mut prefix = vec![0.0f64; n + 1];
    for i in 0..n {
        prefix[i + 1] = prefix[i] + raw_gain[i] as f64;
    }
    (0..n)
        .map(|i| {
            let (a, b) = (i.saturating_sub(half), (i + fade - half).min(n));
            let g = ((prefix[b] - prefix[a]) / fade as f64) as f32;
            at(system, i) + at(mic, i) * g
        })
        .collect()
}

pub fn duck_mix_default(system: &[f32], mic: &[f32]) -> Vec<f32> {
    duck_mix(system, mic, -50.0, 700, 120, 10)
}

/// 正在写入的 wav 末尾 window_s 秒的音量，0～1，界面画电平用。
pub fn track_level(path: &Path, window_s: f64) -> f32 {
    let mut w = GrowingWav::new(path);
    let end = w.seconds();
    if end <= 0.0 {
        return 0.0;
    }
    level(&w.read((end - window_s).max(0.0), end))
}

/// 一段样本的音量，0～1（-60 dB 以下为 0，-10 dB 以上为 1），和 [`track_level`] 同一刻度。
pub fn level(x: &[f32]) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    let e: f32 = x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32;
    let db = 10.0 * (e + 1e-12).log10();
    ((db + 60.0) / 50.0).clamp(0.0, 1.0)
}

/// 整个 wav 里每 window_s 秒一段、最响那段的音量（0～1）；文件读不出或没有数据时返回 None。
pub fn max_level(path: &Path, window_s: f64) -> Option<f32> {
    let x = read_wav16k(path)?;
    let win = ((window_s * SR as f64) as usize).max(1);
    Some(x.chunks(win).map(level).fold(0.0, f32::max))
}

/// 把 wav 截到前 keep_s 秒：截短文件，并改写 data 块长度和 RIFF 头里的总长度。
/// 本来就不比 keep_s 长时不动它，返回 false。
pub fn truncate_wav(path: &Path, keep_s: f64) -> std::io::Result<bool> {
    let Some(h) = parse_header(path) else {
        return Err(std::io::Error::other("not a PCM wav file"));
    };
    let size = std::fs::metadata(path)?.len();
    let frame = (h.width * h.channels) as u64;
    let frames = size.saturating_sub(h.data_start) / frame;
    let keep = (keep_s.max(0.0) * h.rate as f64) as u64;
    if keep >= frames {
        return Ok(false);
    }
    let data_len = keep * frame;
    let mut f = OpenOptions::new().write(true).open(path)?;
    f.set_len(h.data_start + data_len)?;
    f.seek(SeekFrom::Start(4))?;
    f.write_all(&((h.data_start + data_len - 8) as u32).to_le_bytes())?;
    f.seek(SeekFrom::Start(h.data_start - 4))?;
    f.write_all(&(data_len as u32).to_le_bytes())?;
    Ok(true)
}

pub const MIN_CHUNK_S: f64 = 10.0;
pub const MAX_CHUNK_S: f64 = 30.0;
pub const SEARCH_S: f64 = 6.0;
const QUIET_WIN_S: f64 = 0.4;

/// 在片段末尾 SEARCH_S 秒里找最安静的窗口中点作为切点；一直有人说话且未到强制长度时返回 None。
pub fn find_cut(audio: &[f32], force: bool, min_s: f64, search_s: f64) -> Option<usize> {
    let n = audio.len();
    let lo = ((min_s - search_s) * SR as f64)
        .max(n as f64 - search_s * SR as f64)
        .max(0.0) as usize;
    let win = (QUIET_WIN_S * SR as f64) as usize;
    let hop = win / 4;
    if n < lo + win {
        return force.then_some(n);
    }
    let starts: Vec<usize> = (lo..n - win).step_by(hop).collect();
    let energy: Vec<f32> = starts
        .iter()
        .map(|&s| audio[s..s + win].iter().map(|v| v * v).sum::<f32>() / win as f32)
        .collect();
    let (best, &quiet) = energy
        .iter()
        .enumerate()
        .min_by(|a, b| a.1.total_cmp(b.1))?;
    let mut sorted = energy.clone();
    sorted.sort_by(f32::total_cmp);
    let loud = sorted[((sorted.len() - 1) as f32 * 0.9) as usize];
    let db = |e: f32| 10.0 * (e + 1e-12).log10();
    (force || db(quiet) < db(loud) - 15.0).then_some(starts[best] + win / 2)
}

/// 自动增益：把偏轻的声音放大到正常说话的音量再做静音检测和识别。
/// 录音离麦克风远、对方声音小时，整段只有 -45 dBFS 左右，Silero 会把大部分说话当成安静、整句漏掉
/// （实测一场 114 秒的会只识别出 131 字，放大后 598 字）。
/// 每 0.1 秒算一次音量，取前后各约 1 秒里的最大值决定增益（说话声决定增益，停顿处不会把底噪拉上来），
/// 最多放大 `AGC_MAX_GAIN_DB`，增益平滑过渡，峰值限制在 0.98 以内。正常音量的录音基本不变。
pub fn agc(audio: &[f32]) -> Vec<f32> {
    const FRAME: usize = SR / 10;
    const SPAN: usize = 10; // 前后各 10 帧（1 秒）
    const TARGET_DB: f32 = -20.0;
    if audio.is_empty() {
        return Vec::new();
    }
    let rms_db: Vec<f32> = audio
        .chunks(FRAME)
        .map(|c| {
            let e = c.iter().map(|v| v * v).sum::<f32>() / c.len() as f32;
            10.0 * (e + 1e-12).log10()
        })
        .collect();
    let gains: Vec<f32> = (0..rms_db.len())
        .map(|i| {
            let lo = i.saturating_sub(SPAN);
            let hi = (i + SPAN + 1).min(rms_db.len());
            let loud = rms_db[lo..hi].iter().copied().fold(f32::MIN, f32::max);
            10f32.powf((TARGET_DB - loud).clamp(0.0, AGC_MAX_GAIN_DB) / 20.0)
        })
        .collect();
    audio
        .iter()
        .enumerate()
        .map(|(n, &v)| {
            // 相邻两帧的增益之间线性过渡，不会出现台阶
            let pos = n as f32 / FRAME as f32 - 0.5;
            let i = (pos.floor().max(0.0) as usize).min(gains.len() - 1);
            let j = (i + 1).min(gains.len() - 1);
            let t = (pos - i as f32).clamp(0.0, 1.0);
            let g = gains[i] + (gains[j] - gains[i]) * t;
            (v * g).clamp(-0.98, 0.98)
        })
        .collect()
}

/// 自动增益最多放大多少分贝：再大就会把纯底噪拉成"有人说话"
pub const AGC_MAX_GAIN_DB: f32 = 24.0;

/// 一段识别结果至少要比周围底噪响这么多分贝才算有人说话，见 [`NoiseFloor`]。
/// 实测没人说话时（风扇、空调、远处的动静）识别出的"嗯""好的""是不是"只比底噪高 2.5～8 dB，
/// 正常说话大多高 13～37 dB，离麦克风远的轻声说话也大多在 10 dB 以上。
pub const MIN_SPEECH_SNR_DB: f32 = 9.0;
/// 不到 [`SHORT_MS`]、比底噪响但几乎没有浊音（[`voiced_fraction`] 不到这个比例）的段是敲键盘、点鼠标、碰桌子这类声音，
/// 除非响到 [`LOUD_SNR_DB`]。实测这类误识别都不到 1 秒，浊音比例 0～0.04；
/// 更长的段不看浊音：远处有人说话时噪声盖住了音高，整句浊音也可能测成 0，挡掉就漏了真话。
pub const MIN_VOICED: f32 = 0.05;
pub const SHORT_MS: u64 = 1000;
pub const LOUD_SNR_DB: f32 = 20.0;

/// 底噪估计，用来挡掉"没人说话也识别出字"。
///
/// [`agc`] 为了不漏掉轻声说话，会把安静的片段放大最多 24 dB，没人说话时房间底噪也被拉到说话的音量，
/// 静音检测就当成有人在说，识别模型照例吐出"嗯""好的""是的"这类最常见的字，每句声纹又不像任何人，
/// 实时转写里就冒出一个个新的说话人。边录边转每段只有零点几到十几秒，整段都是底噪时增益直接拉满，比整段转写更容易出现。
/// 这里在没放大的原始音频上每 0.1 秒记一次音量，某段话前后各 30 秒里第 10 百分位当底噪，
/// 段内第 90 百分位比底噪响多少就是这段的信噪比；只看相对底噪的高低，不看绝对音量，离麦克风远、整体很轻的会照样保留。
/// 系统声音"让路"时混进来的数字静音（全零）不算底噪。
#[derive(Default)]
pub struct NoiseFloor {
    frames: Vec<f32>, // 从录音开头起每 0.1 秒的音量（dBFS）
    carry: Vec<f32>,  // 不够一帧的尾巴
}

impl NoiseFloor {
    const FRAME: usize = SR / 10;
    const CONTEXT: usize = 300; // 前后各 30 秒
    const MIN_CONTEXT: usize = 10; // 不到 1 秒的非静音数据时不判断
    const DIGITAL_SILENCE_DB: f32 = -100.0;

    /// 依次喂入从录音开头起的原始音频（没放大）
    pub fn feed(&mut self, audio: &[f32]) {
        self.carry.extend_from_slice(audio);
        let whole = self.carry.len() / Self::FRAME * Self::FRAME;
        self.frames.extend(self.carry[..whole].chunks(Self::FRAME).map(level_db));
        self.carry.drain(..whole);
    }

    /// `[start_ms, end_ms)` 这段比周围底噪响多少分贝；数据不够、判断不了时 None。
    pub fn snr(&self, start_ms: u64, end_ms: u64) -> Option<f32> {
        let n = self.frames.len();
        let a = (start_ms / 100) as usize;
        let b = (end_ms.div_ceil(100) as usize).max(a + 1).min(n);
        if a >= b {
            return None;
        }
        let audible = |x: &&f32| **x > Self::DIGITAL_SILENCE_DB;
        let mut seg: Vec<f32> = self.frames[a..b].iter().filter(audible).copied().collect();
        let ctx = &self.frames[a.saturating_sub(Self::CONTEXT)..(b + Self::CONTEXT).min(n)];
        let mut ctx: Vec<f32> = ctx.iter().filter(audible).copied().collect();
        if seg.is_empty() || ctx.len() < Self::MIN_CONTEXT {
            return None;
        }
        Some(percentile(&mut seg, 0.9) - percentile(&mut ctx, 0.1))
    }

    /// `[start_ms, end_ms)` 这段（原始音频 `audio`）像不像有人在说话；判断不了时算是
    pub fn is_speech(&self, start_ms: u64, end_ms: u64, audio: &[f32]) -> bool {
        match self.snr(start_ms, end_ms) {
            None => true,
            Some(snr) if snr < MIN_SPEECH_SNR_DB => false,
            Some(snr) if snr < LOUD_SNR_DB && end_ms.saturating_sub(start_ms) <= SHORT_MS => voiced_fraction(audio) >= MIN_VOICED,
            Some(_) => true,
        }
    }
}

/// 一段音频里有音高（浊音）的比例：40 毫秒一帧、每 20 毫秒一次，只看比段内最响处低不到 25 dB 的帧，
/// 自相关在 70～400 Hz 的基音周期上超过 0.5 就算浊音。说话（连"嗯"这样的鼻音）都有浊音，
/// 键盘、鼠标、碰撞这类声音是宽带的冲击，没有周期。
pub fn voiced_fraction(audio: &[f32]) -> f32 {
    const F: usize = SR / 25;
    const HOP: usize = SR / 50;
    const LAGS: std::ops::Range<usize> = SR / 400..SR / 70;
    if audio.len() < F {
        return 0.0;
    }
    let frames: Vec<&[f32]> = (0..=audio.len() - F).step_by(HOP).map(|i| &audio[i..i + F]).collect();
    let db: Vec<f32> = frames.iter().map(|f| level_db(f)).collect();
    let top = db.iter().copied().fold(f32::MIN, f32::max);
    let (mut loud, mut voiced) = (0, 0);
    for (f, d) in frames.iter().zip(&db) {
        if *d <= top - 25.0 {
            continue;
        }
        loud += 1;
        let mean = f.iter().sum::<f32>() / F as f32;
        let x: Vec<f32> = f.iter().map(|v| v - mean).collect();
        let energy: f32 = x.iter().map(|v| v * v).sum();
        if energy <= 0.0 {
            continue;
        }
        let best = LAGS
            .map(|k| x[..F - k].iter().zip(&x[k..]).map(|(a, b)| a * b).sum::<f32>())
            .fold(f32::MIN, f32::max);
        voiced += (best / energy > 0.5) as usize;
    }
    voiced as f32 / loud.max(1) as f32
}

fn level_db(x: &[f32]) -> f32 {
    10.0 * (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32 + 1e-12).log10()
}

fn percentile(x: &mut [f32], p: f32) -> f32 {
    let k = ((x.len() - 1) as f32 * p).round() as usize;
    *x.select_nth_unstable_by(k, |a, b| a.total_cmp(b)).1
}

/// 边录边转时每段最短、最长秒数：说完一句（遇到停顿）就切，一直没停顿最多攒 12 秒
pub const LIVE_MIN_CHUNK_S: f64 = 2.5;
pub const LIVE_MAX_CHUNK_S: f64 = 12.0;

/// 边录边转的切点：从 `min_s` 起找第一个停顿（0.4 秒窗口比这段里的说话声低 15 dB，或本身接近无声），
/// 说完一句就能马上识别；一直没有停顿、攒到 `max_s` 时退回 [`find_cut`] 在末尾几秒里强制切。
/// 和 [`find_cut`] 不同，不会因为说完后一直安静（末尾全是静音、比不出高低）而一直等到最长。
pub fn find_live_cut(audio: &[f32], min_s: f64, max_s: f64) -> Option<usize> {
    let n = audio.len();
    let min = (min_s * SR as f64) as usize;
    let max = (max_s * SR as f64) as usize;
    let win = (QUIET_WIN_S * SR as f64) as usize;
    let hop = win / 4;
    if n < min.max(win) {
        return None;
    }
    let db = |s: usize| {
        let e = audio[s..s + win].iter().map(|v| v * v).sum::<f32>() / win as f32;
        10.0 * (e + 1e-12).log10()
    };
    let starts: Vec<usize> = (0..=n - win).step_by(hop).collect();
    let levels: Vec<f32> = starts.iter().map(|&s| db(s)).collect();
    let mut sorted = levels.clone();
    sorted.sort_by(f32::total_cmp);
    let loud = sorted[((sorted.len() - 1) as f32 * 0.9) as usize];
    let quiet = |l: f32| l < loud - 15.0 || l < -50.0;
    if let Some(i) = starts.iter().zip(&levels).position(|(&s, &l)| s + win / 2 >= min && quiet(l)) {
        return Some(starts[i] + win / 2);
    }
    (n >= max).then(|| find_cut(&audio[..max.min(n)], true, max_s, 3.0)).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agc_lifts_quiet_speech_and_leaves_normal_audio_alone() {
        let db = |x: &[f32]| 10.0 * (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32 + 1e-12).log10();
        // -45 dBFS 左右的轻声说话：放大到接近正常音量
        let quiet = tone(3.0, 0.008);
        let up = agc(&quiet);
        assert!(db(&up) - db(&quiet) > 18.0, "{} -> {}", db(&quiet), db(&up));
        // 正常音量：不动
        let normal = tone(3.0, 0.2);
        assert!((db(&agc(&normal)) - db(&normal)).abs() < 0.5);
        // 纯静音最多放大到上限，不会变成大声
        let hiss: Vec<f32> = (0..SR * 2).map(|i| if i % 2 == 0 { 0.0005 } else { -0.0005 }).collect();
        assert!(db(&agc(&hiss)) - db(&hiss) <= AGC_MAX_GAIN_DB + 0.1);
        // 不削波
        assert!(agc(&tone(1.0, 0.9)).iter().all(|v| v.abs() <= 0.98));
    }

    #[test]
    fn live_cut_takes_the_first_pause_or_waits() {
        // 说 3 秒、停 0.6 秒、再说：在停顿处切，不等攒满
        let mut x = [tone(3.0, 0.3), silence(0.6), tone(2.0, 0.3)].concat();
        let cut = find_live_cut(&x, 2.5, 12.0).unwrap();
        assert!((3 * SR as usize..(3.6 * SR as f32) as usize).contains(&cut), "{cut}");
        // 还没到最短长度：等
        assert_eq!(find_live_cut(&x[..(2.0 * SR as f32) as usize], 2.5, 12.0), None);
        // 说完后一直安静：马上切，不会一直等到最长
        let done = [tone(3.0, 0.3), silence(4.0)].concat();
        assert!(find_live_cut(&done, 2.5, 12.0).unwrap() < (3.6 * SR as f32) as usize);
        // 整段没人说话：到最短长度就切掉
        assert!(find_live_cut(&silence(3.0), 2.5, 12.0).is_some());
        // 一直在说：没到最长不切，到了强制切
        x = tone(8.0, 0.3);
        assert_eq!(find_live_cut(&x, 2.5, 12.0), None);
        let long = tone(13.0, 0.3);
        assert!(find_live_cut(&long, 2.5, 12.0).unwrap() <= 12 * SR as usize);
    }

    #[test]
    fn noise_floor_drops_noise_and_clicks_but_keeps_quiet_speech() {
        // 底噪约 -57 dBFS 的房间
        let mut seed = 1u32;
        let mut noise = |secs: f32| -> Vec<f32> {
            (0..(secs * SR as f32) as usize)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    (seed as f32 / u32::MAX as f32 - 0.5) * 0.005
                })
                .collect()
        };
        let mix = |a: Vec<f32>, b: Vec<f32>| a.iter().zip(b).map(|(x, y)| x + y).collect::<Vec<f32>>();
        // 0～10 秒底噪；10～12 秒轻声说话（约 -43 dBFS）；12～20 秒底噪，其中 15 秒处 0.5 秒里敲三下键盘
        let mut clicks = noise(0.5);
        for at in [800, 3200, 6000] {
            for v in &mut clicks[at..at + 32] {
                *v += 0.05;
            }
        }
        let audio = [noise(10.0), mix(tone(2.0, 0.01), noise(2.0)), noise(3.0), clicks, noise(4.5)].concat();
        let mut nf = NoiseFloor::default();
        for part in audio.chunks(4000) {
            nf.feed(part);
        }
        let seg = |a: u64, b: u64| &audio[a as usize * SR / 1000..b as usize * SR / 1000];
        assert!(nf.is_speech(10_000, 12_000, seg(10_000, 12_000)), "{:?}", nf.snr(10_000, 12_000));
        assert!(!nf.is_speech(4_000, 6_000, seg(4_000, 6_000)), "{:?}", nf.snr(4_000, 6_000));
        assert!(nf.snr(15_000, 15_500).unwrap() > MIN_SPEECH_SNR_DB);
        assert!(!nf.is_speech(15_000, 15_500, seg(15_000, 15_500)));
        // 还没有足够的数据时不判断，一律保留
        assert!(NoiseFloor::default().is_speech(0, 500, &noise(0.5)));
    }

    #[test]
    fn voicing_tells_a_voice_from_noise() {
        let hum: Vec<f32> = (0..SR / 2).map(|i| 0.1 * (i as f32 * 2.0 * std::f32::consts::PI * 150.0 / SR as f32).sin()).collect();
        assert!(voiced_fraction(&hum) > 0.9);
        let mut seed = 7u32;
        let hiss: Vec<f32> = (0..SR / 2)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                seed as f32 / u32::MAX as f32 - 0.5
            })
            .collect();
        assert!(voiced_fraction(&hiss) < 0.05);
    }

    fn tone(secs: f32, amp: f32) -> Vec<f32> {
        (0..(secs * SR as f32) as usize)
            .map(|i| amp * (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / SR as f32).sin())
            .collect()
    }
    fn silence(secs: f32) -> Vec<f32> {
        vec![0.0; (secs * SR as f32) as usize]
    }

    #[test]
    fn mic_muted_while_system_active_and_before_it_starts() {
        let system = [silence(1.0), tone(1.0, 0.3), silence(2.0)].concat();
        let echo = [silence(0.95), tone(1.4, 0.15), silence(1.65)].concat(); // 回声比系统轨早 50 ms 开始
        let out = duck_mix_default(&system, &echo);
        let a = (0.95 * SR as f32) as usize..(2.6 * SR as f32) as usize;
        let leak = a
            .clone()
            .map(|i| (out[i] - system[i]).abs())
            .fold(0.0, f32::max);
        assert!(leak < 1e-3, "回声漏进来了：{leak}");
    }

    #[test]
    fn mic_passes_when_system_silent() {
        let system = [tone(1.0, 0.3), silence(3.0)].concat();
        let local = [silence(2.5), tone(1.0, 0.2), silence(0.5)].concat();
        let out = duck_mix_default(&system, &local);
        let i = (3.0 * SR as f32) as usize;
        assert!((out[i] - local[i]).abs() < 1e-4);
    }

    #[test]
    fn find_cut_picks_pause_or_waits() {
        let mut x = tone(30.0, 0.3);
        let gap = (24.0 * SR as f32) as usize;
        x[gap..gap + (0.6 * SR as f32) as usize].fill(0.0);
        let cut = find_cut(&x, false, 20.0, 10.0).unwrap();
        assert!((gap..gap + (0.6 * SR as f32) as usize).contains(&cut));
        let speech = tone(30.0, 0.3);
        assert_eq!(find_cut(&speech, false, 20.0, 10.0), None);
        assert!(find_cut(&speech, true, 20.0, 10.0).is_some());
    }

    #[test]
    fn concat_reads_only_the_data_chunk() {
        let dir = std::env::temp_dir().join(format!("tingo-concat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b, out) = (dir.join("a.wav"), dir.join("b.wav"), dir.join("out.wav"));
        crate::record::write_wav16(&a, &tone(1.0, 0.3)).unwrap();
        crate::record::write_wav16(&b, &tone(0.5, 0.3)).unwrap();
        // a 的 data 块后面跟着一个 LIST 块（导入的文件常见）
        let mut f = std::fs::OpenOptions::new().append(true).open(&a).unwrap();
        f.write_all(b"LIST\x04\x00\x00\x00INFO").unwrap();
        drop(f);
        // b 的文件头没回填（被中断的录音）：按实际大小算
        let mut bytes = std::fs::read(&b).unwrap();
        bytes[40..44].copy_from_slice(&0u32.to_le_bytes());
        std::fs::write(&b, bytes).unwrap();
        assert_eq!(concat_wav16(&[a, b], &out).unwrap(), vec![SR as u64, SR as u64 / 2]);
        assert_eq!(read_wav16k(&out).unwrap().len(), SR * 3 / 2);
        // 文件头里的长度过期（比实际短）、后面也不是别的块：整段都算
        let c = dir.join("c.wav");
        crate::record::write_wav16(&c, &tone(1.0, 0.3)).unwrap();
        let mut bytes = std::fs::read(&c).unwrap();
        bytes[40..44].copy_from_slice(&1000u32.to_le_bytes());
        std::fs::write(&c, bytes).unwrap();
        assert_eq!(concat_wav16(&[c], &out).unwrap(), vec![SR as u64]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn truncate_wav_rewrites_header() {
        let dir = std::env::temp_dir().join(format!("tingo-trunc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("mic.wav");
        crate::record::write_wav16(&p, &[tone(1.0, 0.3), silence(2.0)].concat()).unwrap();
        assert!(truncate_wav(&p, 1.5).unwrap());
        let bytes = std::fs::read(&p).unwrap();
        let data_len = (1.5 * SR as f64) as u32 * 2;
        assert_eq!(bytes.len() as u32, 44 + data_len);
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 36 + data_len);
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), data_len);
        assert_eq!(read_wav16k(&p).unwrap().len(), (1.5 * SR as f64) as usize);
        // 已经比要截的短：不动
        assert!(!truncate_wav(&p, 10.0).unwrap());
        assert_eq!(std::fs::metadata(&p).unwrap().len() as u32, 44 + data_len);
        // 电平：有声的那段够响，静音为 0
        assert!(max_level(&p, 0.15).unwrap() > 0.5);
        crate::record::write_wav16(&p, &silence(1.0)).unwrap();
        assert!(max_level(&p, 0.15).unwrap() < 0.01);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn growing_wav_reads_float_and_int16() {
        let dir = std::env::temp_dir().join(format!("tingo-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, tag: u16, bits: u16, rate: u32, body: Vec<u8>| {
            let mut v = b"RIFF\xff\xff\xff\xffWAVEfmt ".to_vec();
            let block = bits / 8;
            v.extend(16u32.to_le_bytes());
            v.extend(tag.to_le_bytes());
            v.extend(1u16.to_le_bytes());
            v.extend(rate.to_le_bytes());
            v.extend((rate * block as u32).to_le_bytes());
            v.extend(block.to_le_bytes());
            v.extend(bits.to_le_bytes());
            v.extend(b"data\xff\xff\xff\xff");
            v.extend(body);
            let p = dir.join(name);
            std::fs::write(&p, v).unwrap();
            p
        };
        let f = write(
            "f.wav",
            3,
            32,
            48000,
            (0..48000).flat_map(|_| 0.5f32.to_le_bytes()).collect(),
        );
        let mut g = GrowingWav::new(&f);
        assert!((g.seconds() - 1.0).abs() < 1e-6);
        let x = g.read(0.25, 0.75);
        assert!((x.len() as i64 - 8000).abs() <= 2);
        assert!(x[100..x.len() - 100].iter().all(|v| (v - 0.5).abs() < 1e-2));
        let i = write(
            "i.wav",
            1,
            16,
            16000,
            (0..16000).flat_map(|_| 16384i16.to_le_bytes()).collect(),
        );
        assert!(read_wav16k(&i)
            .unwrap()
            .iter()
            .all(|v| (v - 0.5).abs() < 1e-4));
        std::fs::remove_dir_all(dir).ok();
    }
}
