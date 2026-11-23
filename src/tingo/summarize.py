"""用任一 OpenAI 兼容接口（或 Anthropic Messages 接口）把转写整理成会议纪要（中文或英文）。"""

from __future__ import annotations

import functools
import re
import urllib.parse
from collections.abc import Callable
from datetime import date, timedelta

from . import i18n
from .config import DEFAULT_TEMPERATURE, LLMConfig
from .i18n import tr
from .transcript import Transcript, fmt_ts

SYSTEM = "你是一位严谨的会议秘书，擅长把口语化、带识别错误的会议转写整理成中国职场常用的会议纪要。"

MINUTES_PROMPT = """\
下面是一场会议的语音转写，格式为「[时间戳] 说话人：内容」。会议日期：{meeting_date}。

请整理成会议纪要，严格使用以下 Markdown 结构，不要输出其他内容：

# <根据内容拟一个会议标题>

## 一句话总结
<一句话>

## 议题与讨论
### 1. <议题>
- <要点>（[mm:ss]）

## 决议
- <已经拍板的事项>（[mm:ss]）

## 待办
| 事项 | 负责人 | 截止时间 | 出处 |
|---|---|---|---|
| <具体动作> | <人名或说话人> | <时间> | [mm:ss] |

## 待定问题与风险
- <没有结论的问题、提到的风险>

## 原因分析
```fishbone
{{"problem": "<被分析的问题，用问句>", "branches": [{{"title": "<一个原因，10 字以内>", "detail": "<一句话补充>", "refs": ["mm:ss", "mm:ss"]}}]}}
```

```terms
{{"terms": [{{"term": "<正确写法>", "variants": ["<转写里出现过的听错写法>"]}}]}}
```

要求：
1. 只依据转写内容，不编造。负责人或截止时间没有明确说出的，写"未明确"。
2. 转写有同音错字，结合上下文改正明显的错误；拿不准的保留原文。
3. 转写里的"下周五""明天"等已由程序标好具体日期，如"下周五（11月13日）"，照抄括号里的日期，不要自己推算。其他相对时间（如"十五号"）保留原话，并查上面的日期对照表在括号里写出具体日期。
4. 每条要点、决议、待办都附上出处时间戳，便于回听核对。
5. 某一节没有内容时写"无"，不要删掉这一节（「原因分析」除外，见第 7 条）。
6. 去掉寒暄和口头禅，语言简洁、书面化。
7. 只有会议里明确分析了某个问题的原因时，才写「原因分析」这一节：每个问题一个 fishbone 代码块（最多两个），每块 2～6 个分支，内容是合法 JSON。refs 填支撑这个原因的原话的时间戳，必须是上面转写里出现过的 [mm:ss]（不带方括号）；找不到原话支撑就留空数组，不要编。没有分析原因的会议，整节省略，不要写"无"。
8. 全文最后（鱼骨图之后）必须原样输出 terms 代码块，内容是合法 JSON：列出会上出现的人名、产品、项目、专有名词的正确写法（优先用术语表/热词里的写法），variants 写转写里出现过的听错写法（没有就空数组）；最多 30 个；没有就写 {{"terms": []}}。纪要正文一律用正确写法。

转写：
{transcript}
"""

CHUNK_PROMPT = """\
下面是一场长会议中的一段转写（第 {index}/{total} 段），格式为「[时间戳] 说话人：内容」。会议日期：{meeting_date}。

请提炼这一段的：讨论要点、已拍板的决议、待办（事项/负责人/截止时间）、待定问题与风险。
每条都附出处时间戳 [mm:ss]，只依据原文，不编造，用简洁的条目输出。

转写：
{transcript}
"""

REDUCE_PROMPT = """\
下面是一场长会议按时间顺序分段提炼的笔记。请把它们合并成一份完整的会议纪要，去重、按议题归并，保留出处时间戳。

{instructions}

分段笔记：
{notes}
"""


SYSTEM_EN = (
    "You are a meticulous meeting secretary who turns colloquial meeting transcripts, "
    "full of speech-recognition errors, into clear, professional meeting minutes."
)

