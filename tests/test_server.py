import io
from pathlib import Path

import numpy as np
import pytest
from fastapi.testclient import TestClient

from tingo import summarize as sm
from tingo.server import create_app
from tingo.transcript import Sentence, Transcript


@pytest.fixture
def root(tmp_path, monkeypatch):
    monkeypatch.setattr("tingo.config.CONFIG_PATH", tmp_path / "config.toml")
    for k in (
        "TINGO_LLM_API_KEY",
        "TINGO_LLM_BASE_URL",
        "TINGO_LLM_MODEL",
        "DEEPSEEK_API_KEY",
        "TINGO_WEBHOOK",
        "TINGO_LLM_API",
        "JIYAO_LLM_API",
    ):
        monkeypatch.delenv(k, raising=False)
    r = tmp_path / "meetings"
    m = r / "2026-11-02_1430_周会"
    m.mkdir(parents=True)
    tr = Transcript(
        str(m / "录音.m4a"), 5000, [Sentence(0, 0, 1000, "下周五上线。"), Sentence(1, 1500, 2500, "好的。")]
    )
    tr.save(m / "transcript.json")
    (m / "纪要.md").write_text("# 周会\n\n## 一句话总结\n定了上线时间。\n")
    (m / "录音.m4a").write_bytes(b"\0" * 2048)
    return r


@pytest.fixture
def client(root):
    return TestClient(create_app(root))


def wait_job(client, states=("done", "error")):
    import time

    for _ in range(200):
        job = client.get("/api/job").json()
        if job["state"] in states:
            return job
        time.sleep(0.02)
    raise AssertionError(job)


def test_list_and_detail(client):
    items = client.get("/api/meetings").json()
    assert [(m["id"], m["title"], m["summary"], m["time"]) for m in items] == [
        ("2026-11-02_1430_周会", "周会", "定了上线时间。", "14:30")
    ]
    d = client.get("/api/meetings/2026-11-02_1430_周会").json()
    assert [t["speaker"] for t in d["turns"]] == ["说话人1", "说话人2"]
    assert d["audio"] is True


def test_path_traversal_rejected(client):
    assert client.get("/api/meetings/..").status_code == 404
    assert client.get("/api/meetings/%2E%2E%2Fetc").status_code == 404


def test_rename_speakers_then_clear(client, root):
    d = client.put("/api/meetings/2026-11-02_1430_周会/speakers", json={"names": ["王总", ""]}).json()
    assert [s["name"] for s in d["speaker_list"]] == ["王总", "说话人2"]
    assert "**王总**" in (root / "2026-11-02_1430_周会" / "转写.md").read_text()
    d = client.put("/api/meetings/2026-11-02_1430_周会/speakers", json={"names": ["", ""]}).json()
    assert [s["name"] for s in d["speaker_list"]] == ["说话人1", "说话人2"]


def test_summarize_job_streams_and_saves(client, root, monkeypatch):
    monkeypatch.setenv("TINGO_LLM_API_KEY", "x")

    def fake_chat(cfg, prompt, on_token=None, think=True, on_think=None):
        assert "下周五（11月13日）" in prompt
        for part in ("# 新标题\n\n", "## 一句话总结\n", "改好了"):
            on_token(part)
        return "# 新标题\n\n## 一句话总结\n改好了"

    monkeypatch.setattr(sm, "_chat", fake_chat)
    assert client.post("/api/meetings/2026-11-02_1430_周会/summarize").status_code == 200
    job = wait_job(client)
    assert job["state"] == "done", job
    text = (root / "2026-11-02_1430_周会" / "纪要.md").read_text()
    assert text.startswith("# 新标题\n\n- 日期：2026-11-02（星期一）")
    versions = client.get("/api/meetings/2026-11-02_1430_周会/minutes/history").json()["versions"]
    assert len(versions) == 1
    assert "定了上线时间" in versions[0]["text"]


MID = "2026-11-02_1430_周会"
MID2 = "2026-11-03_0900_站会"


def wait_all(client, timeout=5):
    import time

    end = time.time() + timeout
    while time.time() < end:
        jobs = client.get("/api/jobs").json()["jobs"]
        if all(j["state"] in ("done", "error") for j in jobs):
            return jobs
        time.sleep(0.02)
    raise AssertionError(jobs)


@pytest.fixture
def second(root):
    m = root / MID2
    m.mkdir()
    Transcript(str(m / "录音.m4a"), 1000, [Sentence(0, 0, 900, "开始。")]).save(m / "transcript.json")
    return m


@pytest.fixture
def gated_chat(monkeypatch):
    """生成纪要卡在 gate 上，直到测试放行；放行前先报一次思考进度"""
    import threading

    monkeypatch.setenv("TINGO_LLM_API_KEY", "x")
    gate = threading.Event()

    def fake(cfg, prompt, on_token=None, think=True, on_think=None):
        if on_think:
            on_think(42)
        gate.wait(5)
        return "# T"

    monkeypatch.setattr(sm, "_chat", fake)
    yield gate
    gate.set()


def test_same_meeting_rejected_other_meeting_in_parallel(client, second, gated_chat):
    a = client.post(f"/api/meetings/{MID}/summarize")
    assert a.status_code == 200 and a.json()["id"].startswith("j")
    r = client.post(f"/api/meetings/{MID}/summarize")
    assert r.status_code == 409 and r.json()["detail"] == "这场会议正在处理，等它完成"
    assert client.post(f"/api/meetings/{MID}/process").status_code == 409
    b = client.post(f"/api/meetings/{MID2}/summarize")
    assert b.status_code == 200 and b.json()["id"] != a.json()["id"]
    jobs = client.get("/api/jobs").json()["jobs"]
    assert [(j["meeting"], j["state"]) for j in jobs] == [(MID, "summarizing"), (MID2, "summarizing")]
    gated_chat.set()
    assert [j["state"] for j in wait_all(client)] == ["done", "done"]
    # 结束之后同一场会议可以再来
    assert client.post(f"/api/meetings/{MID}/summarize").status_code == 200
    wait_all(client)


def test_jobs_shape_thinking_and_frozen_elapsed(client, gated_chat):
    import time

    empty = client.get("/api/jobs").json()
    assert empty["jobs"] == [] and isinstance(empty["model_loaded"], bool)  # 别的测试可能已经加载了真模型
    assert client.get("/api/job").json() == {"state": "idle"}
    jid = client.post(f"/api/meetings/{MID}/summarize").json()["id"]
    for _ in range(100):
        body = client.get("/api/jobs").json()
        if body["jobs"][0]["thinking"]:
            break
        time.sleep(0.02)
    assert set(body) == {"jobs", "model_loaded"}
    j = body["jobs"][0]
    assert j["id"] == jid and j["kind"] == "summarize" and j["state"] == "summarizing"
    assert (
        j["thinking"] == 42
        and j["finished_at"] == 0
        and j["elapsed"] >= 0
        and j["model_loaded"] == empty["model_loaded"]
    )
    assert client.get("/api/job").json()["id"] == jid  # 旧接口返回最近一个进行中的
    gated_chat.set()
    done = wait_all(client)[0]
    assert done["finished_at"] >= done["started_at"]
    assert done["elapsed"] == pytest.approx(done["finished_at"] - done["started_at"])
    time.sleep(0.05)
    assert client.get("/api/jobs").json()["jobs"][0]["elapsed"] == done["elapsed"]  # 结束后用时不再增长
    assert client.get("/api/job").json()["id"] == jid  # 没有进行中的，返回最近一个


