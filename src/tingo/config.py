"""配置：~/.config/tingo/config.toml，环境变量优先。"""

from __future__ import annotations

import json
import math
import os
import tomllib
from dataclasses import dataclass, field
from pathlib import Path


def env_var(name: str, default: str | None = None) -> str | None:
    """读环境变量；改名前的 JIYAO_ 前缀照样认。"""
    for key in (f"TINGO_{name}", f"JIYAO_{name}"):
        if value := os.environ.get(key):
            return value
    return default


def with_legacy(new: Path, old: Path) -> Path:
    """新位置不存在而改名前（jiyao）的位置存在时，沿用旧位置，老用户的配置和会议不用搬。"""
    return old if not new.exists() and old.exists() else new


CONFIG_PATH = Path(
    env_var("CONFIG")
    or with_legacy(Path.home() / ".config" / "tingo" / "config.toml", Path.home() / ".config" / "jiyao" / "config.toml")
)

EXAMPLE = """\
# tingo 配置。任一 OpenAI 兼容接口都可以：DeepSeek、通义千问、Kimi、智谱、本地 Ollama……

# 专有名词、人名、项目代号，提高识别准确率（必须写在下面的 [llm] 之前，否则会被当成 llm 的配置）
hotwords = []

[llm]
base_url = "https://api.deepseek.com"
api_key = "sk-..."
model = "deepseek-chat"
# 本地 Ollama（不出本机）：
# base_url = "http://localhost:11434/v1"
# api_key = "ollama"
# model = "qwen2.5:7b"
# 采样温度（0～2），不写就用 0.2；Kimi 等只接受 1 的模型写 temperature = 1
# temperature = 1

[push]
# 飞书 / 钉钉 / 企业微信 群机器人 webhook，按域名自动识别
webhook = ""
# 机器人开启了"加签"时填
secret = ""
"""


@dataclass
class LLMConfig:
    base_url: str = "https://api.deepseek.com"
    api_key: str = ""
    model: str = "deepseek-chat"
    api: str = "openai"  # 接口协议：openai（OpenAI 兼容的 chat/completions）| anthropic（Claude 的 messages）
    temperature: float | None = None  # 采样温度 0～2，None = 默认 0.2


DEFAULT_TEMPERATURE = 0.2


def valid_temperature(value: object) -> bool:
    """设置里能填的温度：0～2 的数。与桌面版的 valid_temperature 一致。"""
    return isinstance(value, int | float) and not isinstance(value, bool) and math.isfinite(value) and 0 <= value <= 2


LLM_APIS = ("openai", "anthropic")


def normalize_api(value: object) -> str:
    """接口协议只认 openai / anthropic，其它值（包括没写）都当 openai。与桌面版的 normalize_api 一致。"""
    v = value.strip().lower() if isinstance(value, str) else ""
    return v if v in LLM_APIS else "openai"


@dataclass
class ObsidianConfig:
    """存进 Obsidian：库的路径、库里的文件夹、生成纪要后是否自动存、是否附带转写全文"""

    vault: str = ""
    folder: str = "会议纪要"
    auto: bool = True
    transcript: bool = True


@dataclass
class RecordConfig:
    """[record]：连续多少分钟没人说话自动结束录音（0 = 不自动结束）；会议软件退出时是否自动结束"""

    auto_stop_silence_min: int = 10
    auto_stop_app_quit: bool = True


SILENCE_MIN_MAX = 240


def valid_silence_min(value: object) -> bool:
    return isinstance(value, int) and not isinstance(value, bool) and 0 <= value <= SILENCE_MIN_MAX


@dataclass
class Config:
    storage_root: str = ""
    llm: LLMConfig = field(default_factory=LLMConfig)
    hotwords: list[str] = field(default_factory=list)
    webhook: str = ""
    webhook_secret: str = ""
    obsidian: ObsidianConfig = field(default_factory=ObsidianConfig)
    ui_language: str = "auto"  # [ui] language：auto | zh | en，auto 由界面按系统语言决定
    minutes_language: str = "auto"  # [minutes] language：auto | zh | en，auto 跟会议转写的语言
    asr_language: str = "zh"  # [asr] language：zh | en | auto，桌面版的识别模型用；这里的 Paraformer 只认中文
    record: RecordConfig = field(default_factory=RecordConfig)


UI_LANGUAGES = MINUTES_LANGUAGES = ("auto", "zh", "en")
ASR_LANGUAGES = ("zh", "en", "auto")


def _choice(section: object, allowed: tuple[str, ...], default: str) -> str:
    value = section.get("language") if isinstance(section, dict) else None
    value = value.strip().lower() if isinstance(value, str) else None
    return value if value in allowed else default


