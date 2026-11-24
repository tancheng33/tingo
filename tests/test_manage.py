"""会议管理：meta.json（标题、标签、归档）、移到废纸篓、批量操作、存储空间统计和清理。"""

import json
import stat
import threading
from datetime import date, timedelta
from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from tingo import meetings, server
from tingo.server import create_app
from tingo.transcript import Sentence, Transcript

WEEKLY = "2026-11-02_1430_周会"
OLD = "2020-01-05_0900_老会"


def make_meeting(root: Path, name: str, audio: bytes | None = b"\0" * 2048, minutes: str = "") -> Path:
    m = root / name
    m.mkdir(parents=True)
    Transcript(str(m / "录音.m4a"), 5000, [Sentence(0, 0, 1000, "下周五上线。")]).save(m / "transcript.json")
    if minutes:
        (m / "纪要.md").write_text(minutes, encoding="utf-8")
    if audio is not None:
        (m / "录音.m4a").write_bytes(audio)
    return m


@pytest.fixture
def root(tmp_path, monkeypatch):
    monkeypatch.setattr("tingo.config.CONFIG_PATH", tmp_path / "config.toml")
    r = tmp_path / "meetings"
    make_meeting(r, WEEKLY, minutes="# 周会\n\n## 一句话总结\n定了上线时间。\n")
    make_meeting(r, OLD, audio=b"\0" * 100)
    return r


@pytest.fixture
def trashed(tmp_path, monkeypatch):
    """假的废纸篓：移到 tmp_path/Trash 里，记下移了什么。"""
    bin_ = tmp_path / "Trash"
    bin_.mkdir()
    moved: list[str] = []

    def fake(path: Path) -> None:
        moved.append(path.name)
        path.rename(bin_ / f"{len(moved)}_{path.name}")

    monkeypatch.setattr(server, "_trash", fake)
    return moved


@pytest.fixture
def client(root):
    return TestClient(create_app(root))


def item(client, meeting_id):
    return next(m for m in client.get("/api/meetings").json() if m["id"] == meeting_id)


# ---------- meta.json ----------


def test_normalize_tags():
    assert meetings.normalize_tags(["  项目A ", "周会", "", "项目A", "   ", 3, "x" * 25]) == ["项目A", "周会", "x" * 20]
    assert meetings.normalize_tags([f"t{i}" for i in range(15)]) == [f"t{i}" for i in range(10)]
    assert meetings.normalize_tags("周会") == []
    assert meetings.normalize_tags(None) == []


def test_meta_defaults_and_bad_file(tmp_path):
    default = {"title": None, "tags": [], "archived": False}
    assert meetings.read_meta(tmp_path) == default
    (tmp_path / "meta.json").write_text("{坏的", encoding="utf-8")
    assert meetings.read_meta(tmp_path) == default
    (tmp_path / "meta.json").write_text('["不是对象"]', encoding="utf-8")
    assert meetings.read_meta(tmp_path) == default
    (tmp_path / "meta.json").write_text('{"title": 3, "tags": [" a ", "a"], "archived": "yes"}', encoding="utf-8")
    assert meetings.read_meta(tmp_path) == {"title": None, "tags": ["a"], "archived": False}


def test_meta_write_format(tmp_path):
    meetings.write_meta(tmp_path, {"title": " 周会 ", "tags": ["项目A", "周会", "项目A"], "archived": True})
    text = (tmp_path / "meta.json").read_text(encoding="utf-8")
    assert text == '{\n  "title": "周会",\n  "tags": [\n    "项目A",\n    "周会"\n  ],\n  "archived": true\n}'
    assert meetings.read_meta(tmp_path) == {"title": "周会", "tags": ["项目A", "周会"], "archived": True}
    # 标题为空时不写这个键
    meetings.write_meta(tmp_path, {"title": "  ", "tags": [], "archived": False})
    assert (tmp_path / "meta.json").read_text(encoding="utf-8") == '{\n  "tags": [],\n  "archived": false\n}'