def test_dismiss_one_and_all(client, second, gated_chat):
    gated_chat.set()
    a = client.post(f"/api/meetings/{MID}/summarize").json()["id"]
    wait_all(client)
    b = client.post(f"/api/meetings/{MID2}/summarize").json()["id"]
    wait_all(client)
    c = client.post(f"/api/meetings/{MID}/summarize").json()["id"]
    wait_all(client)
    assert client.post(f"/api/jobs/{b}/dismiss").json() == {"ok": True}
    assert [j["id"] for j in client.get("/api/jobs").json()["jobs"]] == [a, c]
    assert client.post("/api/jobs/nope/dismiss").status_code == 200
    gated_chat.clear()
    d = client.post(f"/api/meetings/{MID2}/summarize").json()["id"]
    # 进行中的任务收不起来
    client.post(f"/api/jobs/{d}/dismiss")
    client.post("/api/job/dismiss")
    assert [j["id"] for j in client.get("/api/jobs").json()["jobs"]] == [d]
    gated_chat.set()
    wait_all(client)
    client.post("/api/job/dismiss")
    assert client.get("/api/jobs").json()["jobs"] == []


def test_keeps_only_recent_finished_jobs(client, gated_chat):
    gated_chat.set()
    ids = []
    for _ in range(8):
        ids.append(client.post(f"/api/meetings/{MID}/summarize").json()["id"])
        wait_all(client)
    # 登记新任务时，结束了的超过 5 个就删掉最早的
    assert [j["id"] for j in client.get("/api/jobs").json()["jobs"]] == ids[-6:]


@pytest.fixture
def fake_recorder(monkeypatch):
    """不真的录音：录音组件、边录边转都换成假的；结束录音后的转写卡在 gate 上"""
    import threading

    from tingo import live, record

    gate = threading.Event()

    class Rec:
        def __init__(self, out, system_audio=True, mic=True):
            self.out, self.sources = out, ["mic"]

        def start(self):
            self.out.mkdir(parents=True)

        def stop_capture(self):
            pass

    class Live:
        error = None

        def __init__(self, out, sources, hotwords=None):
            self.hotwords, self.timings = hotwords, {}

        def start(self):
            pass

        def recent(self, n):
            return [{"start_ms": 0, "text": "大家好"}]

        def finish(self, final):
            gate.wait(5)
            return Transcript(str(final), 1000, [Sentence(0, 0, 900, "大家好。")])

    def mixdown(out):
        (out / "mix.m4a").write_bytes(b"\0")
        return out / "mix.m4a"

    monkeypatch.setattr(record, "Recording", Rec)
    monkeypatch.setattr(record, "system_audio_supported", lambda: False)
    monkeypatch.setattr(record, "track_level", lambda path, window_s=0.15: 0.5)
    monkeypatch.setattr(record, "mixdown", mixdown)
    monkeypatch.setattr(record, "running_meeting_apps", lambda: set())
    monkeypatch.setattr(live, "LiveTranscriber", Live)
    monkeypatch.setenv("TINGO_LLM_API_KEY", "x")
    monkeypatch.setattr(sm, "_chat", lambda cfg, prompt, on_token=None, think=True, on_think=None: "# 录音会")
    yield gate
    gate.set()


def test_second_recording_rejected_and_stop_frees_slot(client, fake_recorder):
    first = client.post("/api/record/start", json={"title": "评审"}).json()
    assert first["state"] == "recording" and first["sources"] == ["mic"]
    r = client.post("/api/record/start", json={"title": "评审"})
    assert r.status_code == 409 and r.json()["detail"] == "已经在录音了，先结束这一场"
    rec = client.get("/api/jobs").json()["jobs"][0]
    assert rec["levels"] == {"mic": 0.5} and rec["live"] == [{"start_ms": 0, "text": "大家好"}]
    assert client.get("/api/job").json()["id"] == first["id"]

    stopped = client.post("/api/record/stop").json()
    assert stopped["id"] == first["id"] and stopped["state"] == "saving"
    assert client.post("/api/record/stop").status_code == 400
    # 上一场还在后台转写，马上就能开下一场
    nxt = client.post("/api/record/start", json={"title": "评审"}).json()
    assert nxt["state"] == "recording" and nxt["meeting"] != first["meeting"]
    jobs = {j["id"]: j for j in client.get("/api/jobs").json()["jobs"]}
    assert jobs[first["id"]]["state"] in ("saving", "finalizing") and "levels" not in jobs[first["id"]]
    assert client.get("/api/job").json()["id"] == nxt["id"]  # 旧接口优先返回正在录音的

    fake_recorder.set()
    import time

    for _ in range(250):
        jobs = {j["id"]: j for j in client.get("/api/jobs").json()["jobs"]}
        if jobs[first["id"]]["state"] in ("done", "error"):
            break
        time.sleep(0.02)
    assert jobs[first["id"]]["state"] == "done", jobs[first["id"]]
    assert jobs[nxt["id"]]["state"] == "recording"


def test_edit_minutes(client, root):
    client.put("/api/meetings/2026-11-02_1430_周会/minutes", json={"text": "# 改过的\n"})
    assert (root / "2026-11-02_1430_周会" / "纪要.md").read_text() == "# 改过的\n"


def test_config_roundtrip_keeps_key_when_blank(client, tmp_path):
    body = {
        "base_url": "https://api.deepseek.com",
        "model": "deepseek-chat",
        "api_key": "sk-1234567890abcd",
        "hotwords": ["星河", " ", "Kafka"],
        "webhook": "",
        "secret": None,
    }
    c = client.put("/api/config", json=body).json()
    assert c["api_key_set"] and c["api_key_hint"] == "sk-…abcd"
    assert c["hotwords"] == ["星河", "Kafka"]
    c = client.put("/api/config", json={**body, "api_key": None, "model": "deepseek-reasoner"}).json()
    assert c["api_key_set"] and c["model"] == "deepseek-reasoner"
    assert oct((tmp_path / "config.toml").stat().st_mode)[-3:] == "600"


def test_import_rejects_unknown_type(client):
    r = client.post("/api/import", files={"file": ("a.txt", io.BytesIO(b"x"))})
    assert r.status_code == 400


