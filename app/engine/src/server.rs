//! 本地 HTTP 服务：接口与 Python 版 server.py 完全一致，界面不用改。只监听 127.0.0.1。
//!
//! 同一时间只跑一个任务（下载模型 / 录音 / 转写 / 生成纪要），界面轮询 /api/job 拿进度。
//! 重活（识别、调大模型、下载）都在独立线程里跑，HTTP 处理只读写状态。
//! 提示文字按界面语言（请求头 X-Tingo-Lang，见 [`crate::i18n`]）给中文或英文。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{Multipart, Path as UrlPath, Query, Request, State};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tower::ServiceExt;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::services::ServeFile;

use crate::asr::{AsrKind, Engine, ModelPaths};
use crate::chunked::{transcribe_samples, wav_path, LiveTranscriber};
use crate::config::{self, Config};
use crate::i18n::{self, tr, Lang};
use crate::meetings::{self, AUDIO, MINUTES, TRANSCRIPT_JSON};
use crate::{fishbone, live_minutes, obsidian};
use crate::record::{self, Recorder};
use crate::summarize::{self, Hooks};
use crate::transcript::{fmt_ts, Transcript};
use crate::{audio, autostop, models, push, speakers, terms, update};

pub struct Options {
    pub root: PathBuf,
    pub config_path: PathBuf,
    pub models_dir: PathBuf,
    /// Swift 录音组件（录音、解码）
    pub recorder_bin: PathBuf,
    pub version: String,
    /// 内置示例会议的素材（transcript.json、纪要.md、纪要.en.md、录音.m4a）
    pub sample_dir: PathBuf,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Job {
    /// 任务编号：录音和后台任务（转写、生成纪要、下载模型）可以同时进行，界面按编号区分
    pub id: String,
    pub kind: String,
    pub meeting: String,
    pub state: String,
    pub started_at: f64,
    pub error: String,
    pub minutes: String,
    pub stage: String,
    pub sources: Vec<String>,
    /// 完成后给用户的附加提示（比如"已存到 Obsidian"）
    pub notice: String,
    /// 推理模型已经思考了多少字（还没开始写纪要时显示）
    pub thinking: u64,
    /// 结束时间；结束后用时不再增长
    pub finished_at: f64,
    /// 合并任务：正在被合并的那几场会议。合并完它们会被移走，期间不能改名、删除、重新生成纪要
    pub merging: Vec<String>,
}

fn write_minutes_draft(dir: &Path, text: &str) -> std::io::Result<()> {
    if text.trim().is_empty() { return Ok(()); }
    let tmp = dir.join("在线纪要草稿.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, dir.join("在线纪要草稿.md"))
}

impl Job {
    fn new(kind: &str, meeting: &str, state: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            id: format!("j{}", NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)),
            kind: kind.into(),
            meeting: meeting.into(),
            state: state.into(),
            started_at: now(),
            ..Default::default()
        }
    }
    fn busy(&self) -> bool {
        !matches!(self.state.as_str(), "done" | "error")
    }
}

struct Live {
    /// 录音任务的编号
    job: String,
    recorder: Recorder,
    live: Option<LiveTranscriber>,
    hotwords: Vec<String>,
    /// 最后一次有人说话的时间（录音开始后的秒数）
    last_voice: f64,
    /// 最后一次有人说话时原始音轨的长度（秒）。截掉末尾的静音按这个算：手机睡着时 `last_voice` 用的
    /// 时钟不走，录音组件接着录时却按墙钟补了静音，两边会差出一段
    last_voice_audio: f64,
    /// 没人说话多少分钟后自动结束（0 = 不自动结束），每次检查时按配置更新
    silence_min: u32,
    /// 录音中标记的重点时刻（毫秒），结束时存进会议文件夹的 marks.json
    marks: Vec<u64>,
    /// 「接着录」：录完以后接到这场会议后面，合成一场
    continue_of: Option<String>,
    /// 这次暂停刚被发现时原始音轨的长度（秒）。暂停太久自动结束时截到这里：
    /// 录音组件每次重启失败前都按墙钟补过静音，不截的话文件末尾会多出最长 30 分钟的静音
    pause_audio: Option<f64>,
    /// 边开会边整理的速记版纪要
    minutes: Arc<Mutex<LiveMinutes>>,
}

/// 录音中的速记版纪要（见 [`crate::live_minutes`]）
#[derive(Default)]
struct LiveMinutes {
    notes: live_minutes::Notes,
    /// 每更新一次加一，界面据此判断要不要重画
    version: u64,
    /// 设置里开着、也配了大模型
    enabled: bool,
    /// 正在等模型回复
    busy: bool,
    /// 上一次更新失败的原因；下一次成功就清掉
    error: Option<String>,
    /// 已经整理到第几句（实时转写的句子序号）
    cursor: usize,
    /// 整理到会议的第几毫秒
    upto_ms: u64,
    /// 纪要语言（设置是 auto 时跟着会议说的话定）
    lang: Option<Lang>,
}

pub struct AppState {
    opt: Options,
    auto_minutes: bool,
    root: Mutex<PathBuf>,
    storage_gate: Arc<tokio::sync::RwLock<()>>,
    storage_progress: Mutex<crate::storage::Progress>,
    /// 当前加载的识别引擎；切换模型（或 Fun-ASR-Nano 的热词变了）时换掉
    engine: Mutex<Option<Arc<Engine>>>,
    engine_lock: Mutex<()>,
    /// 进行中和刚结束（还没被界面收起）的任务
    jobs: Mutex<Vec<Job>>,
    recording: Mutex<Option<Live>>,
    /// 检查更新的结果，缓存 6 小时
    update: Mutex<Option<(Instant, update::Release)>>,
}

type Shared = Arc<AppState>;

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[derive(Debug)]
struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "detail": self.1 }))).into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

fn server_error(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, msg.into())
}

/// 会议有进行中的任务（录音、转写、生成纪要……）时的提示
fn busy_msg() -> &'static str {
    tr("这场会议正在处理，等它完成", "This meeting is being processed. Wait for it to finish")
}

fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, tr("找不到这场会议", "Meeting not found").into())
}

impl AppState {
    pub fn new(opt: Options) -> Shared { Self::with_auto_minutes(opt, true) }

    fn with_auto_minutes(opt: Options, auto_minutes: bool) -> Shared {
        Arc::new(Self {
            root: Mutex::new(opt.root.clone()),
            storage_gate: Arc::new(tokio::sync::RwLock::new(())),
            storage_progress: Mutex::new(crate::storage::Progress::default()),
            opt,
            auto_minutes,
            engine: Mutex::new(None),
            engine_lock: Mutex::new(()),
            jobs: Mutex::new(Vec::new()),
            recording: Mutex::new(None),
            update: Mutex::new(None),
        })
    }

    /// Keep requests from starting new work between the idle check and application restart.
    pub fn prepare_app_update(self: &Arc<Self>) -> Result<tokio::sync::OwnedRwLockWriteGuard<()>, String> {
        let guard = self.storage_gate.clone().try_write_owned()
            .map_err(|_| "正在处理请求或切换目录，请稍后再更新 / Work is in progress; try again shortly".to_string())?;
        if self.jobs.lock().unwrap().iter().any(Job::busy) {
            return Err("请先结束录音，并等待转写、纪要及下载任务完成 / Finish recording and wait for active tasks before updating".into());
        }
        Ok(guard)
    }

    fn root(&self) -> PathBuf {
        self.root.lock().unwrap().clone()
    }

    fn config(&self) -> Config {
        config::load(&self.opt.config_path)
    }

    fn model_paths(&self) -> ModelPaths {
        ModelPaths::new(&self.opt.models_dir)
    }

    /// 识别引擎：第一次用时加载（约 1 秒），之后常驻。
    fn engine(&self) -> Result<Arc<Engine>, String> {
        let paths = self.model_paths();
        let kind = paths.active();
        let cfg = self.config();
        // Fun-ASR-Nano 在加载时就要拿到热词；SenseVoice、Fun-ASR-Nano 加载时定下会议语言
        let hot = if kind == AsrKind::FunAsrNano { cfg.hotwords } else { Vec::new() };
        let lang = if kind.uses_language() { cfg.asr_language } else { String::new() };
        let current = || {
            self.engine
                .lock()
                .unwrap()
                .clone()
                .filter(|e| e.kind == kind && e.hotwords == hot && e.language == lang)
        };
        if let Some(e) = current() {
            return Ok(e);
        }
        let _guard = self.engine_lock.lock().unwrap();
        if let Some(e) = current() {
            return Ok(e);
        }
        if !paths.base_ready() || !paths.asr_ready(kind) {
            return Err(tr(
                "识别模型还没有下载，先在首页下载（约 330 MB）",
                "The speech recognition model isn't downloaded yet. Download it on the home page first (about 330 MB)",
            )
            .into());
        }
        let threads = std::thread::available_parallelism()
            .map(|n| n.get().clamp(2, 4) as i32)
            .unwrap_or(4);
        *self.engine.lock().unwrap() = None; // 先放掉旧模型的内存
        let e = Engine::load(&paths, kind, &hot, &lang, threads)?;
        let e = Arc::new(if kind == AsrKind::AppleSpeech { e.with_apple_helper(&self.opt.recorder_bin)? } else { e });
        *self.engine.lock().unwrap() = Some(e.clone());
        Ok(e)
    }

    fn engine_loaded(&self) -> bool {
        self.engine.lock().unwrap().is_some()
    }

    fn folder(&self, id: &str) -> ApiResult<PathBuf> {
        let root = self.root().canonicalize().map_err(|_| not_found())?;
        let path = root.join(id).canonicalize().map_err(|_| not_found())?;
        if path.parent() != Some(root.as_path()) || !path.is_dir() {
            return Err(not_found());
        }
        Ok(path)
    }

    /// 这场会议有没有进行中的任务（模型下载不算，它的 meeting 字段是模型编号）
    fn meeting_busy(&self, id: &str) -> bool {
        self.jobs.lock().unwrap().iter().any(|j| j.busy() && j.kind != "models" && (j.meeting == id || j.merging.iter().any(|m| m == id)))
    }

    /// 会议文件夹，且这场会议没有进行中的任务（否则 409）
    fn idle_folder(&self, id: &str) -> ApiResult<PathBuf> {
        let dir = self.folder(id)?;
        if self.meeting_busy(id) {
            return Err(ApiError(StatusCode::CONFLICT, busy_msg().into()));
        }
        Ok(dir)
    }

    /// 移到系统废纸篓
    fn trash(&self, path: &Path) -> Result<(), String> {
        record::trash(&self.opt.recorder_bin, path)
    }

    /// 把一批文件挨个移到废纸篓，返回移走的字节数；遇到失败就停下报错
    fn trash_files(&self, files: &[PathBuf]) -> Result<u64, String> {
        let mut freed = 0;
        for f in files {
            let n = meetings::size_of(f);
            self.trash(f)?;
            freed += n;
        }
        Ok(freed)
    }

    /// 整场会议移到废纸篓，返回会议文件夹的大小
    fn delete_meeting(&self, id: &str) -> ApiResult<u64> {
        let dir = self.idle_folder(id)?;
        let size = meetings::size_of(&dir);
        self.trash(&dir).map_err(server_error)?;
        Ok(size)
    }

    /// 只把录音（录音.*、system.wav、mic.wav）移到废纸篓，转写、纪要、导出的文件保留。
    /// 没有录音时：`required` 为 true 报 400，否则当成功（释放 0）。
    fn delete_audio(&self, id: &str, required: bool) -> ApiResult<u64> {
        let dir = self.idle_folder(id)?;
        let files = meetings::audio_files(&dir);
        if files.is_empty() {
            if required {
                return Err(bad(tr("这场会议没有录音文件", "This meeting has no recording")));
            }
            return Ok(0);
        }
        self.trash_files(&files).map_err(server_error)
    }

    /// 改 meta.json：只改请求体里给了的字段（title 为 null 或空串 = 恢复默认标题），返回更新后的列表项
    fn update_meta(&self, id: &str, body: &Value) -> ApiResult<Value> {
        // 录音、转写、生成纪要时这场会议的文件夹还在被写，不和它抢 meta.json
        let dir = self.idle_folder(id)?;
        let obj = body.as_object().ok_or_else(|| bad(tr("请求格式不对", "Invalid request")))?;
        let mut meta = meetings::read_meta(&dir);
        if let Some(t) = obj.get("title") {
            meta.title = match t {
                Value::Null => None,
                Value::String(t) => meetings::normalize_title(Some(t)),
                _ => return Err(bad(tr("标题要是文字", "The title must be text"))),
            };
        }
        if let Some(t) = obj.get("tags") {
            let tags: Option<Vec<&str>> = t.as_array().and_then(|a| a.iter().map(Value::as_str).collect());
            let tags = tags.ok_or_else(|| bad(tr("标签要是文字列表", "Tags must be a list of text")))?;
            meta.tags = meetings::normalize_tags(&tags);
        }
        if let Some(a) = obj.get("archived") {
            meta.archived = a.as_bool().ok_or_else(|| bad(tr("archived 要是 true 或 false", "archived must be true or false")))?;
        }
        meetings::write_meta(&dir, &meta).map_err(|e| server_error(e.to_string()))?;
        Ok(serde_json::to_value(meetings::summary(&dir)).unwrap())
    }

    /// 批量操作：逐个处理，某一个失败不影响其他
    fn batch(&self, r: &BatchReq) -> ApiResult<Value> {
        let mut ids: Vec<&str> = Vec::new();
        for id in &r.ids {
            if !ids.contains(&id.as_str()) {
                ids.push(id);
            }
        }
        if ids.is_empty() {
            return Err(bad(tr("没有选中会议", "No meetings selected")));
        }
        let action = r.action.as_str();
        if !matches!(action, "archive" | "unarchive" | "tag_add" | "tag_remove" | "delete" | "delete_audio") {
            return Err(bad(tr(&format!("不认识的操作：{action}"), &format!("Unknown action: {action}"))));
        }
        let tag = meetings::normalize_tag(r.tag.as_deref().unwrap_or(""));
        if action.starts_with("tag_") && tag.is_empty() {
            return Err(bad(tr("要给出标签", "A tag is required")));
        }
        let (mut ok, mut freed, mut failed) = (0u64, 0u64, Vec::new());
        for id in ids {
            let result = match action {
                "delete" => self.delete_meeting(id),
                "delete_audio" => self.delete_audio(id, false),
                _ => self.idle_folder(id).and_then(|dir| {
                    let mut meta = meetings::read_meta(&dir);
                    match action {
                        "archive" => meta.archived = true,
                        "unarchive" => meta.archived = false,
                        "tag_add" => {
                            meta.tags.push(tag.clone());
                            meta.tags = meetings::normalize_tags(&meta.tags);
                        }
                        _ => meta.tags.retain(|t| *t != tag),
                    }
                    meetings::write_meta(&dir, &meta).map_err(|e| server_error(e.to_string()))?;
                    Ok(0)
                }),
            };
            match result {
                Ok(n) => {
                    ok += 1;
                    freed += n;
                }
                Err(ApiError(_, e)) => failed.push(json!({"id": id, "error": e})),
            }
        }
        Ok(json!({"ok": ok, "failed": failed, "freed_bytes": freed}))
    }

    fn storage(&self) -> Value {
        let mut v = meetings::storage(&self.root(), |id| self.meeting_busy(id));
        v["models_bytes"] = meetings::size_of(&self.opt.models_dir).into();
        // iOS 只能存在 App 自己的「文稿」里，也没有选文件夹的录音组件
        v["can_change_directory"] = json!(!cfg!(target_os = "ios") && config::env_var("ROOT").is_none());
        v["changing"] = json!(self.storage_progress.lock().unwrap().active);
        v["free_bytes"] = meetings::free_bytes(&self.root()).into();
        v
    }

    /// 清理：残留文件（leftovers），或者 N 天前的会议的录音（audio）。跳过正在处理的会议。
    fn cleanup(&self, r: &Value) -> ApiResult<Value> {
        let what = r["what"].as_str().unwrap_or("");
        let (mut count, mut freed, mut error) = (0u64, 0u64, None::<String>);
        let mut note = |res: Result<u64, String>, count: &mut u64| match res {
            Ok(n) => {
                *count += 1;
                freed += n;
            }
            Err(e) => {
                eprintln!("清理失败：{e}");
                error.get_or_insert(e);
            }
        };
        let list = meetings::list(&self.root());
        match what {
            "leftovers" => {
                for m in list.iter().filter(|m| !self.meeting_busy(&m.id)) {
                    for f in meetings::leftover_files(&self.root().join(&m.id)) {
                        note(self.trash_files(&[f]), &mut count);
                    }
                }
            }
            "audio" => {
                let days = r
                    .get("older_than_days")
                    .and_then(Value::as_u64)
                    .filter(|d| *d <= 3650)
                    .ok_or_else(|| bad(tr("天数要是 0–3650 的整数", "Days must be a whole number from 0 to 3650")))?;
                let archived_only = match r.get("archived_only") {
                    None | Some(Value::Null) => false,
                    Some(v) => v.as_bool().ok_or_else(|| {
                        bad(tr("archived_only 要是 true 或 false", "archived_only must be true or false"))
                    })?,
                };
                let cutoff = chrono::Local::now().date_naive() - chrono::Days::new(days);
                for m in &list {
                    let dir = self.root().join(&m.id);
                    let old = days == 0 || meetings::meeting_date(&dir) < cutoff;
                    if !old || (archived_only && !m.archived) || self.meeting_busy(&m.id) {
                        continue;
                    }
                    let files = meetings::audio_files(&dir);
                    if !files.is_empty() {
                        note(self.trash_files(&files), &mut count);
                    }
                }
            }
            _ => return Err(bad(tr(&format!("不认识的清理项：{what}"), &format!("Unknown cleanup type: {what}")))),
        }
        // 一个都没移走（比如这个系统不支持废纸篓）时报错，别让人以为清理过了
        if let (0, Some(e)) = (count, error) {
            return Err(server_error(e));
        }
        Ok(json!({"ok": true, "count": count, "freed_bytes": freed}))
    }