MINUTES_PROMPT_EN = """\
Below is a speech transcript of a meeting, formatted as "[timestamp] speaker: text". Meeting date: {meeting_date}.

Turn it into meeting minutes using exactly the following Markdown structure, and output nothing else:

# <a short title>

## Summary
<one sentence>

## Discussion
### 1. <topic>
- <point> ([mm:ss])

## Decisions
- <what was decided> ([mm:ss])

## Action Items
| Item | Owner | Due | Source |
|---|---|---|---|
| <concrete action> | <name or speaker> | <date> | [mm:ss] |

## Open Questions & Risks
- <unresolved questions, risks raised>

## Root Cause Analysis
```fishbone
{{"problem": "<the problem analyzed, phrased as a question>", "branches": [{{"title": "<one cause, a few words>", "detail": "<one sentence>", "refs": ["mm:ss", "mm:ss"]}}]}}
```

```terms
{{"terms": [{{"term": "<correct spelling>", "variants": ["<misrecognized spelling seen in the transcript>"]}}]}}
```

Rules:
1. Use only what is in the transcript; do not make anything up. If an owner or due date was not stated explicitly, write "Not specified".
2. The transcript contains recognition errors such as homophones; fix obvious ones from context, and keep the original wording when unsure.
3. Chinese relative dates in the transcript such as "下周五" or "明天" have already been annotated by the program with the exact date, e.g. "下周五（11月13日）"; use that date (written in English, e.g. Nov 13) and do not work it out yourself. For other relative dates (e.g. "next Wednesday", "the 15th"), keep the original wording and add the exact date in parentheses using the date reference above, e.g. "next Wednesday (Nov 4)".
4. End every point, decision and action item with its source timestamp [mm:ss], so it can be checked against the recording.
5. If a section has no content, write "None" instead of removing it (except "Root Cause Analysis", see rule 7).
6. Drop small talk and filler words; keep the language concise and formal.
7. Write the "Root Cause Analysis" section only when the meeting explicitly analyzed the causes of a problem: one fishbone code block per problem (at most two), 2-6 branches each, valid JSON. refs are the timestamps of the statements supporting that cause and must be [mm:ss] values that appear in the transcript above (without the brackets); if nothing in the transcript supports it, use an empty array, do not invent one. If the meeting did not analyze causes, omit the whole section; do not write "None".
8. Always end the minutes with the terms code block exactly as shown (after everything else, including the fishbone blocks), as valid JSON: list the correct spelling of the people, products, projects and proper nouns mentioned in the meeting (prefer the spelling in the glossary / hotwords), and in variants the misrecognized spellings that appear in the transcript (an empty array if none); at most 30 entries; if there are none, write {{"terms": []}}. Always use the correct spelling in the minutes themselves.

Transcript:
{transcript}
"""

CHUNK_PROMPT_EN = """\
Below is one part of the transcript of a long meeting (part {index}/{total}), formatted as "[timestamp] speaker: text". Meeting date: {meeting_date}.

Extract from this part: discussion points, decisions made, action items (item / owner / due date), and open questions and risks.
End every entry with its source timestamp [mm:ss]. Use only what is in the transcript, do not make anything up, and output concise bullet points.

Transcript:
{transcript}
"""

REDUCE_PROMPT_EN = """\
Below are notes taken part by part, in chronological order, from a long meeting. Merge them into one complete set of meeting minutes: remove duplicates, group by topic, and keep the source timestamps.

{instructions}

Notes by part:
{notes}
"""

# 转写和纪要语言不同时加在提示词最前面
TRANSLATE_TO_EN = "The transcript is in Chinese; write the minutes in English, translating as needed."
TRANSLATE_TO_ZH = "转写是英文的，请用中文写纪要，需要时翻译。"


WEEKDAYS = "一二三四五六日"


