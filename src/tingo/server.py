"""图形界面的本地服务：只监听 127.0.0.1，前端在 web/ 目录。

录音和后台任务（转写、生成纪要）可以同时进行：录音同时只有一场，同一场会议同时只有一个任务，
前端轮询 /api/jobs 拿所有任务的进度。
"""

from __future__ import annotations

import itertools
import math
import os
import platform
import re
import shutil
import subprocess
import tempfile
import threading
import time
from dataclasses import asdict, dataclass, field
from datetime import date, datetime, timedelta
from pathlib import Path
from typing import Annotated, Any

from fastapi import FastAPI, File, Form, HTTPException, UploadFile
from fastapi.responses import FileResponse, HTMLResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel

from . import __version__, config, i18n, meetings, minutes_history, terms
from .log import get as get_logger
from .meetings import AUDIO, MINUTES, TRANSCRIPT_JSON
from .summarize import anthropic_models, is_ollama
from .transcript import Transcript, fmt_ts

WEB_DIR = Path(__file__).with_name("web")
SAMPLE_DIR = Path(__file__).with_name("sample")  # 内置示例会议（包数据）
TEXTUTIL = "/usr/bin/textutil"  # macOS 自带，HTML → Word

# 内置示例会议的文件夹（会议时间 + 标题，标题按界面语言）
DEMO_WHEN = "2026-10-28_1400"
DEMO_TITLE = {"zh": "示例·注册转化复盘", "en": "Sample · Signup conversion review"}

# 本机常见的 OpenAI 兼容中转。recommended 是实测过、又快又好的纪要模型（76 分钟会议 51 秒出纪要）
LOCAL_PROVIDERS = [
    {
        "id": "magpie",
        "name": "Magpie",
        "base_url": "http://127.0.0.1:3425/v1",
        "api_key": "magpie",
        "recommended": "deepseek/deepseek-v4-flash",
        "hint": "Magpie 没有在运行：打开 Magpie 应用后再试",
    },
    {
        "id": "ccswitch",
        "name": "CC Switch",
        "base_url": "http://127.0.0.1:15721/v1",
        "api_key": "cc-switch",
        "recommended": "",
        "hint": "CC Switch 的本地代理没有打开：在 CC Switch 设置里开启「本地代理」（默认端口 15721）",
    },
    {
        "id": "ollama",
        "name": "本地 Ollama",
        "base_url": "http://localhost:11434/v1",
        "api_key": "",
        "recommended": "",
        "hint": "Ollama 没有在运行，或者还没有下载模型",
    },
]

# 界面是英文时替换上面的 name / hint
LOCAL_PROVIDERS_EN = {
    "magpie": {"hint": "Magpie isn't running: open the Magpie app and try again"},
    "ccswitch": {
        "hint": 'CC Switch\'s local proxy is off: enable "Local Proxy" in CC Switch settings (default port 15721)'
    },
    "ollama": {"name": "Local Ollama", "hint": "Ollama isn't running, or no model has been downloaded yet"},
}


def _list_models(base_url: str, api_key: str, api: str = "openai") -> list[str] | None:
    """GET /v1/models；连不上返回 None。api = anthropic 时按 Anthropic 的鉴权头和 /v1 规则。"""
    import httpx

    try:
        if api == "anthropic":
            return anthropic_models(base_url, api_key, timeout=10)
        r = httpx.get(
            base_url.rstrip("/") + "/models", headers={"Authorization": f"Bearer {api_key or 'none'}"}, timeout=2
        )
        if r.status_code != 200:
            return None
        return sorted(m["id"] for m in r.json().get("data", []) if m.get("id"))
    except Exception:  # noqa: BLE001
        return None


log = get_logger()


def _launch(args: list[str]) -> None:
    """后台打开文件 / 链接，不等它结束。"""
    subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def _open(target: str, reveal: bool = False) -> None:
    """用系统默认程序打开；reveal=True 时在访达里选中这个文件。"""
    if platform.system() == "Darwin":
        _launch(["open", "-R", target] if reveal else ["open", target])
    elif platform.system() == "Windows":
        os.startfile(str(Path(target).parent) if reveal else target)
    else:
        _launch(["xdg-open", str(Path(target).parent) if reveal else target])


_job_ids = itertools.count(1)


def _next_job_id() -> str:
    return f"j{next(_job_ids)}"


KEEP_FINISHED = 5  # 结束了的任务只留最近几个，给界面显示结果
SEARCH_HITS = 5  # 搜索：每场会议最多返回几句
SEARCH_MEETINGS = 30  # 搜索：最多返回几场会议


@dataclass
class Job:
    kind: str  # record | process | summarize | models
    meeting: str
    state: str  # recording | saving | finalizing | loading | transcribing | summarizing | done | error
    started_at: float = field(default_factory=time.time)
    error: str = ""
    minutes: str = ""  # 生成中的纪要，流式累加
    stage: str = ""
    sources: list[str] = field(default_factory=list)
    notice: str = ""  # 完成后给用户的附加提示（比如"已存到 Obsidian"）
    # 任务编号：录音和后台任务可以同时进行，界面按编号区分
    id: str = field(default_factory=_next_job_id)
    thinking: int = 0  # 推理模型已经思考了多少字（还没开始写纪要时显示）
    finished_at: float = 0.0  # 结束时间；结束后用时不再增长

    @property
    def busy(self) -> bool:
        return self.state not in ("done", "error")


class RecordRequest(BaseModel):
    title: str = "会议"
    system: bool = True
    mic: bool = True
    hotwords: list[str] = []


class SpeakersRequest(BaseModel):
    names: list[str]


class MinutesRequest(BaseModel):
    text: str


class ObsidianRequest(BaseModel):
    vault: str
    folder: str
    auto: bool
    transcript: bool


class ConfigRequest(BaseModel):
    base_url: str
    model: str
    api_key: str | None = None  # None 表示不改
    api: str | None = None  # openai | anthropic，None 表示不改
    temperature: Any = None  # 0～2 的数，空字符串 = 恢复默认，None 表示不改；类型自己检查（不对时回 400）
    hotwords: list[str] = []
    webhook: str = ""
    secret: str | None = None
    obsidian: ObsidianRequest | None = None  # None 表示不改
    ui_language: str | None = None  # auto | zh | en，None 表示不改
    minutes_language: str | None = None  # auto | zh | en
    asr_language: str | None = None  # zh | en | auto
    # 自动结束录音，None 表示不改；类型自己检查（不对时回 400，不是 422）
    auto_stop_silence_min: Any = None  # 0～240 的整数，0 = 不自动结束
    auto_stop_app_quit: Any = None  # 布尔


AUDIO_MIME = {
    ".m4a": "audio/mp4",
    ".mp4": "audio/mp4",
    ".aac": "audio/mp4",
    ".mp3": "audio/mpeg",
    ".wav": "audio/wav",
    ".flac": "audio/flac",
    ".ogg": "audio/ogg",
    ".opus": "audio/ogg",
    ".mov": "video/quicktime",
}


class PdfRequest(BaseModel):
    html: str


class OpenRequest(BaseModel):
    open: bool = False


class RecordTestRequest(BaseModel):
    seconds: Any = 3  # 1～10


class MetaRequest(BaseModel):
    """字段都可选，没传的不改；title 为 null 或空串 = 恢复默认标题。类型自己检查（不对时回 400，不是 422）"""

    title: Any = None
    tags: Any = None
    archived: Any = None


class BatchRequest(BaseModel):
    ids: Any = None
    action: Any = None
    tag: Any = None


class CleanupRequest(BaseModel):
    what: Any = None  # leftovers | audio
    older_than_days: Any = None  # 0～3650 的整数，0 = 全部
    archived_only: Any = False


BATCH_ACTIONS = ("archive", "unarchive", "tag_add", "tag_remove", "delete", "delete_audio")
CLEANUP_DAYS_MAX = 3650
STORAGE_LARGEST = 10  # 存储空间页列出最大的几场会议


class TrashUnsupported(RuntimeError):
    pass