def test_meta_write_keeps_other_fields(tmp_path):
    (tmp_path / "meta.json").write_text(
        '{"source": "apple_watch", "interrupted": true, "title": "旧"}', encoding="utf-8"
    )
    meetings.write_meta(tmp_path, {"title": None, "tags": ["周会"], "archived": True})
    data = json.loads((tmp_path / "meta.json").read_text(encoding="utf-8"))
    assert data == {"tags": ["周会"], "archived": True, "source": "apple_watch", "interrupted": True}
    (tmp_path / "meta.json").write_text("not json", encoding="utf-8")
    with pytest.raises(OSError):
        meetings.write_meta(tmp_path, {"title": "x", "tags": [], "archived": False})
    assert (tmp_path / "meta.json").read_text(encoding="utf-8") == "not json"


def test_list_fields_and_title_precedence(client, root):
    weekly = item(client, WEEKLY)
    assert weekly["title"] == "周会"  # 纪要第一行
    assert weekly["tags"] == [] and weekly["archived"] is False and weekly["has_audio"] is True
    assert weekly["size_bytes"] == meetings.dir_size(root / WEEKLY) > 2048
    assert item(client, OLD)["title"] == "老会"  # 没有纪要：文件夹名里的会议名
    (root / WEEKLY / "meta.json").write_text('{"title": "改过的", "tags": ["项目A"], "archived": true}', "utf-8")
    weekly = item(client, WEEKLY)
    assert (weekly["title"], weekly["tags"], weekly["archived"]) == ("改过的", ["项目A"], True)
    # 详情也带这些字段；归档的会议照样在列表里
    d = client.get(f"/api/meetings/{WEEKLY}").json()
    assert (d["title"], d["tags"], d["archived"]) == ("改过的", ["项目A"], True)
    assert d["size_bytes"] == weekly["size_bytes"]
    assert len(client.get("/api/meetings").json()) == 2


def test_has_audio_any_extension(client, root):
    (root / OLD / "录音.m4a").unlink()
    assert item(client, OLD)["has_audio"] is False
    (root / OLD / "录音.mp3").write_bytes(b"x")
    assert item(client, OLD)["has_audio"] is True


def test_put_meta(client, root):
    r = client.put(f"/api/meetings/{WEEKLY}/meta", json={"title": "新标题", "tags": [" 项目A ", "周会", "周会"]})
    assert r.status_code == 200
    body = r.json()
    assert (body["id"], body["title"], body["tags"], body["archived"]) == (WEEKLY, "新标题", ["项目A", "周会"], False)
    assert "size_bytes" in body and "has_audio" in body
    assert (root / WEEKLY).is_dir()  # 不改文件夹名
    # 没传的字段不改
    body = client.put(f"/api/meetings/{WEEKLY}/meta", json={"archived": True}).json()
    assert (body["title"], body["tags"], body["archived"]) == ("新标题", ["项目A", "周会"], True)
    # null / 空串 = 恢复默认标题
    assert client.put(f"/api/meetings/{WEEKLY}/meta", json={"title": None}).json()["title"] == "周会"
    client.put(f"/api/meetings/{WEEKLY}/meta", json={"title": "再改"})
    assert client.put(f"/api/meetings/{WEEKLY}/meta", json={"title": ""}).json()["title"] == "周会"
    saved = json.loads((root / WEEKLY / "meta.json").read_text(encoding="utf-8"))
    assert saved == {"tags": ["项目A", "周会"], "archived": True}


def test_put_meta_validation(client):
    url = f"/api/meetings/{WEEKLY}/meta"
    assert client.put(url, json={"archived": "yes"}).status_code == 400
    assert client.put(url, json={"tags": "周会"}).status_code == 400
    assert client.put(url, json={"tags": [1]}).status_code == 400
    assert client.put(url, json={"title": 3}).status_code == 400
    assert client.put("/api/meetings/nope/meta", json={}).status_code == 404


