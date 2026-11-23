from datetime import date

import pytest

from tingo import summarize as sm
from tingo.config import LLMConfig
from tingo.transcript import Sentence, Transcript


def _tr(n=2):
    return Transcript(
        source="m.wav",
        duration_ms=90_000,
        sentences=[Sentence(i % 2, i * 1000, i * 1000 + 900, f"第{i}句话。") for i in range(n)],
    )


def test_strip_think_closed_and_partial():
    assert sm.strip_think("<think>想一想</think>\n# 标题") == "# 标题"
    assert sm.strip_think("<think>还在想", partial=True) == ""
    assert sm.strip_think("# 标题<thi", partial=True) == "# 标题"
    assert sm.strip_think("# 标题<thi") == "# 标题<thi"


def test_split_chunks_respects_limit():
    lines = [f"line{i:02d}" for i in range(10)]  # 每行 6 字符
    chunks = sm.split_chunks(lines, max_chars=20)
    assert all(len(c) <= 21 for c in chunks)
    assert "\n".join(chunks).splitlines() == lines


def test_meta_filled_by_program(monkeypatch):
    monkeypatch.setattr(
        sm,
        "_chat",
        lambda cfg, prompt, on_token=None, think=True, on_think=None: "```markdown\n# 周会\n\n## 一句话总结\n好\n```",
    )
    tr = _tr()
    tr.rename(["张三"])
    out = sm.summarize(tr, LLMConfig(api_key="x"), meeting_date=date(2026, 11, 2))
    assert out.startswith(
        "# 周会\n\n- 日期：2026-11-02（星期一）\n- 时长：01:30\n- 参会人：张三、说话人2\n\n## 一句话总结"
    )


def test_prompt_carries_date_and_transcript(monkeypatch):
    seen = {}

    def fake(cfg, prompt, on_token=None, think=True, on_think=None):
        seen["prompt"] = prompt
        return "# T"

    monkeypatch.setattr(sm, "_chat", fake)
    sm.summarize(_tr(), LLMConfig(api_key="x"), meeting_date=date(2026, 11, 6))
    assert "2026-11-06（星期五）" in seen["prompt"]
    assert "[00:01] 说话人2：第1句话。" in seen["prompt"]


def test_long_transcript_map_reduce(monkeypatch):
    prompts = []

    def fake(cfg, prompt, on_token=None, think=True, on_think=None):
        prompts.append(prompt)
        return "# 合并后" if "分段笔记" in prompt else "- 要点"

    monkeypatch.setattr(sm, "_chat", fake)
    stages = []
    out = sm.summarize(_tr(40), LLMConfig(api_key="x"), max_chars=100, on_stage=stages.append)
    assert len(prompts) > 2
    assert stages[-1] == "合并成纪要"
    assert "分段笔记" in prompts[-1] and "## 待办" in prompts[-1]
    assert out.startswith("# 合并后")


def test_missing_key_for_remote_raises():
    with pytest.raises(RuntimeError, match="API key"):
        sm._client(LLMConfig(base_url="https://api.deepseek.com", api_key=""))


def test_local_ollama_needs_no_key(monkeypatch):
    sm.is_ollama.cache_clear()
    monkeypatch.setattr(sm, "_ollama_show", lambda base_url, model: {"details": {"format": "gguf"}})
    cfg = LLMConfig(base_url="http://localhost:11434/v1", api_key="")
    assert sm._client(cfg).api_key == "ollama"
    assert sm.default_max_chars(cfg) < sm.default_max_chars(LLMConfig())
    sm.is_ollama.cache_clear()


def test_annotate_dates_resolves_relative_days():
    d = date(2026, 11, 2)  # 星期一
    assert sm.annotate_dates("下周五完成", d) == "下周五（11月13日）完成"
    assert sm.annotate_dates("下下周的周一合并", d) == "下下周的周一（11月16日）合并"
    assert sm.annotate_dates("这周三、下个星期二、下礼拜天", d) == (
        "这周三（11月4日）、下个星期二（11月10日）、下礼拜天（11月15日）"
    )
    assert sm.annotate_dates("明天和后天，大后天", d) == "明天（11月3日）和后天（11月4日），大后天（11月5日）"