def calendar_hint(d: date) -> str:
    """日期对照表。小模型算"下周五是几号"经常算错，直接给表让它查。"""

    def md(x: date) -> str:
        return f"{x.month}月{x.day}日"

    monday = d - timedelta(days=d.weekday())
    lines = [
        "日期对照表（每周从周一开始）：",
        f"- 今天 {md(d)}（星期{WEEKDAYS[d.weekday()]}），明天 {md(d + timedelta(1))}，后天 {md(d + timedelta(2))}",
    ]
    for name, offset in (("本周", 0), ("下周", 7), ("下下周", 14)):
        days = "、".join(f"周{WEEKDAYS[i]} {md(monday + timedelta(offset + i))}" for i in range(7))
        lines.append(f"- {name}：{days}")
    return "\n".join(lines)


WEEKDAYS_EN = ("Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday")
MONTHS_EN = ("Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec")


def calendar_hint_en(d: date) -> str:
    """英文模板用的日期对照表，内容与 calendar_hint 相同。"""

    def md(x: date) -> str:
        return f"{MONTHS_EN[x.month - 1]} {x.day}"

    monday = d - timedelta(days=d.weekday())
    today = f"- Today {md(d)} ({WEEKDAYS_EN[d.weekday()]}), tomorrow {md(d + timedelta(1))}"
    lines = ["Date reference (weeks start on Monday):", f"{today}, the day after tomorrow {md(d + timedelta(2))}"]
    for name, offset in (("This week", 0), ("Next week", 7), ("The week after next", 14)):
        days = ", ".join(f"{WEEKDAYS_EN[i][:3]} {md(monday + timedelta(offset + i))}" for i in range(7))
        lines.append(f"- {name}: {days}")
    return "\n".join(lines)


def meeting_day(d: date, en: bool = False) -> str:
    """元信息里的日期：2026-10-28（星期三） / 2026-10-28 (Wednesday)"""
    if en:
        return f"{d.isoformat()} ({WEEKDAYS_EN[d.weekday()]})"
    return f"{d.isoformat()}（星期{WEEKDAYS[d.weekday()]}）"


def resolve_language(setting: str, transcript: Transcript) -> str:
    """纪要语言：设置为 zh / en 时照办，auto 时跟转写的语言（见 i18n.detect）。"""
    return setting if setting in ("zh", "en") else i18n.detect(transcript.text())


_WEEK_OFFSET = {"下下": 14, "下": 7, "本": 0, "这": 0}
_DAY_INDEX = {"一": 0, "二": 1, "三": 2, "四": 3, "五": 4, "六": 5, "日": 6, "天": 6}
_WEEK_RE = re.compile(r"(下下|下|本|这)个?(?:周|星期|礼拜)的?(?:周|星期|礼拜)?([一二三四五六日天])(?![（(])")
_DAY_RE = re.compile(r"(大后天|后天|明天)(?![（(])")
_DAY_OFFSET = {"明天": 1, "后天": 2, "大后天": 3}


def annotate_dates(text: str, d: date) -> str:
    """在"下周五""下下周的周一""明天"后面标上具体日期，换算由程序完成，不靠模型。"""
    monday = d - timedelta(days=d.weekday())

    def week(m: re.Match) -> str:
        x = monday + timedelta(_WEEK_OFFSET[m.group(1)] + _DAY_INDEX[m.group(2)])
        return f"{m.group(0)}（{x.month}月{x.day}日）"

    def day(m: re.Match) -> str:
        x = d + timedelta(_DAY_OFFSET[m.group(1)])
        return f"{m.group(0)}（{x.month}月{x.day}日）"

    return _DAY_RE.sub(day, _WEEK_RE.sub(week, text))


def _api_key(cfg: LLMConfig) -> str:
    api_key = cfg.api_key or ("ollama" if _is_local(cfg) else "")
    if not api_key:
        raise RuntimeError(
            tr(
                "没有配置 LLM API key：运行 `tingo config` 查看配置方法，或设置环境变量 TINGO_LLM_API_KEY",
                "No LLM API key configured: run `tingo config` to see how, or set TINGO_LLM_API_KEY",
            )
        )
    return api_key


def _client(cfg: LLMConfig):
    from openai import OpenAI

    return OpenAI(base_url=cfg.base_url, api_key=_api_key(cfg))


