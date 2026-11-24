"""会议文件夹的约定：一场会议一个文件夹，命令行和图形界面共用。

<根目录>/2026-11-02_1430_周会/
├── 录音.m4a
├── 转写.md
├── transcript.json
├── 纪要.md
└── meta.json   可选：用户改的标题、标签、是否归档；文件夹名不改
"""

from __future__ import annotations

import json
import re
from datetime import date, datetime
from pathlib import Path

from . import terms
from .config import env_var, load, with_legacy
from .transcript import Transcript, fmt_ts

DEFAULT_ROOT = Path(
    env_var("ROOT")
    or load().storage_root
    or with_legacy(Path.home() / "Documents" / "Tingo", Path.home() / "Documents" / "jiyao")
).expanduser()
AUDIO = "录音.m4a"
MINUTES = "纪要.md"
TRANSCRIPT_MD = "转写.md"
TRANSCRIPT_JSON = "transcript.json"
META = "meta.json"
RECORDING_STEM = "录音"  # 录音.m4a / 导入的 录音.mp3 等
RAW_AUDIO = ("system.wav", "mic.wav")  # 录音时的原始音轨（合成失败时会留下）
TEMP_SUFFIXES = (".part", ".tmp")
TAG_MAX_CHARS = 20
TAG_MAX_COUNT = 10

_FOLDER_RE = re.compile(r"^(\d{4}-\d{2}-\d{2})_(\d{2})(\d{2})_(.+)$")
_RAW_TRACKS = {"system.wav", "mic.wav", "mixed.wav"}  # 录音进行中的原始轨道
AUDIO_EXTS = {".m4a", ".mp3", ".wav", ".mp4", ".aac", ".flac", ".ogg", ".webm", ".mov", ".mkv"}


def folder_title(title: str) -> str:
    """会议标题里不能进文件夹名的字符（路径分隔符、空白等）换成下划线。"""
    return re.sub(r'[\\/:*?"<>|\s]+', "_", title.strip()) or "会议"


def new_folder(root: Path, title: str, when: datetime | None = None) -> Path:
    when = when or datetime.now()
    safe = folder_title(title)
    path = root / f"{when:%Y-%m-%d_%H%M}_{safe}"
    n = 2
    while path.exists():
        path = root / f"{when:%Y-%m-%d_%H%M}_{safe}_{n}"
        n += 1
    return path


def save_transcript(tr: Transcript, out_dir: Path) -> None:
    tr.save(out_dir / TRANSCRIPT_JSON)
    (out_dir / TRANSCRIPT_MD).write_text(tr.to_markdown(), encoding="utf-8")


def meeting_date(folder: Path) -> date:
    """文件夹名里的日期优先（tingo 录的），否则用录音或转写文件的修改时间。"""
    m = _FOLDER_RE.match(folder.name)
    if m:
        return date.fromisoformat(m.group(1))
    for f in (find_audio(folder), folder / TRANSCRIPT_JSON):
        if f and f.exists():
            return datetime.fromtimestamp(f.stat().st_mtime).date()
    return date.today()


def find_audio(folder: Path) -> Path | None:
    if (folder / AUDIO).exists():
        return folder / AUDIO
    for p in sorted(folder.iterdir()) if folder.exists() else []:
        if p.suffix.lower() in AUDIO_EXTS and p.name not in _RAW_TRACKS:
            return p
    return None


def normalize_tags(tags: object) -> list[str]:
    """标签：去首尾空白、空的丢掉、每个最长 20 个字符、去重（保持顺序）、最多 10 个。不是列表时当作没有标签。"""
    if not isinstance(tags, list):
        return []
    out: list[str] = []
    for t in tags:
        if not isinstance(t, str):
            continue
        t = t.strip()[:TAG_MAX_CHARS].strip()
        if t and t not in out:
            out.append(t)
    return out[:TAG_MAX_COUNT]