def test_push_without_webhook(client):
    r = client.post("/api/meetings/2026-11-02_1430_周会/push")
    assert r.status_code == 400 and "webhook" in r.json()["detail"]


def test_raw_tracks_are_not_meetings(client, root):
    (root / "2026-11-02_1500_录音中").mkdir()
    (root / "2026-11-02_1500_录音中" / "system.wav").write_bytes(b"\0" * 2048)
    assert [m["id"] for m in client.get("/api/meetings").json()] == ["2026-11-02_1430_周会"]


def test_static_index_served(client):
    r = client.get("/")
    assert r.status_code == 200 and "纪要" in r.text


def test_llm_providers_reports_running_relays(client, monkeypatch):
    from tingo import server

    running = {"http://127.0.0.1:3425/v1": ["deepseek/deepseek-v4-flash", "minimax-cn/MiniMax-M3.1-Flash"]}
    monkeypatch.setattr(server, "_list_models", lambda base_url, api_key: running.get(base_url))
    got = {p["id"]: p for p in client.get("/api/llm/providers").json()}
    assert got["magpie"]["running"] and got["magpie"]["recommended"] in got["magpie"]["models"]
    assert not got["ccswitch"]["running"] and "15721" in got["ccswitch"]["base_url"]
    assert not got["ollama"]["running"]


def test_index_serves_built_frontend(client):
    """界面由 app/ 用 Vite 构建：资源文件名带内容哈希，首页本身不缓存。"""
    import re

    r = client.get("/")
    assets = re.findall(r'(?:src|href)="(/assets/[^"]+)"', r.text)
    assert assets, "首页应引用 Vite 构建出的资源"
    assert r.headers["cache-control"] == "no-cache"
    for a in assets:
        assert client.get(a).status_code == 200
    assert "cache-control" not in client.get("/api/meetings").headers


def test_cors_allows_tauri_and_vite_only(client):
    ok = client.get("/api/meetings", headers={"Origin": "tauri://localhost"})
    assert ok.headers.get("access-control-allow-origin") == "tauri://localhost"
    dev = client.get("/api/meetings", headers={"Origin": "http://localhost:1420"})
    assert dev.headers.get("access-control-allow-origin") == "http://localhost:1420"
    evil = client.get("/api/meetings", headers={"Origin": "https://evil.example"})
    assert "access-control-allow-origin" not in evil.headers


FISHBONE_MINUTES = (
    "# 周会\n\n## 原因分析\n```fishbone\n"
    '{"problem": "为什么延期？", "branches": [{"title": "上线定了", "refs": ["00:00"]}, {"title": "猜的"}]}'
    "\n```\n"
)


@pytest.fixture
def opened(monkeypatch):
    calls = []
    monkeypatch.setattr("tingo.server._open", lambda target, reveal=False: calls.append((target, reveal)))
    return calls


def test_config_obsidian_roundtrip_expands_home(client, tmp_path, monkeypatch):
    monkeypatch.setenv("HOME", str(tmp_path))
    c = client.get("/api/config").json()
    assert c["obsidian"] == {"vault": "", "folder": "会议纪要", "auto": True, "transcript": True}
    body = {"base_url": "https://api.deepseek.com", "model": "deepseek-chat", "hotwords": [], "webhook": ""}
    obs = {"vault": " ~/笔记 ", "folder": "/会议/", "auto": False, "transcript": True}
    c = client.put("/api/config", json={**body, "obsidian": obs}).json()
    assert c["obsidian"] == {"vault": f"{tmp_path}/笔记", "folder": "会议", "auto": False, "transcript": True}
    c = client.put("/api/config", json=body).json()  # 不带 obsidian 就不改
    assert c["obsidian"]["vault"] == f"{tmp_path}/笔记"
    assert "[obsidian]" in (tmp_path / "config.toml").read_text()


def test_obsidian_export_and_open(client, root, tmp_path, opened):
    mid = "2026-11-02_1430_周会"
    r = client.post(f"/api/meetings/{mid}/obsidian")
    assert r.status_code == 400 and "Obsidian 库" in r.json()["detail"]
    (root / mid / "纪要.md").write_text(FISHBONE_MINUTES)
    vault = tmp_path / "vault"
    vault.mkdir()
    from tingo import config

    cfg = config.load()
    cfg.obsidian.vault = str(vault)
    config.save(cfg)
    d = client.post(f"/api/meetings/{mid}/obsidian").json()
    assert d["ok"] and d["drawings"] == 1 and opened == []
    assert d["path"] == str(vault / "会议纪要" / "2026-11-02 周会.md")
    assert d["uri"].startswith("obsidian://open?path=") and "%E5%91%A8%E4%BC%9A.md" in d["uri"]
    d = client.post(f"/api/meetings/{mid}/obsidian", json={"open": True}).json()
    assert opened == [(d["uri"], False)]
    assert client.post("/api/meetings/nope/obsidian").status_code == 404


def test_excalidraw_export(client, root, opened):
    mid = "2026-11-02_1430_周会"
    r = client.post(f"/api/meetings/{mid}/excalidraw")
    assert r.status_code == 400 and r.json()["detail"] == "这份纪要里没有鱼骨图"
    (root / mid / "纪要.md").write_text(FISHBONE_MINUTES + "\n```fishbone\n坏的\n```\n" + FISHBONE_MINUTES[5:])
    d = client.post(f"/api/meetings/{mid}/excalidraw").json()
    assert [p.rsplit("/", 1)[1] for p in d["paths"]] == ["鱼骨图.excalidraw", "鱼骨图2.excalidraw"]
    assert opened == [(d["paths"][0], True)]
    import json

    drawing = json.loads((root / mid / "鱼骨图.excalidraw").read_text())
    texts = [e["text"] for e in drawing["elements"] if e["type"] == "text"]
    assert "为什么延期？" in texts and "上线定了" in texts
    assert "依据很少 · 引用 1 段 · 6 字" not in texts and "无原文依据" not in texts


def test_obsidian_vaults_endpoint(client, tmp_path, monkeypatch):
    monkeypatch.setenv("HOME", str(tmp_path))
    assert client.get("/api/obsidian/vaults").json() == []