def _is_local(cfg: LLMConfig) -> bool:
    # Ollama 只走 OpenAI 兼容协议；选了 anthropic 就不去探测
    return cfg.api != "anthropic" and is_ollama(cfg.base_url, cfg.model)


@functools.lru_cache(maxsize=16)
def is_ollama(base_url: str, model: str) -> bool:
    """是不是本机 Ollama 上的这个模型。

    不能只看地址是不是 127.0.0.1：本机也可能跑着别的 OpenAI 兼容中转，有的还仿了 Ollama 的
    /api/version、/api/tags、/api/show。以 /api/show 里真实的模型文件格式为准。
    """
    if urllib.parse.urlparse(base_url).hostname not in ("localhost", "127.0.0.1", "::1"):
        return False
    return _ollama_show(base_url, model) is not None


def _ollama_show(base_url: str, model: str) -> dict | None:
    import httpx

    root = re.sub(r"/v1/?$", "", base_url.rstrip("/"))
    try:
        r = httpx.post(f"{root}/api/show", json={"model": model}, timeout=5)
        data = r.json() if r.status_code == 200 else None
    except Exception:  # noqa: BLE001 - 不是 Ollama 或连不上
        return None
    # 有的中转连 /api/show 也仿了，但给不出真实的模型文件格式；Ollama 跑的本地模型是 gguf（或 safetensors 导入）
    fmt = (data or {}).get("details", {}).get("format") if isinstance(data, dict) else None
    return data if fmt in ("gguf", "safetensors") else None


CHARS_PER_TOKEN = 1.3  # 中文转写实测约 1.36 字/token，取保守值
RESERVED_TOKENS = 3500  # 提示词、模型思考和输出要留的位置
LOCAL_CHUNK_CHARS = 6000


def default_max_chars(cfg: LLMConfig) -> int:
    """一次交给模型的转写最多多少字，超过就分段提炼再合并。

    本地 Ollama 按模型实际的上下文长度（num_ctx）检查，上下文小就切得更碎：超过上下文时 Ollama 会悄悄截掉开头。
    远程模型普遍 64k 以上。
    """
    if not _is_local(cfg):
        return 40000
    ctx = ollama_context(cfg) or 4096  # 没显式设置 num_ctx 时按 Ollama 的保守默认值
    fits = int((ctx - RESERVED_TOKENS) * CHARS_PER_TOKEN)
    # 上下文够大也不超过 LOCAL_CHUNK_CHARS：实测 8B 模型一次读 1.3 万字，纪要里的待办从 14 条掉到 6 条
    return max(4000, min(LOCAL_CHUNK_CHARS, fits))


def ollama_context(cfg: LLMConfig) -> int | None:
    """问 Ollama 这个模型实际生效的上下文长度；问不到返回 None。"""
    data = _ollama_show(cfg.base_url, cfg.model) or {}
    m = re.search(r"num_ctx\s+(\d+)", data.get("parameters") or "")
    return int(m.group(1)) if m else None


class _Output:
    """把流式片段累计成回答：正文去掉 <think>…</think> 后报给 on_token，思考字数报给 on_think。"""

    def __init__(self, on_token: Callable[[str], None] | None, on_think: Callable[[int], None] | None):
        self.on_token, self.on_think = on_token, on_think
        self.raw = ""
        self.shown = 0
        self.thought = 0

    def think(self, text: str) -> None:
        if text:
            self.thought += len(text)
            if self.on_think:
                self.on_think(self.thought)

    def text(self, delta: str) -> None:
        if not delta:
            return
        self.raw += delta
        visible = strip_think(self.raw, partial=True)
        if len(visible) > self.shown:
            if self.on_token:
                self.on_token(visible[self.shown :])
            self.shown = len(visible)
        elif self.shown == 0 and self.raw.lstrip().startswith("<think>") and self.on_think:
            self.on_think(self.thought + len(self.raw))

    def result(self) -> str:
        return strip_think(self.raw).strip()