def test_search_has_archived(client, root):
    (root / OLD / "meta.json").write_text('{"tags": [], "archived": true}', encoding="utf-8")
    results = client.get("/api/search", params={"q": "上线"}).json()["results"]
    assert {r["id"]: r["archived"] for r in results} == {WEEKLY: False, OLD: True}


# ---------- 删除 ----------


def test_delete_meeting(client, root, trashed):
    assert client.delete(f"/api/meetings/{WEEKLY}").json() == {"ok": True}
    assert not (root / WEEKLY).exists() and trashed == [WEEKLY]
    assert client.delete(f"/api/meetings/{WEEKLY}").status_code == 404
    assert client.delete("/api/meetings/%2E%2E").status_code == 404
    assert client.delete("/api/meetings/%2E%2E/audio").status_code == 404
    assert root.exists()


def test_delete_audio_keeps_text(client, root, trashed):
    m = root / WEEKLY
    (m / "system.wav").write_bytes(b"\0" * 10)
    (m / "mic.wav").write_bytes(b"\0" * 20)
    (m / "录音.mp3").write_bytes(b"\0" * 5)
    (m / "周会.pdf").write_bytes(b"%PDF")
    r = client.delete(f"/api/meetings/{WEEKLY}/audio")
    assert r.json() == {"ok": True, "freed_bytes": 2048 + 10 + 20 + 5}
    assert sorted(trashed) == ["mic.wav", "system.wav", "录音.m4a", "录音.mp3"]
    assert sorted(p.name for p in m.iterdir()) == ["transcript.json", "周会.pdf", "纪要.md"]
    r = client.delete(f"/api/meetings/{WEEKLY}/audio")
    assert r.status_code == 400 and r.json()["detail"] == "这场会议没有录音文件"


def test_delete_busy_meeting_409(client, root, trashed, monkeypatch):
    # 生成纪要的任务一直等着，这场会议就一直"正在处理"
    gate = threading.Event()

    def slow_summarize(*a, **k):
        gate.wait(5)
        return "# 周会\n"

    monkeypatch.setattr("tingo.summarize.summarize", slow_summarize)
    try:
        assert client.post(f"/api/meetings/{WEEKLY}/summarize").status_code == 200
        msg = "这场会议正在处理，等它完成"
        r = client.delete(f"/api/meetings/{WEEKLY}")
        assert r.status_code == 409 and r.json()["detail"] == msg
        r = client.delete(f"/api/meetings/{WEEKLY}/audio")
        assert r.status_code == 409 and r.json()["detail"] == msg
        r = client.post("/api/meetings/batch", json={"ids": [WEEKLY, OLD], "action": "delete_audio"}).json()
        assert r["ok"] == 1 and r["failed"] == [{"id": WEEKLY, "error": msg}] and r["freed_bytes"] == 100
        # 正在处理的会议照样可以改标签、归档
        r = client.post("/api/meetings/batch", json={"ids": [WEEKLY], "action": "archive"}).json()
        assert r["ok"] == 1 and r["failed"] == []
        # 清理录音时跳过它
        (root / OLD / "录音.m4a").write_bytes(b"\0" * 7)
        r = client.post("/api/storage/cleanup", json={"what": "audio", "older_than_days": 0}).json()
        assert r == {"ok": True, "count": 1, "freed_bytes": 7}
        assert (root / WEEKLY / "录音.m4a").exists()
    finally:
        gate.set()
    assert trashed == ["录音.m4a", "录音.m4a"]


def test_trash_unsupported_500(client, root, monkeypatch):
    monkeypatch.setattr("tingo.server.platform.system", lambda: "Windows")
    r = client.delete(f"/api/meetings/{WEEKLY}")
    assert r.status_code == 500 and r.json()["detail"] == "这个系统上没法移到废纸篓"
    assert (root / WEEKLY).exists()
    assert client.delete(f"/api/meetings/{WEEKLY}/audio").status_code == 500
    assert (root / WEEKLY / "录音.m4a").exists()


