//! Windows 上的录音、解码、压缩和选目录，全用系统自带的组件：
//! WASAPI 录麦克风和系统声音（回环录音，不需要任何权限和虚拟声卡），
//! Media Foundation 解码音视频、压 AAC，系统的选文件夹对话框。

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::{HSTRING, HRESULT};
use windows::Win32::Foundation::{E_ACCESSDENIED, ERROR_CANCELLED, ERROR_NOT_FOUND, HWND};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, EDataFlow, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, WAVEFORMATEX,
};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    COINIT_DISABLE_OLE1DDE, COINIT_MULTITHREADED,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::UI::Shell::{
    FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST,
    FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

use crate::asr::SR;
use crate::i18n::tr;
use crate::track::{To16k, TrackWriter};

fn com_mta() {
    // 已经初始化过（哪怕是别的套间）也没关系：WASAPI、Media Foundation 两种套间都能用
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

/// 现在的时间，100 纳秒为单位（和 WASAPI 给每包声音标的采集时间同一个钟）
fn now_hns() -> u64 {
    let (mut count, mut freq) = (0i64, 0i64);
    unsafe {
        let _ = QueryPerformanceCounter(&mut count);
        let _ = QueryPerformanceFrequency(&mut freq);
    }
    if freq <= 0 {
        return 0;
    }
    (count as u128 * 10_000_000 / freq as u128) as u64
}

fn frames_since(t0: u64, hns: u64) -> u64 {
    hns.saturating_sub(t0) * SR as u64 / 10_000_000
}

// ---------------------------------------------------------------- 录音

#[derive(Clone, Copy)]
enum Sample {
    F32,
    I16,
    I24,
    I32,
}

/// 打开的一路录音（默认的麦克风，或默认的播放设备的回环）
struct Stream {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    channels: usize,
    sample: Sample,
    convert: To16k,
}

fn device_error(loopback: bool, e: &windows::core::Error) -> String {
    let code = e.code();
    if code == E_ACCESSDENIED && !loopback {
        return tr(
            "没有麦克风权限：到「设置 → 隐私和安全性 → 麦克风」，打开「麦克风访问权限」和「允许桌面应用访问你的麦克风」",
            "No microphone permission: open Settings → Privacy & security → Microphone and turn on microphone access and “Let desktop apps access your microphone”",
        )
        .into();
    }
    if code == HRESULT::from_win32(ERROR_NOT_FOUND.0) {
        return if loopback {
            tr("没找到播放设备（扬声器或耳机），录不了系统声音", "No playback device (speakers or headphones) found, so system audio can't be recorded")
        } else {
            tr("没找到麦克风", "No microphone found")
        }
        .into();
    }
    if loopback {
        tr(&format!("打开系统声音失败：{e}"), &format!("Couldn't capture system audio: {e}")).into()
    } else {
        tr(&format!("打开麦克风失败：{e}"), &format!("Couldn't open the microphone: {e}")).into()
    }
}

impl Stream {
    fn open(loopback: bool) -> Result<Self, String> {
        let err = |e: windows::core::Error| device_error(loopback, &e);
        unsafe {
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(err)?;
            let flow: EDataFlow = if loopback { eRender } else { eCapture };
            let device = enumerator.GetDefaultAudioEndpoint(flow, eConsole).map_err(err)?;
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(err)?;
            let format = client.GetMixFormat().map_err(err)?;
            let parsed = parse_format(format);
            let flags = if loopback { AUDCLNT_STREAMFLAGS_LOOPBACK } else { 0 };
            // 用设备自己的格式（不让系统转换），缓冲 2 秒，自己转成 16 kHz 单声道
            let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 20_000_000, 0, format, None);
            CoTaskMemFree(Some(format as *const _));
            init.map_err(err)?;
            let (rate, channels, sample) = parsed.ok_or_else(|| {
                tr("录音设备的声音格式不支持", "The audio device uses an unsupported format").to_string()
            })?;
            let capture: IAudioCaptureClient = client.GetService().map_err(err)?;
            client.Start().map_err(err)?;
            Ok(Self { client, capture, channels, sample, convert: To16k::new(rate, channels) })
        }
    }

    /// 把设备里攒着的声音都取出来交给 `sink(采集时间, 16 kHz 单声道样本)`；返回取到了没有
    fn read(&mut self, sink: &mut Sink) -> Result<bool, ReadError> {
        let mut got = false;
        unsafe {
            loop {
                let n = self.capture.GetNextPacketSize().map_err(ReadError::Device)?;
                if n == 0 {
                    return Ok(got);
                }
                let mut data: *mut u8 = std::ptr::null_mut();
                let (mut frames, mut flags, mut qpc) = (0u32, 0u32, 0u64);
                self.capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))
                    .map_err(ReadError::Device)?;
                let count = frames as usize * self.channels;
                let interleaved: Vec<f32> = if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                    vec![0.0; count]
                } else {
                    to_f32(data, count, self.sample)
                };
                self.capture.ReleaseBuffer(frames).map_err(ReadError::Device)?;
                let mono = self.convert.push(&interleaved);
                got = true;
                sink((qpc != 0).then_some(qpc), &mono).map_err(ReadError::Write)?;
            }
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

/// 收声音的回调：采集时间（100 纳秒）、16 kHz 单声道样本
type Sink<'a> = dyn FnMut(Option<u64>, &[f32]) -> std::io::Result<()> + 'a;

enum ReadError {
    /// 设备没了（拔了耳机、换了默认设备）：重新打开默认设备接着录
    Device(windows::core::Error),
    /// 写文件失败：录不下去了
    Write(std::io::Error),
}

/// 设备的声音格式：采样率、声道数、样本类型
unsafe fn parse_format(f: *const WAVEFORMATEX) -> Option<(u32, usize, Sample)> {
    let w = std::ptr::read_unaligned(f);
    let mut tag = w.wFormatTag;
    if tag == 0xFFFE && w.cbSize >= 22 {
        // WAVE_FORMAT_EXTENSIBLE：真实格式在子格式 GUID 的前两个字节（紧跟在 WAVEFORMATEX 后面 8 个字节处）
        let sub = (f as *const u8).add(std::mem::size_of::<WAVEFORMATEX>() + 6) as *const u16;
        tag = std::ptr::read_unaligned(sub);
    }
    let sample = match (tag, w.wBitsPerSample, w.nBlockAlign / w.nChannels.max(1)) {
        (3, 32, _) => Sample::F32,
        (1, 16, _) => Sample::I16,
        (1, 24, 3) => Sample::I24,
        (1, 24, 4) | (1, 32, _) => Sample::I32,
        _ => return None,
    };
    (w.nChannels > 0 && w.nSamplesPerSec > 0).then_some((w.nSamplesPerSec, w.nChannels as usize, sample))
}

unsafe fn to_f32(data: *const u8, count: usize, sample: Sample) -> Vec<f32> {
    match sample {
        Sample::F32 => std::slice::from_raw_parts(data as *const f32, count).to_vec(),
        Sample::I16 => std::slice::from_raw_parts(data as *const i16, count).iter().map(|&s| s as f32 / 32768.0).collect(),
        Sample::I32 => std::slice::from_raw_parts(data as *const i32, count).iter().map(|&s| s as f32 / 2147483648.0).collect(),
        Sample::I24 => std::slice::from_raw_parts(data, count * 3)
            .chunks_exact(3)
            .map(|b| i32::from_le_bytes([0, b[0], b[1], b[2]]) as f32 / 2147483648.0)
            .collect(),
    }
}

/// 正在进行的录音：每路一个线程，写 `<dir>/system.wav`、`<dir>/mic.wav`
pub struct Capture {
    stop: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    threads: Vec<JoinHandle<()>>,
}

impl Capture {
    pub fn start(dir: &Path, system: bool, mic: bool) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        // 两路从同一个时刻算起，没数据的地方补静音，两条音轨就是对齐的
        let t0 = now_hns();
        let mut cap = Self { stop: stop.clone(), error: error.clone(), threads: Vec::new() };
        let mut ready = Vec::new();
        for (name, on) in [("system", system), ("mic", mic)] {
            if !on {
                continue;
            }
            let (tx, rx) = mpsc::channel();
            let path = dir.join(format!("{name}.wav"));
            let (stop, error) = (stop.clone(), error.clone());
            let loopback = name == "system";
            cap.threads.push(std::thread::spawn(move || record_track(loopback, &path, t0, &stop, &error, tx)));
            ready.push(rx);
        }
        for rx in ready {
            let res = rx
                .recv_timeout(Duration::from_secs(15))
                .unwrap_or_else(|_| Err(tr("录音设备没有响应", "The audio device didn't respond").into()));
            if let Err(e) = res {
                cap.stop();
                return Err(e);
            }
        }
        Ok(cap)
    }

    /// 录不下去了（比如写文件失败）时的错误
    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }

    /// 停下来、写好文件头
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop();
    }
}