def _chat(
    cfg: LLMConfig,
    prompt: str,
    on_token: Callable[[str], None] | None = None,
    think: bool = True,
    on_think: Callable[[int], None] | None = None,
    system: str = SYSTEM,
) -> str:
    """流式调用 chat/completions（api = anthropic 时是 messages），返回去掉思考过程的完整回答。

    on_think(已思考字数)：推理模型先长时间思考，不报出来界面就像卡住了。
    思考内容有的放在 reasoning_content / reasoning 字段，有的夹在正文的 <think> 里，两种都算。
    """
    out = _Output(on_token, on_think)
    if cfg.api == "anthropic":
        _chat_anthropic(cfg, prompt, system, out)
        return out.result()
    extra: dict = {}
    if not think and _is_local(cfg):
        # Ollama 的 OpenAI 兼容接口用 reasoning_effort=none 关掉 Qwen3 等模型的思考；只对本地生效，远程接口未必认这个值
        extra["reasoning_effort"] = "none"
    import openai

    request: dict = {
        "model": cfg.model,
        "messages": [{"role": "system", "content": system}, {"role": "user", "content": prompt}],
        "temperature": temperature(cfg),
        "stream": True,
        "extra_body": extra or None,
    }
    client = _client(cfg)
    try:
        stream = client.chat.completions.create(**request)
    except openai.BadRequestError as e:
        if not drop_rejected_temperature(request, str(e)):
            raise
        stream = client.chat.completions.create(**request)
    for chunk in stream:
        if not chunk.choices:
            continue
        d = chunk.choices[0].delta
        reasoning = getattr(d, "reasoning_content", None)
        if not isinstance(reasoning, str):
            reasoning = getattr(d, "reasoning", None)
        if isinstance(reasoning, str):
            out.think(reasoning)
        out.text(d.content or "")
    return out.result()


def temperature(cfg: LLMConfig) -> float:
    """纪要默认用低温度，少发挥；有的模型只接受默认值（Kimi、OpenAI 的推理模型只认 1），设置里可以改。"""
    return DEFAULT_TEMPERATURE if cfg.temperature is None else cfg.temperature


def drop_rejected_temperature(request: dict, error: str) -> bool:
    """模型因为 temperature 回 400 时（只接受默认温度的模型），去掉 temperature 用模型自己的默认值再发一次。

    返回 True 表示去掉了、可以重发；已经去掉过或者不是温度的问题时返回 False。与桌面版的 send_llm 一致。
    """
    if "temperature" not in error.lower() or "temperature" not in request:
        return False
    del request["temperature"]
    return True


ANTHROPIC_VERSION = "2023-06-01"
ANTHROPIC_MAX_TOKENS = 16000


def anthropic_v1(base_url: str) -> str:
    """Anthropic 接口的 /v1 根：https://api.anthropic.com 和 https://api.anthropic.com/v1 都认。"""
    root = base_url.strip().rstrip("/")
    return root if root.endswith("/v1") else root + "/v1"


def anthropic_headers(api_key: str) -> dict[str, str]:
    return {"x-api-key": api_key, "anthropic-version": ANTHROPIC_VERSION, "content-type": "application/json"}


def anthropic_models(base_url: str, api_key: str, timeout: float = 15) -> list[str]:
    """GET {v1}/models，返回排好序的模型 id；连不上或返回非 200 时抛异常。"""
    import httpx

    # 默认一页只有 20 个，一次取全
    r = httpx.get(
        anthropic_v1(base_url) + "/models",
        params={"limit": 1000},
        headers=anthropic_headers(api_key),
        timeout=timeout,
    )
    r.raise_for_status()
    return sorted(m["id"] for m in r.json().get("data", []) if isinstance(m, dict) and m.get("id"))