def test_summarize_auto_syncs_to_obsidian(client, root, tmp_path, monkeypatch):
    from tingo import config

    monkeypatch.setenv("TINGO_LLM_API_KEY", "x")
    monkeypatch.setattr(sm, "_chat", lambda cfg, prompt, on_token=None, think=True, on_think=None: "# 新标题\n\n正文")
    cfg = config.load()
    cfg.obsidian.vault = str(tmp_path / "没有这个库")
    config.save(cfg)
    client.post("/api/meetings/2026-11-02_1430_周会/summarize")
    job = wait_job(client)
    assert job["state"] == "done" and job["notice"].startswith("没能同步到 Obsidian：还没有设置")
    client.post("/api/job/dismiss")
    (tmp_path / "没有这个库").mkdir()
    client.post("/api/meetings/2026-11-02_1430_周会/summarize")
    job = wait_job(client)
    assert job["notice"] == "纪要已同步到 Obsidian"
    assert (tmp_path / "没有这个库" / "会议纪要" / "2026-11-02 周会.md").exists()
    cfg.obsidian.auto = False
    config.save(cfg)
    client.post("/api/job/dismiss")
    client.post("/api/meetings/2026-11-02_1430_周会/summarize")
    assert wait_job(client)["notice"] == ""


def test_pdf_export(client, root, opened, monkeypatch):
    from tingo import record

    mid = "2026-11-02_1430_周会"
    calls = []

    def fake(html, out):
        calls.append(html)
        out.write_bytes(b"%PDF-1.4")

    monkeypatch.setattr(record, "html_to_pdf", fake)
    (root / mid / "纪要.md").write_text("# 周会：发布/排期\n\n## 一句话总结\n定了")
    d = client.post(f"/api/meetings/{mid}/pdf", json={"html": "<p>纪要</p>"}).json()
    assert d["path"].endswith("/周会：发布 排期.pdf") and calls == ["<p>纪要</p>"]
    assert opened == [(d["path"], True)]

    def broken(html, out):
        raise RuntimeError("排版超时")

    monkeypatch.setattr(record, "html_to_pdf", broken)
    r = client.post(f"/api/meetings/{mid}/pdf", json={"html": "<p></p>"})
    assert r.status_code == 500 and r.json()["detail"] == "排版超时"


def test_audio_served_with_standard_mime(client, root):
    mid = "2026-11-02_1430_周会"
    for f in (root / mid).glob("录音.*"):
        f.unlink()
    (root / mid / "录音.m4a").write_bytes(b"\0" * 64)
    r = client.get(f"/api/meetings/{mid}/audio")
    assert r.status_code == 200 and r.headers["content-type"] == "audio/mp4"


def test_ui_language_header_switches_messages(client):
    r = client.get("/api/meetings/nope", headers={"X-Tingo-Lang": "en"})
    assert r.status_code == 404 and r.json()["detail"] == "Meeting not found"
    # 不带请求头的请求（比如 <audio> 拉录音）沿用上一次的界面语言
    assert client.get("/api/meetings/nope").json()["detail"] == "Meeting not found"
    r = client.post(f"/api/meetings/{MID}/push", headers={"X-Tingo-Lang": "en"})
    assert r.status_code == 400 and r.json()["detail"] == "No group bot webhook is set; add one in Settings"
    providers = {p["id"]: p for p in client.get("/api/llm/providers").json()}
    assert providers["ollama"]["name"] == "Local Ollama" and "isn't running" in providers["ollama"]["hint"]
    r = client.get("/api/meetings/nope", headers={"X-Tingo-Lang": "zh"})
    assert r.json()["detail"] == "找不到这场会议"
    assert {p["id"]: p for p in client.get("/api/llm/providers").json()}["ollama"]["name"] == "本地 Ollama"


def test_job_messages_follow_ui_language(client, second, gated_chat):
    en = {"X-Tingo-Lang": "en"}
    assert client.post(f"/api/meetings/{MID}/summarize", headers=en).status_code == 200
    r = client.post(f"/api/meetings/{MID}/summarize", headers=en)
    assert r.status_code == 409 and r.json()["detail"] == "This meeting is being processed; wait for it to finish"
    gated_chat.set()
    wait_all(client)


def test_config_language_fields(client, tmp_path):
    c = client.get("/api/config").json()
    assert (c["ui_language"], c["minutes_language"], c["asr_language"]) == ("auto", "auto", "zh")
    body = {"base_url": "https://api.deepseek.com", "model": "deepseek-chat", "hotwords": [], "webhook": ""}
    c = client.put("/api/config", json={**body, "ui_language": "en", "minutes_language": "zh"}).json()
    assert (c["ui_language"], c["minutes_language"], c["asr_language"]) == ("en", "zh", "zh")
    c = client.put("/api/config", json={**body, "asr_language": "auto"}).json()  # 不传的不改
    assert (c["ui_language"], c["minutes_language"], c["asr_language"]) == ("en", "zh", "auto")
    text = (tmp_path / "config.toml").read_text()
    assert '[ui]\nlanguage = "en"\n\n[minutes]\nlanguage = "zh"\n\n[asr]\nlanguage = "auto"\n\n[record]\n' in text
    r = client.put("/api/config", json={**body, "minutes_language": "fr"}, headers={"X-Tingo-Lang": "en"})
    assert r.status_code == 400 and r.json()["detail"] == "Unsupported language: fr"


def test_config_temperature_set_clear_and_reject(client):
    body = {"base_url": "https://api.moonshot.cn/v1", "model": "kimi", "hotwords": [], "webhook": ""}
    assert client.get("/api/config").json()["temperature"] is None
    assert client.put("/api/config", json={**body, "temperature": 1}).json()["temperature"] == 1.0
    assert client.put("/api/config", json=body).json()["temperature"] == 1.0  # 不传就不改
    for bad in (3, "abc", -0.1, True):
        assert client.put("/api/config", json={**body, "temperature": bad}).status_code == 400
    assert client.put("/api/config", json={**body, "temperature": " 0.5 "}).json()["temperature"] == 0.5
    assert client.put("/api/config", json={**body, "temperature": ""}).json()["temperature"] is None


def test_minutes_language_setting_used_and_english_stages(client, root, monkeypatch):
    from tingo import config

    monkeypatch.setenv("TINGO_LLM_API_KEY", "x")
    seen = []

    def fake(cfg, prompt, on_token=None, think=True, on_think=None, **kw):
        seen.append(prompt)
        return "# New\n\n## Summary\nok"

    monkeypatch.setattr(sm, "_chat", fake)
    cfg = config.load()
    cfg.minutes_language = "en"
    config.save(cfg)
    client.post(f"/api/meetings/{MID}/summarize", headers={"X-Tingo-Lang": "en"})
    job = wait_job(client)
    assert job["state"] == "done", job
    assert seen[0].startswith(sm.TRANSLATE_TO_EN) and "[00:01] Speaker 2: 好的。" in seen[0]
    text = (root / MID / "纪要.md").read_text()
    assert text.startswith(
        "# New\n\n- Date: 2026-11-02 (Monday)\n- Duration: 00:05\n- Participants: Speaker 1, Speaker 2"
    )