def test_annotate_dates_leaves_ambiguous_and_annotated_text_alone():
    d = date(2026, 11, 2)
    assert sm.annotate_dates("周五再说，今天天气好", d) == "周五再说，今天天气好"
    assert sm.annotate_dates("下周五（11月13日）", d) == "下周五（11月13日）"


def test_annotate_dates_across_month_end():
    assert sm.annotate_dates("下周一", date(2026, 11, 29)) == "下周一（11月30日）"
    assert sm.annotate_dates("下下周一", date(2026, 11, 29)) == "下下周一（12月7日）"


def test_prompt_dates_are_annotated(monkeypatch):
    seen = {}
    monkeypatch.setattr(
        sm,
        "_chat",
        lambda cfg, prompt, on_token=None, think=True, on_think=None: seen.setdefault("p", prompt) and "# T",
    )
    tr = Transcript("m.wav", 1000, [Sentence(0, 0, 900, "下周五交付。")])
    sm.summarize(tr, LLMConfig(api_key="x"), meeting_date=date(2026, 11, 2))
    assert "下周五（11月13日）交付" in seen["p"]
    assert "下周：周一 11月9日" in seen["p"]


def test_max_chars_follows_ollama_context(monkeypatch):
    import httpx

    class Resp:
        status_code = 200

        def __init__(self, d):
            self.d = d

        def json(self):
            return self.d

    def serve(data):
        sm.is_ollama.cache_clear()
        monkeypatch.setattr(httpx, "post", lambda url, json, timeout: Resp(data))

    gguf = {"format": "gguf", "family": "qwen3"}
    local = LLMConfig(base_url="http://localhost:11434/v1", model="qwen3-8b-jiyao")
    serve({"parameters": "num_ctx    16384", "details": gguf})
    assert sm.ollama_context(local) == 16384
    assert sm.default_max_chars(local) == sm.LOCAL_CHUNK_CHARS  # 上下文够大也不切太大

    serve({"parameters": "", "details": gguf})
    assert sm.default_max_chars(local) == 4000  # 没设 num_ctx：按 Ollama 的保守默认

    # 本机中转连 /api/show 都仿了，但不是 gguf：按远程模型处理
    relay = LLMConfig(base_url="http://127.0.0.1:3425/v1", api_key="k", model="deepseek/deepseek-v4-flash")
    serve({"parameters": "", "details": {"format": "", "family": "magpie"}})
    assert not sm.is_ollama(relay.base_url, relay.model)
    assert sm.default_max_chars(relay) == 40000

    def boom(*a, **k):
        raise httpx.ConnectError("refused")

    sm.is_ollama.cache_clear()
    monkeypatch.setattr(httpx, "post", boom)
    assert sm.default_max_chars(local) == 40000  # 连不上就不当 Ollama
    assert sm.default_max_chars(LLMConfig(base_url="https://api.deepseek.com")) == 40000
    sm.is_ollama.cache_clear()


def _stream(monkeypatch, deltas):
    """假的流式客户端：每个 delta 是 ChoiceDelta 的字段（reasoning_content 等是供应商加的额外字段）"""
    from types import SimpleNamespace as NS

    from openai.types.chat.chat_completion_chunk import ChoiceDelta

    chunks = [NS(choices=[NS(delta=ChoiceDelta(**d))]) for d in deltas]
    client = NS(chat=NS(completions=NS(create=lambda **kw: iter(chunks))))
    monkeypatch.setattr(sm, "_client", lambda cfg: client)