def _trash(path: Path) -> None:
    """移到废纸篓（不是彻底删除）：macOS 用录音组件的 trash 子命令，Linux 有 gio 就用 gio trash。"""
    if platform.system() == "Darwin":
        from . import record

        binary = record.ensure_recorder()
        res = subprocess.run([str(binary), "trash", str(path)], check=False, capture_output=True, text=True, timeout=60)
        if res.returncode != 0 or "OK" not in res.stdout.split():
            err = next((line[7:] for line in res.stdout.splitlines() if line.startswith("ERROR: ")), "")
            raise RuntimeError(err or i18n.tr("移到废纸篓失败", "Couldn't move it to the Trash"))
        return
    gio = shutil.which("gio") if platform.system() == "Linux" else None
    if gio is None:
        raise TrashUnsupported(
            i18n.tr("这个系统上没法移到废纸篓", "Moving to the Trash isn't supported on this system")
        )
    res = subprocess.run([gio, "trash", str(path)], check=False, capture_output=True, text=True, timeout=60)
    if res.returncode != 0:
        raise RuntimeError(res.stderr.strip() or i18n.tr("移到废纸篓失败", "Couldn't move it to the Trash"))


def _is_number(v: object) -> bool:
    return isinstance(v, int | float) and not isinstance(v, bool)


_CODE_BLOCK = re.compile(r"```.*?(?:\n```[ \t]*(?=\n|\Z)|\Z)", re.DOTALL)


def _norm(s: str) -> str:
    """搜索词比较：忽略大小写和空白"""
    return "".join(s.split()).lower()


def _fuzzy_find(text: str, q: str) -> str | None:
    """text 里和 q 等长、只差一个字符的片段（都已小写）；没有返回 None。"""
    n = len(q)
    for i in range(len(text) - n + 1):
        diff = 0
        for a, b in zip(text[i : i + n], q):
            if a != b:
                diff += 1
                if diff > 1:
                    break
        if diff == 1:
            return text[i : i + n]
    return None


FUZZY_MIN_CHARS = 4  # 查询词至少这么多字才容许错一个字（3 个字时误命中太多，如"验证码"→"验证手"）


class _Matcher:
    """exact：命中查询集合任一项（忽略大小写）；fuzzy：q 有 FUZZY_MIN_CHARS 个字以上时，和 q 等长、只差一个字符。
    都返回命中的那段原文，没命中返回 None。"""

    def __init__(self, needles: list[str], q: str):
        self.lowered = [x.lower() for x in needles if x]
        self.q = q.lower() if len(q) >= FUZZY_MIN_CHARS else ""

    @staticmethod
    def _original(text: str, low: str, hit: str) -> str:
        if len(low) != len(text):  # 少数字符小写后长度会变，这时退回小写的命中词
            return hit
        at = low.find(hit)
        return text[at : at + len(hit)]

    def exact(self, text: str) -> str | None:
        low = text.lower()
        for x in self.lowered:
            if x in low:
                return self._original(text, low, x)
        return None

    def fuzzy(self, text: str) -> str | None:
        if not self.q:
            return None
        low = text.lower()
        hit = _fuzzy_find(low, self.q)
        return None if hit is None else self._original(text, low, hit)

    def any(self, text: str) -> str | None:
        return self.exact(text) or self.fuzzy(text)


class LLMTestRequest(BaseModel):
    base_url: str
    model: str
    api_key: str | None = None
    api: str | None = None  # openai | anthropic；不传时见 _request_api


def _request_api(req: LLMTestRequest) -> str:
    """测试连接 / 列模型用哪种协议：请求里给了就用它；没给时，地址和已保存的一样就沿用已保存的，否则按 openai。"""
    if req.api is not None:
        return config.normalize_api(req.api)
    saved = config.load().llm
    return saved.api if req.base_url.rstrip("/") == saved.base_url.rstrip("/") else "openai"


