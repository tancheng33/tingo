import tomllib

import pytest

from tingo import config
from tingo.config import Config, LLMConfig, ObsidianConfig

# 桌面版（app/engine/src/config.rs 的 save）写出的文件
RUST_SAVED = (
    "# tingo 配置，由图形界面写入；也可以直接编辑。\n\n# 专有名词、人名、项目代号，提高识别准确率\n"
    'hotwords = ["星河", "Kafka"]\n\n[llm]\nbase_url = "http://127.0.0.1:3425/v1"\napi_key = "k\\"q"\n'
    'model = "deepseek/deepseek-v4-flash"\napi = "anthropic"\n\n[push]\nwebhook = ""\nsecret = ""\n\n'
    '[obsidian]\nvault = "/Users/a/笔记"\nfolder = "会议"\nauto = false\ntranscript = true\n'
    '\n[ui]\nlanguage = "auto"\n\n[minutes]\nlanguage = "auto"\n\n[asr]\nlanguage = "zh"\n'
    "\n[record]\nauto_stop_silence_min = 10\nauto_stop_app_quit = true\n"
    '\n[storage]\nroot = ""\n'
)


@pytest.fixture(autouse=True)
def isolated(tmp_path, monkeypatch):
    monkeypatch.setattr(config, "CONFIG_PATH", tmp_path / "config.toml")
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


def test_roundtrip_with_obsidian_matches_desktop_format():
    cfg = Config(
        llm=LLMConfig("http://127.0.0.1:3425/v1", 'k"q', "deepseek/deepseek-v4-flash", "anthropic"),
        hotwords=["星河", "Kafka"],
        obsidian=ObsidianConfig("/Users/a/笔记", "会议", False, True),
    )
    config.save(cfg)
    assert config.CONFIG_PATH.read_text(encoding="utf-8") == RUST_SAVED  # 两个版本写出的文件逐字相同
    assert config.load() == cfg
    config.CONFIG_PATH.write_text(RUST_SAVED, encoding="utf-8")
    assert config.load() == cfg


def test_obsidian_defaults_when_section_missing():
    config.CONFIG_PATH.write_text('hotwords = ["青鸾"]\n\n[llm]\nmodel = "m"\n\n[push]\nwebhook = ""\nsecret = ""\n')
    assert config.load().obsidian == ObsidianConfig("", "会议纪要", True, True)
    config.CONFIG_PATH.write_text('[obsidian]\nvault = "/v"\n')
    assert config.load().obsidian == ObsidianConfig("/v", "会议纪要", True, True)
    assert "obsidian" not in tomllib.loads(config.EXAMPLE)


def test_language_sections_roundtrip_and_defaults():
    cfg = Config(ui_language="en", minutes_language="zh", asr_language="auto")
    config.save(cfg)
    text = config.CONFIG_PATH.read_text(encoding="utf-8")
    assert text.removesuffix('\n[storage]\nroot = ""\n').endswith(
        '[ui]\nlanguage = "en"\n\n[minutes]\nlanguage = "zh"\n\n[asr]\nlanguage = "auto"\n'
        "\n[record]\nauto_stop_silence_min = 10\nauto_stop_app_quit = true\n"
    )
    assert text.index("[obsidian]") < text.index("[ui]")
    assert config.load() == cfg
    # 老配置没有这三节、或者填了不认识的值：用默认；大小写不敏感
    config.CONFIG_PATH.write_text('[llm]\nmodel = "m"\n\n[asr]\nlanguage = "fr"\n\n[minutes]\nlanguage = "EN"\n')
    c = config.load()
    assert (c.ui_language, c.minutes_language, c.asr_language) == ("auto", "en", "zh")


def test_llm_api_defaults_normalizes_and_env_override(monkeypatch):
    # 没写 api（老配置、Python 旧版写出的文件）：openai
    config.CONFIG_PATH.write_text('[llm]\nbase_url = "https://ai.example/v1"\napi_key = "sk"\nmodel = "m"\n')
    assert config.load().llm.api == "openai"
    # 去空白、不分大小写；不认识的值当 openai
    config.CONFIG_PATH.write_text('[llm]\nmodel = "m"\napi = " Anthropic "\n')
    assert config.load().llm.api == "anthropic"
    config.CONFIG_PATH.write_text('[llm]\nmodel = "m"\napi = "gemini"\n')
    assert config.load().llm.api == "openai"
    # 默认值写回时紧跟在 model 后面
    config.save(Config())
    assert '\nmodel = "deepseek-chat"\napi = "openai"\n\n[push]' in config.CONFIG_PATH.read_text(encoding="utf-8")
    # 环境变量优先，改名前的 JIYAO_ 前缀也认
    monkeypatch.setenv("JIYAO_LLM_API", "ANTHROPIC")
    assert config.load().llm.api == "anthropic"
    monkeypatch.setenv("TINGO_LLM_API", "openai")
    assert config.load().llm.api == "openai"
    monkeypatch.setenv("TINGO_LLM_API", "nope")
    assert config.load().llm.api == "openai"


def test_record_section_roundtrip_defaults_and_validation():
    from tingo.config import RecordConfig

    assert Config().record == RecordConfig(10, True)
    cfg = Config(record=RecordConfig(0, False))
    config.save(cfg)
    text = config.CONFIG_PATH.read_text(encoding="utf-8")
    assert text.removesuffix('\n[storage]\nroot = ""\n').endswith(
        '[asr]\nlanguage = "zh"\n\n[record]\nauto_stop_silence_min = 0\nauto_stop_app_quit = false\n'
    )
    assert config.load() == cfg
    # 老配置没有这一节、值不合法：用默认
    config.CONFIG_PATH.write_text('[llm]\nmodel = "m"\n')
    assert config.load().record == RecordConfig(10, True)
    for bad in ("241", "-1", "1.5", '"10"', "true"):
        config.CONFIG_PATH.write_text(f'[record]\nauto_stop_silence_min = {bad}\nauto_stop_app_quit = "yes"\n')
        assert config.load().record == RecordConfig(10, True), bad
    config.CONFIG_PATH.write_text("[record]\nauto_stop_silence_min = 240\n")
    assert config.load().record == RecordConfig(240, True)
    config.CONFIG_PATH.write_text("record = 3\n")
    assert config.load().record == RecordConfig(10, True)


def test_storage_survives_other_setting_changes():
    cfg = Config(storage_root="/Volumes/会议资料/Tingo")
    config.save(cfg)
    restored = config.load()
    assert restored.storage_root == cfg.storage_root
    restored.llm.model = "new-model"
    config.save(restored)
    assert config.load().storage_root == cfg.storage_root


def test_llm_temperature_optional_roundtrip_and_bad_values_ignored():
    for line, want in [
        ("", None),
        ("temperature = 1\n", 1.0),
        ("temperature = 0.7\n", 0.7),
        ("temperature = 9\n", None),
        ('temperature = "1"\n', None),
    ]:
        config.CONFIG_PATH.write_text(f'[llm]\nmodel = "kimi"\n{line}', encoding="utf-8")
        cfg = config.load()
        assert (cfg.llm.model, cfg.llm.temperature) == ("kimi", want), line
    cfg.llm.temperature = 1.0
    config.save(cfg)
    text = config.CONFIG_PATH.read_text(encoding="utf-8")
    # 与桌面版写法一致：紧跟在 api 后面，写成浮点数
    assert 'api = "openai"\ntemperature = 1.0\n\n[push]' in text
    assert config.load().llm.temperature == 1.0
    cfg.llm.temperature = None
    config.save(cfg)
    assert "temperature" not in config.CONFIG_PATH.read_text(encoding="utf-8")