    /// 登记一个任务。录音同时只能有一场；同一场会议、模型下载各自同时只能有一个任务；其余可以并行
    /// （转写共用一个识别引擎，在引擎里排队；生成纪要主要等网络）。
    fn start_job(&self, job: Job) -> ApiResult<()> {
        let mut jobs = self.jobs.lock().unwrap();
        // 已有会议的任务在持锁后再次验证；新录音和导入此时尚未创建目录。
        if job.kind == "summarize" || (job.kind == "process" && job.state == "loading") {
            self.folder(&job.meeting)?;
        }
        let conflict = jobs.iter().filter(|j| j.busy()).find_map(|j| {
            if job.kind == "record" && j.state == "recording" {
                Some(tr("已经在录音了，先结束这一场", "Already recording. Stop the current one first"))
            } else if job.kind == "models" && j.kind == "models" {
                Some(tr("已经在下载模型了，等它下完", "A model is already downloading. Wait for it to finish"))
            } else if job.kind == "update" && j.kind == "update" {
                Some(tr("已经在下载新版了，等它下完", "The update is already downloading. Wait for it to finish"))
            } else if !job.meeting.is_empty() && job.kind != "models" && j.kind != "models" && j.meeting == job.meeting {
                Some(busy_msg())
            } else if j.merging.contains(&job.meeting) || job.merging.iter().any(|m| *m == j.meeting || j.merging.contains(m)) {
                Some(tr("这场会议正在合并或处理，等它完成", "This meeting is being merged or processed. Wait for it to finish"))
            } else {
                None
            }
        });
        if let Some(msg) = conflict {
            return Err(ApiError(StatusCode::CONFLICT, msg.into()));
        }
        // 结束了的任务只留最近几个，给界面显示结果
        let finished: Vec<String> = jobs.iter().filter(|j| !j.busy()).map(|j| j.id.clone()).collect();
        if finished.len() >= 6 {
            let drop = &finished[..finished.len() - 5];
            jobs.retain(|j| !drop.contains(&j.id));
        }
        jobs.push(job);
        Ok(())
    }

    fn update(&self, id: &str, f: impl FnOnce(&mut Job)) {
        if let Some(j) = self.jobs.lock().unwrap().iter_mut().find(|j| j.id == id) {
            f(j);
        }
    }

    fn fail(&self, id: &str, e: impl Into<String>) {
        let e = e.into();
        eprintln!("任务失败：{e}");
        self.update(id, |j| {
            j.state = "error".into();
            j.finished_at = now();
            j.error = e;
        });
    }

    /// 生成纪要：流式写进 job.minutes，写完存到会议文件夹。`extra` 是这场会议额外填的热词。
    fn run_summarize(&self, id: &str, tr: &Transcript, dir: &Path, extra: &[String]) -> Result<(), String> {
        let cfg = self.config();
        let mut hotwords = cfg.hotwords.clone();
        hotwords.extend(
            extra
                .iter()
                .filter(|w| !hotwords.contains(w))
                .cloned()
                .collect::<Vec<_>>(),
        );
        self.update(id, |j| j.state = "summarizing".into());
        let mut last_draft: Option<Instant> = None;
        let result = summarize::summarize(
            tr,
            &cfg.llm,
            meetings::meeting_date(dir),
            &hotwords,
            &cfg.minutes_language,
            Hooks {
                on_token: &mut |t| {
                    self.update(id, |j| j.minutes.push_str(t));
                    if last_draft.is_none_or(|at| at.elapsed() >= Duration::from_secs(2)) {
                        let partial = self.jobs.lock().unwrap().iter().find(|j| j.id == id).map(|j| j.minutes.clone()).unwrap_or_default();
                        let _ = write_minutes_draft(dir, &partial);
                        last_draft = Some(Instant::now());
                    }
                },
                on_stage: &mut |s| {
                    self.update(id, |j| {
                        j.stage = s.into();
                        j.minutes.clear();
                        j.thinking = 0;
                    })
                },
                on_think: &mut |n| self.update(id, |j| j.thinking = n as u64),
            },
        );
        let text = match result {
            Ok(text) => text,
            Err(error) => {
                let partial = self.jobs.lock().unwrap().iter().find(|j| j.id == id).map(|j| j.minutes.clone()).unwrap_or_default();
                if !partial.trim().is_empty() {
                    match write_minutes_draft(dir, &partial) {
                        Ok(()) => return Err(format!("{error}{}", i18n::tr(" 已生成的正文已保存为草稿，原纪要未覆盖。", " Generated text was saved as a draft; existing minutes were preserved."))),
                        Err(save_error) => return Err(format!("{error}{}: {save_error}", i18n::tr(" 草稿保存失败", " Could not save the draft"))),
                    }
                }
                return Err(error);
            }
        };
        write_minutes_draft(dir, &text).map_err(|e| e.to_string())?;
        crate::minutes_history::save(dir, &text).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(dir.join("在线纪要草稿.md"));
        self.update(id, |j| j.minutes = text);
        if cfg.obsidian.auto && !cfg.obsidian.vault.trim().is_empty() {
            // 自动存进 Obsidian；失败不影响纪要本身，只提示一下
            // 界面认"没能" / "Couldn't"开头的提示为出错
            let notice = match obsidian::export(&cfg.obsidian, dir) {
                Ok(_) => i18n::tr("纪要已同步到 Obsidian", "Minutes synced to Obsidian").to_string(),
                Err(e) => i18n::tr(&format!("没能同步到 Obsidian：{e}"), &format!("Couldn't sync to Obsidian: {e}")).to_string(),
            };
            // 自动结束录音的提示也留着
            self.update(id, |j| {
                j.notice = if j.notice.is_empty() {
                    notice
                } else {
                    format!("{notice}{}{}", i18n::tr("；", "; "), j.notice)
                }
            });
        }
        Ok(())
    }

    /// 原生外壳由用户明确选择在线或离线生成，录音和转写不隐式发送给在线服务。
    fn run_auto_minutes(&self, id: &str, tr: &Transcript, dir: &Path, extra: &[String]) -> Result<(), String> {
        if self.auto_minutes { self.run_summarize(id, tr, dir, extra) } else { Ok(()) }
    }

    /// 处理录音文件：解码 → 分段转写 → 存转写 → 生成纪要。
    fn run_process(&self, id: &str, audio_path: &Path, dir: &Path, extra: &[String]) -> Result<(), String> {
        let tr = self.run_transcribe(id, audio_path, dir)?;
        self.run_auto_minutes(id, &tr, dir, extra)
    }

    /// 转写录音文件：解码 → 分段转写 → 存转写。
    fn run_transcribe(&self, id: &str, audio_path: &Path, dir: &Path) -> Result<Transcript, String> {
        self.update(id, |j| j.state = "loading".into());
        let engine = self.engine()?;
        self.update(id, |j| j.state = "transcribing".into());
        let samples = record::decode(&self.opt.recorder_bin, audio_path)?;
        let tr = transcribe_samples(&engine, &samples, audio_path, |f| {
            self.update(id, |j| j.stage = format!("{:.0}%", f * 100.0))
        })?;
        self.update(id, |j| j.stage.clear());
        if tr.sentences.is_empty() {
            return Err(i18n::tr("没有识别出任何语音", "No speech was recognized").into());
        }
        meetings::save_transcript(&tr, dir).map_err(|e| e.to_string())?;
        Ok(tr)
    }

    /// 合并会议：建好新会议 → 把原来的几场移到「已删除会议」→ 有的段没转写时整段重新转写。
    /// 返回合并后的转写（没有识别出语音时是错误，但合并本身已经完成）。
    fn run_merge(&self, id: &str, sources: &[PathBuf], dest: &Path, title: &str) -> Result<Transcript, String> {
        let transcribed = self.merge_files(id, sources, dest, title)?;
        self.merged_transcript(id, dest, transcribed)
    }

    /// 合并的前一半：建好新会议，把原来的几场移到「已删除会议」。出错时什么都没动。
    /// 返回转写是不是已经拼好了。
    fn merge_files(&self, id: &str, sources: &[PathBuf], dest: &Path, title: &str) -> Result<bool, String> {
        let merged = crate::merge::merge(&self.opt.recorder_bin, sources, dest, title, |stage| {
            self.update(id, |j| j.stage = stage)
        })?;
        self.update(id, |j| j.stage.clear());
        // 接着录留下的中间文件用完了，别带进「已删除会议」
        for dir in sources {
            let _ = std::fs::remove_file(dir.join(crate::merge::MIXED_WAV));
        }
        // 新会议已经完整，原来的几场可以收走了。收不走的留在原处并说明，不算合并失败
        let stuck: Vec<String> = sources
            .iter()
            .filter(|dir| meetings::move_to_deleted(&self.root(), dir).is_err())
            .map(|dir| meetings::summary(dir).title)
            .collect();
        if !stuck.is_empty() {
            let names = stuck.join(i18n::tr("、", ", "));
            self.update(id, |j| {
                j.notice = i18n::tr(
                    &format!("没能把原来的会议移到「已删除会议」：{names}，可以手动删除"),
                    &format!("Couldn't move the original meetings to deleted meetings: {names}. You can delete them yourself"),
                )
                .to_string()
            });
        }
        Ok(merged.transcribed)
    }

    /// 合并的后一半：拿到合并后的转写；有的段没转写时对合并后的录音整段转写。
    fn merged_transcript(&self, id: &str, dest: &Path, transcribed: bool) -> Result<Transcript, String> {
        if transcribed {
            Transcript::load(&dest.join(TRANSCRIPT_JSON)).map_err(|e| e.to_string())
        } else {
            self.run_transcribe(id, &dest.join(AUDIO), dest)
        }
    }

    /// 「接着录」结束：把刚录完的这一段（`part`，转写是 `part_tr`，还没转写时为 None）接到原来那场后面合成一场，
    /// 再按合成后的内容生成纪要。接不上（原来那场不见了、磁盘满了）时这一段照常单独保存、单独出纪要，并说明原因。
    fn finish_continued(&self, id: &str, orig: &str, part: &Path, part_tr: Option<&Transcript>, extra: &[String]) -> Result<(), String> {
        let joined = self
            .folder(orig)
            .map_err(|_| i18n::tr("原来那场会议找不到了", "the original meeting is gone").to_string())
            .and_then(|orig_dir| {
                let title = meetings::summary(&orig_dir).title;
                let dest = meetings::new_folder(&self.root(), &title, meetings::meeting_start(&orig_dir));
                let transcribed = self.merge_files(id, &[orig_dir, part.to_path_buf()], &dest, &title)?;
                Ok((dest, transcribed))
            });
        match joined {
            Ok((dest, transcribed)) => {
                // 原来那场和刚录的这一段都已经收走，任务从这里起跟着合成后的会议
                let name = dest.file_name().unwrap().to_string_lossy().into_owned();
                self.update(id, |j| {
                    j.meeting = name;
                    j.merging.clear();
                });
                let tr = self.merged_transcript(id, &dest, transcribed)?;
                self.run_auto_minutes(id, &tr, &dest, extra)
            }
            Err(e) => {
                let _ = std::fs::remove_file(part.join(crate::merge::MIXED_WAV));
                self.update(id, |j| {
                    j.merging.clear();
                    j.notice = i18n::tr(
                        &format!("没能接到原来那场后面：{e}。这一段单独保存了，可以在会议列表里手动合并"),
                        &format!("Couldn't append to the original meeting: {e}. This part was saved on its own — you can merge them from the meeting list"),
                    )
                    .to_string()
                });
                match part_tr {
                    Some(tr) => self.run_auto_minutes(id, tr, part, extra),
                    None => self.run_process(id, &part.join(AUDIO), part, extra),
                }
            }
        }
    }
}

fn spawn_job(state: &Shared, id: &str, work: impl FnOnce(&AppState, &str) -> Result<(), String> + Send + 'static) {
    let s = state.clone();
    let id = id.to_string();
    std::thread::spawn(move || match work(&s, &id) {
        Ok(()) => s.update(&id, |j| {
            j.state = "done".into();
            j.finished_at = now();
        }),
        Err(e) => s.fail(&id, e),
    });
}

// ---------- 环境与配置 ----------

async fn env(State(s): State<Shared>) -> Json<Value> {
    let cfg = s.config();
    let s2 = s.clone();
    let llm_ready = !cfg.llm.api_key.is_empty()
        || tokio::task::spawn_blocking(move || summarize::is_ollama(&s2.config().llm))
            .await
            .unwrap_or(false);
    Json(json!({
        "version": s.opt.version,
        "platform": if cfg!(target_os = "ios") { "ios" } else { std::env::consts::OS },
        "root": s.root(),
        "ffmpeg": true, // 桌面版不需要 ffmpeg
        "merge": true,  // 能合并会议、接着录；界面按这个决定显不显示入口（Python 版后端没有）
        "system_audio": system_audio_supported(&s.opt.recorder_bin),
        "model_cached": s.model_paths().base_ready() && s.model_paths().asr_ready(s.model_paths().active()),
        "asr_model": models::catalog().iter().find(|m| m.kind == s.model_paths().active()).map(|m| m.name),
        "models_downloadable": true, // 桌面版：模型在界面里下载（Python 版首次转写时自动下载，没有这个字段）
        "models_size_mb": models::total_size(models::Source::Github) / 1_000_000,
        "models_size_mb_cn": models::total_size(models::Source::Cn) / 1_000_000,
        "model_loaded": s.engine_loaded(),
        "llm_ready": llm_ready,
        "llm_model": cfg.llm.model,
    }))
}

fn system_audio_supported(bin: &Path) -> bool {
    if record::native().is_some() {
        return false;
    }
    // Windows 用 WASAPI 回环录音，系统自带、不需要权限
    if cfg!(windows) {
        return true;
    }
    if !bin.is_file() {
        return false;
    }
    let out = std::process::Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output();
    out.ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|v| {
            v.trim()
                .split('.')
                .next()
                .and_then(|m| m.parse::<u32>().ok())
        })
        .is_some_and(|major| major >= 15)
}

fn config_json(s: &AppState) -> Value {
    let c = s.config();
    let key = &c.llm.api_key;
    let hint = if key.chars().count() > 10 {
        let chars: Vec<char> = key.chars().collect();
        format!(
            "{}…{}",
            chars[..3].iter().collect::<String>(),
            chars[chars.len() - 4..].iter().collect::<String>()
        )
    } else if key.is_empty() {
        String::new()
    } else {
        tr("已设置", "Set").into()
    };
    json!({
        "base_url": c.llm.base_url,
        "model": c.llm.model,
        "api": c.llm.api,
        "temperature": c.llm.temperature,
        "think": c.llm.think,
        "live_minutes": c.live_minutes,
        "api_key_set": !key.is_empty(),
        "api_key_hint": hint,
        "hotwords": c.hotwords,
        "webhook": c.webhook,
        "secret_set": !c.webhook_secret.is_empty(),
        "obsidian": {
            "vault": c.obsidian.vault,
            "folder": c.obsidian.folder,
            "auto": c.obsidian.auto,
            "transcript": c.obsidian.transcript,
        },
        "ui_language": c.ui_language,
        "minutes_language": c.minutes_language,
        "asr_language": c.asr_language,
        "auto_stop_silence_min": c.auto_stop_silence_min,
        "auto_stop_app_quit": c.auto_stop_app_quit,
        "path": s.opt.config_path,
    })
}

async fn get_config(State(s): State<Shared>) -> Json<Value> {
    Json(config_json(&s))
}

#[derive(Deserialize)]
struct ConfigReq {
    base_url: String,
    model: String,
    api_key: Option<String>,
    #[serde(default)]
    hotwords: Vec<String>,
    #[serde(default)]
    webhook: String,
    secret: Option<String>,
    obsidian: Option<ObsidianReq>,
    /// 三个语言选项（auto | zh | en），不传就不改
    ui_language: Option<String>,
    minutes_language: Option<String>,
    asr_language: Option<String>,
    /// 接口协议 openai | anthropic，不传就不改
    api: Option<String>,
    /// 采样温度：0～2 的数，空字符串 = 恢复默认，不传就不改
    temperature: Option<Value>,
    /// 写纪要时深度思考，不传就不改
    think: Option<bool>,
    /// 开会时实时整理纪要，不传就不改
    live_minutes: Option<bool>,
    /// 自动结束录音：没人说话多少分钟（0–240 的整数，0 = 不自动结束）、会议软件退出时是否结束；不传就不改
    auto_stop_silence_min: Option<Value>,
    auto_stop_app_quit: Option<bool>,
}