def create_app(root: Path | None = None) -> FastAPI:
    root = (root or Path(config.env_var("ROOT") or meetings.DEFAULT_ROOT)).expanduser()
    app = FastAPI(title="tingo", docs_url=None, redoc_url=None)
    lock = threading.Lock()  # 保护 jobs 列表和录音状态
    jobs: list[Job] = []  # 进行中和刚结束（还没被界面收起）的任务，按开始先后
    # 正在进行的那一场录音；auto 是自动结束的判断（record.AutoStop），watch_stop 让检查线程退出
    state: dict = {"recording": None, "live": None, "hotwords": [], "auto": None, "watch_stop": None, "testing": False}

    def folder(meeting_id: str) -> Path:
        path = (root / meeting_id).resolve()
        if path.parent != root.resolve() or not path.is_dir():
            raise HTTPException(404, i18n.tr("找不到这场会议", "Meeting not found"))
        return path

    def conflict(new: Job, j: Job) -> str | None:
        if new.kind == "record" and j.state == "recording":
            return i18n.tr("已经在录音了，先结束这一场", "Already recording; stop the current recording first")
        if new.kind == "models" and j.kind == "models":
            return i18n.tr("已经在下载模型了，等它下完", "Models are already downloading; wait for it to finish")
        if new.meeting and new.kind != "models" and j.kind != "models" and j.meeting == new.meeting:
            return i18n.tr("这场会议正在处理，等它完成", "This meeting is being processed; wait for it to finish")
        return None

    def busy_msg() -> str:
        return i18n.tr("这场会议正在处理，等它完成", "This meeting is being processed; wait for it to finish")

    def is_busy(meeting_id: str) -> bool:
        """这场会议有进行中的任务（录音、转写、生成纪要）"""
        with lock:
            return any(j.busy and j.kind != "models" and j.meeting == meeting_id for j in jobs)

    def trash(path: Path) -> None:
        """移到废纸篓；失败时 500，错误信息原样给用户。"""
        try:
            _trash(path)
        except (RuntimeError, OSError, subprocess.SubprocessError) as e:
            raise HTTPException(500, str(e) or e.__class__.__name__) from e

    def trash_meeting(meeting_id: str) -> None:
        path = folder(meeting_id)
        if is_busy(meeting_id):
            raise HTTPException(409, busy_msg())
        trash(path)
        log.info("%s 已移到废纸篓", meeting_id)

    def trash_audio(meeting_id: str, missing_ok: bool) -> int:
        """只把录音（录音.*、system.wav、mic.wav）移到废纸篓，返回腾出的字节数。"""
        path = folder(meeting_id)
        if is_busy(meeting_id):
            raise HTTPException(409, busy_msg())
        files = meetings.audio_files(path)
        if not files:
            if missing_ok:
                return 0
            raise HTTPException(400, i18n.tr("这场会议没有录音文件", "This meeting has no recording"))
        freed = 0
        for f in files:
            size = meetings.file_size(f)
            trash(f)
            freed += size
        log.info("%s 录音已移到废纸篓（%d 字节）", meeting_id, freed)
        return freed

    def start_job(job: Job) -> None:
        """登记一个任务。录音同时只能有一场；同一场会议、模型下载各自同时只能有一个任务；其余可以并行
        （转写共用一个识别模型，在 asr.infer_lock 上排队；生成纪要主要等网络）。"""
        with lock:
            for j in jobs:
                if j.busy and (msg := conflict(job, j)):
                    raise HTTPException(409, msg)
            finished = [j for j in jobs if not j.busy]
            if len(finished) > KEEP_FINISHED:
                drop = {j.id for j in finished[: len(finished) - KEEP_FINISHED]}
                jobs[:] = [j for j in jobs if j.id not in drop]
            jobs.append(job)

    def finish(job: Job) -> None:
        with lock:
            job.finished_at = time.time()
            job.state = "done"

    def fail(job: Job, e: Exception) -> None:
        with lock:
            job.finished_at = time.time()
            job.error = str(e) or e.__class__.__name__
            job.state = "error"
        log.error("%s %s 失败：%s", job.kind, job.meeting, job.error, exc_info=e)

    def job_json(job: Job) -> dict:
        """调用方持有 lock。录音中的任务另带电平和实时文字（见 snapshot）。"""
        data = asdict(job)
        data["elapsed"] = (job.finished_at or time.time()) - job.started_at
        return data

    def add_notice(job: Job, msg: str) -> None:
        """追加给用户的提示（自动结束录音的提示之后还可能有"已同步到 Obsidian"）"""
        job.notice = f"{job.notice}{i18n.tr('；', '; ')}{msg}" if job.notice else msg

    # ---------- 处理流程 ----------

    def run_summarize(job: Job, tr: Transcript, out: Path) -> None:
        from .summarize import summarize

        cfg = config.load()
        job.state = "summarizing"

        def on_token(t: str) -> None:
            job.minutes += t

        def on_stage(s: str) -> None:
            job.stage = s
            job.minutes = ""
            job.thinking = 0

        def on_think(n: int) -> None:
            job.thinking = n

        t = time.time()
        text = summarize(
            tr,
            cfg.llm,
            meeting_date=meetings.meeting_date(out),
            on_token=on_token,
            on_stage=on_stage,
            on_think=on_think,
            language=cfg.minutes_language,
        )
        minutes_history.save(out, text)
        job.minutes = text
        log.info("%s 纪要完成：模型 %s，%.0f 秒", out.name, cfg.llm.model, time.time() - t)
        if cfg.obsidian.auto and cfg.obsidian.vault.strip():
            # 自动存进 Obsidian；失败不影响纪要本身，只提示一下
            from . import obsidian

            try:
                obsidian.export(cfg.obsidian, out)
                add_notice(job, i18n.tr("纪要已同步到 Obsidian", "Minutes synced to Obsidian"))
            except Exception as e:  # noqa: BLE001
                add_notice(job, i18n.tr(f"没能同步到 Obsidian：{e}", f"Couldn't sync to Obsidian: {e}"))
                log.warning("%s 同步到 Obsidian 失败：%s", out.name, e)

    def run_process(job: Job, audio: Path, out: Path, hotwords: list[str]) -> None:
        from . import asr
        from .live import transcribe_file

        try:
            job.state = "loading"
            asr._load_model()
            job.state = "transcribing"
            t = time.time()
            tr = transcribe_file(
                audio,
                hotwords=config.load().hotwords + hotwords,
                on_progress=lambda f: setattr(job, "stage", f"{f:.0%}"),
            )
            job.stage = ""
            log.info("%s 文件转写完成：%s，%.0f 秒", out.name, fmt_ts(tr.duration_ms), time.time() - t)
            if not tr.sentences:
                raise RuntimeError(i18n.tr("没有识别出任何语音", "No speech was recognized"))
            meetings.save_transcript(tr, out)
            run_summarize(job, tr, out)
            finish(job)
        except Exception as e:  # noqa: BLE001 - 错误原样显示给用户
            fail(job, e)

    # ---------- 环境与配置 ----------

    @app.get("/api/env")
    def env() -> dict:
        from . import asr
        from .record import system_audio_supported

        cfg = config.load()
        cache = Path(os.environ.get("MODELSCOPE_CACHE", Path.home() / ".cache" / "modelscope"))
        return {
            "version": __version__,
            "root": str(root),
            "ffmpeg": shutil.which("ffmpeg") is not None,
            "system_audio": system_audio_supported(),
            "model_cached": asr.model_loaded() or any(cache.glob("**/*speech_seaco_paraformer*")),
            "model_loaded": asr.model_loaded(),
            "llm_ready": bool(cfg.llm.api_key)
            or (cfg.llm.api != "anthropic" and is_ollama(cfg.llm.base_url, cfg.llm.model)),
            "llm_model": cfg.llm.model,
        }

    @app.get("/api/config")
    def get_config() -> dict:
        cfg = config.load()
        key = cfg.llm.api_key
        return {
            "base_url": cfg.llm.base_url,
            "model": cfg.llm.model,
            "api": cfg.llm.api,
            "temperature": cfg.llm.temperature,
            "api_key_set": bool(key),
            "api_key_hint": f"{key[:3]}…{key[-4:]}" if len(key) > 10 else (i18n.tr("已设置", "set") if key else ""),
            "hotwords": cfg.hotwords,
            "webhook": cfg.webhook,
            "secret_set": bool(cfg.webhook_secret),
            "obsidian": asdict(cfg.obsidian),
            "ui_language": cfg.ui_language,
            "minutes_language": cfg.minutes_language,
            "asr_language": cfg.asr_language,
            "auto_stop_silence_min": cfg.record.auto_stop_silence_min,
            "auto_stop_app_quit": cfg.record.auto_stop_app_quit,
            "path": str(config.CONFIG_PATH),
        }

    @app.put("/api/config")
    def put_config(req: ConfigRequest) -> dict:
        cfg = config.load()
        cfg.llm.base_url, cfg.llm.model = req.base_url.strip(), req.model.strip()
        if req.api_key is not None:
            cfg.llm.api_key = req.api_key.strip()
        if req.api is not None:
            api = req.api.strip().lower()
            if api not in config.LLM_APIS:
                raise HTTPException(400, i18n.tr(f"不认识的接口协议：{req.api}", f"Unknown API protocol: {req.api}"))
            cfg.llm.api = api
        if req.temperature is not None:
            t = req.temperature
            if isinstance(t, str):
                try:
                    t = float(t) if t.strip() else None
                except ValueError:
                    t = math.nan
            if t is not None and not config.valid_temperature(t):
                raise HTTPException(
                    400,
                    i18n.tr(
                        "温度要填 0～2 之间的数，留空用默认的 0.2",
                        "Temperature must be a number from 0 to 2; leave it empty for the default 0.2",
                    ),
                )
            cfg.llm.temperature = None if t is None else float(t)
        cfg.hotwords = [w.strip() for w in req.hotwords if w.strip()]
        cfg.webhook = req.webhook.strip()
        if req.secret is not None:
            cfg.webhook_secret = req.secret.strip()
        if req.obsidian is not None:
            o = req.obsidian
            vault = o.vault.strip()
            home = os.environ.get("HOME")
            if vault.startswith("~/") and home is not None:
                vault = f"{home}/{vault[2:]}"  # 设置页里可以填 ~/…
            cfg.obsidian = config.ObsidianConfig(vault, o.folder.strip().strip("/"), o.auto, o.transcript)
        for value, allowed in (
            (req.ui_language, config.UI_LANGUAGES),
            (req.minutes_language, config.MINUTES_LANGUAGES),
            (req.asr_language, config.ASR_LANGUAGES),
        ):
            if value is not None and value not in allowed:
                raise HTTPException(
                    400,
                    i18n.tr(f"不支持的语言：{value}（可选 {'、'.join(allowed)}）", f"Unsupported language: {value}"),
                )
        if req.ui_language is not None:
            cfg.ui_language = req.ui_language
        if req.minutes_language is not None:
            cfg.minutes_language = req.minutes_language
        if req.asr_language is not None:
            cfg.asr_language = req.asr_language
        if req.auto_stop_silence_min is not None:
            if not config.valid_silence_min(req.auto_stop_silence_min):
                raise HTTPException(
                    400,
                    i18n.tr(
                        f"自动结束录音的分钟数只能是 0～{config.SILENCE_MIN_MAX} 的整数",
                        f"The auto-stop minutes must be a whole number from 0 to {config.SILENCE_MIN_MAX}",
                    ),
                )
            cfg.record.auto_stop_silence_min = req.auto_stop_silence_min
        if req.auto_stop_app_quit is not None:
            if not isinstance(req.auto_stop_app_quit, bool):
                raise HTTPException(
                    400, i18n.tr("auto_stop_app_quit 只能是 true 或 false", "auto_stop_app_quit must be true or false")
                )
            cfg.record.auto_stop_app_quit = req.auto_stop_app_quit
        config.save(cfg)
        return get_config()

    @app.get("/api/llm/providers")
    def llm_providers() -> list[dict]:
        """本机常见的模型中转是否在跑、有哪些模型。设置页用来一键选择。"""
        from concurrent.futures import ThreadPoolExecutor

        with ThreadPoolExecutor(len(LOCAL_PROVIDERS)) as pool:
            found = list(pool.map(lambda p: _list_models(p["base_url"], p["api_key"]), LOCAL_PROVIDERS))
        en = i18n.is_en()
        return [
            {
                **p,
                **(LOCAL_PROVIDERS_EN.get(p["id"], {}) if en else {}),
                "running": models is not None,
                "models": models or [],
            }
            for p, models in zip(LOCAL_PROVIDERS, found, strict=True)
        ]

    @app.post("/api/llm/models")
    def llm_models(req: LLMTestRequest) -> dict:
        key = req.api_key if req.api_key is not None else config.load().llm.api_key
        models = _list_models(req.base_url, key or "none", _request_api(req))
        return {"ok": models is not None, "models": models or []}

    @app.post("/api/config/test")
    def test_llm(req: LLMTestRequest) -> dict:
        from openai import OpenAI

        key = req.api_key if req.api_key is not None else config.load().llm.api_key
        try:
            if _request_api(req) == "anthropic":
                ids = anthropic_models(req.base_url, key or "", timeout=10)
            else:
                client = OpenAI(base_url=req.base_url, api_key=key or "ollama", timeout=15)
                ids = [m.id for m in client.models.list().data]
        except Exception as e:  # noqa: BLE001
            return {"ok": False, "message": i18n.tr(f"连不上：{e}", f"Couldn't connect: {e}")}
        # Ollama 列出的模型名带 :latest
        if ids and req.model not in ids and f"{req.model}:latest" not in ids:
            return {
                "ok": False,
                "message": i18n.tr(
                    f"连上了，但没有模型 {req.model}。可用：{'、'.join(ids[:8])}",
                    f"Connected, but there is no model {req.model}. Available: {', '.join(ids[:8])}",
                ),
            }
        return {"ok": True, "message": i18n.tr("连接正常", "Connection OK")}

    def export_stem(path: Path) -> str:
        """导出文件的文件名（不含扩展名）：纪要标题，没有纪要时用文件夹里的会议名。"""
        minutes = (path / MINUTES).read_text(encoding="utf-8") if (path / MINUTES).exists() else ""
        from .obsidian import safe_name

        title = meetings.minutes_title(minutes) or (path.name.split("_", 2)[-1] or path.name)
        return safe_name(title) or safe_name(path.name)

    # ---------- 会议 ----------

    @app.get("/api/meetings")
    def list_meetings() -> list[dict]:
        return meetings.list_meetings(root)

    @app.get("/api/search")
    def search(q: str = "") -> dict:
        """跨会议搜索转写句子和纪要正文：按各场纪要的术语表扩展查询词，4 个字以上的查询词容许错一个字。
        每场会议先列精确命中的句子，名额（5 句）没满再补只是模糊命中的。"""
        query = q.strip()
        if not query:
            raise HTTPException(400, i18n.tr("请输入要搜索的内容", "Enter something to search for"))
        loaded = []
        for info in meetings.list_meetings(root):  # 按会议时间倒序
            p = root / info["id"]
            try:
                minutes = (p / MINUTES).read_text(encoding="utf-8") if (p / MINUTES).exists() else ""
            except OSError:
                minutes = ""
            loaded.append((info, p, minutes))
        nq = _norm(query)
        expanded: list[str] = []
        for _, _, minutes in loaded:
            for term, variants in terms.parse(minutes):
                group = [term, *variants]
                if any(_norm(x) == nq for x in group):
                    expanded += [x for x in group if x.lower() not in {e.lower() for e in expanded}]
        if query.lower() not in {e.lower() for e in expanded}:
            expanded.append(query)
        matcher = _Matcher(expanded, query)
        en = i18n.is_en()
        results = []
        for info, p, minutes in loaded:
            hits: list[dict] = []
            if info["has_transcript"]:
                try:
                    tr = Transcript.load(p / TRANSCRIPT_JSON)
                except Exception:  # noqa: BLE001 - 坏掉的转写不影响搜别的会议
                    tr = None
                sentences = tr.sentences if tr else []
                taken: set[int] = set()
                # 先放精确命中的句子，名额没满再补只是模糊命中的
                for how in (matcher.exact, matcher.fuzzy):
                    for k, s in enumerate(sentences):
                        if len(hits) >= SEARCH_HITS:
                            break
                        if k in taken or (match := how(s.text)) is None:
                            continue
                        taken.add(k)
                        hits.append(
                            {"ms": s.start_ms, "speaker": tr.name_of(s.speaker, en), "text": s.text, "match": match}
                        )
            minutes_hit = bool(minutes) and matcher.any(_CODE_BLOCK.sub("", minutes)) is not None
            if hits or minutes_hit:
                results.append(
                    {
                        "id": info["id"],
                        "title": info["title"],
                        "date": info["date"],
                        "time": info["time"],
                        "archived": info["archived"],
                        "minutes_hit": minutes_hit,
                        "hits": hits,
                    }
                )
                if len(results) >= SEARCH_MEETINGS:
                    break
        return {"q": query, "expanded": expanded, "results": results}

    @app.put("/api/meetings/{meeting_id}/meta")
    def put_meta(meeting_id: str, req: MetaRequest) -> dict:
        """改标题、标签、归档（存在 meta.json，不改文件夹名）；返回更新后的列表项。"""
        path = folder(meeting_id)
        given = req.model_fields_set
        if "title" in given and req.title is not None and not isinstance(req.title, str):
            raise HTTPException(400, i18n.tr("title 只能是文字或 null", "title must be a string or null"))
        if "tags" in given and (not isinstance(req.tags, list) or not all(isinstance(t, str) for t in req.tags)):
            raise HTTPException(400, i18n.tr("tags 只能是文字列表", "tags must be a list of strings"))
        if "archived" in given and not isinstance(req.archived, bool):
            raise HTTPException(400, i18n.tr("archived 只能是 true 或 false", "archived must be true or false"))
        meta = meetings.read_meta(path)
        if "title" in given:
            meta["title"] = req.title
        if "tags" in given:
            meta["tags"] = req.tags
        if "archived" in given:
            meta["archived"] = req.archived
        try:
            meetings.write_meta(path, meta)
        except OSError as e:
            raise HTTPException(500, str(e)) from e
        return meetings.summary(path)

    @app.delete("/api/meetings/{meeting_id}")
    def delete_meeting(meeting_id: str) -> dict:
        """整个会议文件夹移到废纸篓"""
        trash_meeting(meeting_id)
        return {"ok": True}

    @app.delete("/api/meetings/{meeting_id}/audio")
    def delete_audio(meeting_id: str) -> dict:
        """只把录音移到废纸篓，转写、纪要、导出的文件都保留"""
        return {"ok": True, "freed_bytes": trash_audio(meeting_id, missing_ok=False)}

    @app.post("/api/meetings/batch")
    def batch(req: BatchRequest) -> dict:
        """批量归档 / 加减标签 / 删除 / 删录音。逐个处理，某一个失败不影响其他。"""
        ids, action = req.ids, req.action
        if not isinstance(ids, list) or not ids or not all(isinstance(i, str) for i in ids):
            raise HTTPException(400, i18n.tr("没有选中会议", "No meetings selected"))
        if action not in BATCH_ACTIONS:
            raise HTTPException(400, i18n.tr(f"不认识的操作：{action}", f"Unknown action: {action}"))
        tag = req.tag.strip() if isinstance(req.tag, str) else ""
        if action in ("tag_add", "tag_remove") and not tag:
            raise HTTPException(400, i18n.tr("请给出标签", "A tag is required"))
        tag = tag[: meetings.TAG_MAX_CHARS].strip()
        ok, freed, failed = 0, 0, []
        for meeting_id in ids:
            try:
                if action == "delete":
                    trash_meeting(meeting_id)
                elif action == "delete_audio":
                    freed += trash_audio(meeting_id, missing_ok=True)
                else:
                    path = folder(meeting_id)
                    meta = meetings.read_meta(path)
                    if action in ("archive", "unarchive"):
                        meta["archived"] = action == "archive"
                    elif action == "tag_add":
                        meta["tags"] = [*meta["tags"], tag]
                    else:
                        meta["tags"] = [t for t in meta["tags"] if t != tag]
                    meetings.write_meta(path, meta)
                ok += 1
            except HTTPException as e:
                failed.append({"id": meeting_id, "error": str(e.detail)})
            except Exception as e:  # noqa: BLE001 - 一场失败不影响其他
                failed.append({"id": meeting_id, "error": str(e) or e.__class__.__name__})
        return {"ok": ok, "failed": failed, "freed_bytes": freed}

    def meeting_folders() -> list[tuple[dict, Path]]:
        return [(info, root / info["id"]) for info in meetings.list_meetings(root)]

    @app.get("/api/storage")
    def storage() -> dict:
        """会议目录占了多少空间：录音、残留文件、最大的几场会议，以及磁盘剩余空间。每次现算。"""
        items, audio, leftover = [], 0, 0
        for info, path in meeting_folders():
            a = sum(meetings.file_size(f) for f in meetings.audio_files(path))
            audio += a
            leftover += sum(meetings.file_size(f) for f in meetings.leftover_files(path))
            items.append(
                {
                    "id": info["id"],
                    "title": info["title"],
                    "date": info["date"],
                    "bytes": info["size_bytes"],
                    "audio_bytes": a,
                    "archived": info["archived"],
                }
            )
        disk = root
        while not disk.exists() and disk.parent != disk:
            disk = disk.parent
        try:
            free: int | None = shutil.disk_usage(disk).free
        except OSError:
            free = None
        return {
            "root": str(root),
            "count": len(items),
            "meetings_bytes": sum(i["bytes"] for i in items),
            "audio_bytes": audio,
            "leftover_bytes": leftover,
            "models_bytes": None,  # 识别模型在 ModelScope 缓存里，Python 版不统计
            "free_bytes": free,
            "largest": sorted(items, key=lambda i: i["bytes"], reverse=True)[:STORAGE_LARGEST],
        }

    @app.post("/api/storage/cleanup")
    def storage_cleanup(req: CleanupRequest) -> dict:
        """leftovers：残留文件移到废纸篓（count 是文件数）；audio：早于 N 天的会议只删录音（count 是会议数）。
        跳过正在处理的会议。"""
        if req.what not in ("leftovers", "audio"):
            raise HTTPException(400, i18n.tr(f"不认识的清理项：{req.what}", f"Unknown cleanup target: {req.what}"))
        days = req.older_than_days
        if req.what == "audio":
            if not isinstance(days, int) or isinstance(days, bool) or not 0 <= days <= CLEANUP_DAYS_MAX:
                raise HTTPException(
                    400,
                    i18n.tr(
                        f"天数只能是 0～{CLEANUP_DAYS_MAX} 的整数",
                        f"The number of days must be a whole number from 0 to {CLEANUP_DAYS_MAX}",
                    ),
                )
            if not isinstance(req.archived_only, bool):
                raise HTTPException(
                    400, i18n.tr("archived_only 只能是 true 或 false", "archived_only must be true or false")
                )
        cutoff = date.today() - timedelta(days=days) if req.what == "audio" and days else None
        count, freed, errors = 0, 0, []
        for info, path in meeting_folders():
            if is_busy(info["id"]):
                continue
            if req.what == "leftovers":
                files = meetings.leftover_files(path)
            else:
                if req.archived_only and not info["archived"]:
                    continue
                if cutoff is not None and date.fromisoformat(info["date"]) >= cutoff:
                    continue
                files = meetings.audio_files(path)
            done = 0
            for f in files:
                size = meetings.file_size(f)
                try:
                    _trash(f)
                except TrashUnsupported as e:
                    raise HTTPException(500, str(e)) from e
                except (RuntimeError, OSError, subprocess.SubprocessError) as e:
                    errors.append(str(e) or e.__class__.__name__)
                    log.warning("%s 清理 %s 失败：%s", info["id"], f.name, e)
                    continue
                freed += size
                done += 1
            count += done if req.what == "leftovers" else (1 if done else 0)
        if errors and not count:
            raise HTTPException(500, errors[0])
        log.info("清理%s：%d，腾出 %d 字节", req.what, count, freed)
        return {"ok": True, "count": count, "freed_bytes": freed}

    @app.post("/api/demo")
    def demo() -> dict:
        """把内置的示例会议复制进会议目录（标题和纪要按界面语言）；已经有了就直接返回。"""
        lang = "en" if i18n.is_en() else "zh"
        name = f"{DEMO_WHEN}_{meetings.folder_title(DEMO_TITLE[lang])}"
        dst = root / name
        if dst.exists():
            return {"id": name}
        files = {
            TRANSCRIPT_JSON: TRANSCRIPT_JSON,
            AUDIO: AUDIO,
            MINUTES: "纪要.en.md" if lang == "en" else MINUTES,
        }
        try:
            dst.mkdir(parents=True)
            for target, source in files.items():
                shutil.copyfile(SAMPLE_DIR / source, dst / target)
        except OSError as e:
            shutil.rmtree(dst, ignore_errors=True)
            raise HTTPException(500, i18n.tr(f"复制示例会议失败：{e}", f"Couldn't copy the sample meeting: {e}")) from e
        return {"id": name}

    @app.get("/api/meetings/{meeting_id}")
    def get_meeting(meeting_id: str) -> dict:
        path = folder(meeting_id)
        info = meetings.summary(path)
        minutes_path = path / MINUTES
        data: dict = {
            **info,
            "minutes": minutes_path.read_text(encoding="utf-8") if minutes_path.exists() else "",
            "turns": [],
            "speaker_list": [],
            "audio": meetings.find_audio(path) is not None,
            "path": str(path),
        }
        if info["has_transcript"]:
            tr = Transcript.load(path / TRANSCRIPT_JSON)
            order = tr.speakers_in_order()
            data["speaker_list"] = [
                {"index": i, "name": tr.name_of(s), "custom": s in tr.speaker_names} for i, s in enumerate(order)
            ]
            index_of_name = {tr.name_of(s): i for i, s in enumerate(order)}
            data["turns"] = [
                {
                    "speaker": t.speaker,
                    "color": index_of_name.get(t.speaker, 0),
                    "start_ms": t.start_ms,
                    "ts": fmt_ts(t.start_ms),
                    "text": t.text,
                }
                for t in tr.turns()
            ]
        return data

    @app.get("/api/meetings/{meeting_id}/audio")
    def get_audio(meeting_id: str) -> FileResponse:
        audio = meetings.find_audio(folder(meeting_id))
        if not audio:
            raise HTTPException(404, i18n.tr("没有录音文件", "No recording file"))
        # 显式给标准类型：按扩展名猜的 m4a 类型在 WKWebView（pywebview 窗口）里可能播不了
        return FileResponse(audio, media_type=AUDIO_MIME.get(audio.suffix.lower()))

    @app.put("/api/meetings/{meeting_id}/speakers")
    def put_speakers(meeting_id: str, req: SpeakersRequest) -> dict:
        path = folder(meeting_id)
        tr = Transcript.load(path / TRANSCRIPT_JSON)
        tr.speaker_names = {s: n.strip() for s, n in zip(tr.speakers_in_order(), req.names) if n.strip()}
        meetings.save_transcript(tr, path)
        return get_meeting(meeting_id)

    @app.put("/api/meetings/{meeting_id}/minutes")
    def put_minutes(meeting_id: str, req: MinutesRequest) -> dict:
        path = folder(meeting_id)
        minutes_history.save(path, req.text.rstrip() + "\n")
        return {"ok": True}

    @app.get("/api/meetings/{meeting_id}/minutes/history")
    def get_minutes_history(meeting_id: str) -> dict:
        return {"versions": minutes_history.versions(folder(meeting_id))}

    @app.delete("/api/meetings/{meeting_id}/minutes/history/{version}")
    def delete_minutes_history(meeting_id: str, version: str) -> dict:
        try:
            minutes_history.delete(folder(meeting_id), version)
        except ValueError as e:
            raise HTTPException(400, str(e)) from e
        except FileNotFoundError as e:
            raise HTTPException(404, "Version not found") from e
        return {"ok": True}

    @app.post("/api/meetings/{meeting_id}/summarize")
    def post_summarize(meeting_id: str) -> dict:
        path = folder(meeting_id)
        if not (path / TRANSCRIPT_JSON).exists():
            raise HTTPException(
                400, i18n.tr("还没有转写，不能生成纪要", "No transcript yet, so minutes can't be generated")
            )
        job = Job("summarize", meeting_id, "summarizing")
        start_job(job)

        def work() -> None:
            try:
                run_summarize(job, Transcript.load(path / TRANSCRIPT_JSON), path)
                finish(job)
            except Exception as e:  # noqa: BLE001
                fail(job, e)

        reply = asdict(job)
        threading.Thread(target=work, daemon=True).start()
        return reply

    @app.post("/api/meetings/{meeting_id}/process")
    def post_process(meeting_id: str) -> dict:
        """有录音但还没转写的会议（比如上次处理被打断）重新跑一遍。"""
        path = folder(meeting_id)
        audio = meetings.find_audio(path)
        if not audio:
            raise HTTPException(400, i18n.tr("没有录音文件", "No recording file"))
        job = Job("process", meeting_id, "loading")
        start_job(job)
        reply = asdict(job)
        threading.Thread(target=run_process, args=(job, audio, path, []), daemon=True).start()
        return reply

    def selected_minutes(path: Path, version: str | None) -> str:
        try:
            return (
                minutes_history.read(path, version)["text"]
                if version is not None
                else (path / MINUTES).read_text(encoding="utf-8")
            )
        except (OSError, ValueError) as e:
            raise HTTPException(404, "Minutes version not found") from e

    def version_stem(path: Path, version: str | None) -> str:
        if version is None:
            return export_stem(path)
        from .obsidian import safe_name

        text = selected_minutes(path, version)
        return safe_name(meetings.minutes_title(text) or path.name) + f"-history-{version}"

    @app.post("/api/meetings/{meeting_id}/push")
    def post_push(meeting_id: str, version: str | None = None) -> dict:
        from .push import detect, send

        path = folder(meeting_id)
        cfg = config.load()
        if not cfg.webhook:
            raise HTTPException(
                400,
                i18n.tr(
                    "还没有配置群机器人 webhook，去设置里填一下", "No group bot webhook is set; add one in Settings"
                ),
            )
        if not (path / MINUTES).exists():
            raise HTTPException(400, i18n.tr("还没有纪要", "No minutes yet"))
        text = selected_minutes(path, version)
        try:
            send(cfg.webhook, text, cfg.webhook_secret)
        except Exception as e:
            raise HTTPException(502, i18n.tr(f"推送失败：{e}", f"Push failed: {e}")) from e
        platform_id = detect(cfg.webhook)
        if i18n.is_en():
            name = {"feishu": "Feishu", "dingtalk": "DingTalk", "wecom": "WeCom"}[platform_id]
            return {"ok": True, "message": f"Pushed to the {name} group"}
        name = {"feishu": "飞书", "dingtalk": "钉钉", "wecom": "企业微信"}[platform_id]
        return {"ok": True, "message": f"已推送到{name}群"}

    @app.post("/api/meetings/{meeting_id}/reveal")
    def reveal(meeting_id: str) -> dict:
        path = folder(meeting_id)
        if platform.system() == "Darwin":
            subprocess.run(["open", str(path)], check=False)
        elif platform.system() == "Windows":
            os.startfile(path)
        else:
            subprocess.run(["xdg-open", str(path)], check=False)
        return {"ok": True}

    @app.post("/api/meetings/{meeting_id}/obsidian")
    def post_obsidian(meeting_id: str, req: OpenRequest | None = None, version: str | None = None) -> dict:
        """存进 Obsidian；open=true 时顺便用 Obsidian 打开这篇笔记"""
        from . import obsidian

        path = folder(meeting_id)
        try:
            out = obsidian.export(config.load().obsidian, path, version)
        except Exception as e:
            raise HTTPException(400, str(e)) from e
        uri = obsidian.uri(out.note)
        if req and req.open:
            _open(uri)
        return {"ok": True, "path": str(out.note), "uri": uri, "drawings": len(out.drawings)}

    @app.post("/api/meetings/{meeting_id}/excalidraw")
    def post_excalidraw(meeting_id: str) -> dict:
        """把纪要里的鱼骨图导出成 .excalidraw 文件（放在会议文件夹里），并在访达里选中，
        可以拖进 excalidraw.com 或用 Obsidian 的 Excalidraw 插件继续改。"""
        from . import fishbone

        path = folder(meeting_id)
        if not (path / MINUTES).exists():
            raise HTTPException(400, i18n.tr("还没有纪要", "No minutes yet"))
        minutes = (path / MINUTES).read_text(encoding="utf-8")
        en = i18n.doc_is_en(minutes)  # 文件名和图上的文字跟纪要的语言走
        try:
            transcript: Transcript | None = Transcript.load(path / TRANSCRIPT_JSON)
        except Exception:  # noqa: BLE001
            transcript = None
        turns = transcript.turns(en) if transcript is not None else []
        duration = transcript.duration_ms if transcript is not None else 2**64 - 1
        specs = [s for _, s in fishbone.blocks(minutes) if s is not None]
        if not specs:
            raise HTTPException(400, i18n.tr("这份纪要里没有鱼骨图", "These minutes have no fishbone diagram"))
        paths = []
        stem = "fishbone" if en else "鱼骨图"
        for k, spec in enumerate(specs):
            grounds = [fishbone.ground(b, turns, duration) for b in spec.branches]
            p = path / (f"{stem}.excalidraw" if k == 0 else f"{stem}{k + 1}.excalidraw")
            p.write_text(fishbone.dumps(fishbone.excalidraw(spec, grounds, en)), encoding="utf-8")
            paths.append(str(p))
        _open(paths[0], reveal=True)
        return {"ok": True, "paths": paths}

    @app.post("/api/meetings/{meeting_id}/pdf")
    def post_pdf(meeting_id: str, req: PdfRequest, version: str | None = None) -> dict:
        """导出 PDF：界面排好版的 HTML 交给录音组件，用系统 WebKit 转成 A4 PDF，存进会议文件夹并在访达里选中。"""
        from . import record

        path = folder(meeting_id)
        if not (path / MINUTES).exists():
            raise HTTPException(400, i18n.tr("还没有纪要", "No minutes yet"))
        out = path / f"{version_stem(path, version)}.pdf"
        try:
            record.html_to_pdf(req.html, out)
        except (RuntimeError, OSError, subprocess.SubprocessError) as e:
            raise HTTPException(500, str(e)) from e
        _open(str(out), reveal=True)
        return {"ok": True, "path": str(out)}

    @app.post("/api/meetings/{meeting_id}/srt")
    def post_srt(meeting_id: str) -> dict:
        """导出 SRT 字幕（一句一条），存进会议文件夹并在访达里选中。说话人名、冒号按界面语言。"""
        path = folder(meeting_id)
        try:
            tr = Transcript.load(path / TRANSCRIPT_JSON)
        except (OSError, ValueError, KeyError, TypeError):
            tr = None
        if tr is None or not tr.sentences:
            raise HTTPException(400, i18n.tr("还没有转写", "No transcript yet"))
        out = path / f"{export_stem(path)}.srt"
        try:
            out.write_text(tr.to_srt(i18n.is_en()), encoding="utf-8")
        except OSError as e:
            raise HTTPException(500, str(e)) from e
        _open(str(out), reveal=True)
        return {"ok": True, "path": str(out)}

    @app.post("/api/meetings/{meeting_id}/docx")
    def post_docx(meeting_id: str, req: PdfRequest, version: str | None = None) -> dict:
        """导出 Word：界面排好版的 HTML 交给 macOS 自带的 textutil 转成 .docx，存进会议文件夹并在访达里选中。"""
        path = folder(meeting_id)
        if not Path(TEXTUTIL).exists():
            raise HTTPException(500, i18n.tr("导出 Word 只支持 macOS", "Word export is only available on macOS"))
        out = path / f"{version_stem(path, version)}.docx"
        try:
            out.unlink(missing_ok=True)
            with tempfile.TemporaryDirectory(prefix="tingo-docx-") as tmp:
                src = Path(tmp) / "minutes.html"
                src.write_text(req.html, encoding="utf-8")
                # 不指定编码时，没写 <meta charset> 的 HTML 会被当成别的编码，中文变乱码
                res = subprocess.run(
                    [TEXTUTIL, "-convert", "docx", "-inputencoding", "UTF-8", "-output", str(out), str(src)],
                    check=False,
                    capture_output=True,
                    text=True,
                    timeout=60,
                )
        except (OSError, subprocess.SubprocessError) as e:
            raise HTTPException(500, str(e)) from e
        if res.returncode != 0 or not out.exists():
            raise HTTPException(500, res.stderr.strip() or i18n.tr("导出 Word 失败", "Word export failed"))
        _open(str(out), reveal=True)
        return {"ok": True, "path": str(out)}

    @app.post("/api/meetings/{meeting_id}/speakers/guess")
    def guess_speakers(meeting_id: str) -> dict:
        """让大模型根据自我介绍、点名等线索猜每位说话人的名字，只返回建议，不改转写。"""
        from . import summarize as sm

        path = folder(meeting_id)
        if not (path / TRANSCRIPT_JSON).exists():
            raise HTTPException(400, i18n.tr("还没有转写", "No transcript yet"))
        tr = Transcript.load(path / TRANSCRIPT_JSON)
        cfg = config.load()
        try:
            sm._api_key(cfg.llm)
        except RuntimeError as e:
            raise HTTPException(400, str(e)) from e
        try:
            suggestions = sm.guess_speakers(tr, cfg.llm, i18n.is_en())
        except sm.GuessError as e:
            log.warning("%s 猜说话人名字：模型输出解析不了：%.300s", meeting_id, e)
            raise HTTPException(
                502, i18n.tr("大模型没有给出可用的结果", "The model didn't return a usable result")
            ) from e
        except Exception as e:
            log.warning("%s 猜说话人名字失败：%s", meeting_id, e)
            raise HTTPException(502, str(e) or e.__class__.__name__) from e
        return {"suggestions": suggestions}

    @app.get("/api/obsidian/vaults")
    def obsidian_vaults() -> list[dict]:
        from . import obsidian

        return [{"name": n, "path": p} for n, p in obsidian.vaults()]

    # ---------- 录音与导入 ----------

    @app.post("/api/record/start")
    def record_start(req: RecordRequest) -> dict:
        from .record import Recording, system_audio_supported

        if not req.system and not req.mic:
            raise HTTPException(
                400, i18n.tr("系统声音和麦克风至少开一个", "Turn on at least one of system audio and microphone")
            )
        system = req.system and system_audio_supported()
        out = meetings.new_folder(root, req.title)
        job = Job(
            "record", out.name, "recording", sources=[n for n, on in (("system", system), ("mic", req.mic)) if on]
        )
        start_job(job)
        rec = Recording(out, system_audio=system, mic=req.mic)
        try:
            rec.start()
        except Exception as e:
            with lock:
                jobs[:] = [j for j in jobs if j.id != job.id]
            shutil.rmtree(out, ignore_errors=True)
            raise HTTPException(500, str(e)) from e
        from . import record
        from .live import LiveTranscriber

        started = time.time()
        cfg = config.load()
        # 边录边转：录音时就分段识别，结束后只剩最后一段和一次说话人聚类
        live = LiveTranscriber(out, rec.sources, hotwords=cfg.hotwords + req.hotwords)
        live.start()
        rc = cfg.record
        apps = record.running_meeting_apps() if rc.auto_stop_app_quit else set()
        auto = record.AutoStop(rc.auto_stop_silence_min, rc.auto_stop_app_quit, apps, started)
        watch_stop = threading.Event()
        with lock:
            state["recording"], state["hotwords"], state["live"] = rec, req.hotwords, live
            state["auto"], state["watch_stop"] = auto, watch_stop
        threading.Thread(
            target=watch_recording, args=(job, out, list(rec.sources), live, auto, watch_stop), daemon=True
        ).start()
        log.info(
            "%s 开始录音：%s（%s 分钟没人说话自动结束%s）",
            out.name,
            "+".join(rec.sources),
            rc.auto_stop_silence_min or "不",
            f"，会议软件 {'、'.join(sorted(apps))} 退出时自动结束" if auto.watch_apps else "",
        )
        return asdict(job)

    def watch_recording(job: Job, out: Path, sources: list[str], live, auto, stop: threading.Event) -> None:
        """录音时每 5 秒看一次有没有人说话（任一音轨最近 5 秒的电平，或边录边转出了新文字）、
        会议软件还在不在，该结束时走和"结束录音"一样的流程。"""
        from . import record

        seen = len(getattr(live, "lines", []))
        while not stop.wait(record.CHECK_S):
            try:
                voiced = any(
                    record.track_level(out / f"{s}.wav", window_s=record.CHECK_S) >= record.VOICE_LEVEL for s in sources
                )
                n = len(getattr(live, "lines", []))
                voiced, seen = voiced or n > seen, n
                apps = record.running_meeting_apps() if auto.watch_apps else None
                reason = auto.observe(time.time(), voiced, apps)
            except Exception as e:  # noqa: BLE001 - 检查出错不影响录音本身
                log.warning("%s 自动结束录音的检查出错：%s", job.meeting, e)
                continue
            if reason == record.AutoStop.SILENCE:
                n_min = auto.silence_min
                notice = i18n.tr(
                    f"{n_min} 分钟没人说话，已自动结束录音",
                    f"No one spoke for {n_min} minutes — recording stopped automatically",
                )
                if stop_recording(only=job, notice=notice, keep_s=auto.keep_s()) is not None:
                    log.info("%s %d 分钟没人说话，自动结束录音", job.meeting, n_min)
                return
            if reason == record.AutoStop.APP_QUIT:
                notice = i18n.tr(
                    "会议软件已退出，已自动结束录音", "The meeting app quit — recording stopped automatically"
                )
                if stop_recording(only=job, notice=notice) is not None:
                    log.info("%s 会议软件已退出，自动结束录音", job.meeting)
                return

    @app.post("/api/record/stop")
    def record_stop() -> dict:
        reply = stop_recording()
        if reply is None:
            raise HTTPException(400, i18n.tr("没有正在进行的录音", "No recording in progress"))
        return reply

    def stop_recording(only: Job | None = None, notice: str = "", keep_s: float | None = None) -> dict | None:
        """结束正在进行的录音，转写、生成纪要转到后台。没有在录的返回 None。

        only：自动结束时只结束这一场（检查线程醒来时用户可能已经结束了它、又开了下一场）。
        keep_s：因为没人说话而自动结束时，原始音轨截到前这么多秒（最后有声之后再留 30 秒）。
        """
        with lock:
            job = next((j for j in jobs if j.kind == "record" and j.state == "recording"), None)
            rec = state["recording"]
            if job is None or rec is None or (only is not None and job is not only):
                return None
            # 录音停下后转到后台：转写、生成纪要的同时就可以开下一场
            job.state = "saving"
            if notice:
                job.notice = notice
            reply = asdict(job)  # 后台线程马上会改状态，回给界面的固定是 saving
            live, hotwords = state["live"], state["hotwords"]
            if state["watch_stop"] is not None:
                state["watch_stop"].set()
            state["recording"], state["live"], state["hotwords"] = None, None, []
            state["auto"], state["watch_stop"] = None, None

        def work() -> None:
            from .record import mixdown, truncate_wav

            out = root / job.meeting
            final = out / AUDIO
            stopped = time.time()
            try:
                rec.stop_capture()
                if keep_s is not None:
                    for s in rec.sources:
                        if (out / f"{s}.wav").exists() and truncate_wav(out / f"{s}.wav", keep_s):
                            log.info("%s %s.wav 截到前 %.0f 秒", out.name, s, keep_s)
                job.state = "finalizing"
                tr = live.finish(final) if live else None  # 原始音轨合成前先把最后几秒转完
                if tr is not None and keep_s is not None:
                    tr.duration_ms = min(tr.duration_ms, round(keep_s * 1000))
            except Exception as e:  # noqa: BLE001
                fail(job, e)
                return
            if live:
                log.info(
                    "%s 结束录音后转写：%.1f 秒（剩余 %d 段 %.1f 秒，%d 个声纹聚类 %.1f 秒）%s",
                    out.name,
                    time.time() - stopped,
                    live.timings.get("tail_chunks", 0),
                    live.timings.get("tail_s", 0),
                    live.timings.get("voiceprints", 0),
                    live.timings.get("cluster_s", 0),
                    f"，边录边转出错：{live.error!r}" if live.error else "",
                )
            if tr is None or not tr.sentences:
                # 边录边转出错（或什么都没识别到）时退回整段转写；合成音频在先，因为要用它转写
                try:
                    mixdown(out).rename(final)
                except Exception as e:  # noqa: BLE001
                    fail(job, e)
                    return
                run_process(job, final, out, hotwords)
                return
            # 转写先落盘，合成音频（长会议要二三十秒）和生成纪要同时进行
            meetings.save_transcript(tr, out)
            mix_error: list[Exception] = []

            def mix() -> None:
                t = time.time()
                try:
                    mixdown(out).rename(final)
                    log.info("%s 合成录音：%.1f 秒", out.name, time.time() - t)
                except Exception as e:  # noqa: BLE001
                    mix_error.append(e)

            mixer = threading.Thread(target=mix, daemon=True)
            mixer.start()
            try:
                run_summarize(job, tr, out)
                mixer.join()
                if mix_error:
                    raise RuntimeError(
                        i18n.tr(
                            f"纪要已生成，但合成录音失败：{mix_error[0]}（原始音轨还在会议文件夹里）",
                            f"Minutes are ready, but mixing the recording failed: {mix_error[0]} "
                            "(the raw tracks are still in the meeting folder)",
                        )
                    )
                finish(job)
            except Exception as e:  # noqa: BLE001
                fail(job, e)

        threading.Thread(target=work, daemon=True).start()
        return reply

    @app.post("/api/record/test")
    def record_test(req: RecordTestRequest | None = None) -> dict:
        """试录几秒（系统声音 + 麦克风），报告每一路录没录到声音。第一次调用会触发 macOS 的权限弹窗，新手引导用。"""
        from . import record

        seconds = req.seconds if req is not None else 3
        if not _is_number(seconds) or not 1 <= seconds <= 10:
            raise HTTPException(400, i18n.tr("试录时长只能是 1～10 秒", "The test length must be 1–10 seconds"))
        with lock:
            if any(j.state == "recording" for j in jobs) or state["recording"] is not None:
                raise HTTPException(
                    409, i18n.tr("正在录音，先结束录音再测试", "A recording is in progress; stop it before testing")
                )
            if state["testing"]:
                raise HTTPException(409, i18n.tr("正在试录，等它结束", "A test recording is already running"))
            state["testing"] = True
        try:
            result = record.probe_devices(float(seconds))
        finally:
            with lock:
                state["testing"] = False
        if not result.get("message"):
            labels = (
                {"ok": "OK", "silent": "almost silent", "denied": "no permission", "unsupported": "not supported"}
                if i18n.is_en()
                else {"ok": "正常", "silent": "几乎没声音", "denied": "没有权限", "unsupported": "本机不支持"}
            )
            sys_l = labels.get(result["system"], result["system"])
            mic_l = labels.get(result["mic"], result["mic"])
            result["message"] = i18n.tr(
                f"系统声音：{sys_l}；麦克风：{mic_l}", f"System audio: {sys_l}; microphone: {mic_l}"
            )
        return result

    @app.post("/api/import")
    def import_file(
        file: Annotated[UploadFile, File()],
        title: Annotated[str, Form()] = "",
        modified: Annotated[float, Form()] = 0,  # 文件的修改时间（毫秒），当作会议时间
    ) -> dict:
        name = Path(file.filename or "录音.m4a")
        if name.suffix.lower() not in meetings.AUDIO_EXTS:
            raise HTTPException(
                400, i18n.tr(f"不支持的文件类型：{name.suffix}", f"Unsupported file type: {name.suffix}")
            )
        when = datetime.fromtimestamp(modified / 1000) if modified else datetime.now()
        out = meetings.new_folder(root, title or name.stem, when)
        job = Job("process", out.name, "saving")
        start_job(job)
        out.mkdir(parents=True)
        audio = out / f"录音{name.suffix.lower()}"
        with audio.open("wb") as f:
            shutil.copyfileobj(file.file, f)
        reply = asdict(job)
        threading.Thread(target=run_process, args=(job, audio, out, []), daemon=True).start()
        return reply

    def snapshot() -> list[dict]:
        from . import asr
        from .record import track_level

        loaded = asr.model_loaded()
        with lock:
            data = [job_json(j) for j in jobs]
            live, auto = state["live"], state["auto"]
        now = time.time()
        for d in data:
            d["model_loaded"] = loaded
            if d["state"] == "recording":
                out = root / d["meeting"]
                d["levels"] = {s: track_level(out / f"{s}.wav") for s in d["sources"]}
                d["live"] = live.recent(8) if live else []
                if auto is not None:
                    d["silent_s"] = auto.silent_s(now)
                    if (left := auto.auto_stop_s(now)) is not None:
                        d["auto_stop_s"] = left
        return data

    @app.get("/api/jobs")
    def get_jobs() -> dict:
        """所有任务（进行中的和刚结束的），按开始时间排列"""
        from . import asr

        return {"jobs": snapshot(), "model_loaded": asr.model_loaded()}

    @app.get("/api/job")
    def get_job() -> dict:
        """旧接口：只返回一个任务（正在录音的优先，其次最近一个进行中的，再次最近一个）"""
        data = snapshot()
        busy = [d for d in data if d["state"] not in ("done", "error")]
        pick = next((d for d in data if d["state"] == "recording"), None) or (busy or data or [None])[-1]
        return pick or {"state": "idle"}

    @app.post("/api/job/dismiss")
    def dismiss() -> dict:
        """收起所有已结束的任务"""
        with lock:
            jobs[:] = [j for j in jobs if j.busy]
        return {"ok": True}

    @app.post("/api/jobs/{job_id}/dismiss")
    def dismiss_one(job_id: str) -> dict:
        """收起一个已结束的任务"""
        with lock:
            jobs[:] = [j for j in jobs if j.busy or j.id != job_id]
        return {"ok": True}

    from fastapi.middleware.cors import CORSMiddleware

    # 桌面版：界面跑在 Tauri 的 webview 里（tauri://localhost），开发时在 Vite（localhost:1420）
    app.add_middleware(
        CORSMiddleware,
        allow_origin_regex=r"^(tauri://localhost|https?://tauri\.localhost|http://localhost:1420)$",
        allow_methods=["*"],
        allow_headers=["*"],
    )

    @app.middleware("http")
    async def ui_language(request, call_next):
        # 前端每个请求都带界面语言；不带的（比如 <audio> 拉录音）沿用上一次的
        if value := request.headers.get(i18n.HEADER):
            i18n.set_lang(value)
        return await call_next(request)

    @app.middleware("http")
    async def no_cache_web(request, call_next):
        # 界面文件随版本更新，浏览器 / 独立窗口缓存旧的 CSS、JS 会出现"新结构 + 旧样式"的错乱
        response = await call_next(request)
        if not request.url.path.startswith("/api/"):
            response.headers["Cache-Control"] = "no-cache"
        return response

    @app.get("/", include_in_schema=False)
    def index() -> HTMLResponse:
        # 界面由 app/ 用 Vite 构建，资源文件名自带内容哈希；首页本身不缓存，更新后立即生效
        return HTMLResponse((WEB_DIR / "index.html").read_text(encoding="utf-8"))

    if WEB_DIR.exists():
        app.mount("/", StaticFiles(directory=WEB_DIR, html=True), name="web")
    return app