def _chat_anthropic(cfg: LLMConfig, prompt: str, system: str, out: _Output) -> None:
    """Anthropic Messages API 的流式调用（SSE）：text_delta 是正文，thinking_delta 只计思考字数。"""
    import json

    import httpx

    key = _api_key(cfg)
    body = {
        "model": cfg.model,
        "max_tokens": ANTHROPIC_MAX_TOKENS,
        "system": system,
        "messages": [{"role": "user", "content": prompt}],
        "temperature": temperature(cfg),
        "stream": True,
    }
    url = cfg.base_url
    try:
        # 第二遍只在模型拒绝 temperature、去掉它重发时才会走到
        for _ in range(2):
            with httpx.stream(
                "POST",
                anthropic_v1(cfg.base_url) + "/messages",
                headers=anthropic_headers(key),
                json=body,
                timeout=httpx.Timeout(1800, connect=20),
            ) as r:
                if r.status_code >= 400:
                    text = r.read().decode("utf-8", "replace")[:300]
                    if r.status_code == 400 and drop_rejected_temperature(body, text):
                        continue
                    raise RuntimeError(
                        tr(
                            f"大模型返回 {r.status_code}：{text}",
                            f"The language model returned {r.status_code}: {text}",
                        )
                    )
                try:
                    for line in r.iter_lines():
                        if not line.startswith("data:"):
                            continue
                        try:
                            event = json.loads(line[len("data:") :].strip())
                        except ValueError:
                            continue
                        if not isinstance(event, dict):
                            continue
                        kind = event.get("type")
                        if kind == "content_block_delta":
                            delta = event.get("delta") or {}
                            if delta.get("type") == "text_delta":
                                out.text(delta.get("text") or "")
                            elif delta.get("type") == "thinking_delta":
                                out.think(delta.get("thinking") or "")
                        elif kind == "error":
                            msg = (event.get("error") or {}).get("message") or "unknown error"
                            raise RuntimeError(
                                tr(f"大模型返回错误：{msg}", f"The language model returned an error: {msg}")
                            )
                        elif kind == "message_stop":
                            break
                except httpx.HTTPError as e:
                    raise RuntimeError(
                        tr(f"读取大模型输出中断：{e}", f"Lost the connection while reading the model's output: {e}")
                    ) from e
            break
    except httpx.HTTPError as e:
        raise RuntimeError(tr(f"连不上大模型（{url}）：{e}", f"Couldn't reach the language model ({url}): {e}")) from e


def strip_think(text: str, partial: bool = False) -> str:
    """去掉推理模型（Qwen3、DeepSeek-R1 等）输出里的 <think>…</think>。

    partial=True 用于流式输出：未闭合的 <think> 以及末尾半截标签先不显示。
    """
    text = re.sub(r"<think>.*?</think>\s*", "", text, flags=re.DOTALL)
    if partial:
        open_at = text.find("<think>")
        if open_at != -1:
            text = text[:open_at]
        for i in range(len("<think>") - 1, 0, -1):
            if text.endswith("<think>"[:i]):
                return text[:-i]
    return text


def split_chunks(lines: list[str], max_chars: int) -> list[str]:
    chunks: list[str] = []
    buf: list[str] = []
    size = 0
    for line in lines:
        if buf and size + len(line) > max_chars:
            chunks.append("\n".join(buf))
            buf, size = [], 0
        buf.append(line)
        size += len(line) + 1
    if buf:
        chunks.append("\n".join(buf))
    return chunks


