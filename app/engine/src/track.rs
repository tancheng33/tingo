//! 边录边写的音轨：16 kHz 单声道 16 位 wav，按采集时间对齐。
//! Windows 上没有声音在放时系统声音一个样本都不给，设备没送数据的那段要自己补成静音，
//! 否则系统声音和麦克风两条音轨对不齐、边录边转也会停在没声音的地方。

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use crate::asr::SR;

const HEADER_LEN: u64 = 44;

/// 一段声音的开头比已经写到的位置晚这么多帧以上，才认为中间断过、补静音。
/// 小于这个数的差当成正常的抖动和重采样延迟，直接接着写，不然每一包都会多插一点静音。
pub const GAP_FRAMES: u64 = SR as u64 / 10;

pub struct TrackWriter {
    file: BufWriter<File>,
    written: u64,
}

impl TrackWriter {
    pub fn create(path: &Path) -> std::io::Result<Self> {
        let mut file = BufWriter::new(File::create(path)?);
        // 长度先写 0：边录边转按文件大小算长度（GrowingWav），录完 finish 再回填
        file.write_all(&header(0))?;
        file.flush()?;
        Ok(Self { file, written: 0 })
    }

    /// 已经写了多少帧（16 kHz）
    pub fn frames(&self) -> u64 {
        self.written
    }

    /// 写一段声音。`at` 是这段声音开头对应的帧位置（从开始录音算，16 kHz），不知道时传 None 直接接着写。
    pub fn put(&mut self, at: Option<u64>, samples: &[f32]) -> std::io::Result<()> {
        if let Some(at) = at {
            if at > self.written + GAP_FRAMES {
                self.pad_to(at)?;
            }
        }
        let bytes: Vec<u8> = samples
            .iter()
            .flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())
            .collect();
        self.file.write_all(&bytes)?;
        self.written += samples.len() as u64;
        Ok(())
    }

    /// 用静音补到第 `frame` 帧；已经写过这个位置时什么也不做
    pub fn pad_to(&mut self, frame: u64) -> std::io::Result<()> {
        const ZEROS: [u8; 8192] = [0; 8192];
        while self.written < frame {
            let n = ((frame - self.written) as usize).min(ZEROS.len() / 2);
            self.file.write_all(&ZEROS[..n * 2])?;
            self.written += n as u64;
        }
        Ok(())
    }

    /// 把缓冲写进文件，让边录边转读得到
    pub fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }

    /// 录完：回填文件头里的长度
    pub fn finish(mut self) -> std::io::Result<()> {
        self.file.flush()?;
        let data = (self.written * 2).min((u32::MAX - 36) as u64) as u32;
        let file = self.file.get_mut();
        file.seek(SeekFrom::Start(4))?;
        file.write_all(&(36 + data).to_le_bytes())?;
        file.seek(SeekFrom::Start(HEADER_LEN - 4))?;
        file.write_all(&data.to_le_bytes())?;
        file.sync_all()
    }
}

fn header(data: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity(HEADER_LEN as usize);
    v.extend(b"RIFF");
    v.extend((36 + data).to_le_bytes());
    v.extend(b"WAVEfmt ");
    v.extend(16u32.to_le_bytes());
    v.extend(1u16.to_le_bytes());
    v.extend(1u16.to_le_bytes());
    v.extend((SR as u32).to_le_bytes());
    v.extend((SR as u32 * 2).to_le_bytes());
    v.extend(2u16.to_le_bytes());
    v.extend(16u16.to_le_bytes());
    v.extend(b"data");
    v.extend(data.to_le_bytes());
    v
}

/// 把任意采样率、声道的声音流式地变成 16 kHz 单声道
pub struct To16k {
    channels: usize,
    resampler: Option<sherpa_onnx::LinearResampler>,
}

impl To16k {
    pub fn new(rate: u32, channels: usize) -> Self {
        let resampler = (rate as usize != SR).then(|| sherpa_onnx::LinearResampler::create(rate as i32, SR as i32)).flatten();
        Self { channels: channels.max(1), resampler }
    }

    /// `interleaved` 是交错排列的多声道样本
    pub fn push(&self, interleaved: &[f32]) -> Vec<f32> {
        let mono: Vec<f32> = if self.channels == 1 {
            interleaved.to_vec()
        } else {
            interleaved
                .chunks_exact(self.channels)
                .map(|f| f.iter().sum::<f32>() / self.channels as f32)
                .collect()
        };
        match &self.resampler {
            Some(r) if !mono.is_empty() => r.resample(&mono, false),
            _ => mono,
        }
    }

    /// 最后一段：把重采样器里剩下的吐出来
    pub fn finish(&self) -> Vec<f32> {
        match &self.resampler {
            Some(r) => r.resample(&[], true),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{read_wav16k, GrowingWav};

    fn temp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("tingo-track-{}-{name}.wav", std::process::id()))
    }

    #[test]
    fn gaps_become_silence_and_jitter_does_not() {
        let path = temp("gap");
        let mut t = TrackWriter::create(&path).unwrap();
        t.put(Some(0), &[0.5; 1600]).unwrap();
        // 晚了 50 帧：算抖动，接着写
        t.put(Some(1650), &[0.5; 1600]).unwrap();
        assert_eq!(t.frames(), 3200);
        // 中间断了 2 秒（没声音时系统声音不给数据）：补静音，后面的声音落在原本的时间上
        t.put(Some(3200 + 32000), &[0.5; 1600]).unwrap();
        assert_eq!(t.frames(), 3200 + 32000 + 1600);
        t.flush().unwrap();
        // 没写完也读得到（边录边转）
        assert!((GrowingWav::new(&path).seconds() - 36800.0 / 16000.0).abs() < 1e-6);
        t.pad_to(40000).unwrap();
        t.pad_to(100).unwrap();
        t.finish().unwrap();
        let x = read_wav16k(&path).unwrap();
        assert_eq!(x.len(), 40000);
        assert!(x[3199] > 0.4 && x[3200].abs() < 1e-6 && x[35199].abs() < 1e-6 && x[35200] > 0.4);
        assert!(x[39999].abs() < 1e-6);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn converts_to_16k_mono() {
        let c = To16k::new(48000, 2);
        let stereo: Vec<f32> = (0..48000).flat_map(|i| {
            let v = (i as f32 * 440.0 * std::f32::consts::TAU / 48000.0).sin() * 0.5;
            [v, v]
        }).collect();
        let mut out = Vec::new();
        for chunk in stereo.chunks(960) {
            out.extend(c.push(chunk));
        }
        out.extend(c.finish());
        assert!((out.len() as i64 - 16000).abs() < 50, "{}", out.len());
        let peak = out[1000..15000].iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!((peak - 0.5).abs() < 0.05, "{peak}");
    }
}