def test_english_obsidian_notice(client, tmp_path, monkeypatch):
    from tingo import config

    monkeypatch.setenv("TINGO_LLM_API_KEY", "x")
    monkeypatch.setattr(sm, "_chat", lambda cfg, prompt, on_token=None, think=True, on_think=None: "# T")
    cfg = config.load()
    cfg.obsidian.vault = str(tmp_path / "missing")
    config.save(cfg)
    client.post(f"/api/meetings/{MID}/summarize", headers={"X-Tingo-Lang": "en"})
    assert wait_job(client)["notice"].startswith("Couldn't sync to Obsidian: No Obsidian vault is set")
    client.post("/api/job/dismiss")
    (tmp_path / "missing").mkdir()
    client.post(f"/api/meetings/{MID}/summarize")
    assert wait_job(client)["notice"] == "Minutes synced to Obsidian"


def test_excalidraw_export_english_minutes(client, root, opened):
    import json

    en = FISHBONE_MINUTES.replace("# 周会\n\n", "# Weekly\n\n## Summary\nok\n\n")
    (root / MID / "纪要.md").write_text(en + "\n" + en[en.index("```fishbone") :])
    d = client.post(f"/api/meetings/{MID}/excalidraw").json()
    assert [p.rsplit("/", 1)[1] for p in d["paths"]] == ["fishbone.excalidraw", "fishbone2.excalidraw"]
    drawing = json.loads((root / MID / "fishbone.excalidraw").read_text())
    texts = [e["text"] for e in drawing["elements"] if e["type"] == "text"]
    assert "上线定了" in texts
    assert "Thin evidence · 1 quote(s) · 6 chars" not in texts and "No evidence" not in texts
    assert "00:00 Speaker 1: 下周五上线。" not in texts


def test_meeting_card_reads_english_summary(client, root):
    (root / MID / "纪要.md").write_text("# Weekly\n\n- Date: 2026-11-02 (Monday)\n\n## Summary\nShip on Friday.\n")
    assert client.get("/api/meetings").json()[0]["summary"] == "Ship on Friday."


def test_config_api_roundtrip_and_invalid_rejected(client):
    body = {"base_url": "https://api.anthropic.com", "model": "claude-sonnet-4-5", "api_key": "sk-ant-x"}
    assert client.get("/api/config").json()["api"] == "openai"
    assert client.put("/api/config", json={**body, "api": " Anthropic "}).json()["api"] == "anthropic"
    assert client.put("/api/config", json=body).json()["api"] == "anthropic"  # 不传就不改
    r = client.put("/api/config", json={**body, "api": "gemini"})
    assert r.status_code == 400 and r.json()["detail"] == "不认识的接口协议：gemini"
    r = client.put("/api/config", json={**body, "api": "gemini"}, headers={"X-Tingo-Lang": "en"})
    assert r.status_code == 400 and r.json()["detail"] == "Unknown API protocol: gemini"
    assert client.get("/api/config").json()["api"] == "anthropic"  # 拒绝时不写入


def test_llm_models_and_test_pick_protocol(client, monkeypatch):
    from tingo import server

    seen = []

    def fake_anthropic(base_url, api_key, timeout=15):
        seen.append(("anthropic", base_url, api_key))
        return ["claude-haiku-4-5", "claude-sonnet-4-5"]

    def fake_list(base_url, api_key, api="openai"):
        seen.append((api, base_url, api_key))
        return fake_anthropic(base_url, api_key) if api == "anthropic" else ["gpt"]

    monkeypatch.setattr(server, "_list_models", fake_list)
    monkeypatch.setattr(server, "anthropic_models", fake_anthropic)
    saved = {
        "base_url": "https://api.anthropic.com",
        "model": "claude-sonnet-4-5",
        "api_key": "sk-ant",
        "api": "anthropic",
    }
    client.put("/api/config", json=saved)

    # 地址与已保存的一样：沿用已保存的协议；不一样：openai；显式给了：照办
    probe = {"base_url": saved["base_url"] + "/", "model": "claude-sonnet-4-5"}  # 末尾斜杠不影响比较
    assert client.post("/api/llm/models", json=probe).json()["models"] == ["claude-haiku-4-5", "claude-sonnet-4-5"]
    assert client.post("/api/llm/models", json={**probe, "base_url": "https://other/v1"}).json()["models"] == ["gpt"]
    other = {**probe, "base_url": "https://other", "api": "anthropic"}
    assert client.post("/api/llm/models", json=other).json()["ok"]
    assert [s[0] for s in seen] == ["anthropic", "anthropic", "openai", "anthropic", "anthropic"]

    seen.clear()
    assert client.post("/api/config/test", json=probe).json()["ok"]
    assert seen == [("anthropic", "https://api.anthropic.com/", "sk-ant")]
    r = client.post("/api/config/test", json={**probe, "model": "claude-x"}).json()
    assert not r["ok"] and "claude-haiku-4-5" in r["message"]


# ---------- 导出 SRT / Word ----------


def test_srt_export(client, root, opened):
    mid = "2026-11-02_1430_周会"
    client.put(f"/api/meetings/{mid}/speakers", json={"names": ["林舟", ""]})
    d = client.post(f"/api/meetings/{mid}/srt").json()
    assert d == {"ok": True, "path": str(root / mid / "周会.srt")} and opened == [(d["path"], True)]
    raw = (root / mid / "周会.srt").read_bytes()
    assert not raw.startswith(b"\xef\xbb\xbf") and b"\r" not in raw
    assert raw.decode() == (
        "1\n00:00:00,000 --> 00:00:01,000\n林舟：下周五上线。\n\n2\n00:00:01,500 --> 00:00:02,500\n说话人2：好的。\n"
    )
    # 英文界面；没有纪要时用文件夹里的会议名
    (root / mid / "纪要.md").unlink()
    client.post(f"/api/meetings/{mid}/srt", headers={"X-Tingo-Lang": "en"})
    assert "\nSpeaker 2: 好的。\n" in (root / mid / "周会.srt").read_text()
    (root / mid / "transcript.json").unlink()
    r = client.post(f"/api/meetings/{mid}/srt")
    assert r.status_code == 400 and r.json()["detail"] == "No transcript yet"
    assert client.post(f"/api/meetings/{mid}/srt", headers={"X-Tingo-Lang": "zh"}).json()["detail"] == "还没有转写"