def test_chat_reports_thinking_from_reasoning_fields(monkeypatch):
    _stream(
        monkeypatch,
        [
            {"role": "assistant", "content": None, "reasoning_content": "先看议题"},
            {"content": "", "reasoning_content": "再看待办"},
            {"reasoning": "OpenRouter 风格"},
            {"content": "# 标题"},
            {"content": "\n正文"},
        ],
    )
    thinking, tokens = [], []
    out = sm._chat(LLMConfig(api_key="x"), "p", tokens.append, on_think=thinking.append)
    assert thinking == [4, 8, 21]
    assert "".join(tokens) == out == "# 标题\n正文"


def test_chat_reports_thinking_inside_think_tag(monkeypatch):
    _stream(
        monkeypatch, [{"content": "<think>"}, {"content": "想一想"}, {"content": "</think>\n# T"}, {"content": "，好"}]
    )
    thinking, tokens = [], []
    out = sm._chat(LLMConfig(api_key="x"), "p", tokens.append, on_think=thinking.append)
    assert thinking == [7, 10]  # 正文出现前按 <think> 里累计的字数报
    assert out == "# T，好" and "".join(tokens) == out
    sm._chat(LLMConfig(api_key="x"), "p")  # 不传回调也能跑


def test_only_final_call_reports_thinking(monkeypatch):
    calls = []

    def fake(cfg, prompt, on_token=None, think=True, on_think=None):
        calls.append(on_think)
        if on_think:
            on_think(7)
        return "# 合并后" if "分段笔记" in prompt else "- 要点"

    monkeypatch.setattr(sm, "_chat", fake)
    seen = []
    sm.summarize(_tr(40), LLMConfig(api_key="x"), max_chars=100, on_think=seen.append)
    assert all(c is None for c in calls[:-1]) and calls[-1] is not None
    assert seen == [7]


def _en_tr(n=2):
    return Transcript(
        source="m.wav",
        duration_ms=120_000,
        sentences=[Sentence(i % 2, i * 1000, i * 1000 + 900, f"Sentence number {i}.") for i in range(n)],
    )


def _capture(monkeypatch, reply):
    calls = []

    def fake(cfg, prompt, on_token=None, think=True, on_think=None, **kw):
        calls.append((prompt, kw))
        return reply(prompt) if callable(reply) else reply

    monkeypatch.setattr(sm, "_chat", fake)
    return calls


def test_english_minutes_template_and_meta(monkeypatch):
    calls = _capture(monkeypatch, "```markdown\n# Weekly sync\n\n## Summary\nGood.\n```")
    tr = _en_tr()
    tr.rename(["Lin Zhou"])
    out = sm.summarize(tr, LLMConfig(api_key="x"), meeting_date=date(2026, 10, 28))
    assert out.startswith(
        "# Weekly sync\n\n- Date: 2026-10-28 (Wednesday)\n- Duration: 02:00\n- Participants: Lin Zhou, Speaker 2\n\n"
        "## Summary"
    )
    prompt, kw = calls[0]
    assert kw == {"system": sm.SYSTEM_EN}
    for heading in (
        "# <a short title>",
        "## Summary",
        "## Discussion\n### 1. <topic>",
        "## Decisions",
        "## Action Items\n| Item | Owner | Due | Source |\n|---|---|---|---|",
        "## Open Questions & Risks",
        '## Root Cause Analysis\n```fishbone\n{"problem":',
    ):
        assert heading in prompt
    assert "Meeting date: 2026-10-28 (Wednesday)" in prompt
    assert "- Next week: Mon Nov 2, Tue Nov 3, Wed Nov 4" in prompt
    assert "[00:01] Speaker 2: Sentence number 1." in prompt  # 没命名的说话人在英文提示词里叫 Speaker N
    assert "translating" not in prompt  # 转写本来就是英文
    assert tr.name_of(1) == "说话人2"  # 存的数据不变


