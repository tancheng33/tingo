//! 录音与音频文件：启停 Swift 录音组件、解码任意音视频、录完后混音并压成 m4a。
//! 全部依赖 macOS 自带能力（ScreenCaptureKit、AVFoundation、afconvert），不需要 ffmpeg。
//! iOS 上不能起子进程，这些能力由外壳通过 [`set_native`] 注入（Swift 插件）。

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crate::asr::SR;
use crate::audio::{duck_mix_default, read_wav16k};
use crate::i18n::tr;

/// 麦克风暂停后超过这么多分钟还没恢复，就不再接着录，把这场会议结束在断掉的地方。
/// 由录音组件判断（RecorderPlugin.swift 的 maxPause），它会在状态里报出自己用的上限（`MicPause::limit_s`），
/// 提示语按报上来的写；这个数只在录音组件没报的时候（旧版本）兜底。
pub const MAX_PAUSE_MIN: u64 = 30;

/// 录音暂时停着：麦克风被来电或别的 App 占用，或者 App 被系统挂起过。录音组件会自己接着录
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MicPause {
    /// 已经停了多少秒
    pub seconds: f64,
    /// 停得太久，不会再接着录了
    pub expired: bool,
    /// 录音组件停多久就不再接着录（秒）；没报时为 None
    pub limit_s: Option<f64>,
}

impl MicPause {
    /// 不再接着录的上限，按分钟（提示语用）
    pub fn limit_min(&self) -> u64 {
        self.limit_s.map_or(MAX_PAUSE_MIN, |s| (s / 60.0).round().max(1.0) as u64)
    }
}

/// 录音组件报告的状态
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MicStatus {
    /// 录不下去了（比如写文件失败）时的错误
    pub error: Option<String>,
    /// 录音暂时停着时有值
    pub pause: Option<MicPause>,
    /// 这场录音里暂停过、已经恢复的时间加起来有多少秒（这些时间没录到，文件里是静音）
    pub lost_s: f64,
}

/// 由 App 进程自己提供的录音、解码、排版能力（iOS）。设置后代替 tingo-recorder 子进程、afconvert。
/// 录音的约定和子进程一样：往 `<dir>/mic.wav` 持续追加，边录边转直接读这个文件。
pub trait Native: Send + Sync {
    /// 开始录麦克风，写到 `<dir>/mic.wav`；没有权限等情况直接返回错误
    fn start_mic(&self, dir: &Path) -> Result<(), String>;
    /// 停止录音并写好文件头
    fn stop_mic(&self);
    /// 录音中途出错录不下去了（比如写文件失败）时返回错误
    fn mic_error(&self) -> Option<String>;
    /// 录音的状态：除了错误，还有是不是暂时停着。不区分暂停的实现只报错误
    fn mic_status(&self) -> MicStatus {
        MicStatus { error: self.mic_error(), ..Default::default() }
    }
    /// 任意音视频 → 16 kHz 单声道 16 位 wav
    fn decode(&self, input: &Path, out_wav: &Path) -> Result<(), String>;
    /// wav → m4a（AAC）
    fn compress(&self, wav: &Path, out_m4a: &Path) -> Result<(), String>;
    /// HTML 文件 → A4 分页的 PDF
    fn html_to_pdf(&self, html: &Path, out: &Path) -> Result<(), String>;
}

static NATIVE: OnceLock<Arc<dyn Native>> = OnceLock::new();

/// 外壳启动时调用一次（只在 iOS 上）。
pub fn set_native(n: Arc<dyn Native>) {
    let _ = NATIVE.set(n);
}

pub fn native() -> Option<&'static Arc<dyn Native>> {
    NATIVE.get()
}

pub struct Recorder {
    /// 子进程录音时有值；`Native` 录音时为空
    child: Option<Child>,
    stdout: Option<BufReader<ChildStdout>>,
    pub dir: PathBuf,
    pub sources: Vec<String>,
    pub started: Instant,
}