def _fake_recorder(tmp_path: Path, body: str) -> Path:
    exe = tmp_path / "fake-recorder"
    exe.write_text(f"#!/bin/sh\n{body}\n", encoding="utf-8")
    exe.chmod(exe.stat().st_mode | stat.S_IXUSR)
    return exe


def test_trash_uses_recorder_on_macos(tmp_path, monkeypatch):
    log = tmp_path / "calls.txt"
    exe = _fake_recorder(tmp_path, f'echo "$1 $2" >> "{log}"; rm -rf "$2"; echo OK')
    monkeypatch.setattr("tingo.server.platform.system", lambda: "Darwin")
    monkeypatch.setattr("tingo.record.ensure_recorder", lambda: exe)
    target = tmp_path / "会议"
    target.mkdir()
    server._trash(target)
    assert not target.exists()
    assert log.read_text(encoding="utf-8").strip() == f"trash {target}"


def test_trash_recorder_error(tmp_path, monkeypatch):
    exe = _fake_recorder(tmp_path, 'echo "ERROR: 移到废纸篓失败：没有权限"; exit 1')
    monkeypatch.setattr("tingo.server.platform.system", lambda: "Darwin")
    monkeypatch.setattr("tingo.record.ensure_recorder", lambda: exe)
    with pytest.raises(RuntimeError, match="没有权限"):
        server._trash(tmp_path)


def test_trash_gio_on_linux(tmp_path, monkeypatch):
    log = tmp_path / "calls.txt"
    exe = _fake_recorder(tmp_path, f'echo "$1 $2" >> "{log}"')
    monkeypatch.setattr("tingo.server.platform.system", lambda: "Linux")
    monkeypatch.setattr("tingo.server.shutil.which", lambda name: str(exe) if name == "gio" else None)
    server._trash(tmp_path / "x")
    assert log.read_text(encoding="utf-8").strip() == f"trash {tmp_path / 'x'}"
    monkeypatch.setattr("tingo.server.shutil.which", lambda name: None)
    with pytest.raises(server.TrashUnsupported):
        server._trash(tmp_path / "x")


# ---------- 批量 ----------


def test_batch_archive_and_tags(client, root):
    ids = [WEEKLY, OLD]
    assert client.post("/api/meetings/batch", json={"ids": ids, "action": "archive"}).json() == {
        "ok": 2,
        "failed": [],
        "freed_bytes": 0,
    }
    assert all(m["archived"] for m in client.get("/api/meetings").json())
    client.post("/api/meetings/batch", json={"ids": [OLD], "action": "unarchive"})
    assert item(client, OLD)["archived"] is False and item(client, WEEKLY)["archived"] is True
    client.put(f"/api/meetings/{WEEKLY}/meta", json={"tags": ["周会"]})
    r = client.post("/api/meetings/batch", json={"ids": ids, "action": "tag_add", "tag": " 项目A "}).json()
    assert r["ok"] == 2
    assert item(client, WEEKLY)["tags"] == ["周会", "项目A"] and item(client, OLD)["tags"] == ["项目A"]
    client.post("/api/meetings/batch", json={"ids": ids, "action": "tag_add", "tag": "项目A"})
    assert item(client, WEEKLY)["tags"] == ["周会", "项目A"]  # 已有的不重复加
    client.post("/api/meetings/batch", json={"ids": ids, "action": "tag_remove", "tag": "周会"})
    assert item(client, WEEKLY)["tags"] == ["项目A"] and item(client, OLD)["tags"] == ["项目A"]


def test_batch_validation(client):
    url = "/api/meetings/batch"
    assert client.post(url, json={"ids": [], "action": "archive"}).status_code == 400
    assert client.post(url, json={"action": "archive"}).status_code == 400
    assert client.post(url, json={"ids": [WEEKLY], "action": "explode"}).status_code == 400
    assert client.post(url, json={"ids": [WEEKLY], "action": "tag_add"}).status_code == 400
    assert client.post(url, json={"ids": [WEEKLY], "action": "tag_remove", "tag": "  "}).status_code == 400