def test_docx_export(client, root, opened, tmp_path, monkeypatch):
    from tingo import server

    mid = "2026-11-02_1430_周会"
    (root / mid / "纪要.md").write_text("# 周会：发布/排期\n\n## 一句话总结\n定了")
    seen = tmp_path / "seen.txt"
    fake = tmp_path / "textutil"
    # 假的 textutil：记下参数，把 HTML 原样当成 docx 写出去
    fake.write_text(f'#!/bin/sh\nprintf "%s\\n" "$@" > "{seen}"\ncat "$7" > "$6"\n')
    fake.chmod(0o755)
    monkeypatch.setattr(server, "TEXTUTIL", str(fake))
    d = client.post(f"/api/meetings/{mid}/docx", json={"html": "<p>纪要</p>"}).json()
    assert d == {"ok": True, "path": str(root / mid / "周会：发布 排期.docx")} and opened == [(d["path"], True)]
    assert (root / mid / "周会：发布 排期.docx").read_text() == "<p>纪要</p>"
    args = seen.read_text().splitlines()
    assert args[:5] == ["-convert", "docx", "-inputencoding", "UTF-8", "-output"] and args[6].endswith(".html")
    assert not Path(args[6]).exists()  # 临时 HTML 已删

    fake.write_text('#!/bin/sh\necho "坏了" >&2\nexit 3\n')
    r = client.post(f"/api/meetings/{mid}/docx", json={"html": "<p></p>"})
    assert r.status_code == 500 and r.json()["detail"] == "坏了"

    monkeypatch.setattr(server, "TEXTUTIL", str(tmp_path / "missing"))
    r = client.post(f"/api/meetings/{mid}/docx", json={"html": "<p></p>"})
    assert r.status_code == 500 and r.json()["detail"] == "导出 Word 只支持 macOS"
    r = client.post(f"/api/meetings/{mid}/docx", json={"html": "<p></p>"}, headers={"X-Tingo-Lang": "en"})
    assert r.json()["detail"] == "Word export is only available on macOS"


# ---------- 猜说话人名字 ----------


def test_guess_speakers_endpoint(client, root, monkeypatch):
    mid = "2026-11-02_1430_周会"
    url = f"/api/meetings/{mid}/speakers/guess"
    r = client.post(url)
    assert r.status_code == 400 and "没有配置 LLM API key" in r.json()["detail"]

    monkeypatch.setenv("TINGO_LLM_API_KEY", "x")
    prompts = []

    def fake(cfg, prompt, on_token=None, think=True, on_think=None, **kw):
        prompts.append(prompt)
        return '```json\n{"suggestions": [{"index": 1, "name": "林舟", "evidence": "[00:01] 说话人2：好的。"}, {"index": 9, "name": "x"}]}\n```'

    monkeypatch.setattr(sm, "_chat", fake)
    assert client.post(url).json() == {
        "suggestions": [{"index": 1, "name": "林舟", "evidence": "[00:01] 说话人2：好的。"}]
    }
    assert "[00:00] 说话人1：下周五上线。" in prompts[-1]
    client.post(url, headers={"X-Tingo-Lang": "en"})
    assert "[00:00] Speaker 1: 下周五上线。" in prompts[-1]

    monkeypatch.setattr(sm, "_chat", lambda cfg, prompt, **kw: "我猜不出来")
    r = client.post(url)
    assert r.status_code == 502 and r.json()["detail"] == "The model didn't return a usable result"
    r = client.post(url, headers={"X-Tingo-Lang": "zh"})
    assert r.json()["detail"] == "大模型没有给出可用的结果"

    def down(cfg, prompt, **kw):
        raise RuntimeError("连不上")

    monkeypatch.setattr(sm, "_chat", down)
    r = client.post(url)
    assert r.status_code == 502 and r.json()["detail"] == "连不上"
    (root / mid / "transcript.json").unlink()
    assert client.post(url).json()["detail"] == "还没有转写"


# ---------- 搜索 ----------


def _meeting(root, name, sentences, minutes="", names=None):
    m = root / name
    m.mkdir()
    Transcript(str(m / "录音.m4a"), 60000, [Sentence(*s) for s in sentences], names or {}).save(m / "transcript.json")
    if minutes:
        (m / "纪要.md").write_text(minutes)
    return m


def test_search_expands_terms_and_fuzzy(client, root):
    _meeting(
        root,
        "2026-11-03_0900_复盘",
        [(0, 12000, 13000, "历史中单要清理。"), (1, 14000, 15000, "历史工单很多。"), (1, 16000, 17000, "无关的话。")],
        "# 工单复盘\n\n## 一句话总结\n清理历史工单。\n\n```terms\n"
        '{"terms": [{"term": "历史工单", "variants": ["历史中单"]}]}\n```\n',
        {0: "林舟"},
    )
    _meeting(
        root,
        "2026-11-01_1000_周会",
        [(0, 1000, 2000, "历史攻单的事下周说。")]
        + [(1, 3000 + i, 3001 + i, f"历史攻单{i}") for i in range(4)]
        + [(0, 9000, 9001, "这里有历史工单。")],
    )
    _meeting(root, "2026-10-30_1000_老会", [(0, 1000, 2000, "什么都没说。")], "# 老会\n\n提到了历史工单。\n")
    d = client.get("/api/search", params={"q": " 历史中单 "}).json()
    assert d["q"] == "历史中单" and d["expanded"] == ["历史工单", "历史中单"]
    assert [(r["id"], r["minutes_hit"], len(r["hits"])) for r in d["results"]] == [
        ("2026-11-03_0900_复盘", True, 2),
        ("2026-11-01_1000_周会", False, 5),  # 精确命中的在前，名额没满再补模糊命中（和"历史中单"只差一个字）
        ("2026-10-30_1000_老会", True, 0),  # 只有纪要命中
    ]
    first = d["results"][0]
    assert (first["title"], first["date"], first["time"]) == ("工单复盘", "2026-11-03", "09:00")
    assert first["hits"] == [
        {"ms": 12000, "speaker": "林舟", "text": "历史中单要清理。", "match": "历史中单"},
        {"ms": 14000, "speaker": "说话人2", "text": "历史工单很多。", "match": "历史工单"},
    ]
    second = d["results"][1]["hits"]
    assert [h["match"] for h in second] == ["历史工单", "历史攻单", "历史攻单", "历史攻单", "历史攻单"]
    assert second[0]["ms"] == 9000 and [h["ms"] for h in second[1:]] == [1000, 3000, 3001, 3002]
    # 英文界面：没改名的说话人叫 Speaker N
    d = client.get("/api/search", params={"q": "历史工单"}, headers={"X-Tingo-Lang": "en"}).json()
    assert d["expanded"] == ["历史工单", "历史中单"] and d["results"][0]["hits"][1]["speaker"] == "Speaker 2"