impl Recorder {
    /// 启动录音组件，等它报告 READY（或出错）。
    pub fn start(bin: &Path, dir: &Path, system: bool, mic: bool) -> Result<Self, String> {
        if !system && !mic {
            return Err(tr(
                "系统声音和麦克风至少开一个",
                "Turn on at least one of system audio and microphone",
            )
            .into());
        }
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        if let Some(n) = native() {
            if !mic {
                return Err(tr("这台设备只能录麦克风", "Only the microphone can be recorded on this device").into());
            }
            n.start_mic(dir)?;
            return Ok(Self {
                child: None,
                stdout: None,
                dir: dir.to_path_buf(),
                sources: vec!["mic".into()],
                started: Instant::now(),
            });
        }
        let mut cmd = Command::new(bin);
        cmd.arg(dir);
        if !mic {
            cmd.arg("--no-mic");
        }
        if !system {
            cmd.arg("--no-system");
        }
        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                tr(&format!("启动录音组件失败：{e}"), &format!("Couldn't start the recorder: {e}")).to_string()
            })?;
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).map_err(|e| e.to_string())?;
        let line = line.trim();
        if line != "READY" {
            let _ = child.wait();
            return Err(line
                .strip_prefix("ERROR: ")
                .unwrap_or(tr("录音组件意外退出", "The recorder exited unexpectedly"))
                .to_string());
        }
        let sources = [("system", system), ("mic", mic)]
            .iter()
            .filter(|(_, on)| *on)
            .map(|(s, _)| s.to_string())
            .collect();
        Ok(Self {
            child: Some(child),
            stdout: Some(stdout),
            dir: dir.to_path_buf(),
            sources,
            started: Instant::now(),
        })
    }

    pub fn elapsed(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    /// 录音组件是否中途退出了（比如权限被收回）；退出时返回它最后报的错。
    pub fn exited(&mut self) -> Option<String> {
        let Some(child) = self.child.as_mut() else {
            return native().and_then(|n| n.mic_error());
        };
        match child.try_wait() {
            Ok(Some(_)) => {
                let mut rest = String::new();
                if let Some(mut out) = self.stdout.take() {
                    let _ = std::io::Read::read_to_string(&mut out, &mut rest);
                }
                Some(
                    rest.lines()
                        .find_map(|l| l.strip_prefix("ERROR: "))
                        .unwrap_or(tr("录音组件意外退出", "The recorder exited unexpectedly"))
                        .to_string(),
                )
            }
            _ => None,
        }
    }

    /// 录音的状态：中途退出时的错误、是不是暂时停着（后者只有 App 自己录音时才有）。
    pub fn status(&mut self) -> MicStatus {
        if self.child.is_none() {
            return native().map(|n| n.mic_status()).unwrap_or_default();
        }
        MicStatus { error: self.exited(), ..Default::default() }
    }

    /// 录音是不是暂时停着。只问这一件事，不去读子进程退出时报的错（那个只能读一次，留给 `status`）。
    pub fn pause(&self) -> Option<MicPause> {
        if self.child.is_some() {
            return None;
        }
        native().and_then(|n| n.mic_status().pause)
    }

    /// 停止录音：发 SIGINT 让它写完文件头，15 秒内没退出就强杀。原始音轨留着。
    pub fn stop(&mut self) {
        let Some(child) = self.child.as_mut() else {
            if let Some(n) = native() {
                n.stop_mic();
            }
            return;
        };
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        unsafe {
            libc::kill(child.id() as i32, libc::SIGINT);
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 任意音视频 → 16 kHz 单声道样本（经录音组件里的 AVFoundation 解码）。
/// HTML → PDF：交给录音组件用系统 WebKit 排版（A4 分页）。
pub fn html_to_pdf(bin: &Path, html: &str, out: &Path) -> Result<(), String> {
    let tmp = std::env::temp_dir().join(format!("tingo-pdf-{}-{}.html", std::process::id(), rand_suffix()));
    std::fs::write(&tmp, html).map_err(|e| e.to_string())?;
    if let Some(n) = native() {
        let res = n.html_to_pdf(&tmp, out);
        let _ = std::fs::remove_file(&tmp);
        return res;
    }
    let res = Command::new(bin).arg("pdf").arg(&tmp).arg(out).output();
    let _ = std::fs::remove_file(&tmp);
    let res = res.map_err(|e| {
        tr(&format!("启动排版失败：{e}"), &format!("Couldn't start the PDF renderer: {e}")).to_string()
    })?;
    if res.status.success() && out.is_file() {
        return Ok(());
    }
    let msg = String::from_utf8_lossy(&res.stdout);
    Err(msg
        .lines()
        .find_map(|l| l.strip_prefix("ERROR: "))
        .unwrap_or(tr("导出 PDF 失败", "PDF export failed"))
        .to_string())
}

/// 移到废纸篓（不是彻底删除）。macOS 上交给录音组件的 `trash <路径>`（FileManager.trashItem，
/// 成功打印 OK，失败打印 ERROR: …）；其它系统有 gio 就用 `gio trash`，否则报不支持。
pub fn trash(bin: &Path, path: &Path) -> Result<(), String> {
    let failed = |e: &str| tr(&format!("没能移到废纸篓：{e}"), &format!("Couldn't move to the Trash: {e}")).to_string();
    if cfg!(target_os = "macos") || bin.is_file() {
        let out = Command::new(bin).arg("trash").arg(path).stdin(Stdio::null()).output().map_err(|e| failed(&e.to_string()))?;
        let text = String::from_utf8_lossy(&out.stdout);
        if out.status.success() && text.lines().any(|l| l.trim() == "OK") {
            return Ok(());
        }
        return Err(match text.lines().find_map(|l| l.trim().strip_prefix("ERROR: ")) {
            Some(e) => e.to_string(), // 录音组件报的错已经说了是"移到废纸篓失败"
            None => failed(String::from_utf8_lossy(&out.stderr).trim()),
        });
    }
    let Some(gio) = find_in_path("gio") else {
        return Err(tr("这个系统上没法移到废纸篓", "Moving to the Trash isn't supported on this system").into());
    };
    let out = Command::new(gio).arg("trash").arg(path).stdin(Stdio::null()).output().map_err(|e| failed(&e.to_string()))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(failed(String::from_utf8_lossy(&out.stderr).trim()))
    }
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| p.is_file())
}

pub const TEXTUTIL: &str = "/usr/bin/textutil";

/// HTML → Word：macOS 自带的 textutil。没有 textutil（非 macOS）时报错。
pub fn html_to_docx(html: &str, out: &Path) -> Result<(), String> {
    if !Path::new(TEXTUTIL).is_file() {
        return Err(tr("导出 Word 只支持 macOS", "Word export is only available on macOS").into());
    }
    let tmp = std::env::temp_dir().join(format!("tingo-docx-{}-{}.html", std::process::id(), rand_suffix()));
    std::fs::write(&tmp, html).map_err(|e| e.to_string())?;
    // 不指定输入编码时，HTML 里没写 charset 的话中文会变成乱码
    let res = Command::new(TEXTUTIL)
        .args(["-inputencoding", "UTF-8", "-convert", "docx", "-output"])
        .arg(out)
        .arg(&tmp)
        .output();
    let _ = std::fs::remove_file(&tmp);
    let res = res.map_err(|e| {
        tr(&format!("启动 textutil 失败：{e}"), &format!("Couldn't start textutil: {e}")).to_string()
    })?;
    if res.status.success() && out.is_file() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&res.stderr).trim().to_string();
    Err(tr(&format!("导出 Word 失败：{err}"), &format!("Word export failed: {err}")).into())
}