/// 设置页传来的语言选项；不认识的值报错
fn language_field(v: Option<String>, slot: &mut String) -> ApiResult<()> {
    if let Some(v) = v {
        let v = v.trim().to_ascii_lowercase();
        if !config::LANGUAGES.contains(&v.as_str()) {
            return Err(bad(tr(
                &format!("不支持的语言：{v}（可选 auto、zh、en）"),
                &format!("Unsupported language: {v} (use auto, zh or en)"),
            )));
        }
        *slot = v;
    }
    Ok(())
}

#[derive(Deserialize)]
struct ObsidianReq {
    vault: String,
    folder: String,
    auto: bool,
    transcript: bool,
}

async fn put_config(State(s): State<Shared>, Json(r): Json<ConfigReq>) -> ApiResult<Json<Value>> {
    let mut c = s.config();
    c.llm.base_url = r.base_url.trim().into();
    c.llm.model = r.model.trim().into();
    if let Some(a) = &r.api {
        if !matches!(a.trim().to_ascii_lowercase().as_str(), "openai" | "anthropic") {
            return Err(bad(tr(&format!("不认识的接口协议：{a}"), &format!("Unknown API protocol: {a}"))));
        }
        c.llm.api = config::normalize_api(Some(a));
    }
    if let Some(k) = r.api_key {
        c.llm.api_key = k.trim().into();
    }
    if let Some(t) = &r.temperature {
        c.llm.temperature = match t {
            Value::Null => None,
            Value::String(v) if v.trim().is_empty() => None,
            Value::Number(n) => n.as_f64(),
            Value::String(v) => Some(v.trim().parse().unwrap_or(f64::NAN)),
            _ => Some(f64::NAN),
        };
        if c.llm.temperature.is_some_and(|t| !config::valid_temperature(t)) {
            return Err(bad(tr("温度要填 0～2 之间的数，留空用默认的 0.2", "Temperature must be a number from 0 to 2; leave it empty for the default 0.2")));
        }
    }
    if let Some(t) = r.think {
        c.llm.think = t;
    }
    if let Some(l) = r.live_minutes {
        c.live_minutes = l;
    }
    c.hotwords = r
        .hotwords
        .iter()
        .map(|w| w.trim().to_string())
        .filter(|w| !w.is_empty())
        .collect();
    c.webhook = r.webhook.trim().into();
    if let Some(sec) = r.secret {
        c.webhook_secret = sec.trim().into();
    }
    if let Some(o) = r.obsidian {
        c.obsidian = config::ObsidianConfig {
            // 设置页里可以填 ~/…
            vault: crate::platform::expand_home(o.vault.trim())
                .map_or_else(|| o.vault.trim().into(), |p| p.to_string_lossy().into_owned()),
            folder: o.folder.trim().trim_matches('/').into(),
            auto: o.auto,
            transcript: o.transcript,
        };
    }
    language_field(r.ui_language, &mut c.ui_language)?;
    language_field(r.minutes_language, &mut c.minutes_language)?;
    language_field(r.asr_language, &mut c.asr_language)?;
    if let Some(v) = &r.auto_stop_silence_min {
        c.auto_stop_silence_min = v
            .as_u64()
            .filter(|m| *m <= config::MAX_SILENCE_MIN as u64)
            .ok_or_else(|| {
                bad(tr(
                    "没人说话自动结束录音的分钟数要是 0–240 的整数",
                    "Auto-stop minutes must be a whole number from 0 to 240",
                ))
            })? as u32;
    }
    if let Some(v) = r.auto_stop_app_quit {
        c.auto_stop_app_quit = v;
    }
    config::save(&s.opt.config_path, &c).map_err(|e| bad(e.to_string()))?;
    Ok(Json(config_json(&s)))
}

#[derive(Deserialize)]
struct LlmReq {
    base_url: String,
    #[serde(default)]
    model: String,
    api_key: Option<String>,
    /// openai | anthropic；不传时，地址和已保存的一样就用已保存的协议，否则当 openai
    api: Option<String>,
}

impl LlmReq {
    fn api(&self, s: &AppState) -> String {
        match &self.api {
            Some(a) => config::normalize_api(Some(a)),
            None => {
                let c = s.config().llm;
                if c.base_url.trim_end_matches('/') == self.base_url.trim_end_matches('/') { c.api } else { "openai".into() }
            }
        }
    }
}

fn list_models(base_url: &str, key: &str, api: &str) -> Option<Vec<String>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(if api == "anthropic" { 10 } else { 3 }))
        .build()
        .ok()?;
    let req = if api == "anthropic" {
        client
            .get(format!("{}/models?limit=1000", summarize::anthropic_v1(base_url))) // 默认一页只有 20 个
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
    } else {
        client
            .get(format!("{}/models", base_url.trim_end_matches('/')))
            .bearer_auth(if key.is_empty() { "none" } else { key })
    };
    let r = req.send().ok()?;
    if !r.status().is_success() {
        return None;
    }
    let v: Value = r.json().ok()?;
    let mut ids: Vec<String> = v["data"]
        .as_array()?
        .iter()
        .filter_map(|m| m["id"].as_str().map(String::from))
        .collect();
    ids.sort();
    Some(ids)
}

async fn test_llm(State(s): State<Shared>, Json(r): Json<LlmReq>) -> Json<Value> {
    let api = r.api(&s);
    let key = r.api_key.unwrap_or_else(|| s.config().llm.api_key);
    let (url, model) = (r.base_url, r.model);
    let ids = tokio::task::spawn_blocking(move || list_models(&url, &key, &api))
        .await
        .ok()
        .flatten();
    Json(match ids {
        None => json!({"ok": false, "message": tr("连不上：接口地址或 API Key 不对", "Couldn't connect: check the API URL or API key")}),
        // Ollama 列出的模型名带 :latest
        Some(ids)
            if !ids.is_empty()
                && !ids.contains(&model)
                && !ids.contains(&format!("{model}:latest")) =>
        {
            let some: Vec<&str> = ids.iter().take(8).map(String::as_str).collect();
            let message = tr(
                &format!("连上了，但没有模型 {model}。可用：{}", some.join("、")),
                &format!("Connected, but there is no model {model}. Available: {}", some.join(", ")),
            )
            .to_string();
            json!({"ok": false, "message": message})
        }
        Some(_) => json!({"ok": true, "message": tr("连接正常", "Connection OK")}),
    })
}

async fn llm_models(State(s): State<Shared>, Json(r): Json<LlmReq>) -> Json<Value> {
    let api = r.api(&s);
    let key = r.api_key.unwrap_or_else(|| s.config().llm.api_key);
    let url = r.base_url;
    let ids = tokio::task::spawn_blocking(move || list_models(&url, &key, &api))
        .await
        .ok()
        .flatten();
    Json(json!({"ok": ids.is_some(), "models": ids.unwrap_or_default()}))
}

/// 本机常见的 OpenAI 兼容中转。recommended 是实测过、又快又好的纪要模型。
fn local_providers() -> Vec<Value> {
    vec![
        json!({"id": "magpie", "name": "Magpie", "base_url": "http://127.0.0.1:3425/v1", "api_key": "magpie",
               "recommended": "deepseek/deepseek-v4-flash",
               "hint": tr("Magpie 没有在运行：打开 Magpie 应用后再试", "Magpie isn't running: open the Magpie app and try again")}),
        json!({"id": "ccswitch", "name": "CC Switch", "base_url": "http://127.0.0.1:15721/v1", "api_key": "cc-switch",
               "recommended": "",
               "hint": tr(
                   "CC Switch 的本地代理没有打开：在 CC Switch 设置里开启「本地代理」（默认端口 15721）",
                   "CC Switch's local proxy is off: turn on \"Local Proxy\" in CC Switch settings (default port 15721)",
               )}),
        json!({"id": "ollama", "name": tr("本地 Ollama", "Local Ollama"), "base_url": "http://localhost:11434/v1", "api_key": "",
               "recommended": "",
               "hint": tr("Ollama 没有在运行，或者还没有下载模型", "Ollama isn't running, or no model has been pulled yet")}),
    ]
}