def summarize(
    transcript: Transcript,
    cfg: LLMConfig,
    meeting_date: date | None = None,
    max_chars: int | None = None,
    on_token: Callable[[str], None] | None = None,
    on_stage: Callable[[str], None] | None = None,
    on_think: Callable[[int], None] | None = None,
    language: str = "auto",
) -> str:
    """on_think(已思考字数) 只报最终成文那一次调用的思考进度；分段提炼不报。

    language：纪要语言（配置里的 [minutes] language），auto 跟转写的语言。
    on_stage 报的进度文字按界面语言。
    """
    meeting_date = meeting_date or date.today()
    en = resolve_language(language, transcript) == "en"
    spoken_en = i18n.detect(transcript.text()) == "en"
    note = ""
    if en != spoken_en:
        note = (TRANSLATE_TO_EN if en else TRANSLATE_TO_ZH) + "\n\n"
    minutes_prompt, chunk_prompt, reduce_prompt = (
        (MINUTES_PROMPT_EN, CHUNK_PROMPT_EN, REDUCE_PROMPT_EN) if en else (MINUTES_PROMPT, CHUNK_PROMPT, REDUCE_PROMPT)
    )
    # 英文纪要换系统提示；中文时不传，调用与原来完全一样
    extra: dict = {"system": SYSTEM_EN} if en else {}
    day = meeting_day(meeting_date, en)
    dated = f"{day}\n\n{(calendar_hint_en if en else calendar_hint)(meeting_date)}"
    max_chars = max_chars or default_max_chars(cfg)
    text = annotate_dates(transcript.to_prompt_text(en), meeting_date)

    if len(text) <= max_chars:
        prompt = note + minutes_prompt.format(meeting_date=dated, transcript=text)
        body = _chat(cfg, prompt, on_token, on_think=on_think, **extra)
    else:
        chunks = split_chunks(text.splitlines(), max_chars)
        notes = []
        for i, chunk in enumerate(chunks, 1):
            if on_stage:
                on_stage(tr(f"分段提炼 {i}/{len(chunks)}", f"Summarizing part {i}/{len(chunks)}"))
            prompt = note + chunk_prompt.format(index=i, total=len(chunks), meeting_date=dated, transcript=chunk)
            notes.append(
                (f"### Part {i}\n" if en else f"### 第 {i} 段\n")
                + _chat(
                    cfg,
                    prompt,
                    think=False,  # 分段提炼主要是摘条目，不需要思考；最后合并时再思考
                    **extra,
                )
            )
        if on_stage:
            on_stage(tr("合并成纪要", "Merging into minutes"))
        instructions = minutes_prompt.split("Transcript:" if en else "转写：")[0].format(
            meeting_date=dated, transcript=""
        )
        body = _chat(
            cfg,
            note + reduce_prompt.format(instructions=instructions, notes="\n\n".join(notes)),
            on_token,
            on_think=on_think,
            **extra,
        )

    return _with_meta(body, transcript, day, en)


def _with_meta(body: str, transcript: Transcript, day: str, en: bool = False) -> str:
    """会议时间、时长、参会人由程序填，不让模型猜。"""
    body = _strip_fence(body)
    names = [transcript.name_of(s, en) for s in transcript.speakers_in_order()]
    duration = fmt_ts(transcript.duration_ms)
    if en:
        meta = f"- Date: {day}\n- Duration: {duration}\n- Participants: {', '.join(names) or 'None'}"
    else:
        meta = f"- 日期：{day}\n- 时长：{duration}\n- 参会人：{'、'.join(names) or '无'}"
    title, rest = ("# Meeting Minutes" if en else "# 会议纪要"), body
    first, _, remainder = body.partition("\n")
    if first.startswith("# "):
        title, rest = first, remainder
    return f"{title}\n\n{meta}\n\n{rest.strip()}\n"


def _strip_fence(text: str) -> str:
    t = text.strip()
    if t.startswith("```"):
        t = t.split("\n", 1)[1] if "\n" in t else ""
        if t.rstrip().endswith("```"):
            t = t.rstrip()[:-3]
    return t.strip()


# ---------- 猜说话人名字 ----------

GUESS_MAX_CHARS = 12000  # 转写太长时只看开头这么多字：自我介绍、点名大多在前面
GUESS_NAME_MAX = 20

GUESS_SYSTEM = "你是一位细心的会议助理，只根据会议转写里的明确线索判断每位说话人是谁。"

GUESS_PROMPT = """\
下面是一场会议的语音转写，格式为「[时间戳] 说话人：内容」。转写按声音把人分开了，但只标了编号：

{speakers}

请根据明确线索推断每位说话人的名字或称呼（如"王总""小李"）：
- 线索只能是：自我介绍（"我是林舟"）、被别人点名称呼（"林舟你说一下"之后由这位说话人接话）、回应点名（"小李？""在"）等。
- 不能凭说话内容猜职位或身份；没有明确线索的说话人不要输出。
- name 写转写里出现的名字或称呼本身；evidence 照抄能证明的那一句原话，带时间戳和说话人。
- 转写有同音错字，名字明显听错时可以改正。

只输出 JSON，不要输出其他内容：
{{"suggestions": [{{"index": 0, "name": "<名字或称呼>", "evidence": "[mm:ss] 说话人N：<原话>"}}]}}
index 是上面列表里的编号。

转写：
{transcript}
"""