def test_english_meta_without_title_or_speakers(monkeypatch):
    _capture(monkeypatch, "## Summary\nNothing.")
    out = sm.summarize(
        Transcript("m.wav", 0, []), LLMConfig(api_key="x"), meeting_date=date(2026, 11, 1), language="en"
    )
    assert out.startswith("# Meeting Minutes\n\n- Date: 2026-11-01 (Sunday)\n- Duration: 00:00\n- Participants: None\n")


def test_language_setting_and_translation_note(monkeypatch):
    calls = _capture(monkeypatch, "# T")
    # 中文转写、纪要选英文：英文模板，并要求翻译
    sm.summarize(_tr(), LLMConfig(api_key="x"), meeting_date=date(2026, 11, 2), language="en")
    prompt, kw = calls[-1]
    assert prompt.startswith(sm.TRANSLATE_TO_EN + "\n\n") and "## Action Items" in prompt
    assert "write the minutes in English, translating as needed" in prompt and kw == {"system": sm.SYSTEM_EN}
    assert "[00:01] Speaker 2: 第1句话。" in prompt
    # 英文转写、纪要选中文：中文模板，并要求翻译
    sm.summarize(_en_tr(), LLMConfig(api_key="x"), meeting_date=date(2026, 11, 2), language="zh")
    prompt, kw = calls[-1]
    assert prompt.startswith(sm.TRANSLATE_TO_ZH + "\n\n") and "## 待办" in prompt and kw == {}
    # auto：跟转写的语言，不加翻译要求；中文提示词与原来逐字相同
    sm.summarize(_tr(), LLMConfig(api_key="x"), meeting_date=date(2026, 11, 2))
    prompt, kw = calls[-1]
    assert kw == {} and prompt.startswith("下面是一场会议的语音转写")


def test_resolve_language():
    assert sm.resolve_language("auto", _tr()) == "zh"
    assert sm.resolve_language("auto", _en_tr()) == "en"
    assert sm.resolve_language("en", _tr()) == "en" and sm.resolve_language("zh", _en_tr()) == "zh"
    # 判断语言只看发言内容，不看"说话人N"这样的默认名
    short = Transcript("m.wav", 1000, [Sentence(i, i, i + 1, "OK") for i in range(5)])
    assert sm.resolve_language("auto", short) == "en"


def test_english_map_reduce_prompts_and_stages(monkeypatch):
    from tingo import i18n

    calls = _capture(monkeypatch, lambda p: "# Merged" if "Notes by part" in p else "- point")
    i18n.set_lang("en")  # 进度文字按界面语言
    stages = []
    out = sm.summarize(_en_tr(40), LLMConfig(api_key="x"), max_chars=200, on_stage=stages.append)
    assert stages[0] == f"Summarizing part 1/{len(calls) - 1}" and stages[-1] == "Merging into minutes"
    first, last = calls[0][0], calls[-1][0]
    assert first.startswith("Below is one part of the transcript of a long meeting (part 1/")
    assert "Notes by part:\n### Part 1\n- point" in last and "## Action Items" in last and "Transcript:" not in last
    assert all(kw == {"system": sm.SYSTEM_EN} for _, kw in calls)
    assert out.startswith("# Merged\n\n- Date:")
    i18n.set_lang("zh")
    stages.clear()
    sm.summarize(_en_tr(40), LLMConfig(api_key="x"), max_chars=200, on_stage=stages.append)
    assert stages[0].startswith("分段提炼 1/") and stages[-1] == "合并成纪要"


def test_calendar_hint_en():
    hint = sm.calendar_hint_en(date(2026, 10, 28))
    assert hint.splitlines()[1] == "- Today Oct 28 (Wednesday), tomorrow Oct 29, the day after tomorrow Oct 30"
    assert "- This week: Mon Oct 26, Tue Oct 27, Wed Oct 28, Thu Oct 29, Fri Oct 30, Sat Oct 31, Sun Nov 1" in hint
    assert "- The week after next: Mon Nov 9," in hint