async fn llm_providers() -> Json<Value> {
    let found = tokio::task::spawn_blocking(|| {
        let handles: Vec<_> = local_providers()
            .into_iter()
            .map(|mut p| {
                std::thread::spawn(move || {
                    let models = list_models(
                        p["base_url"].as_str().unwrap(),
                        p["api_key"].as_str().unwrap(),
                        "openai",
                    );
                    p["running"] = models.is_some().into();
                    p["models"] = json!(models.unwrap_or_default());
                    p
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|h| h.join().ok())
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    Json(Value::Array(found))
}

// ---------- 会议 ----------

async fn list_meetings(State(s): State<Shared>) -> Json<Value> {
    Json(json!(meetings::list(&s.root())))
}


fn meeting_json(dir: &Path) -> Value {
    let info = meetings::summary(dir);
    let mut data = serde_json::to_value(&info).unwrap();
    data["minutes"] = std::fs::read_to_string(dir.join(MINUTES))
        .unwrap_or_default()
        .into();
    data["audio"] = meetings::find_audio(dir).is_some().into();
    data["path"] = dir.to_string_lossy().into();
    data["marks"] = json!(meetings::read_marks(dir));
    data["turns"] = json!([]);
    data["speaker_list"] = json!([]);
    if let Ok(tr) = Transcript::load(&dir.join(TRANSCRIPT_JSON)) {
        let order = tr.speakers_in_order();
        data["speaker_list"] = order
            .iter()
            .enumerate()
            .map(|(i, &s)| json!({"index": i, "name": tr.name_of(s), "custom": tr.speaker_names.contains_key(&s)}))
            .collect();
        let index_of: BTreeMap<String, usize> = order
            .iter()
            .enumerate()
            .map(|(i, &s)| (tr.name_of(s), i))
            .collect();
        data["turns"] = tr
            .turns()
            .into_iter()
            .map(|t| {
                json!({"speaker": t.speaker, "color": index_of.get(&t.speaker).copied().unwrap_or(0),
                       "start_ms": t.start_ms, "ts": fmt_ts(t.start_ms), "text": t.text})
            })
            .collect();
    }
    data
}

async fn get_meeting(
    State(s): State<Shared>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(meeting_json(&s.folder(&id)?)))
}

/// 整场会议移到废纸篓
async fn delete_meeting(State(s): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult<Json<Value>> {
    tokio::task::spawn_blocking(move || s.delete_meeting(&id))
        .await
        .map_err(|e| server_error(e.to_string()))??;
    Ok(Json(json!({"ok": true})))
}

/// 只把录音移到废纸篓
async fn delete_audio(State(s): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult<Json<Value>> {
    let freed = tokio::task::spawn_blocking(move || s.delete_audio(&id, true))
        .await
        .map_err(|e| server_error(e.to_string()))??;
    Ok(Json(json!({"ok": true, "freed_bytes": freed})))
}

async fn put_meta(
    State(s): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    s.update_meta(&id, &body).map(Json)
}

#[derive(Deserialize)]
struct BatchReq {
    #[serde(default)]
    ids: Vec<String>,
    #[serde(default)]
    action: String,
    tag: Option<String>,
}

async fn post_batch(State(s): State<Shared>, Json(r): Json<BatchReq>) -> ApiResult<Json<Value>> {
    tokio::task::spawn_blocking(move || s.batch(&r))
        .await
        .map_err(|e| server_error(e.to_string()))?
        .map(Json)
}

// Serialize library switches against requests, including requests that start background jobs.
// The exclusive guard moves into the copy worker so disconnecting cannot unlock it early.
async fn storage_gate(State(s): State<Shared>, req: Request, next: axum::middleware::Next) -> Response {
    let path = req.uri().path();
    if matches!(path, "/api/storage/directory" | "/api/storage/progress") {
        return next.run(req).await;
    }
    let _guard = s.storage_gate.read().await;
    // 先看目录在不在（一次 stat），不在时才读配置：录音时界面每秒要请求好几次，不必每次都读、解析 config.toml
    if !s.root().is_dir() && !s.config().storage_root.is_empty()
        && !matches!(path, "/api/storage" | "/api/storage/choose" | "/api/env" | "/api/config" | "/api/jobs" | "/api/job") {
        return bad(tr("存储目录不可用，请连接磁盘或在设置中选择其他目录", "Storage folder unavailable. Connect the disk or choose another folder in Settings")).into_response();
    }
    next.run(req).await
}

#[derive(Deserialize)]
struct StorageDirectoryReq {
    path: String,
    copy_existing: bool,
}

async fn choose_storage(State(s): State<Shared>) -> ApiResult<Json<Value>> {
    #[cfg(windows)]
    let value = tokio::task::spawn_blocking(move || {
        crate::win::choose_directory(&s.root()).map(|path| json!({ "path": path })).map_err(bad)
    }).await.map_err(|e| server_error(e.to_string()))??;
    #[cfg(not(windows))]
    let value = tokio::task::spawn_blocking(move || {
        let output = std::process::Command::new(&s.opt.recorder_bin)
            .arg("choose-directory").arg(s.root()).output().map_err(|e| bad(e.to_string()))?;
        if !output.status.success() { return Err(bad(String::from_utf8_lossy(&output.stderr).to_string())); }
        serde_json::from_slice::<Value>(&output.stdout).map_err(|e| bad(e.to_string()))
    }).await.map_err(|e| server_error(e.to_string()))??;
    Ok(Json(value))
}

async fn storage_progress(State(s): State<Shared>) -> Json<crate::storage::Progress> {
    Json(s.storage_progress.lock().unwrap().clone())
}

async fn change_storage(State(s): State<Shared>, Json(r): Json<StorageDirectoryReq>) -> ApiResult<Json<Value>> {
    let guard = s.storage_gate.clone().write_owned().await;
    if config::env_var("ROOT").is_some() {
        return Err(bad(tr("存储目录由启动环境变量指定，无法在界面修改", "The storage folder is set by an environment variable")));
    }
    if s.jobs.lock().unwrap().iter().any(Job::busy) {
        return Err(ApiError(StatusCode::CONFLICT, tr("有任务正在进行，请完成录音、转写或纪要生成后再切换目录", "Wait for recording, transcription and other tasks to finish before switching folders").into()));
    }
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        let source = s.root();
        let target = crate::storage::destination(&source, &r.path).map_err(bad)?;
        if source.canonicalize().ok().as_ref() == Some(&target) { return Ok(Json(json!({"root": target}))); }
        *s.storage_progress.lock().unwrap() = crate::storage::Progress { active: true, ..Default::default() };
        let result = (|| {
            if r.copy_existing {
                crate::storage::copy_library(&source, &target, |copied, total| {
                    let mut p = s.storage_progress.lock().unwrap();
                    p.copied_bytes = copied;
                    p.total_bytes = total;
                }).map_err(bad)?;
            }
            let mut cfg = s.config();
            cfg.storage_root = target.to_str().ok_or_else(|| bad("Invalid folder name"))?.to_string();
            config::save(&s.opt.config_path, &cfg).map_err(|e| bad(e.to_string()))?;
            *s.root.lock().unwrap() = target.clone();
            // Finished jobs belong to the previous library and may reuse IDs in the new one.
            s.jobs.lock().unwrap().clear();
            Ok(Json(json!({"root": target})))
        })();
        s.storage_progress.lock().unwrap().active = false;
        result
    }).await.map_err(|e| server_error(e.to_string()))?
}

async fn get_storage(State(s): State<Shared>) -> ApiResult<Json<Value>> {
    tokio::task::spawn_blocking(move || s.storage())
        .await
        .map(Json)
        .map_err(|e| server_error(e.to_string()))
}

async fn post_cleanup(State(s): State<Shared>, Json(r): Json<Value>) -> ApiResult<Json<Value>> {
    tokio::task::spawn_blocking(move || s.cleanup(&r))
        .await
        .map_err(|e| server_error(e.to_string()))?
        .map(Json)
}

#[derive(Deserialize)]
struct MergeReq {
    ids: Vec<String>,
    /// 合并后的标题；不填用最早那场的
    #[serde(default)]
    title: String,
}

/// 把几场会议合成一场：一场会被打断后录成了几段时用。录音按先后接起来，转写拼接（有的段还没转写就整段重转），
/// 原来的几场移到「已删除会议」。合成的是一场新会议，纪要要重新生成，这里不替用户调大模型。
async fn merge_meetings(State(s): State<Shared>, Json(r): Json<MergeReq>) -> ApiResult<Json<Job>> {
    let mut ids = r.ids.clone();
    ids.sort();
    ids.dedup();
    if ids.len() < 2 {
        return Err(bad(tr("至少选两场会议才能合并", "Pick at least two meetings to merge")));
    }
    let mut sources = Vec::with_capacity(ids.len());
    for id in &ids {
        let dir = s.folder(id)?;
        if meetings::find_audio(&dir).is_none() {
            let name = meetings::summary(&dir).title;
            return Err(bad(tr(
                &format!("「{name}」没有录音，不能合并"),
                &format!("“{name}” has no recording, so it can't be merged"),
            )));
        }
        sources.push(dir);
    }
    // 按开始时间排先后；同一分钟开始的按文件夹名
    sources.sort_by_key(|d| (meetings::meeting_start(d), d.file_name().map(|n| n.to_os_string())));
    let title = match r.title.trim() {
        "" => meetings::summary(&sources[0]).title,
        t => t.to_string(),
    };
    let dest = meetings::new_folder(&s.root(), &title, meetings::meeting_start(&sources[0]));
    let name = dest.file_name().unwrap().to_string_lossy().into_owned();
    let mut job = Job::new("merge", &name, "merging");
    job.merging = sources
        .iter()
        .map(|d| d.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    s.start_job(job.clone())?;
    spawn_job(&s, &job.id, move |st, id| st.run_merge(id, &sources, &dest, &title).map(|_| ()));
    Ok(Json(job))
}

/// 播放用的标准类型。按扩展名猜出来的 audio/m4a 不是标准写法，Chrome 照样能放，
/// 但桌面版用的 WKWebView（Safari 内核）会当成不认识的格式，播放器一直是 --:--。
fn audio_mime(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "m4a" | "mp4" | "aac" => "audio/mp4",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "ogg" | "opus" => "audio/ogg",
        "mov" => "video/quicktime",
        _ => return None,
    })
}

async fn get_audio(
    State(s): State<Shared>,
    UrlPath(id): UrlPath<String>,
    req: Request,
) -> ApiResult<Response> {
    let audio = meetings::find_audio(&s.folder(&id)?)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, tr("没有录音文件", "No recording file").into()))?;
    let mime = audio_mime(&audio);
    // ServeFile 支持 Range 请求：播放器拖动进度条时只取需要的那段
    let mut resp = ServeFile::new(audio)
        .oneshot(req)
        .await
        .map(IntoResponse::into_response)
        .unwrap_or_else(|e| match e {});
    if let Some(m) = mime {
        resp.headers_mut().insert(axum::http::header::CONTENT_TYPE, HeaderValue::from_static(m));
    }
    Ok(resp)
}

#[derive(Deserialize)]
struct SpeakersReq {
    names: Vec<String>,
}

async fn put_speakers(
    State(s): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Json(r): Json<SpeakersReq>,
) -> ApiResult<Json<Value>> {
    let dir = s.folder(&id)?;
    let mut tr = Transcript::load(&dir.join(TRANSCRIPT_JSON)).map_err(|_| bad(tr("还没有转写", "No transcript yet")))?;
    tr.speaker_names = tr
        .speakers_in_order()
        .into_iter()
        .zip(r.names)
        .filter(|(_, n)| !n.trim().is_empty())
        .map(|(spk, n)| (spk, n.trim().to_string()))
        .collect();
    meetings::save_transcript(&tr, &dir).map_err(|e| bad(e.to_string()))?;
    Ok(Json(meeting_json(&dir)))
}

#[derive(Deserialize)]
struct MinutesReq {
    text: String,
}

async fn put_minutes(
    State(s): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Json(r): Json<MinutesReq>,
) -> ApiResult<Json<Value>> {
    let dir = s.folder(&id)?;
    crate::minutes_history::save(&dir, &format!("{}\n", r.text.trim_end()))
        .map_err(|e| bad(e.to_string()))?;
    Ok(Json(json!({"ok": true})))
}

async fn get_minutes_history(State(s): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult<Json<Value>> {
    let versions = crate::minutes_history::list(&s.folder(&id)?).map_err(|e| bad(e.to_string()))?;
    Ok(Json(json!({"versions": versions})))
}
async fn delete_minutes_history(State(s): State<Shared>, UrlPath((id, version)): UrlPath<(String, String)>) -> ApiResult<Json<Value>> {
    crate::minutes_history::delete(&s.folder(&id)?, &version).map_err(|e| bad(e.to_string()))?;
    Ok(Json(json!({"ok": true})))
}

async fn post_summarize(
    State(s): State<Shared>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<Job>> {
    let dir = s.folder(&id)?;
    let tr = Transcript::load(&dir.join(TRANSCRIPT_JSON))
        .map_err(|_| bad(tr("还没有转写，不能生成纪要", "No transcript yet, so minutes can't be generated")))?;
    let job = Job::new("summarize", &id, "summarizing");
    s.start_job(job.clone())?;
    spawn_job(&s, &job.id, move |st, id| st.run_summarize(id, &tr, &dir, &[]));
    Ok(Json(job))
}

async fn post_process(
    State(s): State<Shared>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<Job>> {
    let dir = s.folder(&id)?;
    let audio = meetings::find_audio(&dir).ok_or_else(|| bad(tr("没有录音文件", "No recording file")))?;
    let job = Job::new("process", &id, "loading");
    s.start_job(job.clone())?;
    spawn_job(&s, &job.id, move |st, id| st.run_process(id, &audio, &dir, &[]));
    Ok(Json(job))
}

#[derive(Deserialize, Default)]
struct VersionQuery { version: Option<String> }
impl VersionQuery {
    fn minutes(&self, dir: &Path) -> ApiResult<String> {
        match &self.version {
            Some(id) => crate::minutes_history::read(dir, id).map(|v| v.text)
                .map_err(|e| ApiError(StatusCode::NOT_FOUND, e.to_string())),
            None => std::fs::read_to_string(dir.join(MINUTES)).map_err(|_| bad(no_minutes())),
        }
    }
    fn suffix(&self) -> String { self.version.as_ref().map(|v| format!("-history-{v}")).unwrap_or_default() }
}

async fn post_push(
    State(s): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Query(version): Query<VersionQuery>,
) -> ApiResult<Json<Value>> {
    let dir = s.folder(&id)?;
    let cfg = s.config();
    if cfg.webhook.is_empty() {
        return Err(bad(tr(
            "还没有配置群机器人 webhook，去设置里填一下",
            "No group bot webhook configured yet. Add one in Settings",
        )));
    }
    let minutes = version.minutes(&dir)?;
    let platform = tokio::task::spawn_blocking(move || {
        push::send(&cfg.webhook, &minutes, &cfg.webhook_secret)
    })
    .await
    .map_err(|e| bad(e.to_string()))?
    .map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e))?;
    let name = platform.name();
    let message = tr(&format!("已推送到{name}群"), &format!("Pushed to the {name} group")).to_string();
    Ok(Json(json!({"ok": true, "message": message})))
}

async fn obsidian_vaults() -> Json<Value> {
    let v: Vec<Value> = obsidian::vaults().into_iter().map(|(name, path)| json!({"name": name, "path": path})).collect();
    Json(json!(v))
}

#[derive(Deserialize, Default)]
struct OpenReq {
    #[serde(default)]
    open: bool,
}

/// 存进 Obsidian；open=true 时顺便用 Obsidian 打开这篇笔记
async fn post_obsidian(
    State(s): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Query(version): Query<VersionQuery>,
    body: Option<Json<OpenReq>>,
) -> ApiResult<Json<Value>> {
    let dir = s.folder(&id)?;
    version.minutes(&dir)?;
    let cfg = s.config().obsidian;
    let out = tokio::task::spawn_blocking(move || obsidian::export_version(&cfg, &dir, version.version.as_deref()))
        .await
        .map_err(|e| bad(e.to_string()))?
        .map_err(bad)?;
    let uri = obsidian::uri(&out.note);
    if body.is_some_and(|b| b.open) {
        crate::platform::open(&uri);
    }
    Ok(Json(json!({"ok": true, "path": out.note, "uri": uri, "drawings": out.drawings.len()})))
}

/// 把纪要里的鱼骨图导出成 .excalidraw 文件（放在会议文件夹里），并在访达里选中，
/// 可以拖进 excalidraw.com 或用 Obsidian 的 Excalidraw 插件继续改。
async fn post_excalidraw(State(s): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult<Json<Value>> {
    let dir = s.folder(&id)?;
    let minutes = std::fs::read_to_string(dir.join(MINUTES)).map_err(|_| bad(no_minutes()))?;
    // 画板上的文字、文件名跟纪要的语言走
    let lang = i18n::doc_lang(&minutes);
    let transcript = Transcript::load(&dir.join(TRANSCRIPT_JSON)).ok().map(|t| t.localized(lang));
    let turns = transcript.as_ref().map(|t| t.turns()).unwrap_or_default();
    let duration = transcript.as_ref().map(|t| t.duration_ms).unwrap_or(u64::MAX);
    let specs: Vec<_> = fishbone::blocks(&minutes).into_iter().filter_map(|(_, s)| s).collect();
    if specs.is_empty() {
        return Err(bad(tr("这份纪要里没有鱼骨图", "These minutes have no fishbone diagram")));
    }
    let mut paths = Vec::new();
    for (k, spec) in specs.iter().enumerate() {
        let grounds: Vec<_> = spec.branches.iter().map(|b| fishbone::ground(b, &turns, duration)).collect();
        let p = dir.join(excalidraw_name(k, lang));
        let text = serde_json::to_string_pretty(&fishbone::excalidraw(spec, &grounds, lang)).unwrap();
        std::fs::write(&p, text).map_err(|e| bad(e.to_string()))?;
        paths.push(p);
    }
    crate::platform::reveal(&paths[0]);
    Ok(Json(json!({"ok": true, "paths": paths})))
}

/// 第 k 张鱼骨图的文件名：鱼骨图.excalidraw、鱼骨图2.excalidraw / fishbone.excalidraw、fishbone2.excalidraw
fn excalidraw_name(k: usize, lang: Lang) -> String {
    let base = lang.pick("鱼骨图", "fishbone");
    if k == 0 {
        format!("{base}.excalidraw")
    } else {
        format!("{base}{}.excalidraw", k + 1)
    }
}

fn no_minutes() -> &'static str {
    tr("还没有纪要", "No minutes yet")
}

#[derive(Deserialize)]
struct PdfReq {
    html: String,
}

/// 导出 PDF：界面排好版的 HTML 交给录音组件，用系统 WebKit 转成 A4 PDF，存进会议文件夹并在访达里选中
async fn post_pdf(
    State(s): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Query(version): Query<VersionQuery>,
    Json(r): Json<PdfReq>,
) -> ApiResult<Json<Value>> {
    let dir = s.folder(&id)?;
    let minutes = version.minutes(&dir)?;
    let title = meetings::minutes_title(&minutes).unwrap_or_else(|| id.splitn(3, '_').nth(2).unwrap_or(&id).to_string());
    let out = dir.join(format!("{}{}.pdf", obsidian::safe_name(&title), version.suffix()));
    let bin = s.opt.recorder_bin.clone();
    let target = out.clone();
    tokio::task::spawn_blocking(move || record::html_to_pdf(&bin, &r.html, &target))
        .await
        .map_err(|e| bad(e.to_string()))?
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    crate::platform::reveal(&out);
    Ok(Json(json!({"ok": true, "path": out})))
}

/// 导出文件的文件名（不含扩展名）：纪要标题，没有纪要时用文件夹里的会议名
fn export_stem(dir: &Path, id: &str) -> String {
    let minutes = std::fs::read_to_string(dir.join(MINUTES)).unwrap_or_default();
    let topic = id.splitn(3, '_').nth(2).unwrap_or(id);
    let name = obsidian::safe_name(&meetings::minutes_title(&minutes).unwrap_or_else(|| topic.to_string()));
    if name.is_empty() {
        obsidian::safe_name(topic)
    } else {
        name
    }
}

/// 导出 SRT 字幕（按句，说话人名跟界面语言），存进会议文件夹并在访达里选中
async fn post_srt(State(s): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult<Json<Value>> {
    let dir = s.folder(&id)?;
    let t = Transcript::load(&dir.join(TRANSCRIPT_JSON)).map_err(|_| bad(tr("还没有转写", "No transcript yet")))?;
    let out = dir.join(format!("{}.srt", export_stem(&dir, &id)));
    std::fs::write(&out, t.to_srt(i18n::ui())).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    crate::platform::reveal(&out);
    Ok(Json(json!({"ok": true, "path": out})))
}

/// 导出 Word：界面排好版的 HTML 转成 .docx（macOS 用自带的 textutil，见 `record::html_to_docx`）
async fn post_docx(
    State(s): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Query(version): Query<VersionQuery>,
    Json(r): Json<PdfReq>,
) -> ApiResult<Json<Value>> {
    let dir = s.folder(&id)?;
    let stem = if version.version.is_some() {
        let minutes = version.minutes(&dir)?;
        obsidian::safe_name(&meetings::minutes_title(&minutes).unwrap_or_else(|| id.clone()))
    } else { export_stem(&dir, &id) };
    let out = dir.join(format!("{}{}.docx", stem, version.suffix()));
    let target = out.clone();
    tokio::task::spawn_blocking(move || record::html_to_docx(&r.html, &target))
        .await
        .map_err(|e| bad(e.to_string()))?
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    crate::platform::reveal(&out);
    Ok(Json(json!({"ok": true, "path": out})))
}

/// 让大模型根据自我介绍、点名等明确线索猜说话人的名字
async fn guess_speakers(State(s): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult<Json<Value>> {
    let dir = s.folder(&id)?;
    let t = Transcript::load(&dir.join(TRANSCRIPT_JSON)).map_err(|_| bad(tr("还没有转写", "No transcript yet")))?;
    let cfg = s.config().llm;
    let lang = i18n::ui();
    let list = tokio::task::spawn_blocking(move || {
        if cfg.api_key.is_empty() && !summarize::is_ollama(&cfg) {
            return Err(bad(tr(
                "没有配置大模型的 API Key：在设置里填写",
                "No API key configured for the language model: add one in Settings",
            )));
        }
        let (system, prompt) = speakers::prompt(&t, lang);
        let text = summarize::chat(&cfg, system, &prompt, false, |_| {}, |_| {})
            .map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e))?;
        speakers::parse(&text, t.speakers_in_order().len()).ok_or_else(|| {
            ApiError(
                StatusCode::BAD_GATEWAY,
                tr("大模型没有给出可用的结果", "The model didn't return a usable result").into(),
            )
        })
    })
    .await
    .map_err(|e| bad(e.to_string()))??;
    Ok(Json(json!({"suggestions": list})))
}

#[derive(Deserialize)]
struct SearchQuery {
    #[serde(default)]
    q: String,
}

/// 跨会议搜索：查转写句子和纪要正文，用各场纪要的术语表扩展查询词，允许差一个字
async fn search(State(s): State<Shared>, Query(p): Query<SearchQuery>) -> ApiResult<Json<Value>> {
    if p.q.trim().is_empty() {
        return Err(bad(tr("搜索词不能为空", "Enter something to search for")));
    }
    let root = s.root();
    let lang = i18n::ui();
    let v = tokio::task::spawn_blocking(move || terms::search(&root, &p.q, lang))
        .await
        .map_err(|e| bad(e.to_string()))?;
    Ok(Json(v))
}

/// 示例会议的文件夹名：示例素材是 2026-10-28 14:00 的会
fn demo_folder(en: bool) -> String {
    let title = if en { "Sample · Signup conversion review" } else { "示例·注册转化复盘" };
    format!("2026-10-28_1400_{}", meetings::folder_title(title))
}

/// 把内置的示例会议复制进会议目录（已经有了就直接返回）
async fn post_demo(State(s): State<Shared>) -> ApiResult<Json<Value>> {
    let en = i18n::is_en();
    let name = demo_folder(en);
    let dir = s.root().join(&name);
    if dir.exists() {
        return Ok(Json(json!({"id": name})));
    }
    let src = &s.opt.sample_dir;
    let minutes = src.join(if en { "纪要.en.md" } else { MINUTES });
    if !src.join(TRANSCRIPT_JSON).is_file() || !minutes.is_file() {
        return Err(ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            tr("找不到示例会议的素材", "The sample meeting files are missing").into(),
        ));
    }
    let copy = || -> std::io::Result<()> {
        std::fs::create_dir_all(&dir)?;
        std::fs::copy(src.join(TRANSCRIPT_JSON), dir.join(TRANSCRIPT_JSON))?;
        std::fs::copy(&minutes, dir.join(MINUTES))?;
        if src.join(AUDIO).is_file() {
            std::fs::copy(src.join(AUDIO), dir.join(AUDIO))?;
        }
        Ok(())
    };
    if let Err(e) = copy() {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
    }
    Ok(Json(json!({"id": name})))
}

async fn reveal(State(s): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult<Json<Value>> {
    let dir = s.folder(&id)?;
    crate::platform::open(dir);
    Ok(Json(json!({"ok": true})))
}

// ---------- 录音与导入 ----------

#[derive(Deserialize)]
struct RecordReq {
    #[serde(default = "default_title")]
    title: String,
    #[serde(default = "yes")]
    system: bool,
    #[serde(default = "yes")]
    mic: bool,
    #[serde(default)]
    hotwords: Vec<String>,
    /// 「接着录」：这一段录完以后接到这场会议后面（会议被打断、App 被关掉之后用）
    #[serde(default)]
    continue_of: Option<String>,
}

fn default_title() -> String {
    "会议".into()
}

fn yes() -> bool {
    true
}

async fn record_start(State(s): State<Shared>, Json(r): Json<RecordReq>) -> ApiResult<Json<Job>> {
    if !r.system && !r.mic {
        return Err(bad(tr(
            "系统声音和麦克风至少开一个",
            "Turn on at least one of system audio and microphone",
        )));
    }
    let engine = s.engine().map_err(bad)?; // 边录边转需要模型；没下载就先提示下载
    let system = r.system && system_audio_supported(&s.opt.recorder_bin);
    // 接着录：先确认原来那场还在、有录音可接；用文件夹的本名，后面合并时按它找
    let continue_of = match r.continue_of.as_deref().map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => {
            let orig = s.folder(id)?;
            if meetings::find_audio(&orig).is_none() {
                return Err(bad(tr("这场会议没有录音，没法接着录", "This meeting has no recording to continue")));
            }
            Some(orig.file_name().unwrap().to_string_lossy().into_owned())
        }
        None => None,
    };
    let dir = meetings::new_folder(&s.root(), &r.title, chrono::Local::now().naive_local());
    let name = dir.file_name().unwrap().to_string_lossy().into_owned();
    let mut job = Job::new("record", &name, "recording");
    // 原来那场在录音期间不能删、不能改名、不能另外处理
    job.merging = continue_of.iter().cloned().collect();
    s.start_job(job.clone())?;
    let recorder = match Recorder::start(&s.opt.recorder_bin, &dir, system, r.mic) {
        Ok(rec) => rec,
        Err(e) => {
            s.jobs.lock().unwrap().retain(|j| j.id != job.id);
            let _ = std::fs::remove_dir_all(&dir);
            return Err(ApiError(StatusCode::INTERNAL_SERVER_ERROR, e));
        }
    };
    let live = LiveTranscriber::start(engine, &dir, &recorder.sources);
    let minutes = Arc::new(Mutex::new(LiveMinutes::default()));
    job.sources = recorder.sources.clone();
    s.update(&job.id, |j| j.sources = job.sources.clone());
    *s.recording.lock().unwrap() = Some(Live {
        job: job.id.clone(),
        recorder,
        live: Some(live),
        hotwords: r.hotwords,
        last_voice: 0.0,
        last_voice_audio: 0.0,
        silence_min: s.config().auto_stop_silence_min,
        marks: Vec::new(),
        continue_of,
        pause_audio: None,
        minutes: minutes.clone(),
    });
    spawn_watcher(&s, job.id.clone(), dir.clone(), job.sources.clone());
    spawn_live_minutes(&s, job.id.clone(), dir, minutes);
    Ok(Json(job))
}

/// 录音时每 5 秒检查一次：有没有人说话（任一音轨最近 5 秒够响，或者边录边转出了新的文字）、
/// 开始时在运行的会议软件是不是都退出了；到了条件就自动结束录音（和点「结束录音」一样）。
/// 录音暂时停着（麦克风被占用、App 被挂起过）的时间不算"没人说话"；停得太久录音组件不再接着录，也在这里结束。
fn spawn_watcher(s: &Shared, id: String, dir: PathBuf, sources: Vec<String>) {
    let s = s.clone();
    std::thread::spawn(move || {
        let apps = autostop::running_meeting_apps();
        let mut lines_seen = 0;
        loop {
            std::thread::sleep(Duration::from_secs(autostop::CHECK_S));
            let cfg = s.config();
            let window = autostop::CHECK_S as f64;
            let loud = sources
                .iter()
                .any(|src| audio::track_level(&wav_path(&dir, src), window) >= autostop::VOICE_LEVEL);
            let audio_s = sources
                .iter()
                .map(|src| audio::GrowingWav::new(wav_path(&dir, src)).seconds())
                .fold(0.0, f64::max);
            let apps_quit =
                cfg.auto_stop_app_quit && !apps.is_empty() && autostop::running_meeting_apps().is_disjoint(&apps);
            let stop = {
                let mut rec = s.recording.lock().unwrap();
                let Some(live) = rec.as_mut().filter(|l| l.job == id) else { return };
                let lines = live.live.as_ref().map_or(0, |l| l.line_count());
                let now = live.recorder.elapsed();
                let pause = live.recorder.pause();
                if pause.is_none() {
                    live.pause_audio = None;
                } else if live.pause_audio.is_none() {
                    live.pause_audio = Some(audio_s);
                }
                if pause.is_some() || loud || lines > lines_seen {
                    live.last_voice = now;
                    live.last_voice_audio = audio_s;
                }
                lines_seen = lines;
                let n = cfg.auto_stop_silence_min;
                live.silence_min = n;
                if let Some(p) = pause.filter(|p| p.expired) {
                    let m = p.limit_min();
                    let notice = tr(
                        &format!("录音中断超过 {m} 分钟没有恢复，已自动结束（中断前的内容都在）"),
                        &format!("Recording was interrupted for over {m} minutes and has been stopped — everything before that is kept"),
                    )
                    .to_string();
                    Some((notice, live.pause_audio))
                } else if n > 0 && now - live.last_voice >= n as f64 * 60.0 {
                    let notice = tr(
                        &format!("{n} 分钟没人说话，已自动结束录音"),
                        &format!("No one spoke for {n} minutes — recording stopped automatically"),
                    )
                    .to_string();
                    Some((notice, Some(live.last_voice_audio + autostop::KEEP_AFTER_S)))
                } else if apps_quit {
                    let notice = tr(
                        "会议软件已退出，已自动结束录音",
                        "The meeting app quit — recording stopped automatically",
                    );
                    Some((notice.to_string(), None))
                } else {
                    None
                }
            };
            if let Some((notice, keep)) = stop {
                if let Err(ApiError(_, e)) = stop_recording(&s, Some(&id), Some(notice), keep) {
                    eprintln!("自动结束录音失败：{e}");
                }
                return;
            }
        }
    });
}

/// 配了能用的大模型：填了 Key，或者是本机的 Ollama
fn llm_configured(cfg: &config::LlmConfig) -> bool {
    !cfg.api_key.trim().is_empty() || summarize::is_ollama(cfg)
}

/// 录音时边开会边整理速记版纪要：说够一段话后整理第一次，之后每两分钟把新说的话交给模型。
/// 录音结束后这个线程自己退出，正式纪要照常用整场转写写。速记版存在会议文件夹的 .live-minutes.md，
/// 会后写正式纪要失败时还有它可看。
fn spawn_live_minutes(s: &Shared, id: String, dir: PathBuf, minutes: Arc<Mutex<LiveMinutes>>) {
    let s = s.clone();
    std::thread::spawn(move || {
        let started = Instant::now();
        let mut last_try: Option<Instant> = None;
        loop {
            std::thread::sleep(Duration::from_secs(5));
            let cfg = s.config();
            // 每次都看一下：开会中途在设置里关掉、填上 Key 都马上生效
            let enabled = cfg.live_minutes && llm_configured(&cfg.llm);
            minutes.lock().unwrap().enabled = enabled;
            if !enabled {
                continue;
            }
            // 上次没成功（网络抖了一下、模型回的格式不对）半分钟后就再试，成功了才隔两分钟
            let wait = if minutes.lock().unwrap().error.is_some() { live_minutes::RETRY_S } else { live_minutes::INTERVAL_S };
            if last_try.is_some_and(|t| t.elapsed().as_secs_f64() < wait) {
                continue;
            }
            let cursor = minutes.lock().unwrap().cursor;
            let (lines, total) = {
                let rec = s.recording.lock().unwrap();
                let Some(live) = rec.as_ref().filter(|l| l.job == id) else { return };
                live.live.as_ref().map(|l| l.new_lines(cursor)).unwrap_or_default()
            };
            let lines: Vec<_> = lines.into_iter().filter(|l| !l.text.trim().is_empty()).collect();
            let chars: usize = lines.iter().map(|l| l.text.chars().count()).sum();
            let first = minutes.lock().unwrap().notes.is_empty();
            // 第一次等说够一段话，或者开会两分钟后有点内容就先整理
            let need = if !first {
                live_minutes::MIN_NEW_CHARS
            } else if started.elapsed().as_secs_f64() >= live_minutes::INTERVAL_S {
                live_minutes::MIN_NEW_CHARS
            } else {
                live_minutes::FIRST_CHARS
            };
            if chars < need {
                continue;
            }
            let words: String = lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n");
            let lang = i18n::resolve(&cfg.minutes_language, &words);
            let text = live_lines_text(&lines, lang);
            let upto_ms = lines.last().map_or(0, |l| l.start_ms);
            let notes = {
                let mut m = minutes.lock().unwrap();
                m.busy = true;
                m.notes.clone()
            };
            last_try = Some(Instant::now());
            let result = live_minutes::update(&cfg.llm, &notes, &text, &cfg.hotwords, lang);
            // 模型回复时录音可能已经结束了：结果照样记下，正式纪要不受影响
            let mut m = minutes.lock().unwrap();
            m.busy = false;
            match result {
                Ok(next) => {
                    let _ = std::fs::write(dir.join(".live-minutes.md"), next.markdown(lang));
                    m.notes = next;
                    m.version += 1;
                    m.cursor = total;
                    m.upto_ms = upto_ms;
                    m.lang = Some(lang);
                    m.error = None;
                }
                Err(e) => m.error = Some(e),
            }
            drop(m);
            if s.recording.lock().unwrap().as_ref().filter(|l| l.job == id).is_none() {
                return;
            }
        }
    });
}

/// 实时转写的句子排成给模型看的样子：同一人连着说的并成一段，「[分:秒] 说话人1：内容」
fn live_lines_text(lines: &[crate::chunked::LiveLine], lang: Lang) -> String {
    let name = |sp: Option<usize>| match sp {
        Some(n) => lang.pick(&format!("说话人{}", n + 1), &format!("Speaker {}", n + 1)).to_string(),
        None => lang.pick("说话人", "Speaker").to_string(),
    };
    let sep = lang.pick("：", ": ");
    let mut out: Vec<(Option<usize>, u64, String)> = Vec::new();
    for l in lines {
        match out.last_mut() {
            Some((sp, _, text)) if *sp == l.speaker => {
                text.push_str(lang.pick("", " "));
                text.push_str(l.text.trim());
            }
            _ => out.push((l.speaker, l.start_ms, l.text.trim().to_string())),
        }
    }
    out.into_iter().map(|(sp, ms, text)| format!("[{}] {}{sep}{text}", fmt_ts(ms), name(sp))).collect::<Vec<_>>().join("\n")
}

#[derive(Deserialize)]
struct LiveQuery {
    #[serde(default)]
    from: usize,
    /// 界面手上的速记版是第几版；和现在的一样就不再传正文
    #[serde(default)]
    notes_v: Option<u64>,
    /// 界面手里的说话人版本；过时了就从头给（录音中会整场重新聚类说话人）
    #[serde(default)]
    rev: u64,
}

/// 录音中的全部实时转写，从第 `from` 句起（界面每次只取新增的），带上标记过的重点时刻
async fn live_lines(State(s): State<Shared>, Query(q): Query<LiveQuery>) -> ApiResult<Json<Value>> {
    let rec = s.recording.lock().unwrap();
    let live = rec.as_ref().ok_or_else(|| ApiError(StatusCode::NOT_FOUND, tr("没有正在进行的录音", "Nothing is being recorded").into()))?;
    let (lines, total, rev) = live.live.as_ref().map(|l| l.lines_from(q.from, q.rev)).unwrap_or_default();
    Ok(Json(json!({
        "job": live.job,
        "from": total - lines.len(),
        "total": total,
        "rev": rev,
        "lines": lines.into_iter().map(|l| json!({"start_ms": l.start_ms, "speaker": l.speaker, "text": l.text})).collect::<Vec<_>>(),
        "marks": live.marks,
        "notes": live_notes_json(&live.minutes.lock().unwrap(), q.notes_v),
    })))
}

fn live_notes_json(m: &LiveMinutes, have: Option<u64>) -> Value {
    let mut v = json!({"v": m.version, "enabled": m.enabled, "busy": m.busy, "error": m.error, "upto_ms": m.upto_ms});
    if m.version > 0 && have != Some(m.version) {
        v["markdown"] = m.notes.markdown(m.lang.unwrap_or(Lang::Zh)).into();
    }
    v
}

#[derive(Deserialize)]
struct MarkReq {
    /// 标记哪一刻（毫秒）；不给就是现在
    ms: Option<u64>,
}

/// 标记重点（再标同一处就取消）：录音中顶栏按钮、⌘M，或点某一句
async fn record_mark(State(s): State<Shared>, Json(r): Json<MarkReq>) -> ApiResult<Json<Value>> {
    let mut rec = s.recording.lock().unwrap();
    let live = rec.as_mut().ok_or_else(|| bad(tr("没有正在进行的录音", "Nothing is being recorded")))?;
    let ms = r.ms.unwrap_or_else(|| (live.recorder.elapsed() * 1000.0) as u64);
    if let Some(i) = live.marks.iter().position(|&m| m.abs_diff(ms) < 1500) {
        live.marks.remove(i);
    } else {
        live.marks.push(ms);
        live.marks.sort_unstable();
    }
    Ok(Json(json!({"marks": live.marks})))
}

async fn record_stop(State(s): State<Shared>) -> ApiResult<Json<Job>> {
    stop_recording(&s, None, None, None).map(Json)
}

/// 结束录音：界面点「结束录音」和自动结束都走这里。`only` 有值时只结束这个任务的录音；
/// `notice` 是给任务的提示；`keep_s` 有值时先把原始音轨截到前这么多秒（没人说话自动结束时去掉末尾的静音）。
fn stop_recording(s: &Shared, only: Option<&str>, notice: Option<String>, keep_s: Option<f64>) -> ApiResult<Job> {
    let nothing = || bad(tr("没有正在进行的录音", "Nothing is being recorded"));
    let mut live = {
        let mut rec = s.recording.lock().unwrap();
        if rec.as_ref().is_none_or(|l| only.is_some_and(|id| l.job != id)) {
            return Err(nothing());
        }
        rec.take().ok_or_else(nothing)?
    };
    let job = s.jobs.lock().unwrap().iter().find(|j| j.id == live.job).cloned();
    let Some(job) = job else {
        return Err(nothing());
    };
    // 录音停下后转到后台：转写、生成纪要的同时就可以开下一场
    s.update(&job.id, |j| {
        j.state = "saving".into();
        if let Some(n) = notice {
            j.notice = n;
        }
    });
    let dir = s.root().join(&job.meeting);
    if !live.marks.is_empty() {
        if let Err(e) = meetings::write_marks(&dir, &live.marks) {
            eprintln!("保存重点标记失败：{e}");
        }
    }
    spawn_job(s, &job.id, move |st, id| {
        live.recorder.stop();
        if let Some(keep) = keep_s {
            for src in &live.recorder.sources {
                if let Err(e) = audio::truncate_wav(&wav_path(&dir, src), keep) {
                    eprintln!("截断 {src}.wav 失败：{e}");
                }
            }
        }
        st.update(id, |j| j.state = "finalizing".into());
        let engine = st.engine()?;
        let final_audio = dir.join(AUDIO);
        // 原始音轨合成前先把最后几秒转完
        let result = live.live.take().map(|l| l.finish(&engine, &final_audio));
        match result {
            Some(Ok((tr, t))) if !tr.sentences.is_empty() => {
                eprintln!(
                    "结束录音后转写：剩余 {:.1}s，{} 段，聚类 {:.2}s；边录边转说完到出字平均 {:.1}s、最长 {:.1}s",
                    t.tail_s, t.segments, t.cluster_s, t.live_lag_avg_s, t.live_lag_max_s
                );
                meetings::save_transcript(&tr, &dir).map_err(|e| e.to_string())?;
                if let Some(orig) = &live.continue_of {
                    record::mixdown_keeping_wav(&dir, &final_audio)?;
                    return st.finish_continued(id, orig, &dir, Some(&tr), &live.hotwords);
                }
                // 合成录音和生成纪要同时进行
                let mix_dir = dir.clone();
                let mixer =
                    std::thread::spawn(move || record::mixdown(&mix_dir, &mix_dir.join(AUDIO)));
                let summary = st.run_auto_minutes(id, &tr, &dir, &live.hotwords);
                let mixed = mixer
                    .join()
                    .map_err(|_| i18n::tr("合成录音线程异常", "The audio mixing thread crashed").to_string())?;
                summary?;
                mixed.map_err(|e| {
                    i18n::tr(
                        &format!("纪要已生成，但合成录音失败：{e}（原始音轨还在会议文件夹里）"),
                        &format!("Minutes are ready, but mixing the recording failed: {e} (the raw tracks are still in the meeting folder)"),
                    )
                    .to_string()
                })
            }
            // 边录边转出错或什么都没识别到：合成录音后整段重转
            _ => {
                if live.continue_of.is_some() {
                    record::mixdown_keeping_wav(&dir, &final_audio)?;
                } else {
                    record::mixdown(&dir, &final_audio)?;
                }
                if let Some(orig) = &live.continue_of {
                    // 原来那场有转写时，先把这一段单独转写好再去接：原来那场的转写（改过的说话人名字）不用重来。
                    // 原来那场没有转写（录音时 App 被关掉恢复出来的会议通常就是这样）时，合并后反正要整段重转，这里不白转一遍
                    let orig_transcribed = st.folder(orig).is_ok_and(|d| d.join(TRANSCRIPT_JSON).is_file());
                    let tr = if orig_transcribed { Some(st.run_transcribe(id, &final_audio, &dir)?) } else { None };
                    return st.finish_continued(id, orig, &dir, tr.as_ref(), &live.hotwords);
                }
                st.run_process(id, &final_audio, &dir, &live.hotwords)
            }
        }
    });
    let mut out = job;
    out.state = "saving".into();
    Ok(out)
}

#[derive(Deserialize)]
struct RecordTestReq {
    #[serde(default = "three")]
    seconds: u64,
}

fn three() -> u64 {
    3
}

/// 录音测试：系统声音 + 麦克风试录几秒（1–10），看有没有权限、有没有声音。
/// 第一次调用会触发 macOS 的权限弹窗，新手引导里用。
async fn record_test(State(s): State<Shared>, body: Option<Json<RecordTestReq>>) -> ApiResult<Json<Value>> {
    if s.jobs.lock().unwrap().iter().any(|j| j.state == "recording") {
        return Err(ApiError(
            StatusCode::CONFLICT,
            tr("正在录音，结束后再测试", "A recording is in progress. Test after it ends").into(),
        ));
    }
    let secs = body.map_or(3, |b| b.seconds).clamp(1, 10);
    let bin = s.opt.recorder_bin.clone();
    let v = tokio::task::spawn_blocking(move || record_test_run(&bin, secs))
        .await
        .map_err(|e| bad(e.to_string()))?;
    Ok(Json(v))
}

fn record_test_run(bin: &Path, secs: u64) -> Value {
    let system_ok = system_audio_supported(bin);
    let mut status: BTreeMap<String, &str> = BTreeMap::from([
        ("system".to_string(), if system_ok { "error" } else { "unsupported" }),
        ("mic".to_string(), "error"),
    ]);
    let failed = |e: &str| if record::is_permission_error(e) { "denied" } else { "error" };
    let fill = |levels: Vec<(String, Option<f32>)>, status: &mut BTreeMap<String, &str>| {
        for (src, level) in levels {
            let st = match level {
                None => "error",
                Some(l) if l >= 0.01 => "ok",
                Some(_) => "silent",
            };
            status.insert(src, st);
        }
    };
    let mut message = String::new();
    match record::probe(bin, system_ok, true, secs) {
        Ok(levels) => fill(levels, &mut status),
        // Windows 录系统声音不要权限，权限问题只会出在麦克风上：单独再测一下系统声音
        Err(e) if cfg!(windows) && system_ok && record::is_permission_error(&e) => {
            status.insert("mic".into(), "denied");
            match record::probe(bin, true, false, secs) {
                Ok(levels) => fill(levels, &mut status),
                Err(e2) => {
                    status.insert("system".into(), failed(&e2));
                }
            }
            message = e;
        }
        // 没有录屏与系统录音权限：单独再测一下麦克风（只录麦克风不需要这个权限）
        Err(e) if system_ok && record::is_permission_error(&e) => {
            status.insert("system".into(), "denied");
            match record::probe(bin, false, true, secs) {
                Ok(levels) => fill(levels, &mut status),
                Err(e2) => {
                    status.insert("mic".into(), failed(&e2));
                }
            }
            message = e;
        }
        Err(e) => {
            let st = failed(&e);
            if system_ok {
                status.insert("system".into(), st);
            }
            status.insert("mic".into(), st);
            message = e;
        }
    }
    let any = |x: &str| status.values().any(|v| *v == x);
    if message.is_empty() {
        message = if any("error") {
            tr("没有录到声音文件", "No audio file was recorded").to_string()
        } else if any("silent") {
            tr(
                "录到了，但几乎没有声音：放点声音、说句话再试",
                "Recorded, but almost silent: play something or say a few words and try again",
            )
            .to_string()
        } else {
            tr("录音正常", "Recording works").to_string()
        };
    }
    json!({"system": status["system"], "mic": status["mic"], "message": message})
}

async fn import_file(State(s): State<Shared>, mut form: Multipart) -> ApiResult<Json<Job>> {
    let (mut data, mut filename, mut title, mut modified) =
        (None, String::from("录音.m4a"), String::new(), 0f64);
    while let Some(field) = form.next_field().await.map_err(|e| bad(e.to_string()))? {
        match field.name() {
            Some("file") => {
                filename = field.file_name().unwrap_or("录音.m4a").to_string();
                data = Some(field.bytes().await.map_err(|e| bad(e.to_string()))?);
            }
            Some("title") => title = field.text().await.unwrap_or_default(),
            Some("modified") => {
                modified = field
                    .text()
                    .await
                    .unwrap_or_default()
                    .parse()
                    .unwrap_or(0.0)
            }
            _ => {}
        }
    }
    let data = data.ok_or_else(|| bad(tr("没有收到文件", "No file received")))?;
    let ext = Path::new(&filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if !meetings::AUDIO_EXTS.contains(&ext.as_str()) {
        return Err(bad(tr(&format!("不支持的文件类型：.{ext}"), &format!("Unsupported file type: .{ext}"))));
    }
    let when = if modified > 0.0 {
        chrono::DateTime::from_timestamp_millis(modified as i64)
            .map(|d| d.with_timezone(&chrono::Local).naive_local())
            .unwrap_or_else(|| chrono::Local::now().naive_local())
    } else {
        chrono::Local::now().naive_local()
    };
    let stem = Path::new(&filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("会议")
        .to_string();
    let dir = meetings::new_folder(
        &s.root(),
        if title.is_empty() { &stem } else { &title },
        when,
    );
    let name = dir.file_name().unwrap().to_string_lossy().into_owned();
    let job = Job::new("process", &name, "saving");
    s.start_job(job.clone())?;
    std::fs::create_dir_all(&dir).map_err(|e| bad(e.to_string()))?;
    let audio = dir.join(format!("录音.{ext}"));
    std::fs::write(&audio, &data).map_err(|e| bad(e.to_string()))?;
    spawn_job(&s, &job.id, move |st, id| st.run_process(id, &audio, &dir, &[]));
    Ok(Json(job))
}

/// 所有任务（进行中的和刚结束的），按开始时间排列
async fn get_jobs(State(s): State<Shared>) -> Json<Value> {
    let jobs = s.jobs.lock().unwrap().clone();
    let list: Vec<Value> = jobs.iter().map(|j| job_json(&s, j)).collect();
    Json(json!({"jobs": list, "model_loaded": s.engine_loaded()}))
}

/// 旧接口：只返回一个任务（正在录音的优先，其次最近一个进行中的，再次最近一个）
async fn get_job(State(s): State<Shared>) -> Json<Value> {
    let jobs = s.jobs.lock().unwrap().clone();
    let pick = jobs
        .iter()
        .find(|j| j.state == "recording")
        .or_else(|| jobs.iter().rev().find(|j| j.busy()))
        .or_else(|| jobs.last());
    match pick {
        Some(j) => Json(job_json(&s, j)),
        None => Json(json!({"state": "idle"})),
    }
}

fn job_json(s: &AppState, job: &Job) -> Value {
    let job = job.clone();
    let mut data = serde_json::to_value(&job).unwrap();
    let end = if job.finished_at > 0.0 { job.finished_at } else { now() };
    data["elapsed"] = (end - job.started_at).into();
    data["model_loaded"] = s.engine_loaded().into();
    if job.state == "recording" {
        let dir = s.root().join(&job.meeting);
        let levels: BTreeMap<&str, f32> = job
            .sources
            .iter()
            .map(|src| {
                (
                    src.as_str(),
                    audio::track_level(&dir.join(format!("{src}.wav")), 0.15),
                )
            })
            .collect();
        data["levels"] = json!(levels);
        let mut rec = s.recording.lock().unwrap();
        if let Some(live) = rec.as_mut() {
            data["live"] = live
                .live
                .as_ref()
                .map(|l| {
                    l.recent(8)
                        .into_iter()
                        .map(|l| json!({"start_ms": l.start_ms, "speaker": l.speaker, "text": l.text}))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
                .into();
            // 距最后有人说话过了多少秒；开了自动结束时再给还有多少秒自动结束
            let silent = (live.recorder.elapsed() - live.last_voice).max(0.0) as u64;
            data["silent_s"] = silent.into();
            if live.silence_min > 0 {
                data["auto_stop_s"] = (live.silence_min as u64 * 60).saturating_sub(silent).into();
            }
            let status = live.recorder.status();
            if let Some(err) = status.error {
                data["error"] = tr(&format!("录音中断：{err}"), &format!("Recording stopped: {err}")).into();
            }
            // 录音暂时停着（麦克风被占用、App 被挂起过）：界面要说清楚现在没在录
            if let Some(pause) = status.pause {
                data["paused_s"] = (pause.seconds.max(0.0) as u64).into();
            }
            // 之前暂停过、已经接上的时间：界面在它变多时提示有一段没录到
            if status.lost_s >= 1.0 {
                data["lost_s"] = (status.lost_s as u64).into();
            }
        }
    }
    data
}

/// 收起所有已结束的任务
async fn dismiss(State(s): State<Shared>) -> Json<Value> {
    s.jobs.lock().unwrap().retain(Job::busy);
    Json(json!({"ok": true}))
}

/// 收起一个已结束的任务
async fn dismiss_one(State(s): State<Shared>, UrlPath(id): UrlPath<String>) -> Json<Value> {
    s.jobs.lock().unwrap().retain(|j| j.busy() || j.id != id);
    Json(json!({"ok": true}))
}

// ---------- 模型下载（桌面版新增） ----------

fn model_kind(id: &str) -> ApiResult<AsrKind> {
    AsrKind::from_id(id).ok_or_else(|| {
        ApiError(
            StatusCode::NOT_FOUND,
            tr(&format!("没有这个识别模型：{id}"), &format!("No such speech recognition model: {id}")).into(),
        )
    })
}

/// 设置页的识别模型列表：哪些下载了、当前用哪个
async fn list_asr_models(State(s): State<Shared>) -> Json<Value> {
    let paths = s.model_paths();
    let active = paths.active();
    let language = s.config().asr_language;
    let english_meetings = language == "en";
    let bin = s.opt.recorder_bin.clone();
    // Apple Speech 要靠 macOS 版的录音组件子进程，iOS、Windows 上不列出来
    let no_apple = !cfg!(target_os = "macos");
    let apple = if no_apple {
        Err(String::new())
    } else {
        tokio::task::spawn_blocking(move || crate::apple_speech::status(&bin, &language)).await
            .map_err(|e| e.to_string()).and_then(|r| r)
    };
    let list: Vec<Value> = models::catalog()
        .iter()
        .filter(|m| !(no_apple && m.kind == AsrKind::AppleSpeech))
        .map(|m| {
            let mut v = json!({
                "id": m.kind.id(),
                "name": m.name,
                "desc": m.desc,
                "size_mb": m.size / 1_000_000,
                "ready": paths.asr_ready(m.kind),
                "active": m.kind == active,
            });
            if m.kind == AsrKind::AppleSpeech {
                v["system"] = true.into();
                match &apple {
                    Ok(status) => { v["available"] = status.available.into(); v["ready"] = (status.ready && paths.base_ready()).into(); }
                    Err(e) => { v["available"] = false.into(); v["ready"] = false.into(); v["warning"] = e.clone().into(); }
                }
            }
            if m.kind == AsrKind::Paraformer && english_meetings {
                v["warning"] = paraformer_warning().into();
            }
            v
        })
        .collect();
    Json(json!({"active": active.id(), "base_ready": paths.base_ready(), "models": list}))
}

/// 会议语言选了英文时，提醒 Paraformer 只认中文
fn paraformer_warning() -> &'static str {
    tr(
        "Paraformer 只认中文；英文会议请换成 SenseVoice",
        "Paraformer only recognizes Chinese; switch to SenseVoice for English meetings",
    )
}

/// 下载一个识别模型，下好了直接切过去
#[derive(Deserialize, Default)]
struct DownloadReq {
    /// cn（国内镜像）| github，界面里让用户选；不传按 cn
    #[serde(default)]
    source: String,
}

fn download_source(r: Option<Json<DownloadReq>>) -> ApiResult<models::Source> {
    let id = r.map(|Json(r)| r.source).unwrap_or_default();
    if id.is_empty() {
        return Ok(models::Source::Cn);
    }
    models::Source::from_id(&id).ok_or_else(|| bad(tr(&format!("不认识的下载线路：{id}"), &format!("Unknown download source: {id}"))))
}

async fn download_model(
    State(s): State<Shared>,
    UrlPath(id): UrlPath<String>,
    body: Option<Json<DownloadReq>>,
) -> ApiResult<Json<Job>> {
    let kind = model_kind(&id)?;
    let source = download_source(body)?;
    let paths = s.model_paths();
    if kind != AsrKind::AppleSpeech && paths.base_ready() && paths.asr_ready(kind) {
        return Err(bad(tr("这个模型已经下载好了", "This model is already downloaded")));
    }
    let job = Job::new("models", kind.id(), "downloading");
    s.start_job(job.clone())?;
    spawn_job(&s, &job.id, move |st, id| {
        let paths = st.model_paths();
        if kind == AsrKind::AppleSpeech {
            st.update(id, |j| j.stage = tr("正在检查／安装系统语音模型…", "Checking/installing system speech assets…").into());
            crate::apple_speech::run(&st.opt.recorder_bin, "speech-install", &st.config().asr_language, None)?;
        }
        models::ensure_model(&paths, kind, source, |done, total| {
            st.update(id, |j| {
                j.stage = format!("{:.0} / {:.0} MB", done as f64 / 1e6, total as f64 / 1e6)
            })
        })?;
        paths.set_active(kind).map_err(|e| e.to_string())?;
        Ok(())
    });
    Ok(Json(job))
}

async fn use_model(State(s): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult<Json<Value>> {
    let kind = model_kind(&id)?;
    let paths = s.model_paths();
    if !paths.asr_ready(kind) {
        return Err(bad(tr("这个模型还没有下载", "This model isn't downloaded yet")));
    }
    if kind == AsrKind::AppleSpeech {
        let bin = s.opt.recorder_bin.clone(); let lang = s.config().asr_language;
        let status = tokio::task::spawn_blocking(move || crate::apple_speech::status(&bin, &lang)).await.map_err(|e| bad(e.to_string()))?.map_err(bad)?;
        if !status.available || !status.ready || !paths.base_ready() { return Err(bad("请先检查／安装模型 / Check and install models first")); }
    }
    paths.set_active(kind).map_err(|e| bad(e.to_string()))?;
    // 下次转写时按新模型加载；正在进行的转写用的还是旧引擎，不受影响
    if !s.engine.lock().unwrap().as_ref().is_some_and(|e| e.kind == kind) {
        *s.engine.lock().unwrap() = None;
    }
    Ok(Json(json!({"ok": true, "active": kind.id()})))
}

async fn delete_model(State(s): State<Shared>, UrlPath(id): UrlPath<String>) -> ApiResult<Json<Value>> {
    let kind = model_kind(&id)?;
    let paths = s.model_paths();
    if paths.active() == kind {
        return Err(bad(tr(
            "正在使用这个模型，先切换到别的模型再删除",
            "This model is in use. Switch to another model before deleting it",
        )));
    }
    if s.jobs.lock().unwrap().iter().any(|j| j.busy() && j.kind == "models" && j.meeting == kind.id()) {
        return Err(bad(tr("这个模型正在下载", "This model is downloading")));
    }
    models::remove_model(&paths, kind).map_err(bad)?;
    Ok(Json(json!({"ok": true})))
}

async fn download_models(State(s): State<Shared>, body: Option<Json<DownloadReq>>) -> ApiResult<Json<Job>> {
    let source = download_source(body)?;
    if s.model_paths().ready() {
        return Err(bad(tr("模型已经下载好了", "The models are already downloaded")));
    }
    let job = Job::new("models", "", "downloading");
    s.start_job(job.clone())?;
    spawn_job(&s, &job.id, move |st, id| {
        models::ensure(&st.model_paths(), source, |done, total| {
            st.update(id, |j| {
                j.stage = format!("{:.0} / {:.0} MB", done as f64 / 1e6, total as f64 / 1e6)
            })
        })?;
        st.engine().map(|_| ())
    });
    Ok(Json(job))
}

// ---------- 检查更新（桌面版） ----------

const UPDATE_TTL: Duration = Duration::from_secs(6 * 3600);

/// 最新 Release；6 小时内查过就用上次的结果
fn latest_release(s: &AppState, force: bool) -> Result<update::Release, String> {
    latest_release_with(s, force, || update::fetch_latest(&s.opt.version))
}

fn latest_release_with(s: &AppState, force: bool, fetch: impl FnOnce() -> Result<update::Release, String>) -> Result<update::Release, String> {
    if let Some((at, r)) = s.update.lock().unwrap().as_ref() {
        if !force && at.elapsed() < UPDATE_TTL {
            return Ok(r.clone());
        }
    }
    let r = fetch()?;
    *s.update.lock().unwrap() = Some((Instant::now(), r.clone()));
    Ok(r)
}

#[derive(Deserialize, Default)]
struct UpdateQuery {
    #[serde(default)]
    force: bool,
}

async fn get_update(State(s): State<Shared>, Query(query): Query<UpdateQuery>) -> Json<Value> {
    let s2 = s.clone();
    let r = tokio::task::spawn_blocking(move || latest_release(&s2, query.force))
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r);
    let current = &s.opt.version;
    Json(match r {
        Ok(r) => r.to_json(current),
        Err(e) => json!({"current": current, "newer": false, "error": e}),
    })
}

/// 下载新版安装包到「下载」文件夹，校验 sha256 后打开（macOS 挂载 dmg，Windows 运行安装程序）
async fn download_update(State(s): State<Shared>) -> ApiResult<Json<Job>> {
    let s2 = s.clone();
    let r = tokio::task::spawn_blocking(move || latest_release(&s2, false))
        .await
        .map_err(|e| bad(e.to_string()))?
        .map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e))?;
    let name = Path::new(&r.dmg_name).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let url = match &r.dmg {
        Some(u) if r.newer_than(&s.opt.version) && !name.is_empty() => u.clone(),
        _ => return Err(bad(tr("没有可用的更新", "No update available"))),
    };
    let job = Job::new("update", "", "downloading");
    s.start_job(job.clone())?;
    spawn_job(&s, &job.id, move |st, id| {
        let home = crate::platform::home_dir().ok_or("HOME")?;
        let dest = home.join("Downloads").join(&name);
        let _ = std::fs::remove_file(&dest);
        let _ = std::fs::remove_file(dest.with_extension("part"));
        let client = update::client(&st.opt.version, None)?;
        let total = r.size as f64 / 1e6;
        models::fetch(&client, &url, &dest, |n| {
            st.update(id, |j| j.stage = format!("{:.0} / {:.0} MB", n as f64 / 1e6, total))
        })?;
        if let Some(want) = &r.sha256 {
            if update::sha256_file(&dest).ok().as_deref() != Some(want.as_str()) {
                let _ = std::fs::remove_file(&dest);
                return Err(i18n::tr(
                    "下载的安装包校验不通过，已删除",
                    "The downloaded installer failed verification and was deleted",
                )
                .into());
            }
        }
        crate::platform::open(&dest);
        st.update(id, |j| {
            j.notice = if cfg!(windows) {
                i18n::tr(
                    "新版已下载并校验，按安装程序的提示完成更新",
                    "Downloaded and verified — follow the installer to finish updating",
                )
            } else {
                i18n::tr(
                    "新版已下载并校验，把 Tingo 拖进「应用程序」替换即可",
                    "Downloaded and verified — drag Tingo into Applications to replace the old version",
                )
            }
            .into()
        });
        Ok(())
    });
    Ok(Json(job))
}

pub fn router(state: Shared) -> Router {
    let origins = [
        "tauri://localhost",
        "http://tauri.localhost",
        "https://tauri.localhost",
        "http://localhost:1420",
    ];
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(
            origins.iter().map(|o| HeaderValue::from_static(o)),
        ))
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::RANGE,
            axum::http::HeaderName::from_static(i18n::HEADER),
        ]);
    Router::new()
        .route("/api/env", get(env))
        .route("/api/config", get(get_config).put(put_config))
        .route("/api/config/test", post(test_llm))
        .route("/api/llm/providers", get(llm_providers))
        .route("/api/llm/models", post(llm_models))
        .route("/api/meetings", get(list_meetings))
        .route("/api/meetings/batch", post(post_batch))
        .route("/api/meetings/merge", post(merge_meetings))
        .route("/api/meetings/{id}", get(get_meeting).delete(delete_meeting))
        .route("/api/meetings/{id}/audio", get(get_audio).delete(delete_audio))
        .route("/api/meetings/{id}/meta", put(put_meta))
        .route("/api/storage", get(get_storage))
        .route("/api/storage/choose", post(choose_storage))
        .route("/api/storage/directory", post(change_storage))
        .route("/api/storage/progress", get(storage_progress))
        .route("/api/storage/cleanup", post(post_cleanup))
        .route("/api/meetings/{id}/speakers", put(put_speakers))
        .route("/api/meetings/{id}/minutes", put(put_minutes))
        .route("/api/meetings/{id}/minutes/history", get(get_minutes_history))
        .route("/api/meetings/{id}/minutes/history/{version}", delete(delete_minutes_history))
        // iOS 0.4.2 的旧路径
        .route("/api/meetings/{id}/minutes/versions", get(get_minutes_history))
        .route("/api/meetings/{id}/minutes/versions/{version}", delete(delete_minutes_history))
        .route("/api/meetings/{id}/summarize", post(post_summarize))
        .route("/api/meetings/{id}/process", post(post_process))
        .route("/api/meetings/{id}/push", post(post_push))
        .route("/api/meetings/{id}/reveal", post(reveal))
        .route("/api/meetings/{id}/obsidian", post(post_obsidian))
        .route("/api/meetings/{id}/excalidraw", post(post_excalidraw))
        .route("/api/meetings/{id}/pdf", post(post_pdf))
        .route("/api/meetings/{id}/srt", post(post_srt))
        .route("/api/meetings/{id}/docx", post(post_docx))
        .route("/api/meetings/{id}/speakers/guess", post(guess_speakers))
        .route("/api/search", get(search))
        .route("/api/demo", post(post_demo))
        .route("/api/record/test", post(record_test))
        .route("/api/update", get(get_update))
        .route("/api/update/download", post(download_update))
        .route("/api/obsidian/vaults", get(obsidian_vaults))
        .route("/api/record/start", post(record_start))
        .route("/api/record/stop", post(record_stop))
        .route("/api/record/mark", post(record_mark))
        .route("/api/live", get(live_lines))
        .route("/api/import", post(import_file))
        .route("/api/job", get(get_job))
        .route("/api/job/dismiss", post(dismiss))
        .route("/api/jobs", get(get_jobs))
        .route("/api/jobs/{id}/dismiss", post(dismiss_one))
        .route("/api/models/download", post(download_models))
        .route("/api/models", get(list_asr_models))
        .route("/api/models/{id}/download", post(download_model))
        .route("/api/models/{id}/use", post(use_model))
        .route("/api/models/{id}", delete(delete_model))
        .layer(axum::middleware::from_fn_with_state(state.clone(), storage_gate))
        .layer(axum::extract::DefaultBodyLimit::max(4 << 30)) // 导入的录屏可能有几个 GB
        .layer(axum::middleware::from_fn(i18n::layer)) // 每个请求都记下界面语言
        .layer(cors)
        .with_state(state)
}