/// 试录 `secs` 秒（录进临时目录，录完就删），返回每条音轨（system / mic）最响那段的音量（0～1）；
/// 音轨没录出来时是 None。录音组件起不来或中途退出时返回它报的错。
pub fn probe(bin: &Path, system: bool, mic: bool, secs: u64) -> Result<Vec<(String, Option<f32>)>, String> {
    let dir = std::env::temp_dir().join(format!("tingo-rectest-{}-{}", std::process::id(), rand_suffix()));
    let result = (|| {
        let mut rec = Recorder::start(bin, &dir, system, mic)?;
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if let Some(e) = rec.exited() {
                return Err(e);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        rec.stop();
        Ok(rec
            .sources
            .iter()
            .map(|s| (s.clone(), crate::audio::max_level(&dir.join(format!("{s}.wav")), 0.15)))
            .collect())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// 录音组件报的错是不是没有权限（录屏与系统录音、麦克风）
pub fn is_permission_error(msg: &str) -> bool {
    let m = msg.to_lowercase();
    ["权限", "permission", "declined", "not authorized", "not permitted", "tcc"].iter().any(|k| m.contains(k))
}

pub fn decode(bin: &Path, input: &Path) -> Result<Vec<f32>, String> {
    if input
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("wav"))
    {
        if let Some(x) = read_wav16k(input) {
            return Ok(x);
        }
    }
    let tmp = temp_wav("decode");
    let result = decode_to_wav(bin, input, &tmp).and_then(|()| {
        read_wav16k(&tmp).ok_or_else(|| tr("解码结果读不出来", "Couldn't read the decoded audio").to_string())
    });
    let _ = std::fs::remove_file(&tmp);
    result
}

/// 临时目录里一个不会重名的 wav 路径
pub fn temp_wav(purpose: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("tingo-{purpose}-{}-{}-{n}.wav", std::process::id(), rand_suffix()))
}

/// 任意音视频 → 16 kHz 单声道 16 位的 wav 文件。声音不经过内存，长录音也不占地方。
pub fn decode_to_wav(bin: &Path, input: &Path, out_wav: &Path) -> Result<(), String> {
    if let Some(n) = native() {
        return n.decode(input, out_wav);
    }
    let out = Command::new(bin)
        .arg("decode")
        .arg(input)
        .arg(out_wav)
        .output()
        .map_err(|e| tr(&format!("启动解码失败：{e}"), &format!("Couldn't start the decoder: {e}")).to_string())?;
    if out.status.success() {
        return Ok(());
    }
    let msg = String::from_utf8_lossy(&out.stdout);
    Err(msg
        .lines()
        .find_map(|l| l.strip_prefix("ERROR: "))
        .unwrap_or(tr("解码失败", "Decoding failed"))
        .to_string())
}

/// wav → m4a（AAC 48 kbps）：iOS 上交给 App 自己，macOS 用自带的 afconvert。
pub fn compress(wav: &Path, dest: &Path) -> Result<(), String> {
    if let Some(n) = native() {
        return n.compress(wav, dest);
    }
    let out = Command::new("/usr/bin/afconvert")
        .args(["-f", "m4af", "-d", "aac", "-b", "48000"]) // 16 kHz 单声道 AAC 最高 48 kbps
        .arg(wav)
        .arg(dest)
        .output()
        .map_err(|e| tr(&format!("压缩录音失败：{e}"), &format!("Compressing the recording failed: {e}")).to_string())?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(tr(
            &format!("压缩录音失败：{err}"),
            &format!("Compressing the recording failed: {err}"),
        )
        .into());
    }
    Ok(())
}

fn rand_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// 16 kHz 单声道 16 位 wav。
pub fn write_wav16(path: &Path, samples: &[f32]) -> std::io::Result<()> {
    let data: Vec<u8> = samples
        .iter()
        .flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())
        .collect();
    let mut v = Vec::with_capacity(44 + data.len());
    v.extend(b"RIFF");
    v.extend((36 + data.len() as u32).to_le_bytes());
    v.extend(b"WAVEfmt ");
    v.extend(16u32.to_le_bytes());
    v.extend(1u16.to_le_bytes());
    v.extend(1u16.to_le_bytes());
    v.extend((SR as u32).to_le_bytes());
    v.extend((SR as u32 * 2).to_le_bytes());
    v.extend(2u16.to_le_bytes());
    v.extend(16u16.to_le_bytes());
    v.extend(b"data");
    v.extend((data.len() as u32).to_le_bytes());
    v.extend(data);
    std::fs::write(path, v)
}