class _SSE:
    """假的 httpx.stream 响应：按行给出 SSE，记下请求。"""

    def __init__(self, lines, status=200, body=b""):
        self.lines, self.status_code, self.body = lines, status, body
        self.calls = []

    def __call__(self, method, url, headers, json, timeout):
        self.calls.append({"method": method, "url": url, "headers": headers, "json": json})
        return self

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False

    def read(self):
        return self.body

    def iter_lines(self):
        yield from self.lines


def _event(kind, **fields):
    import json

    return "data: " + json.dumps({"type": kind, **fields}, ensure_ascii=False)


def _text(t):
    return _event("content_block_delta", index=1, delta={"type": "text_delta", "text": t})


ANTHROPIC = LLMConfig(
    base_url="https://api.anthropic.com/", api_key="sk-ant", model="claude-sonnet-4-5", api="anthropic"
)


def test_anthropic_chat_streams_text_and_thinking(monkeypatch):
    import httpx

    sse = _SSE(
        [
            "event: message_start",
            _event("message_start", message={"id": "m"}),
            "",
            _event("content_block_start", index=0, content_block={"type": "thinking", "thinking": ""}),
            _event("content_block_delta", index=0, delta={"type": "thinking_delta", "thinking": "先看议题"}),
            _event("content_block_delta", index=0, delta={"type": "thinking_delta", "thinking": "再看待办"}),
            _event("content_block_delta", index=0, delta={"type": "signature_delta", "signature": "sig"}),
            _event("ping"),
            "data: not json",
            _text("<think>x</think>"),
            _text("# 标题"),
            _text("\n正文"),
            _event("message_stop"),
            _text("停止之后的不要"),
        ]
    )
    monkeypatch.setattr(httpx, "stream", sse)
    thinking, tokens = [], []
    out = sm._chat(ANTHROPIC, "转写", tokens.append, think=False, on_think=thinking.append, system="系统")
    assert out == "".join(tokens) == "# 标题\n正文"
    assert thinking == [4, 8, 24]  # 正文里夹的 <think>…</think> 同 openai 协议一样计入思考字数

    (call,) = sse.calls
    assert call["method"] == "POST" and call["url"] == "https://api.anthropic.com/v1/messages"
    assert call["headers"]["x-api-key"] == "sk-ant" and call["headers"]["anthropic-version"] == "2023-06-01"
    assert "Authorization" not in call["headers"]
    assert call["json"] == {
        "model": "claude-sonnet-4-5",
        "max_tokens": 16000,
        "system": "系统",
        "messages": [{"role": "user", "content": "转写"}],
        "temperature": 0.2,
        "stream": True,
    }  # 没有 reasoning_effort：只对 openai 协议的本地 Ollama 生效


def test_anthropic_chat_errors(monkeypatch):
    import httpx

    monkeypatch.setattr(
        httpx,
        "stream",
        _SSE([_text("半截"), _event("error", error={"type": "overloaded_error", "message": "Overloaded"})]),
    )
    with pytest.raises(RuntimeError, match="Overloaded"):
        sm._chat(ANTHROPIC, "p")

    monkeypatch.setattr(httpx, "stream", _SSE([], status=401, body=b'{"error":{"message":"invalid x-api-key"}}'))
    with pytest.raises(RuntimeError, match="大模型返回 401：.*invalid x-api-key"):
        sm._chat(ANTHROPIC, "p")

    def refuse(*a, **k):
        raise httpx.ConnectError("refused")

    monkeypatch.setattr(httpx, "stream", refuse)
    with pytest.raises(RuntimeError, match="连不上大模型"):
        sm._chat(ANTHROPIC, "p")

    # 没有 key：与 openai 协议同样的提示；anthropic 不去探测 Ollama
    monkeypatch.setattr(sm, "_ollama_show", lambda *a: pytest.fail("anthropic 不该探测 Ollama"))
    with pytest.raises(RuntimeError, match="API key"):
        sm._chat(LLMConfig(base_url="http://localhost:11434", api_key="", api="anthropic"), "p")
    assert sm.default_max_chars(LLMConfig(base_url="http://localhost:11434", api="anthropic")) == 40000