/// 在 127.0.0.1 上启动服务（port 为 0 时随机选），返回实际端口和要一直跑的服务。
pub async fn serve(
    opt: Options,
    port: u16,
) -> std::io::Result<(u16, impl std::future::Future<Output = ()>)> {
    serve_state(AppState::new(opt), port).await
}

pub async fn serve_state(
    state: Arc<AppState>,
    port: u16,
) -> std::io::Result<(u16, impl std::future::Future<Output = ()>)> {
    listen(router(state), port).await
}

/// 建好全部状态的服务（任务、录音、模型）；克隆后共用同一份状态。
pub type App = Router;

/// 原生端：录音完成只保存转写，纪要由显式的在线/离线操作生成。
pub fn app_manual_minutes(opt: Options) -> App {
    router(AppState::with_auto_minutes(opt, false))
}

/// 和 [`listen`] 分开，重新监听时保留录音和任务。
pub fn app(opt: Options) -> App {
    router(AppState::new(opt))
}

pub async fn listen(
    app: App,
    port: u16,
) -> std::io::Result<(u16, impl std::future::Future<Output = ()>)> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let port = listener.local_addr()?.port();
    Ok((port, async move {
        let _ = axum::serve(listener, app).await;
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_checkpoint_preserves_current_minutes() {
        let dir = std::env::temp_dir().join(format!("tingo-draft-{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MINUTES), "原纪要").unwrap();
        write_minutes_draft(&dir, "收到第一段").unwrap();
        write_minutes_draft(&dir, "收到第一段和第二段").unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(MINUTES)).unwrap(), "原纪要");
        assert_eq!(std::fs::read_to_string(dir.join("在线纪要草稿.md")).unwrap(), "收到第一段和第二段");
        assert!(!dir.join("在线纪要草稿.tmp").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn native_processing_does_not_implicitly_generate_online_minutes() {
        let opt = Options { root: PathBuf::new(), config_path: PathBuf::new(), models_dir: PathBuf::new(), recorder_bin: PathBuf::new(), version: "test".into(), sample_dir: PathBuf::new() };
        let state = AppState::with_auto_minutes(opt, false);
        assert!(state.run_auto_minutes("test", &Transcript::default(), Path::new("/nonexistent-native-test"), &[]).is_ok());
    }

    /// 临时会议目录 + 假的录音组件（只在 Unix 上写，是个 sh 脚本）：`trash <路径>` 把东西 mv 进 trash 目录并打印 OK；
    /// 路径里带 FAIL 的打印 ERROR 失败
    fn test_state(name: &str) -> (Shared, PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("tingo-srv-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (root, trash, models) = (base.join("root"), base.join("trash"), base.join("models"));
        for d in [&root, &trash, &models] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(models.join("model.onnx"), vec![0u8; 123]).unwrap();
        let bin = base.join("fake-recorder");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(
                &bin,
                format!(
                    "#!/bin/sh\n[ \"$1\" = trash ] || {{ echo 'ERROR: bad command'; exit 2; }}\n\
                     case \"$2\" in *FAIL*) echo 'ERROR: 不让删'; exit 1;; esac\n\
                     d=$(mktemp -d '{}/t.XXXXXX') && mv \"$2\" \"$d/\" && echo OK\n",
                    trash.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let state = AppState::new(Options {
            root: root.clone(),
            config_path: base.join("config.toml"),
            models_dir: models,
            recorder_bin: bin,
            version: "0.0.0".into(),
            sample_dir: base.join("sample"),
        });
        (state, root, trash)
    }

    #[tokio::test]
    async fn update_install_excludes_active_jobs_and_new_requests() {
        let (s, root, _) = test_state("install-update");
        mark_busy(&s, "meeting");
        assert!(s.prepare_app_update().is_err());
        s.jobs.lock().unwrap().clear();
        let read = s.storage_gate.read().await;
        assert!(s.prepare_app_update().is_err());
        drop(read);
        let install = s.prepare_app_update().unwrap();
        assert!(s.storage_gate.try_read().is_err());
        assert!(s.prepare_app_update().is_err());
        drop(install);
        assert!(s.storage_gate.try_read().is_ok());
        std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[test]
    fn manual_update_check_bypasses_backend_cache() {
        let (s, root, _) = test_state("update-cache");
        let old = update::Release { latest: "0.5.0".into(), ..Default::default() };
        *s.update.lock().unwrap() = Some((Instant::now(), old.clone()));
        assert_eq!(latest_release_with(&s, false, || panic!("automatic checks should use cache")).unwrap(), old);
        let new = update::Release { latest: "0.5.1".into(), ..Default::default() };
        assert_eq!(latest_release_with(&s, true, || Ok(new.clone())).unwrap(), new);
        assert_eq!(latest_release_with(&s, false, || panic!("fresh result should be cached")).unwrap(), new);
        assert!(latest_release_with(&s, true, || Err("offline".into())).is_err());
        std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn storage_copy_switch_preserves_history_and_configuration() {
        let (s, root, _) = test_state("directory-switch");
        let target = root.parent().unwrap().join("新目录");
        std::fs::create_dir(&target).unwrap();
        let dir = meeting(&root, "2026-10-01_0900_a", &[("录音.m4a", 123), (MINUTES, 20)]);
        std::fs::create_dir(dir.join("minutes-history")).unwrap();
        std::fs::write(dir.join("minutes-history/123.json"), "history").unwrap();
        let modified = std::fs::metadata(dir.join(MINUTES)).unwrap().modified().unwrap();
        let request = || Json(StorageDirectoryReq { path: target.to_string_lossy().into(), copy_existing: true });
        mark_busy(&s, "2026-10-01_0900_a");
        assert_eq!(status(change_storage(State(s.clone()), request()).await), StatusCode::CONFLICT);
        assert_eq!(s.root(), root);
        assert!(std::fs::read_dir(&target).unwrap().next().is_none());
        s.jobs.lock().unwrap().clear();
        let _ = change_storage(State(s.clone()), request()).await.unwrap();
        assert_eq!(s.root(), target.canonicalize().unwrap());
        assert_eq!(PathBuf::from(s.config().storage_root), s.root());
        let copied = target.join("2026-10-01_0900_a");
        assert_eq!(std::fs::read(copied.join("minutes-history/123.json")).unwrap(), b"history");
        assert_eq!(std::fs::metadata(copied.join(MINUTES)).unwrap().modified().unwrap(), modified);
        assert!(dir.join("录音.m4a").exists());
        assert!(!s.storage_progress.lock().unwrap().active);
        // Switching back without copying restores the original library immediately.
        let _ = change_storage(State(s.clone()), Json(StorageDirectoryReq { path: root.to_string_lossy().into(), copy_existing: false })).await.unwrap();
        assert_eq!(s.root(), root.canonicalize().unwrap());
        std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn storage_rejects_conflicts_nested_folders_and_links() {
        let (s, root, _) = test_state("directory-errors");
        let target = root.parent().unwrap().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("keep.txt"), "keep").unwrap();
        let request = || Json(StorageDirectoryReq { path: target.to_string_lossy().into(), copy_existing: true });
        assert!(change_storage(State(s.clone()), request()).await.is_err());
        assert_eq!(std::fs::read_to_string(target.join("keep.txt")).unwrap(), "keep");
        assert_eq!(s.root(), root);
        std::fs::remove_file(target.join("keep.txt")).unwrap();
        let nested = root.join("nested");
        std::fs::create_dir(&nested).unwrap();
        assert!(crate::storage::destination(&root, nested.to_str().unwrap()).is_err());
        assert!(crate::storage::destination(&root, root.parent().unwrap().to_str().unwrap()).is_err());
        std::os::unix::fs::symlink(&target, root.join("link")).unwrap();
        assert!(change_storage(State(s.clone()), request()).await.is_err());
        assert_eq!(s.root(), root);
        assert!(!s.opt.config_path.exists());
        assert!(std::fs::read_dir(&target).unwrap().next().is_none());
        std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn config_temperature_set_clear_and_reject() {
        let (s, root, _) = test_state("temperature");
        let put = |t: Option<Value>| {
            let mut body = json!({"base_url": "https://api.moonshot.cn/v1", "model": "kimi"});
            if let Some(t) = t { body["temperature"] = t; }
            put_config(State(s.clone()), Json(serde_json::from_value(body).unwrap()))
        };
        assert!(put(Some(json!(1))).await.is_ok());
        assert_eq!(s.config().llm.temperature, Some(1.0));
        assert_eq!(config_json(&s)["temperature"], 1.0);
        // 不传就不改（别的设置页保存时不带这个字段）
        assert!(put(None).await.is_ok());
        assert_eq!(s.config().llm.temperature, Some(1.0));
        for bad in [json!(3), json!("abc"), json!(-0.1)] {
            assert_eq!(status(put(Some(bad)).await), StatusCode::BAD_REQUEST);
        }
        assert_eq!(s.config().llm.temperature, Some(1.0));
        assert!(put(Some(json!(" 0.5 "))).await.is_ok());
        assert_eq!(s.config().llm.temperature, Some(0.5));
        assert!(put(Some(json!(""))).await.is_ok());
        assert_eq!(s.config().llm.temperature, None);
        assert!(config_json(&s)["temperature"].is_null());
        std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[test]
    fn live_lines_are_grouped_by_speaker_for_the_model() {
        use crate::chunked::LiveLine;
        let l = |ms: u64, sp: Option<usize>, t: &str| LiveLine { start_ms: ms, speaker: sp, text: t.into() };
        let lines = [l(1_000, Some(0), "下周三"), l(3_000, Some(0), "发报价。"), l(65_000, Some(1), "好的。"), l(70_000, None, " 还有一件事")];
        assert_eq!(live_lines_text(&lines, Lang::Zh), "[00:01] 说话人1：下周三发报价。\n[01:05] 说话人2：好的。\n[01:10] 说话人：还有一件事");
        let en = [l(0, Some(0), "Ship it"), l(900, Some(0), "on Friday.")];
        assert_eq!(live_lines_text(&en, Lang::En), "[00:00] Speaker 1: Ship it on Friday.");
    }

    #[test]
    fn live_notes_are_sent_only_when_the_page_has_an_older_version() {
        let mut m = LiveMinutes { enabled: true, ..Default::default() };
        assert!(live_notes_json(&m, None).get("markdown").is_none(), "还没整理出东西");
        let reply = "## 一句话总结\n讨论报价。\n\n## 议题与讨论\n### 报价\n- 周三前发 [00:01]\n\n## 决议\n- 无\n\n## 待办\n| 事项 | 负责人 | 截止 |\n|---|---|---|\n\n## 待定问题与风险\n- 无";
        m.notes = live_minutes::apply(&m.notes, reply).unwrap();
        m.version = 1;
        m.lang = Some(Lang::Zh);
        let v = live_notes_json(&m, None);
        assert!(v["markdown"].as_str().unwrap().starts_with("## 一句话总结\n讨论报价。"));
        assert_eq!((v["v"].as_u64(), v["enabled"].as_bool()), (Some(1), Some(true)));
        assert!(live_notes_json(&m, Some(1)).get("markdown").is_none());
    }

    #[tokio::test]
    async fn config_think_defaults_off_and_is_kept_when_not_sent() {
        let (s, root, _) = test_state("think");
        assert_eq!(config_json(&s)["think"], false);
        let put = |t: Option<bool>| {
            let mut body = json!({"base_url": "https://api.minimaxi.com/v1", "model": "MiniMax-M3.1-Flash"});
            if let Some(t) = t { body["think"] = t.into(); }
            put_config(State(s.clone()), Json(serde_json::from_value(body).unwrap()))
        };
        assert!(put(Some(true)).await.is_ok());
        assert!(s.config().llm.think);
        assert!(put(None).await.is_ok());
        assert_eq!(config_json(&s)["think"], true);
        assert!(put(Some(false)).await.is_ok());
        assert!(!s.config().llm.think);
        std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn storage_failed_config_save_does_not_switch() {
        let (s, root, _) = test_state("directory-config-error");
        let target = root.parent().unwrap().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::create_dir(&s.opt.config_path).unwrap(); // cannot replace a directory with a config file
        assert!(change_storage(State(s.clone()), Json(StorageDirectoryReq { path: target.to_string_lossy().into(), copy_existing: false })).await.is_err());
        assert_eq!(s.root(), root);
        std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn apple_model_status_switch_and_unavailable() {
        let (s, root, _) = test_state("apple-models");
        let paths = s.model_paths();
        for p in paths.base().into_iter().chain(paths.asr_files(AsrKind::Paraformer)) {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "test").unwrap();
        }
        std::fs::write(&s.opt.recorder_bin, "#!/bin/sh\necho '{\"available\":true,\"ready\":true}'\n").unwrap();
        let list = list_asr_models(State(s.clone())).await.0;
        let apple = list["models"].as_array().unwrap().iter().find(|m| m["id"] == "apple-speech").unwrap();
        assert_eq!(apple["ready"], true);
        assert_eq!(apple["system"], true);
        assert!(use_model(State(s.clone()), UrlPath("apple-speech".into())).await.is_ok());
        assert_eq!(paths.active(), AsrKind::AppleSpeech);
        assert!(use_model(State(s.clone()), UrlPath("paraformer".into())).await.is_ok());
        assert_eq!(paths.active(), AsrKind::Paraformer);
        assert!(delete_model(State(s.clone()), UrlPath("apple-speech".into())).await.is_err());
        std::fs::write(&s.opt.recorder_bin, "#!/bin/sh\necho 'ERROR: requires macOS 26'; exit 1\n").unwrap();
        assert!(use_model(State(s.clone()), UrlPath("apple-speech".into())).await.is_err());
        assert_eq!(paths.active(), AsrKind::Paraformer);
        let list = list_asr_models(State(s.clone())).await.0;
        let apple = list["models"].as_array().unwrap().iter().find(|m| m["id"] == "apple-speech").unwrap();
        assert_eq!(apple["available"], false);
        std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    fn meeting(root: &Path, id: &str, files: &[(&str, usize)]) -> PathBuf {
        let d = root.join(id);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(TRANSCRIPT_JSON), serde_json::to_string(&Transcript::default()).unwrap()).unwrap();
        for (f, n) in files {
            std::fs::write(d.join(f), vec![0u8; *n]).unwrap();
        }
        d
    }

    fn mark_busy(s: &AppState, id: &str) {
        s.jobs.lock().unwrap().push(Job::new("process", id, "transcribing"));
    }

    fn status(r: ApiResult<impl std::fmt::Debug>) -> StatusCode {
        match r {
            Ok(v) => panic!("expected an error, got {v:?}"),
            Err(ApiError(code, _)) => code,
        }
    }

    #[test]
    fn routes_build() {
        let (s, root, _) = test_state("routes");
        let _ = router(s);
        std::fs::remove_dir_all(root.parent().unwrap()).ok();
    }

    #[test]
    fn put_meta_updates_only_given_fields() {
        let (s, root, _) = test_state("meta");
        let a = meeting(&root, "2026-10-01_0900_周会", &[]);
        std::fs::write(a.join(MINUTES), "# 纪要标题\n").unwrap();
        let v = s.update_meta("2026-10-01_0900_周会", &json!({"title": " 新标题 ", "tags": ["项目A", "项目A", " "]})).unwrap();
        assert_eq!((v["title"].clone(), v["tags"].clone(), v["archived"].clone()), (json!("新标题"), json!(["项目A"]), json!(false)));
        let v = s.update_meta("2026-10-01_0900_周会", &json!({"archived": true})).unwrap();
        assert_eq!((v["title"].clone(), v["tags"].clone(), v["archived"].clone()), (json!("新标题"), json!(["项目A"]), json!(true)));
        // null / 空串恢复默认标题，meta.json 里不留 title
        let v = s.update_meta("2026-10-01_0900_周会", &json!({"title": null})).unwrap();
        assert_eq!(v["title"], "纪要标题");
        assert!(!std::fs::read_to_string(a.join(meetings::META)).unwrap().contains("title"));
        s.update_meta("2026-10-01_0900_周会", &json!({"title": "x"})).unwrap();
        assert_eq!(s.update_meta("2026-10-01_0900_周会", &json!({"title": ""})).unwrap()["title"], "纪要标题");
        assert_eq!(status(s.update_meta("2026-10-01_0900_周会", &json!({"tags": "a"}))), StatusCode::BAD_REQUEST);
        assert_eq!(status(s.update_meta("2026-10-01_0900_周会", &json!({"archived": 1}))), StatusCode::BAD_REQUEST);
        assert_eq!(status(s.update_meta("nope", &json!({}))), StatusCode::NOT_FOUND);
        assert_eq!(status(s.update_meta("../root", &json!({}))), StatusCode::NOT_FOUND);
        // 正在处理：409，meta.json 不动
        mark_busy(&s, "2026-10-01_0900_周会");
        assert_eq!(status(s.update_meta("2026-10-01_0900_周会", &json!({"title": "y"}))), StatusCode::CONFLICT);
        s.jobs.lock().unwrap().clear();
        // 详情也带 tags、archived、size_bytes
        let d = meeting_json(&a);
        assert_eq!((d["tags"].clone(), d["archived"].clone()), (json!(["项目A"]), json!(true)));
        assert!(d["size_bytes"].as_u64().unwrap() > 0);
        std::fs::remove_dir_all(root.parent().unwrap()).ok();
    }

    #[cfg(unix)]
    #[test]
    fn delete_meeting_and_audio() {
        let (s, root, trash) = test_state("delete");
        let a = meeting(&root, "2026-10-01_0900_a", &[("录音.m4a", 100), ("system.wav", 30), ("mic.wav", 20), (MINUTES, 5), ("a.pdf", 5)]);
        let freed = s.delete_audio("2026-10-01_0900_a", true).unwrap();
        assert_eq!(freed, 150);
        let mut left: Vec<String> =
            std::fs::read_dir(&a).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, vec!["a.pdf", TRANSCRIPT_JSON, MINUTES]);
        assert_eq!(std::fs::read_dir(&trash).unwrap().count(), 3); // 进了废纸篓，不是删掉
        // 没有录音：单个删报 400
        assert_eq!(status(s.delete_audio("2026-10-01_0900_a", true)), StatusCode::BAD_REQUEST);
        // 正在处理：409
        meeting(&root, "2026-10-02_0900_b", &[("录音.m4a", 10)]);
        mark_busy(&s, "2026-10-02_0900_b");
        assert_eq!(status(s.delete_meeting("2026-10-02_0900_b")), StatusCode::CONFLICT);
        assert_eq!(status(s.delete_audio("2026-10-02_0900_b", true)), StatusCode::CONFLICT);
        // 模型下载任务不算占用会议
        meeting(&root, "2026-10-03_0900_c", &[("录音.m4a", 10)]);
        s.jobs.lock().unwrap().push(Job::new("models", "2026-10-03_0900_c", "downloading"));
        s.delete_meeting("2026-10-03_0900_c").unwrap();
        assert!(!root.join("2026-10-03_0900_c").exists());
        s.delete_meeting("2026-10-01_0900_a").unwrap();
        assert!(!a.exists());
        assert_eq!(status(s.delete_meeting("2026-10-01_0900_a")), StatusCode::NOT_FOUND);
        // 废纸篓失败：500，文件还在
        let f = meeting(&root, "2026-10-04_0900_FAIL", &[("录音.m4a", 10)]);
        assert_eq!(status(s.delete_meeting("2026-10-04_0900_FAIL")), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(f.exists());
        std::fs::remove_dir_all(root.parent().unwrap()).ok();
    }

    #[cfg(unix)]
    #[test]
    fn batch_actions() {
        let (s, root, _) = test_state("batch");
        let ids = ["2026-10-01_0900_a", "2026-10-02_0900_b", "2026-10-03_0900_c"];
        meeting(&root, ids[0], &[("录音.m4a", 100)]);
        meeting(&root, ids[1], &[]);
        meeting(&root, ids[2], &[("录音.wav", 50), ("mic.wav", 5)]);
        let req = |ids: &[&str], action: &str, tag: Option<&str>| BatchReq {
            ids: ids.iter().map(|s| s.to_string()).collect(),
            action: action.into(),
            tag: tag.map(String::from),
        };
        // 校验
        assert_eq!(status(s.batch(&req(&[], "archive", None))), StatusCode::BAD_REQUEST);
        assert_eq!(status(s.batch(&req(&ids, "explode", None))), StatusCode::BAD_REQUEST);
        assert_eq!(status(s.batch(&req(&ids, "tag_add", None))), StatusCode::BAD_REQUEST);
        assert_eq!(status(s.batch(&req(&ids, "tag_add", Some("  ")))), StatusCode::BAD_REQUEST);
        // 归档：不存在的那个进 failed，其他照样成功；重复的 id 只算一次
        let v = s.batch(&req(&[ids[0], "nope", ids[1], ids[0]], "archive", None)).unwrap();
        assert_eq!(v["ok"], 2);
        assert_eq!(v["failed"].as_array().unwrap().len(), 1);
        assert_eq!(v["failed"][0]["id"], "nope");
        assert!(meetings::read_meta(&root.join(ids[0])).archived && meetings::read_meta(&root.join(ids[1])).archived);
        s.batch(&req(&[ids[1]], "unarchive", None)).unwrap();
        assert!(!meetings::read_meta(&root.join(ids[1])).archived);
        // 标签
        s.batch(&req(&ids, "tag_add", Some(" 周会 "))).unwrap();
        s.batch(&req(&ids[..2], "tag_add", Some("项目A"))).unwrap();
        s.batch(&req(&ids, "tag_add", Some("周会"))).unwrap(); // 已经有了不重复加
        assert_eq!(meetings::read_meta(&root.join(ids[0])).tags, vec!["周会", "项目A"]);
        s.batch(&req(&ids, "tag_remove", Some("周会"))).unwrap();
        assert_eq!(meetings::read_meta(&root.join(ids[0])).tags, vec!["项目A"]);
        assert!(meetings::read_meta(&root.join(ids[2])).tags.is_empty());
        // 只删录音：没录音的算成功、freed 0；正在处理的进 failed
        mark_busy(&s, ids[0]);
        let v = s.batch(&req(&ids, "delete_audio", None)).unwrap();
        assert_eq!((v["ok"].clone(), v["freed_bytes"].clone()), (json!(2), json!(55)));
        assert_eq!(v["failed"], json!([{"id": ids[0], "error": busy_msg()}]));
        assert!(root.join(ids[0]).join("录音.m4a").exists());
        assert!(!root.join(ids[2]).join("录音.wav").exists() && root.join(ids[2]).join(TRANSCRIPT_JSON).exists());
        // 删除整场：正在处理的、不存在的进 failed
        let v = s.batch(&req(&[ids[0], ids[1], "nope"], "delete", None)).unwrap();
        assert_eq!(v["ok"], 1);
        assert_eq!(v["failed"].as_array().unwrap().iter().map(|f| f["id"].as_str().unwrap()).collect::<Vec<_>>(), vec![ids[0], "nope"]);
        assert!(v["freed_bytes"].as_u64().unwrap() > 0);
        assert!(root.join(ids[0]).exists() && !root.join(ids[1]).exists());
        std::fs::remove_dir_all(root.parent().unwrap()).ok();
    }

    #[test]
    fn storage_endpoint() {
        let (s, root, _) = test_state("storage");
        meeting(&root, "2026-10-01_0900_a", &[("录音.m4a", 100), ("system.wav", 30)]);
        let v = s.storage();
        assert_eq!(v["count"], 1);
        assert_eq!((v["audio_bytes"].clone(), v["leftover_bytes"].clone()), (json!(130), json!(30)));
        assert_eq!(v["models_bytes"], 123);
        assert!(v["free_bytes"].as_u64().is_some_and(|n| n > 0));
        assert_eq!(v["root"], json!(root));
        std::fs::remove_dir_all(root.parent().unwrap()).ok();
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_leftovers_and_old_audio() {
        let (s, root, _) = test_state("cleanup");
        // 校验
        for body in [
            json!({"what": "nope"}),
            json!({"what": "audio"}),
            json!({"what": "audio", "older_than_days": -1}),
            json!({"what": "audio", "older_than_days": 3651}),
            json!({"what": "audio", "older_than_days": 1.5}),
            json!({"what": "audio", "older_than_days": "30"}),
            json!({"what": "audio", "older_than_days": 30, "archived_only": "yes"}),
        ] {
            assert_eq!(status(s.cleanup(&body)), StatusCode::BAD_REQUEST, "{body}");
        }
        let today = chrono::Local::now().date_naive();
        let id = |days: u64, name: &str| format!("{}_0900_{name}", (today - chrono::Days::new(days)).format("%Y-%m-%d"));
        let (old, old_arch, recent, busy) = (id(40, "old"), id(50, "arch"), id(5, "recent"), id(60, "busy"));
        meeting(&root, &old, &[("录音.m4a", 100), ("system.wav", 10), ("x.part", 1)]);
        meeting(&root, &old_arch, &[("录音.m4a", 200)]);
        meeting(&root, &recent, &[("录音.m4a", 300), ("mic.wav", 20)]);
        meeting(&root, &busy, &[("录音.m4a", 400), ("mic.wav", 40)]);
        s.update_meta(&old_arch, &json!({"archived": true})).unwrap();
        mark_busy(&s, &busy);
        // 残留：old 的 system.wav + x.part、recent 的 mic.wav；busy 的跳过
        let v = s.cleanup(&json!({"what": "leftovers"})).unwrap();
        assert_eq!(v, json!({"ok": true, "count": 3, "freed_bytes": 31}));
        assert!(root.join(&old).join("录音.m4a").exists() && !root.join(&old).join("system.wav").exists());
        assert!(root.join(&busy).join("mic.wav").exists());
        // 30 天前、只归档的
        let v = s.cleanup(&json!({"what": "audio", "older_than_days": 30, "archived_only": true})).unwrap();
        assert_eq!(v, json!({"ok": true, "count": 1, "freed_bytes": 200}));
        // 30 天前的全部（busy 跳过）
        let v = s.cleanup(&json!({"what": "audio", "older_than_days": 30})).unwrap();
        assert_eq!(v, json!({"ok": true, "count": 1, "freed_bytes": 100}));
        assert!(root.join(&recent).join("录音.m4a").exists() && root.join(&old).join(TRANSCRIPT_JSON).exists());
        // 0 = 全部
        let v = s.cleanup(&json!({"what": "audio", "older_than_days": 0, "archived_only": false})).unwrap();
        assert_eq!(v, json!({"ok": true, "count": 1, "freed_bytes": 300}));
        assert!(root.join(&busy).join("录音.m4a").exists());
        std::fs::remove_dir_all(root.parent().unwrap()).ok();
    }

    #[test]
    fn demo_folder_names() {
        assert_eq!(demo_folder(false), "2026-10-28_1400_示例·注册转化复盘");
        assert_eq!(demo_folder(true), "2026-10-28_1400_Sample_·_Signup_conversion_review");
    }
}