def test_batch_partial_failure(client, root, trashed):
    r = client.post("/api/meetings/batch", json={"ids": [WEEKLY, "不存在", OLD], "action": "delete"}).json()
    assert r["ok"] == 2 and r["freed_bytes"] == 0
    assert r["failed"] == [{"id": "不存在", "error": "找不到这场会议"}]
    assert not (root / WEEKLY).exists() and not (root / OLD).exists()


def test_batch_delete_audio(client, root, trashed):
    (root / OLD / "录音.m4a").unlink()  # 没录音的算成功、freed 0
    (root / WEEKLY / "system.wav").write_bytes(b"\0" * 10)
    r = client.post("/api/meetings/batch", json={"ids": [WEEKLY, OLD], "action": "delete_audio"}).json()
    assert r == {"ok": 2, "failed": [], "freed_bytes": 2058}
    assert (root / WEEKLY / "纪要.md").exists() and (root / WEEKLY / "transcript.json").exists()


def test_batch_trash_error_goes_to_failed(client, root, monkeypatch):
    def broken(path):
        if path.name == WEEKLY:
            raise RuntimeError("磁盘只读")
        path.rename(path.with_name(path.name + ".gone"))

    monkeypatch.setattr(server, "_trash", broken)
    r = client.post("/api/meetings/batch", json={"ids": [WEEKLY, OLD], "action": "delete"}).json()
    assert r["ok"] == 1 and r["failed"] == [{"id": WEEKLY, "error": "磁盘只读"}]


# ---------- 存储空间 ----------


def test_storage_stats(client, root):
    w, o = root / WEEKLY, root / OLD
    (w / "system.wav").write_bytes(b"\0" * 30)  # 已经有 录音.m4a：残留
    (w / "下载.part").write_bytes(b"\0" * 3)
    (o / "录音.m4a").unlink()
    (o / "mic.wav").write_bytes(b"\0" * 40)  # 没有 录音.*：不算残留（可能还没合成）
    (o / "x.tmp").write_bytes(b"\0" * 4)
    big = make_meeting(root, "2026-10-01_1000_大会", audio=b"\0" * 50_000)
    (big / "meta.json").write_text('{"title": "大会改名", "tags": [], "archived": true}', encoding="utf-8")
    s = client.get("/api/storage").json()
    assert s["root"] == str(root) and s["count"] == 3
    assert s["audio_bytes"] == 2048 + 30 + 40 + 50_000
    assert s["leftover_bytes"] == 30 + 3 + 4
    assert s["models_bytes"] is None
    assert isinstance(s["free_bytes"], int) and s["free_bytes"] > 0
    assert s["meetings_bytes"] == sum(meetings.dir_size(p) for p in (w, o, big))
    largest = s["largest"]
    assert [x["id"] for x in largest] == [big.name, WEEKLY, OLD]
    assert largest[0] == {
        "id": big.name,
        "title": "大会改名",
        "date": "2026-10-01",
        "bytes": meetings.dir_size(big),
        "audio_bytes": 50_000,
        "archived": True,
    }
    assert largest[2]["audio_bytes"] == 40


def test_storage_largest_top10(client, root):
    for i in range(12):
        make_meeting(root, f"2026-09-{i + 1:02d}_1000_m{i}", audio=b"\0" * (3000 + i * 100))
    largest = client.get("/api/storage").json()["largest"]
    assert len(largest) == 10
    assert [x["bytes"] for x in largest] == sorted((x["bytes"] for x in largest), reverse=True)
    assert largest[0]["id"] == "2026-09-12_1000_m11"


def test_storage_empty_root(tmp_path, monkeypatch):
    monkeypatch.setattr("tingo.config.CONFIG_PATH", tmp_path / "config.toml")
    s = TestClient(create_app(tmp_path / "没有")).get("/api/storage").json()
    assert (s["count"], s["meetings_bytes"], s["audio_bytes"], s["largest"]) == (0, 0, 0, [])
    assert s["free_bytes"] > 0