def read_meta(folder: Path) -> dict:
    """meta.json：{"title": 用户改的标题或 None, "tags": [...], "archived": bool}。没有或读不了时全是默认值。"""
    try:
        data = json.loads((folder / META).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        data = {}
    if not isinstance(data, dict):
        data = {}
    title = data.get("title")
    title = title.strip() if isinstance(title, str) else ""
    archived = data.get("archived")
    return {
        "title": title or None,
        "tags": normalize_tags(data.get("tags")),
        "archived": archived if isinstance(archived, bool) else False,
    }


def write_meta(folder: Path, meta: dict) -> dict:
    """写 meta.json（UTF-8、缩进 2、中文原样）。只改 title（为空时不写）、tags、archived，
    其他字段（Watch 录音的 source / watch_recording_id、中断恢复的 interrupted 等）原样保留；
    现有文件读不出或不是 JSON 对象时不覆盖，抛 OSError。返回规范化后的内容。"""
    title = meta.get("title")
    title = title.strip() if isinstance(title, str) else ""
    norm = {"title": title or None, "tags": normalize_tags(meta.get("tags")), "archived": meta.get("archived") is True}
    path = folder / META
    try:
        existing = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        existing = {}
    except (OSError, ValueError) as e:
        raise OSError(f"meta.json 读不出，没有覆盖：{e}") from e
    if not isinstance(existing, dict):
        raise OSError("meta.json 格式不对，没有覆盖")
    data: dict = {"title": title} if title else {}
    data["tags"], data["archived"] = norm["tags"], norm["archived"]
    data.update({k: v for k, v in existing.items() if k not in ("title", "tags", "archived")})
    tmp = folder / ".meta.json.tmp"
    tmp.write_text(json.dumps(data, ensure_ascii=False, indent=2), encoding="utf-8")
    tmp.replace(path)
    return norm


def _files(folder: Path) -> list[Path]:
    try:
        return sorted(p for p in folder.iterdir() if p.is_file())
    except OSError:
        return []


def _is_recording(p: Path) -> bool:
    """录音.*（任何扩展名）"""
    return p.name.startswith(RECORDING_STEM + ".") and len(p.name) > len(RECORDING_STEM) + 1


def audio_files(folder: Path) -> list[Path]:
    """这场会议的录音文件：录音.*、system.wav、mic.wav。"""
    return [p for p in _files(folder) if _is_recording(p) or p.name in RAW_AUDIO]


def leftover_files(folder: Path) -> list[Path]:
    """残留文件：已经有 录音.* 时还留着的 system.wav / mic.wav（合成录音失败前的原始音轨），以及 *.part、*.tmp。"""
    files = _files(folder)
    has_recording = any(_is_recording(p) for p in files)
    return [p for p in files if (has_recording and p.name in RAW_AUDIO) or p.suffix.lower() in TEMP_SUFFIXES]


def file_size(p: Path) -> int:
    try:
        return p.stat().st_size
    except OSError:
        return 0


def dir_size(folder: Path) -> int:
    """整个文件夹（含子文件夹）的大小；符号链接不跟进。"""
    total = 0
    for p in folder.rglob("*"):
        try:
            if p.is_file() and not p.is_symlink():
                total += p.stat().st_size
        except OSError:
            continue
    return total


def minutes_title(text: str) -> str | None:
    for line in text.splitlines():
        if line.startswith("# "):
            return line[2:].strip()
    return None


def one_liner(text: str) -> str:
    """纪要里"一句话总结"（英文纪要是 Summary）下面的那句。术语表代码块不算。"""
    lines = terms.remove(text).splitlines()
    for i, line in enumerate(lines):
        if line.strip() in ("## 一句话总结", "## Summary"):
            for nxt in lines[i + 1 :]:
                if nxt.strip():
                    return nxt.strip()
    return ""


def summary(folder: Path) -> dict:
    """会议列表里一张卡片要的信息，只读小文件，不加载转写全文。"""
    minutes_path = folder / MINUTES
    minutes = minutes_path.read_text(encoding="utf-8") if minutes_path.exists() else ""
    m = _FOLDER_RE.match(folder.name)
    started = f"{m.group(2)}:{m.group(3)}" if m else ""
    fallback_title = m.group(4) if m else folder.name
    meta = read_meta(folder)
    info: dict = {
        "id": folder.name,
        # 标题：用户改的 → 纪要第一行 # 标题 → 文件夹名里的会议名
        "title": meta["title"] or minutes_title(minutes) or fallback_title,
        "date": meeting_date(folder).isoformat(),
        "time": started,
        "summary": one_liner(minutes),
        "has_minutes": bool(minutes),
        "has_transcript": (folder / TRANSCRIPT_JSON).exists(),
        "duration": "",
        "speakers": [],
        "tags": meta["tags"],
        "archived": meta["archived"],
        "size_bytes": dir_size(folder),
        "has_audio": any(_is_recording(p) for p in _files(folder)),
    }
    if info["has_transcript"]:
        tr = Transcript.load(folder / TRANSCRIPT_JSON)
        info["duration"] = fmt_ts(tr.duration_ms)
        info["speakers"] = [tr.name_of(s) for s in tr.speakers_in_order()]
    return info


def list_meetings(root: Path) -> list[dict]:
    if not root.exists():
        return []
    folders = [
        p for p in root.iterdir() if p.is_dir() and ((p / TRANSCRIPT_JSON).exists() or find_audio(p) is not None)
    ]
    items = [summary(p) for p in folders]
    return sorted(items, key=lambda x: (x["date"], x["time"], x["id"]), reverse=True)