/// 录完后：系统声音和麦克风按"让路"规则合成一条，压成 m4a（macOS 自带 afconvert，AAC 48 kbps），删掉原始轨道。
pub fn mixdown(dir: &Path, dest: &Path) -> Result<(), String> {
    mix(dir, dest, false)
}

/// 和 `mixdown` 一样，但留下合成好的 mixed.wav（接着录的那一段马上要拿去合并，用完由调用方删掉）
pub fn mixdown_keeping_wav(dir: &Path, dest: &Path) -> Result<(), String> {
    mix(dir, dest, true)
}

fn mix(dir: &Path, dest: &Path, keep_wav: bool) -> Result<(), String> {
    let (sys, mic) = (dir.join("system.wav"), dir.join("mic.wav"));
    let read = |p: &Path| if p.is_file() { read_wav16k(p) } else { None };
    let mixed = match (read(&sys), read(&mic)) {
        (Some(s), Some(m)) => duck_mix_default(&s, &m),
        (Some(x), None) | (None, Some(x)) => x,
        (None, None) => return Err(tr("没有录到任何声音", "Nothing was recorded").into()),
    };
    let wav = dir.join("mixed.wav");
    write_wav16(&wav, &mixed).map_err(|e| e.to_string())?;
    compress(&wav, dest)?;
    for p in [&wav, &sys, &mic] {
        if keep_wav && p == &wav {
            continue;
        }
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}

/// iOS 启动时恢复上次被中断、尚未合成的麦克风录音。
pub fn recover_interrupted(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        let dir = entry.path();
        // 仅处理 iOS 的单麦克风录音；已有正式音频时不重复恢复。
        if !dir.is_dir() || dir.join("system.wav").exists()
            || crate::meetings::find_audio(&dir).is_some()
        {
            continue;
        }
        let source = dir.join("mic.wav");
        if !source.is_file() { continue; }
        match crate::audio::recover_wav(&source, &dir.join("录音.wav")) {
            Ok(()) => {
                eprintln!("已恢复中断的录音：{}", dir.display());
                // 做个记号，界面据此提示「接着录」；记不上不影响恢复
                let _ = crate::meetings::mark_interrupted(&dir);
            }
            Err(e) => eprintln!("恢复录音失败（原文件保留）：{}: {e}", source.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只会报错误、不区分暂停的录音实现（比如还没跟上的原生外壳）照样能用：状态里只有错误
    #[test]
    fn native_without_pause_support_reports_only_the_error() {
        struct Plain(Option<String>);
        impl Native for Plain {
            fn start_mic(&self, _: &Path) -> Result<(), String> {
                Ok(())
            }
            fn stop_mic(&self) {}
            fn mic_error(&self) -> Option<String> {
                self.0.clone()
            }
            fn decode(&self, _: &Path, _: &Path) -> Result<(), String> {
                Ok(())
            }
            fn compress(&self, _: &Path, _: &Path) -> Result<(), String> {
                Ok(())
            }
            fn html_to_pdf(&self, _: &Path, _: &Path) -> Result<(), String> {
                Ok(())
            }
        }
        assert_eq!(Plain(None).mic_status(), MicStatus::default());
        let failed = Plain(Some("磁盘满了".into())).mic_status();
        assert_eq!(failed.error.as_deref(), Some("磁盘满了"));
        assert_eq!(failed.pause, None);
        assert_eq!(failed.lost_s, 0.0);
    }

    #[test]
    fn interrupted_mic_recording_becomes_visible_without_changing_original() {
        let root = std::env::temp_dir().join(format!("tingo-recover-{}", rand_suffix()));
        let dir = root.join("2026-11-04_1400_长会议");
        std::fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("mic.wav");
        write_wav16(&raw, &[0.5; 16000]).unwrap();
        let mut bytes = std::fs::read(&raw).unwrap();
        // 进程被杀，没有机会回填 RIFF 和 data 的长度；末尾还可能有半个采样。
        bytes[4..8].copy_from_slice(&0u32.to_le_bytes());
        bytes[40..44].copy_from_slice(&0u32.to_le_bytes());
        bytes.push(255);
        std::fs::write(&raw, &bytes).unwrap();
        assert!(crate::meetings::list(&root).is_empty());

        recover_interrupted(&root);

        let listed = crate::meetings::list(&root);
        assert_eq!(listed.len(), 1);
        // 恢复出来的会议带着"被中断过"的记号，界面据此提示接着录
        assert!(listed[0].interrupted);
        let audio = crate::meetings::find_audio(&dir).unwrap();
        let recovered = std::fs::read(&audio).unwrap();
        assert_eq!(u32::from_le_bytes(recovered[40..44].try_into().unwrap()), 32000);
        assert_eq!(recovered.len(), 32044);
        assert_eq!(&recovered[44..], &bytes[44..32044]);
        assert_eq!(std::fs::read(&raw).unwrap(), bytes);
        recover_interrupted(&root);
        assert_eq!(std::fs::read(audio).unwrap(), recovered);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_leaves_finished_and_empty_recordings_alone() {
        let root = std::env::temp_dir().join(format!("tingo-recover-skip-{}", rand_suffix()));
        let complete = root.join("2026-11-04_1400_已结束");
        let empty = root.join("2026-11-04_1500_无声音");
        std::fs::create_dir_all(&complete).unwrap();
        std::fs::create_dir_all(&empty).unwrap();
        write_wav16(&complete.join("mic.wav"), &[0.5; 100]).unwrap();
        std::fs::write(complete.join(crate::meetings::AUDIO), b"existing audio").unwrap();
        write_wav16(&empty.join("mic.wav"), &[]).unwrap();
        recover_interrupted(&root);
        assert_eq!(crate::meetings::list(&root).len(), 1);
        assert_eq!(std::fs::read(complete.join(crate::meetings::AUDIO)).unwrap(), b"existing audio");
        assert!(!complete.join("录音.wav").exists());
        assert!(crate::meetings::find_audio(&empty).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mixdown_writes_m4a_and_cleans_tracks() {
        let dir = std::env::temp_dir().join(format!("tingo-mix-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tone: Vec<f32> = (0..SR).map(|i| 0.3 * (i as f32 * 0.05).sin()).collect();
        write_wav16(&dir.join("system.wav"), &tone).unwrap();
        write_wav16(&dir.join("mic.wav"), &vec![0.0; SR / 2]).unwrap();
        let dest = dir.join("录音.m4a");
        mixdown(&dir, &dest).unwrap();
        assert!(std::fs::metadata(&dest).unwrap().len() > 1000);
        assert!(!dir.join("system.wav").exists() && !dir.join("mixed.wav").exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn mixdown_without_audio_fails() {
        let dir = std::env::temp_dir().join(format!("tingo-mix-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(mixdown(&dir, &dir.join("录音.m4a")).is_err());
        std::fs::remove_dir_all(dir).ok();
    }
}