def test_anthropic_models_url_and_headers(monkeypatch):
    import httpx

    calls = []

    class Resp:
        status_code = 200

        def raise_for_status(self):
            pass

        def json(self):
            return {
                "data": [{"id": "claude-sonnet-4-5"}, {"id": "claude-haiku-4-5"}, {"type": "model"}],
                "has_more": False,
            }

    def get(url, headers, timeout, params=None):
        assert params == {"limit": 1000}  # 默认一页只有 20 个
        calls.append((url, headers))
        return Resp()

    monkeypatch.setattr(httpx, "get", get)
    for base in (
        "https://api.anthropic.com",
        "https://api.anthropic.com/",
        "https://api.anthropic.com/v1",
        "https://api.anthropic.com/v1/",
    ):
        assert sm.anthropic_models(base, "sk-ant") == ["claude-haiku-4-5", "claude-sonnet-4-5"]
    assert {url for url, _ in calls} == {"https://api.anthropic.com/v1/models"}
    assert calls[0][1] == {"x-api-key": "sk-ant", "anthropic-version": "2023-06-01", "content-type": "application/json"}
    assert sm.anthropic_v1(" https://relay.example/anthropic ") == "https://relay.example/anthropic/v1"

    # 服务端列模型时连不上 / 非 200：返回 None
    from tingo import server

    assert server._list_models("https://api.anthropic.com", "sk-ant", "anthropic") == [
        "claude-haiku-4-5",
        "claude-sonnet-4-5",
    ]

    class Bad(Resp):
        status_code = 401

        def raise_for_status(self):
            raise httpx.HTTPStatusError("401", request=httpx.Request("GET", "https://x"), response=httpx.Response(401))

    monkeypatch.setattr(httpx, "get", lambda url, headers, timeout: Bad())
    assert server._list_models("https://api.anthropic.com", "bad", "anthropic") is None


def test_templates_ask_for_terms_block_last(monkeypatch):
    for template, marker in ((sm.MINUTES_PROMPT, "转写："), (sm.MINUTES_PROMPT_EN, "Transcript:")):
        text = template.format(meeting_date="D", transcript="T")
        head = text.split(marker)[0]
        assert head.index("```fishbone") < head.index('```terms\n{"terms": [{"term":')
        assert '{"terms": []}' in head and "30" in head
    # 长会议：合并提示词里同样要求
    calls = _capture(monkeypatch, lambda p: "# 合并" if "分段笔记" in p else "- 要点")
    sm.summarize(_tr(40), LLMConfig(api_key="x"), max_chars=200)
    assert "```terms" in calls[-1][0] and "原样输出 terms 代码块" in calls[-1][0]
    assert "```terms" not in calls[0][0]


def test_parse_speaker_guess_tolerates_wrapping_and_drops_bad_items():
    text = (
        "好的，结果如下：\n```json\n"
        '{"suggestions": [{"index": 1, "name": "  林舟  ", "evidence": "[00:30] 说话人1：林舟你说一下"},'
        ' {"index": 5, "name": "越界"}, {"index": -1, "name": "负数"}, {"index": true, "name": "布尔"},'
        ' {"index": "0", "name": "字符串"}, {"index": 0, "name": "   "}, {"index": 0, "name": 3},'
        ' {"index": 0, "name": "' + "王" * 30 + '"}, {"index": 1, "name": "重复"}]}\n```\n以上。'
    )
    assert sm.parse_speaker_guess(text, 2) == [
        {"index": 1, "name": "林舟", "evidence": "[00:30] 说话人1：林舟你说一下"},
        {"index": 0, "name": "王" * 20, "evidence": ""},
    ]
    assert sm.parse_speaker_guess('<think>{"x": 1}</think>{"suggestions": []}', 3) == []
    for bad in ("没有线索", "{不是 JSON}", '{"suggestions": {}}', "[1, 2]", '{"other": []}'):
        with pytest.raises(sm.GuessError):
            sm.parse_speaker_guess(bad, 2)