fn record_track(
    loopback: bool,
    path: &Path,
    t0: u64,
    stop: &AtomicBool,
    error: &Mutex<Option<String>>,
    ready: mpsc::Sender<Result<(), String>>,
) {
    com_mta();
    let opened = Stream::open(loopback).and_then(|s| {
        let track = TrackWriter::create(path).map_err(|e| e.to_string())?;
        Ok((s, track))
    });
    let (stream, mut track) = match opened {
        Ok(x) => x,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    let write_failed = |e: std::io::Error| {
        *error.lock().unwrap() =
            Some(tr(&format!("写录音文件失败：{e}"), &format!("Couldn't write the recording: {e}")).into());
    };
    let mut stream = Some(stream);
    let mut retry_at = Instant::now();
    let put = |track: &mut TrackWriter, at: Option<u64>, x: &[f32]| track.put(at.map(|t| frames_since(t0, t)), x);
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(10));
        let got = match stream.as_mut() {
            Some(s) => match s.read(&mut |at, x| put(&mut track, at, x)) {
                Ok(got) => got,
                Err(ReadError::Write(e)) => {
                    write_failed(e);
                    return;
                }
                Err(ReadError::Device(e)) => {
                    eprintln!("录音设备断开，稍后重新打开：{e}");
                    stream = None;
                    retry_at = Instant::now() + Duration::from_millis(500);
                    false
                }
            },
            None => {
                if Instant::now() >= retry_at {
                    match Stream::open(loopback) {
                        Ok(s) => stream = Some(s),
                        Err(_) => retry_at = Instant::now() + Duration::from_secs(1),
                    }
                }
                false
            }
        };
        // 没声音在放时回环录音不给数据：落后现在 0.2 秒以上就先补上静音，边录边转不会卡住。
        // 留 0.2 秒余量，晚到的那一包还能按自己的采集时间放回原位
        let behind = frames_since(t0, now_hns()).saturating_sub(SR as u64 / 5);
        let res = if !got && track.frames() < behind { track.pad_to(behind) } else { Ok(()) };
        if let Err(e) = res.and_then(|()| track.flush()) {
            write_failed(e);
            return;
        }
    }
    if let Some(mut s) = stream.take() {
        let _ = s.read(&mut |at, x| put(&mut track, at, x));
        let tail = s.convert.finish();
        let _ = track.put(None, &tail);
    }
    // 两条音轨都补到按停止的那一刻，一样长
    let end = frames_since(t0, now_hns());
    if let Err(e) = track.pad_to(end).and_then(|()| track.finish()) {
        write_failed(e);
    }
}