def load() -> Config:
    data: dict = {}
    if CONFIG_PATH.exists():
        data = tomllib.loads(CONFIG_PATH.read_text(encoding="utf-8"))

    llm = data.get("llm", {})
    push = data.get("push", {})
    obs = data.get("obsidian", {})
    rec = data.get("record", {})
    rec = rec if isinstance(rec, dict) else {}
    silence = rec.get("auto_stop_silence_min")
    app_quit = rec.get("auto_stop_app_quit")
    cfg = Config(
        storage_root=str(data.get("storage", {}).get("root", "")),
        llm=LLMConfig(
            base_url=llm.get("base_url", LLMConfig.base_url),
            api_key=llm.get("api_key", ""),
            model=llm.get("model", LLMConfig.model),
            api=normalize_api(llm.get("api")),
            # 写错了只忽略这一项，不连带别的配置
            temperature=float(t) if valid_temperature(t := llm.get("temperature")) else None,
        ),
        hotwords=list(data.get("hotwords", llm.get("hotwords", []))),  # 早期模板误把 hotwords 写在 [llm] 下
        webhook=push.get("webhook", ""),
        webhook_secret=push.get("secret", ""),
        obsidian=ObsidianConfig(
            vault=obs.get("vault", ObsidianConfig.vault),
            folder=obs.get("folder", ObsidianConfig.folder),
            auto=obs.get("auto", ObsidianConfig.auto),
            transcript=obs.get("transcript", ObsidianConfig.transcript),
        ),
        ui_language=_choice(data.get("ui"), UI_LANGUAGES, Config.ui_language),
        minutes_language=_choice(data.get("minutes"), MINUTES_LANGUAGES, Config.minutes_language),
        asr_language=_choice(data.get("asr"), ASR_LANGUAGES, Config.asr_language),
        record=RecordConfig(
            auto_stop_silence_min=silence if valid_silence_min(silence) else RecordConfig.auto_stop_silence_min,
            auto_stop_app_quit=app_quit if isinstance(app_quit, bool) else RecordConfig.auto_stop_app_quit,
        ),
    )

    cfg.llm.base_url = env_var("LLM_BASE_URL", cfg.llm.base_url)
    cfg.llm.model = env_var("LLM_MODEL", cfg.llm.model)
    if api := env_var("LLM_API"):
        cfg.llm.api = normalize_api(api)
    cfg.llm.api_key = env_var("LLM_API_KEY") or cfg.llm.api_key or os.environ.get("DEEPSEEK_API_KEY", "")
    cfg.webhook = env_var("WEBHOOK", cfg.webhook)
    cfg.webhook_secret = env_var("WEBHOOK_SECRET", cfg.webhook_secret)
    return cfg


def write_example() -> Path:
    CONFIG_PATH.parent.mkdir(parents=True, exist_ok=True)
    if not CONFIG_PATH.exists():
        CONFIG_PATH.write_text(EXAMPLE, encoding="utf-8")
    return CONFIG_PATH


def _toml_str(s: str) -> str:
    # JSON 字符串的转义规则是 TOML 基本字符串的子集
    return json.dumps(s, ensure_ascii=False)


def _toml_bool(b: bool) -> str:
    return "true" if b else "false"


def save(cfg: Config) -> Path:
    """写回配置文件（图形界面的设置页用）。注释不保留，格式与桌面版（Rust）的 save 一致。"""
    words = ", ".join(_toml_str(w) for w in cfg.hotwords)
    text = (
        "# tingo 配置，由图形界面写入；也可以直接编辑。\n\n"
        "# 专有名词、人名、项目代号，提高识别准确率\n"
        f"hotwords = [{words}]\n\n"
        "[llm]\n"
        f"base_url = {_toml_str(cfg.llm.base_url)}\n"
        f"api_key = {_toml_str(cfg.llm.api_key)}\n"
        f"model = {_toml_str(cfg.llm.model)}\n"
        f"api = {_toml_str(cfg.llm.api)}\n"
        + (f"temperature = {float(cfg.llm.temperature)!r}\n" if cfg.llm.temperature is not None else "")
        + "\n"
        "[push]\n"
        f"webhook = {_toml_str(cfg.webhook)}\n"
        f"secret = {_toml_str(cfg.webhook_secret)}\n\n"
        "[obsidian]\n"
        f"vault = {_toml_str(cfg.obsidian.vault)}\n"
        f"folder = {_toml_str(cfg.obsidian.folder)}\n"
        f"auto = {_toml_bool(cfg.obsidian.auto)}\n"
        f"transcript = {_toml_bool(cfg.obsidian.transcript)}\n\n"
        "[ui]\n"
        f"language = {_toml_str(cfg.ui_language)}\n\n"
        "[minutes]\n"
        f"language = {_toml_str(cfg.minutes_language)}\n\n"
        "[asr]\n"
        f"language = {_toml_str(cfg.asr_language)}\n\n"
        "[record]\n"
        f"auto_stop_silence_min = {cfg.record.auto_stop_silence_min}\n"
        f"auto_stop_app_quit = {_toml_bool(cfg.record.auto_stop_app_quit)}\n\n"
        "[storage]\n"
        f"root = {_toml_str(cfg.storage_root)}\n"
    )
    CONFIG_PATH.parent.mkdir(parents=True, exist_ok=True)
    CONFIG_PATH.write_text(text, encoding="utf-8")
    CONFIG_PATH.chmod(0o600)  # 里面有 API key
    return CONFIG_PATH