# ---------- 清理 ----------


def test_cleanup_leftovers(client, root, trashed):
    (root / WEEKLY / "system.wav").write_bytes(b"\0" * 30)
    (root / WEEKLY / "mic.wav").write_bytes(b"\0" * 20)
    (root / OLD / "a.tmp").write_bytes(b"\0" * 5)
    r = client.post("/api/storage/cleanup", json={"what": "leftovers"}).json()
    assert r == {"ok": True, "count": 3, "freed_bytes": 55}
    assert sorted(trashed) == ["a.tmp", "mic.wav", "system.wav"]
    assert (root / WEEKLY / "录音.m4a").exists()
    assert client.get("/api/storage").json()["leftover_bytes"] == 0


def test_cleanup_audio_older_than(client, root, trashed):
    today = date.today()
    recent = make_meeting(root, f"{today - timedelta(days=3):%Y-%m-%d}_1000_近", audio=b"\0" * 9)
    (root / OLD / "system.wav").write_bytes(b"\0" * 11)
    r = client.post("/api/storage/cleanup", json={"what": "audio", "older_than_days": 30}).json()
    assert r == {"ok": True, "count": 1, "freed_bytes": 100 + 11}  # 只有 2020-01-05 那场早于 30 天前
    assert (recent / "录音.m4a").exists()
    assert (root / OLD / "transcript.json").exists()
    # 0 表示全部；已经没录音的会议不算
    r = client.post("/api/storage/cleanup", json={"what": "audio", "older_than_days": 0}).json()
    assert r == {"ok": True, "count": 2, "freed_bytes": 9 + 2048}
    assert (root / WEEKLY / "纪要.md").exists() and (root / WEEKLY / "transcript.json").exists()


def test_cleanup_audio_boundary(client, root, trashed):
    today = date.today()
    exact = make_meeting(root, f"{today - timedelta(days=30):%Y-%m-%d}_1000_正好", audio=b"\0" * 1)
    older = make_meeting(root, f"{today - timedelta(days=31):%Y-%m-%d}_1000_更早", audio=b"\0" * 1)
    client.post("/api/storage/cleanup", json={"what": "audio", "older_than_days": 30, "archived_only": True})
    assert trashed == []  # 都没归档
    for m in (exact, older):
        (m / "meta.json").write_text('{"tags": [], "archived": true}', encoding="utf-8")
    r = client.post("/api/storage/cleanup", json={"what": "audio", "older_than_days": 30, "archived_only": True})
    assert r.json() == {"ok": True, "count": 1, "freed_bytes": 1}
    assert (exact / "录音.m4a").exists() and not (older / "录音.m4a").exists()
    assert (root / OLD / "录音.m4a").exists()  # 早，但没归档


def test_cleanup_archived_only(client, root, trashed):
    (root / OLD / "meta.json").write_text('{"tags": [], "archived": true}', encoding="utf-8")
    r = client.post("/api/storage/cleanup", json={"what": "audio", "older_than_days": 0, "archived_only": True})
    assert r.json() == {"ok": True, "count": 1, "freed_bytes": 100}
    assert (root / WEEKLY / "录音.m4a").exists() and not (root / OLD / "录音.m4a").exists()


@pytest.mark.parametrize(
    "body",
    [
        {"what": "everything"},
        {},
        {"what": "audio"},
        {"what": "audio", "older_than_days": -1},
        {"what": "audio", "older_than_days": 3651},
        {"what": "audio", "older_than_days": 1.5},
        {"what": "audio", "older_than_days": "30"},
        {"what": "audio", "older_than_days": True},
        {"what": "audio", "older_than_days": 30, "archived_only": "yes"},
    ],
)
def test_cleanup_validation(client, root, trashed, body):
    assert client.post("/api/storage/cleanup", json=body).status_code == 400
    assert trashed == []


def test_cleanup_accepts_bounds(client, trashed):
    assert client.post("/api/storage/cleanup", json={"what": "audio", "older_than_days": 3650}).status_code == 200