def test_search_case_limits_and_empty(client, root):
    _meeting(root, "2026-11-03_0900_英文", [(0, i * 1000, i * 1000 + 500, f"We use KAFKA here {i}.") for i in range(8)])
    d = client.get("/api/search", params={"q": "kafka"}).json()
    assert d["expanded"] == ["kafka"]
    hits = d["results"][0]["hits"]
    assert len(hits) == 5 and hits[0]["match"] == "KAFKA" and hits[0]["speaker"] == "说话人1"
    # 3 个字以内的查询词不做模糊（"验证码"不能命中"验证手"）；代码块里的内容不算纪要命中
    _meeting(root, "2026-11-04_0900_验证", [(0, 0, 1, "先验证手机号。")])
    assert client.get("/api/search", params={"q": "验证码"}).json()["results"] == []
    (root / "2026-11-02_1430_周会" / "纪要.md").write_text('# 周会\n```fishbone\n{"problem": "上限"}\n```\n')
    d = client.get("/api/search", params={"q": "上限"}).json()
    assert d["results"] == []
    # 纪要正文模糊命中也算
    (root / "2026-11-02_1430_周会" / "纪要.md").write_text("# 周会\n\n清理历史攻单。\n")
    d = client.get("/api/search", params={"q": "历史工单"}).json()
    assert [(r["id"], r["minutes_hit"], r["hits"]) for r in d["results"]] == [("2026-11-02_1430_周会", True, [])]
    for i in range(35):
        _meeting(root, f"2026-01-{i % 28 + 1:02d}_{1000 + i}_m{i}", [(0, 0, 1, "上线")])
    d = client.get("/api/search", params={"q": "上线"}).json()
    assert len(d["results"]) == 30 and d["results"][0]["id"] == "2026-11-02_1430_周会"
    assert client.get("/api/search", params={"q": "  "}).status_code == 400
    assert client.get("/api/search").status_code == 400


# ---------- 配置：自动结束录音 ----------


def test_config_record_fields_and_validation(client, tmp_path):
    body = {"base_url": "https://api.deepseek.com", "model": "deepseek-chat", "hotwords": [], "webhook": ""}
    c = client.get("/api/config").json()
    assert (c["auto_stop_silence_min"], c["auto_stop_app_quit"]) == (10, True)
    c = client.put("/api/config", json={**body, "auto_stop_silence_min": 0, "auto_stop_app_quit": False}).json()
    assert (c["auto_stop_silence_min"], c["auto_stop_app_quit"]) == (0, False)
    c = client.put("/api/config", json={**body, "auto_stop_silence_min": 240}).json()  # 不传的不改
    assert (c["auto_stop_silence_min"], c["auto_stop_app_quit"]) == (240, False)
    assert (
        (tmp_path / "config.toml")
        .read_text()
        .endswith('\n\n[record]\nauto_stop_silence_min = 240\nauto_stop_app_quit = false\n\n[storage]\nroot = ""\n')
    )
    for bad in (241, -1, 1.5, "10", True):
        r = client.put("/api/config", json={**body, "auto_stop_silence_min": bad})
        assert r.status_code == 400 and "0～240" in r.json()["detail"], bad
    r = client.put("/api/config", json={**body, "auto_stop_app_quit": "yes"})
    assert r.status_code == 400
    assert client.get("/api/config").json()["auto_stop_silence_min"] == 240


# ---------- 示例会议 ----------


def test_demo_copies_sample_once(client, root):
    from tingo.server import SAMPLE_DIR

    d = client.post("/api/demo").json()
    assert d == {"id": "2026-10-28_1400_示例·注册转化复盘"}
    m = root / d["id"]
    assert sorted(p.name for p in m.iterdir()) == sorted(["transcript.json", "纪要.md", "录音.m4a"])
    assert (m / "纪要.md").read_bytes() == (SAMPLE_DIR / "纪要.md").read_bytes()
    assert (m / "录音.m4a").read_bytes() == (SAMPLE_DIR / "录音.m4a").read_bytes()
    (m / "纪要.md").write_text("# 改过的\n")
    assert client.post("/api/demo").json() == d  # 已存在：不覆盖
    assert (m / "纪要.md").read_text() == "# 改过的\n"
    detail = client.get(f"/api/meetings/{d['id']}").json()
    assert detail["has_transcript"] and detail["audio"] and len(detail["speaker_list"]) == 4

    en = client.post("/api/demo", headers={"X-Tingo-Lang": "en"}).json()
    assert en == {"id": "2026-10-28_1400_Sample_·_Signup_conversion_review"}
    assert (root / en["id"] / "纪要.md").read_bytes() == (SAMPLE_DIR / "纪要.en.md").read_bytes()
    assert not (root / en["id"] / "纪要.en.md").exists()
    ids = [x["id"] for x in client.get("/api/meetings").json()]
    assert d["id"] in ids and en["id"] in ids


def test_demo_into_missing_root(tmp_path, monkeypatch):
    monkeypatch.setattr("tingo.config.CONFIG_PATH", tmp_path / "config.toml")
    c = TestClient(create_app(tmp_path / "new" / "root"))
    d = c.post("/api/demo").json()
    assert (tmp_path / "new" / "root" / d["id"] / "transcript.json").exists()


# ---------- 试录 ----------


def test_record_test_endpoint(client, fake_recorder, monkeypatch):
    from tingo import record

    calls = []

    def probe(seconds):
        calls.append(seconds)
        return {"system": "silent", "mic": "ok", "message": ""}

    monkeypatch.setattr(record, "probe_devices", probe)
    assert client.post("/api/record/test", json={"seconds": 2}).json() == {
        "system": "silent",
        "mic": "ok",
        "message": "系统声音：几乎没声音；麦克风：正常",
    }
    r = client.post("/api/record/test", json={}, headers={"X-Tingo-Lang": "en"}).json()
    assert r["message"] == "System audio: almost silent; microphone: OK" and calls == [2, 3]
    for bad in (0, 11, "3", True, None):
        assert client.post("/api/record/test", json={"seconds": bad}).status_code == 400, bad
    monkeypatch.setattr(record, "probe_devices", lambda s: {"system": "denied", "mic": "error", "message": "没权限"})
    assert client.post("/api/record/test", json={"seconds": 1}).json()["message"] == "没权限"
    client.post("/api/record/start", json={"title": "评审"})
    r = client.post("/api/record/test", json={"seconds": 1}, headers={"X-Tingo-Lang": "zh"})
    assert r.status_code == 409 and r.json()["detail"] == "正在录音，先结束录音再测试"


# ---------- 自动结束录音 ----------


def test_recording_job_reports_silence(client, fake_recorder, tmp_path):
    body = {"base_url": "https://api.deepseek.com", "model": "deepseek-chat", "hotwords": [], "webhook": ""}
    client.post("/api/record/start", json={"title": "评审"})
    rec = client.get("/api/jobs").json()["jobs"][0]
    assert rec["silent_s"] == 0 and rec["auto_stop_s"] == 600
    client.post("/api/record/stop")
    jobs = client.get("/api/jobs").json()["jobs"]
    assert "silent_s" not in jobs[0] and "auto_stop_s" not in jobs[0]
    # 关了自动结束：照样给 silent_s，不给 auto_stop_s
    client.put("/api/config", json={**body, "auto_stop_silence_min": 0})
    client.post("/api/record/start", json={"title": "评审2"})
    rec = next(j for j in client.get("/api/jobs").json()["jobs"] if j["state"] == "recording")
    assert rec["silent_s"] == 0 and "auto_stop_s" not in rec