def test_guess_prompt_lists_speakers_and_truncates(monkeypatch):
    tr = _tr(4)
    tr.rename(["", "王总"])
    p = sm.guess_prompt(tr, en=False)
    assert "- 0：说话人1\n- 1：王总" in p and "[00:01] 王总：第1句话。" in p and "只输出 JSON" in p
    p = sm.guess_prompt(tr, en=True)
    assert "- 0: Speaker 1\n- 1: 王总" in p and "[00:00] Speaker 1: 第0句话。" in p and "Output only JSON" in p
    long = Transcript("m.wav", 0, [Sentence(0, 0, 1, "字" * 20000)])
    assert len(sm.guess_prompt(long, en=False).rsplit("转写：\n", 1)[1]) == sm.GUESS_MAX_CHARS + 1  # 末尾换行

    calls = []

    def fake(cfg, prompt, on_token=None, think=True, on_think=None, **kw):
        calls.append({"think": think, **kw})
        return '{"suggestions": [{"index": 0, "name": "林舟", "evidence": "我是林舟"}]}'

    monkeypatch.setattr(sm, "_chat", fake)
    assert sm.guess_speakers(tr, LLMConfig(api_key="x"), en=True) == [
        {"index": 0, "name": "林舟", "evidence": "我是林舟"}
    ]
    assert calls == [{"think": False, "system": sm.GUESS_SYSTEM_EN}]


def test_temperature_rejected_is_resent_without_it(monkeypatch):
    """issue #2：Kimi 只接受 temperature=1。模型因为温度回 400 时去掉 temperature 重发。"""
    from types import SimpleNamespace as NS

    import httpx
    import openai

    sent = []

    def create(**kw):
        sent.append(dict(kw))
        if "temperature" in kw:
            resp = httpx.Response(400, request=httpx.Request("POST", "https://api.moonshot.cn/v1/chat/completions"))
            raise openai.BadRequestError(
                "invalid temperature: only 1 is allowed for this model", response=resp, body=None
            )
        return iter([NS(choices=[NS(delta=NS(content="# 纪要"))])])

    monkeypatch.setattr(sm, "_client", lambda cfg: NS(chat=NS(completions=NS(create=create))))
    assert sm._chat(LLMConfig(api_key="x", model="kimi"), "p") == "# 纪要"
    assert [s.get("temperature") for s in sent] == [0.2, None] and "temperature" not in sent[1]

    # 设置里填了就先用填的
    sent.clear()
    sm._chat(LLMConfig(api_key="x", temperature=1.0), "p")
    assert sent[0]["temperature"] == 1.0

    # 别的 400 原样抛出、不重发

    def other(**kw):
        sent.append(kw)
        resp = httpx.Response(400, request=httpx.Request("POST", "https://x"))
        raise openai.BadRequestError("model not found", response=resp, body=None)

    sent.clear()
    monkeypatch.setattr(sm, "_client", lambda cfg: NS(chat=NS(completions=NS(create=other))))
    with pytest.raises(openai.BadRequestError, match="model not found"):
        sm._chat(LLMConfig(api_key="x"), "p")
    assert len(sent) == 1


def test_anthropic_temperature_rejected_is_resent_without_it(monkeypatch):
    import httpx

    bodies = []

    def stream(method, url, headers, json, timeout):
        bodies.append(dict(json))
        if "temperature" in json:
            return _SSE([], status=400, body=b'{"error":{"message":"temperature: only 1 is allowed"}}')
        return _SSE([_text("# 纪要"), _event("message_stop")])

    monkeypatch.setattr(httpx, "stream", stream)
    assert sm._chat(ANTHROPIC, "p") == "# 纪要"
    assert [b.get("temperature") for b in bodies] == [0.2, None]