// ---------------------------------------------------------------- Media Foundation

struct Mf;

impl Mf {
    fn start() -> Result<Self, String> {
        com_mta();
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) }.map_err(|e| {
            tr(
                &format!("系统的媒体组件用不了（Windows N 版需要先装「媒体功能包」）：{e}"),
                &format!("Windows Media Foundation is unavailable (Windows N editions need the Media Feature Pack): {e}"),
            )
            .to_string()
        })?;
        Ok(Self)
    }
}

impl Drop for Mf {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
        }
    }
}

const FIRST_AUDIO: u32 = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;

/// 任意音视频 → 16 kHz 单声道 16 位 wav
pub fn decode(input: &Path, out_wav: &Path) -> Result<(), String> {
    let _mf = Mf::start()?;
    let failed = |e: windows::core::Error| {
        tr(&format!("解码失败：{}", e.message()), &format!("Decoding failed: {}", e.message())).to_string()
    };
    let res = (|| unsafe {
        let reader = MFCreateSourceReaderFromURL(&HSTRING::from(input.as_os_str()), None).map_err(failed)?;
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false).map_err(failed)?;
        reader
            .SetStreamSelection(FIRST_AUDIO, true)
            .map_err(|_| tr("这个文件里没有声音", "This file has no audio track").to_string())?;
        let want = MFCreateMediaType().map_err(failed)?;
        want.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).map_err(failed)?;
        want.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float).map_err(failed)?;
        reader.SetCurrentMediaType(FIRST_AUDIO, None, &want).map_err(failed)?;
        let got = reader.GetCurrentMediaType(FIRST_AUDIO).map_err(failed)?;
        let channels = got.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).map_err(failed)?.max(1) as usize;
        let rate = got.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND).map_err(failed)?;
        let convert = To16k::new(rate, channels);
        let mut track = TrackWriter::create(out_wav).map_err(|e| e.to_string())?;
        loop {
            let mut flags = 0u32;
            let mut sample: Option<IMFSample> = None;
            reader.ReadSample(FIRST_AUDIO, 0, None, Some(&mut flags), None, Some(&mut sample)).map_err(failed)?;
            if let Some(sample) = sample {
                let buf = sample.ConvertToContiguousBuffer().map_err(failed)?;
                let (mut ptr, mut len) = (std::ptr::null_mut::<u8>(), 0u32);
                buf.Lock(&mut ptr, None, Some(&mut len)).map_err(failed)?;
                let x = std::slice::from_raw_parts(ptr as *const f32, len as usize / 4).to_vec();
                let _ = buf.Unlock();
                track.put(None, &convert.push(&x)).map_err(|e| e.to_string())?;
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                break;
            }
        }
        track.put(None, &convert.finish()).map_err(|e| e.to_string())?;
        if track.frames() == 0 {
            return Err(tr("没有解出声音", "No audio was decoded").to_string());
        }
        track.finish().map_err(|e| e.to_string())
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(out_wav);
    }
    res
}