@pytest.fixture
def fast_watch(monkeypatch):
    from tingo import record

    monkeypatch.setattr(record, "CHECK_S", 0.02)
    monkeypatch.setattr(record, "SECONDS_PER_MIN", 0.1)
    monkeypatch.setattr(record, "KEEP_AFTER_VOICE_S", 1.0)


def _wait_recording_done(client, job_id, timeout=5):
    import time

    end = time.time() + timeout
    while time.time() < end:
        job = next(j for j in client.get("/api/jobs").json()["jobs"] if j["id"] == job_id)
        if job["state"] != "recording":
            return job
        time.sleep(0.02)
    raise AssertionError(job)


def test_auto_stop_after_silence_truncates_tracks(client, root, fake_recorder, fast_watch, monkeypatch):
    import soundfile as sf

    from tingo import record

    body = {"base_url": "https://api.deepseek.com", "model": "deepseek-chat", "hotwords": [], "webhook": ""}
    client.put("/api/config", json={**body, "auto_stop_silence_min": 2})
    monkeypatch.setattr(record, "track_level", lambda path, window_s=0.15: 0.01)  # 低于 0.04：没人说话
    real_start = record.Recording.start

    def start(self):
        real_start(self)
        sf.write(self.out / "mic.wav", np.zeros(48000 * 3, dtype=np.float32), 48000, subtype="FLOAT")

    monkeypatch.setattr(record.Recording, "start", start)
    job = client.post("/api/record/start", json={"title": "评审"}).json()
    stopped = _wait_recording_done(client, job["id"])
    assert stopped["notice"] == "2 分钟没人说话，已自动结束录音"
    fake_recorder.set()
    done = wait_all(client)[0]
    assert done["state"] == "done" and done["notice"] == "2 分钟没人说话，已自动结束录音"
    assert sf.info(root / job["meeting"] / "mic.wav").frames == 48000  # 截到最后有声 + 1 秒（测试里改小）
    assert (root / job["meeting"] / "纪要.md").exists()


def test_auto_stop_when_meeting_app_quits(client, root, fake_recorder, fast_watch, monkeypatch):
    from tingo import record

    running = [{"zoom.us"}]
    monkeypatch.setattr(record, "running_meeting_apps", lambda: running[0])
    job = client.post("/api/record/start", json={"title": "评审"}).json()
    import time

    time.sleep(0.1)
    assert next(j for j in client.get("/api/jobs").json()["jobs"])["state"] == "recording"  # 还开着：不结束
    running[0] = set()
    stopped = _wait_recording_done(client, job["id"])
    assert stopped["notice"] == "会议软件已退出，已自动结束录音"
    fake_recorder.set()
    assert wait_all(client)[0]["state"] == "done"
    # 开始时没有会议软件在运行：不会因为"退出"而结束
    nxt = client.post("/api/record/start", json={"title": "评审2"}).json()
    time.sleep(0.1)
    assert next(j for j in client.get("/api/jobs").json()["jobs"] if j["id"] == nxt["id"])["state"] == "recording"


def test_auto_stop_does_not_touch_next_recording(client, fake_recorder, fast_watch, monkeypatch):
    """用户手动结束后又开了下一场：上一场的检查线程不能把下一场结束掉"""
    from tingo import record

    monkeypatch.setattr(record, "SECONDS_PER_MIN", 1000.0)
    first = client.post("/api/record/start", json={"title": "一"}).json()
    client.post("/api/record/stop")
    nxt = client.post("/api/record/start", json={"title": "二"}).json()
    import time

    time.sleep(0.1)
    jobs = {j["id"]: j for j in client.get("/api/jobs").json()["jobs"]}
    assert jobs[nxt["id"]]["state"] == "recording" and jobs[first["id"]]["notice"] == ""


def test_minutes_history_edit_and_delete(client):
    base = f"/api/meetings/{MID}/minutes"
    assert client.get(base + "/history").json() == {"versions": []}
    assert client.put(base, json={"text": "新版"}).status_code == 200
    assert client.put(base, json={"text": "新版"}).status_code == 200
    versions = client.get(base + "/history").json()["versions"]
    assert len(versions) == 1
    assert "定了上线时间" in versions[0]["text"]
    assert client.delete(base + "/history/not-valid").status_code == 400
    assert client.delete(base + "/history/" + versions[0]["id"]).status_code == 200
    assert client.get(base + "/history").json() == {"versions": []}
    assert client.get(f"/api/meetings/{MID}").json()["minutes"] == "新版\n"


def test_minutes_history_archive_failure_preserves_current(tmp_path):
    from tingo import minutes_history

    minutes_history.save(tmp_path, "原文")
    (tmp_path / "minutes-history").write_text("blocked")
    with pytest.raises(OSError):
        minutes_history.save(tmp_path, "新文")
    assert (tmp_path / "纪要.md").read_text() == "原文"
    assert not (tmp_path / ".minutes-new.tmp").exists()


def test_history_exports_and_push_use_selected_version(client, root, opened, tmp_path, monkeypatch):
    from tingo import config, record

    path = root / MID
    old = (path / "纪要.md").read_text()
    client.put(f"/api/meetings/{MID}/minutes", json={"text": "# 当前新版\n新内容"})
    version = client.get(f"/api/meetings/{MID}/minutes/history").json()["versions"][0]["id"]
    base = f"/api/meetings/{MID}"
    cfg = config.load()
    cfg.webhook = "https://open.feishu.cn/open-apis/bot/v2/hook/test"
    cfg.obsidian.vault = str(tmp_path / "vault")
    Path(cfg.obsidian.vault).mkdir()
    config.save(cfg)
    sent = []
    monkeypatch.setattr("tingo.push.send", lambda webhook, text, secret: sent.append(text))
    assert client.post(base + f"/push?version={version}").status_code == 200
    assert sent == [old]
    assert client.post(base + "/push?version=999").status_code == 404
    assert sent == [old]
    monkeypatch.setattr(record, "html_to_pdf", lambda html, out: out.write_text(html))
    pdf = client.post(base + f"/pdf?version={version}", json={"html": "old pdf"}).json()
    assert f"-history-{version}.pdf" in pdf["path"]
    assert Path(pdf["path"]).read_text() == "old pdf"
    assert client.post(base + "/pdf?version=bad", json={"html": "bad"}).status_code == 404
    current_note = client.post(base + "/obsidian").json()["path"]
    old_note = client.post(base + f"/obsidian?version={version}").json()["path"]
    assert current_note != old_note and f"-history-{version}" in old_note
    assert "当前新版" in Path(current_note).read_text()
    assert "定了上线时间" in Path(old_note).read_text()
    assert "当前新版" in (path / "纪要.md").read_text()