GUESS_SYSTEM_EN = (
    "You are a careful meeting assistant. You identify each speaker in a meeting transcript "
    "only from explicit clues in what was said."
)

GUESS_PROMPT_EN = """\
Below is a speech transcript of a meeting, formatted as "[timestamp] speaker: text". The transcript separates people by voice but only labels them with numbers:

{speakers}

Infer each speaker's name or form of address (e.g. "Mr. Wang", "Lisa") from explicit clues only:
- Valid clues are: self-introductions ("I'm Lin Zhou"), being addressed by name by someone else ("Lin, go ahead" followed by this speaker answering), answering a roll call, and the like.
- Do not guess job titles or roles from what someone talks about; leave out any speaker without an explicit clue.
- name is the name or form of address as it appears in the transcript; evidence quotes the line that proves it, with its timestamp and speaker.
- The transcript may contain recognition errors; you may correct a name that was clearly misheard.

Output only JSON and nothing else:
{{"suggestions": [{{"index": 0, "name": "<name or form of address>", "evidence": "[mm:ss] Speaker N: <quote>"}}]}}
index is the number from the list above.

Transcript:
{transcript}
"""


class GuessError(Exception):
    """大模型没有给出可用的结果。"""


def parse_speaker_guess(text: str, count: int) -> list[dict]:
    """解析猜名字的输出：容忍 ```json 包裹和前后多余文字（取第一个 { 到最后一个 }）。

    名字去空白、最长 GUESS_NAME_MAX 字；index 越界或不是整数的丢掉，同一个 index 只留第一条。
    整体解析不了时抛 GuessError。
    """
    import json

    text = strip_think(text)
    start, end = text.find("{"), text.rfind("}")
    if start < 0 or end <= start:
        raise GuessError(text)
    try:
        data = json.loads(text[start : end + 1])
    except ValueError as e:
        raise GuessError(text) from e
    items = data.get("suggestions") if isinstance(data, dict) else None
    if not isinstance(items, list):
        raise GuessError(text)
    out: list[dict] = []
    seen: set[int] = set()
    for item in items:
        if not isinstance(item, dict):
            continue
        index, name, evidence = item.get("index"), item.get("name"), item.get("evidence", "")
        if not isinstance(index, int) or isinstance(index, bool) or not 0 <= index < count or index in seen:
            continue
        if not isinstance(name, str) or not (name := name.strip()[:GUESS_NAME_MAX].strip()):
            continue
        seen.add(index)
        out.append({"index": index, "name": name, "evidence": evidence.strip() if isinstance(evidence, str) else ""})
    return out


def guess_prompt(transcript: Transcript, en: bool) -> str:
    """en 跟界面语言：英文界面时默认名叫 Speaker N。"""
    order = transcript.speakers_in_order()
    sep = ": " if en else "："
    speakers = "\n".join(f"- {i}{sep}{transcript.name_of(s, en)}" for i, s in enumerate(order))
    text = transcript.to_prompt_text(en)[:GUESS_MAX_CHARS]
    return (GUESS_PROMPT_EN if en else GUESS_PROMPT).format(speakers=speakers, transcript=text)


def guess_speakers(transcript: Transcript, cfg: LLMConfig, en: bool = False) -> list[dict]:
    """让大模型根据自我介绍、点名等线索猜说话人的名字：[{index, name, evidence}]。"""
    _api_key(cfg)  # 没配大模型时先报和生成纪要一样的错
    reply = _chat(cfg, guess_prompt(transcript, en), think=False, system=GUESS_SYSTEM_EN if en else GUESS_SYSTEM)
    return parse_speaker_guess(reply, len(transcript.speakers_in_order()))