/// AAC 编码器只认 44.1k / 48k 的输入；48k 正好是 16k 的 3 倍
const AAC_RATE: u32 = 48_000;
/// 编码器支持的最低码率：12000 字节/秒 = 96 kbps
const AAC_BYTES_PER_SEC: u32 = 12_000;

/// wav（任意格式）→ m4a（AAC）
pub fn compress(wav: &Path, dest: &Path) -> Result<(), String> {
    let _mf = Mf::start()?;
    let failed = |e: windows::core::Error| {
        tr(&format!("压缩录音失败：{}", e.message()), &format!("Compressing the recording failed: {}", e.message())).to_string()
    };
    let _ = std::fs::remove_file(dest);
    let res = (|| unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 1).map_err(failed)?;
        let attrs = attrs.ok_or_else(|| failed(windows::core::Error::from(HRESULT(-1))))?;
        // 按 MP4 容器写（.m4a 本来就是 MP4，免得按扩展名认不出来）
        attrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4).map_err(failed)?;
        let writer = MFCreateSinkWriterFromURL(&HSTRING::from(dest.as_os_str()), None, &attrs).map_err(failed)?;

        let out = MFCreateMediaType().map_err(failed)?;
        out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).map_err(failed)?;
        out.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC).map_err(failed)?;
        out.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).map_err(failed)?;
        out.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, AAC_RATE).map_err(failed)?;
        out.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 1).map_err(failed)?;
        out.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AAC_BYTES_PER_SEC).map_err(failed)?;
        let stream = writer.AddStream(&out).map_err(failed)?;

        let input = MFCreateMediaType().map_err(failed)?;
        input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).map_err(failed)?;
        input.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM).map_err(failed)?;
        input.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).map_err(failed)?;
        input.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, AAC_RATE).map_err(failed)?;
        input.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 1).map_err(failed)?;
        input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 2).map_err(failed)?;
        input.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AAC_RATE * 2).map_err(failed)?;
        writer.SetInputMediaType(stream, &input, None).map_err(failed)?;
        writer.BeginWriting().map_err(failed)?;

        // 一次读 10 秒，长录音也不整个读进内存
        let mut src = crate::audio::GrowingWav::new(wav);
        let total = src.seconds();
        if total <= 0.0 {
            return Err(tr("录音是空的", "The recording is empty").to_string());
        }
        let up = sherpa_onnx::LinearResampler::create(SR as i32, AAC_RATE as i32)
            .ok_or_else(|| tr("重采样失败", "Resampling failed").to_string())?;
        let mut written: u64 = 0;
        let mut write = |x: &[f32]| -> Result<(), String> {
            if x.is_empty() {
                return Ok(());
            }
            let bytes: Vec<u8> = x.iter().flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()).collect();
            let buf = MFCreateMemoryBuffer(bytes.len() as u32).map_err(failed)?;
            let mut ptr = std::ptr::null_mut::<u8>();
            buf.Lock(&mut ptr, None, None).map_err(failed)?;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
            buf.Unlock().map_err(failed)?;
            buf.SetCurrentLength(bytes.len() as u32).map_err(failed)?;
            let sample = MFCreateSample().map_err(failed)?;
            sample.AddBuffer(&buf).map_err(failed)?;
            let hns = |frames: u64| (frames * 10_000_000 / AAC_RATE as u64) as i64;
            sample.SetSampleTime(hns(written)).map_err(failed)?;
            sample.SetSampleDuration(hns(x.len() as u64)).map_err(failed)?;
            writer.WriteSample(stream, &sample).map_err(failed)?;
            written += x.len() as u64;
            Ok(())
        };
        let mut t = 0.0;
        while t < total {
            let end = (t + 10.0).min(total);
            let chunk = src.read(t, end);
            write(&up.resample(&chunk, false))?;
            t = end;
        }
        write(&up.resample(&[], true))?;
        writer.Finalize().map_err(failed)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(dest);
    }
    res
}

// ---------------------------------------------------------------- 选目录

/// 系统的选文件夹对话框，从 `start` 打开；取消时返回 None
pub fn choose_directory(start: &Path) -> Result<Option<String>, String> {
    let start = start.to_path_buf();
    // 对话框要单线程套间，开个新线程，不受调用线程之前初始化成什么套间影响
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let err = |e: windows::core::Error| e.message().to_string();
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).map_err(err)?;
        let options = dialog.GetOptions().map_err(err)?;
        dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST).map_err(err)?;
        let _ = dialog.SetTitle(&HSTRING::from(tr("选择存储目录", "Choose a storage folder")));
        if start.is_dir() {
            if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(start.as_os_str()), None) {
                let _ = dialog.SetFolder(&item);
            }
        }
        // 用户刚在 App 窗口里点了按钮，前台窗口就是 App：挂在它上面，对话框不会跑到后面去
        let owner = GetForegroundWindow();
        let owner = (owner != HWND::default()).then_some(owner);
        match dialog.Show(owner) {
            Ok(()) => {}
            Err(e) if e.code() == HRESULT::from_win32(ERROR_CANCELLED.0) => return Ok(None),
            Err(e) => return Err(err(e)),
        }
        let item = dialog.GetResult().map_err(err)?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).map_err(err)?;
        let path = name.to_string().map_err(|e| e.to_string());
        CoTaskMemFree(Some(name.0 as *const _));
        path.map(Some)
    })
    .join()
    .map_err(|_| tr("选择目录失败", "Choosing a folder failed").to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(path: &Path, secs: f32) {
        let x: Vec<f32> = (0..(secs * SR as f32) as usize)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / SR as f32).sin() * 0.5)
            .collect();
        crate::record::write_wav16(path, &x).unwrap();
    }

    /// 压成 m4a 再解回来：长度、音量都对得上
    #[test]
    fn aac_round_trip() {
        let dir = std::env::temp_dir().join(format!("tingo-mf 测试-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (wav, m4a, back) = (dir.join("in.wav"), dir.join("录音.m4a"), dir.join("back.wav"));
        tone(&wav, 12.5);
        compress(&wav, &m4a).unwrap();
        let size = std::fs::metadata(&m4a).unwrap().len();
        assert!(size > 50_000 && size < 400_000, "{size}");
        decode(&m4a, &back).unwrap();
        let x = crate::audio::read_wav16k(&back).unwrap();
        assert!((x.len() as f32 / SR as f32 - 12.5).abs() < 0.2, "{}", x.len());
        let level = crate::audio::level(&x[SR..SR * 10]);
        assert!(level > 0.2, "{level}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn decoding_a_non_audio_file_fails_cleanly() {
        let dir = std::env::temp_dir().join(format!("tingo-mf-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("x.mp3");
        std::fs::write(&bad, b"not audio at all").unwrap();
        assert!(decode(&bad, &dir.join("out.wav")).is_err());
        assert!(!dir.join("out.wav").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 录 2 秒：CI 的机器多半没有声卡，打不开设备就跳过；打开了就要录出对齐的两条音轨
    #[test]
    fn capture_writes_aligned_tracks_when_devices_exist() {
        let dir = std::env::temp_dir().join(format!("tingo-cap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (system, mic) in [(true, false), (false, true), (true, true)] {
            let Ok(mut cap) = Capture::start(&dir, system, mic) else { continue };
            std::thread::sleep(Duration::from_secs(2));
            cap.stop();
            assert!(cap.error().is_none());
            for (name, on) in [("system", system), ("mic", mic)] {
                if on {
                    let x = crate::audio::read_wav16k(&dir.join(format!("{name}.wav"))).unwrap();
                    let secs = x.len() as f32 / SR as f32;
                    assert!((secs - 2.0).abs() < 0.3, "{name}: {secs}");
                }
            }
            let _ = std::fs::remove_file(dir.join("system.wav"));
            let _ = std::fs::remove_file(dir.join("mic.wav"));
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
