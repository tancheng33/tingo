<p align="center"><img src="brand/tingo-icon.svg" width="120" alt="Tingo"></p>

<h1 align="center">Tingo</h1>

<p align="center"><b>Every meeting, heard.</b><br>Local-first AI meeting notes for Mac. Your audio stays on your machine, and every conclusion links back to what was actually said.</p>

<p align="center">
  <a href="https://github.com/tancheng33/tingo/releases/latest"><img src="https://img.shields.io/badge/download-macOS%20app-1f4bff?logo=apple" alt="Download"></a>
  <img src="https://img.shields.io/badge/macOS-13%2B-000?logo=apple" alt="macOS 13+">
  <img src="https://img.shields.io/badge/license-MIT-34c6f4" alt="MIT">
  <a href="https://tancheng33.github.io/tingo/en.html"><img src="https://img.shields.io/badge/website-tancheng33.github.io%2Ftingo-555" alt="Website"></a>
  <a href="README.zh-CN.md"><img src="https://img.shields.io/badge/README-%E4%B8%AD%E6%96%87-555" alt="中文"></a>
</p>

<p align="center"><img src="docs/screenshots/demo.gif" width="880" alt="Demo: search across meetings, jump to the original words, expand fishbone quotes, watch the minutes being written"></p>

Tingo records the audio of Zoom, Teams, Google Meet, Tencent Meeting or Feishu calls together with your microphone, transcribes it **on your Mac**, tells the speakers apart, and has the LLM of your choice write structured minutes: topics, decisions, action items with owners and due dates, and risks. When the meeting discussed *why* something happened, it also draws a **fishbone diagram whose every branch is bound to the original quotes**.

- **Audio never leaves your Mac.** Speech recognition and speaker diarization run locally; nothing is uploaded. Minutes can be written by OpenAI, Claude, Gemini, DeepSeek, Qwen, Kimi or any OpenAI-compatible API, or by a local Ollama model, fully offline.
- **Multilingual.** English, Chinese, Japanese, Korean and Cantonese are recognized with SenseVoice, even when they're mixed in one meeting. For Chinese meetings, FunASR's Paraformer handles punctuation, numbers and names far better than English-first stacks. Relative dates ("next Wednesday", "end of month") are resolved against the meeting date.
- **Every claim has a source.** Each line of the minutes carries a timestamp that jumps to that second of the recording. The fishbone diagram counts how much original speech backs each branch and flags the ones with **no evidence**, so hand-waving is visible at a glance.
- **English and Chinese interface.** The app, the minutes and every export work in both languages, and the minutes can be written in a different language from the meeting.
- **About 13 MB to download.** Tauri + Rust, no Python or ffmpeg needed. A live recording is fully transcribed seconds after you hit stop; a 76-minute file imports in about 3.5 minutes on an M1 Pro.

Looking for a **free, open-source, local alternative to Otter.ai, Granola, Fireflies.ai or Fathom**, an AI meeting note taker that never uploads your audio? That's what Tingo is for.

*Tingo* comes from 听 (*tīng*, "to listen"); its Chinese name 听过 means "heard it".

> The meetings, names and numbers in the screenshots are fictional demo data.

[Download the latest release](https://github.com/tancheng33/tingo/releases/latest) · see [Releases](https://github.com/tancheng33/tingo/releases) for what changed in each version.

## Features

### Evidence-bound fishbone diagrams

When a meeting analyzes causes (why conversion dropped, why a project slipped), the minutes include a fishbone diagram. Every branch is bound to the quotes it's based on: expand it to see who said what and when, and click a timestamp to listen.

Each branch counts how many passages and how much original speech it cites. Branches whose cited timestamps can't be found in the transcript are marked **no evidence** (red); thinly supported ones are marked amber. Conclusions someone reached on gut feeling stand out immediately.

<p align="center"><img src="docs/screenshots/fishbone.png" width="820" alt="Fishbone diagram bound to quotes"></p>

Export the fishbone to [Excalidraw](https://excalidraw.com) with one click to keep editing or present it:

<p align="center"><img src="docs/screenshots/excalidraw.png" width="720" alt="Fishbone exported to Excalidraw"></p>

### Never wait: start the next meeting right away

After you stop recording, transcription and minutes run in the background and the home page is ready for the next meeting. The sidebar shows where each task is; open a meeting to watch the LLM write the minutes section by section. Reasoning models think for a minute or two before writing, so Tingo shows how much they've thought instead of looking stuck.

<p align="center"><img src="docs/screenshots/home.png" width="49%" alt="Home: background tasks"> <img src="docs/screenshots/generating.png" width="49%" alt="Minutes being written"></p>

### Search everything that was said

Search the meeting list and Tingo looks through every transcript, lists the matching sentences, and jumps to that second of that meeting. The minutes end with a **names & terms** table recording the correct spelling and how the recognizer misheard it, so searching finds the misheard variants too. Add them all to your hotwords in one click.

### Who said what

Transcription happens while you record, so the full transcript is ready seconds after you stop. Speakers are separated by voice. Instead of renaming them one by one, let the AI guess who's who from how people address each other and introduce themselves; every guess comes with its evidence. Names you've already entered are kept, and you can save and regenerate the minutes in one step.

Forgot to stop? Recording ends by itself when the meeting app quits or after 10 minutes of silence (adjustable), and the silent tail is trimmed. Click any sentence to play from there; the current sentence is highlighted during playback.

<p align="center"><img src="docs/screenshots/transcript.png" width="820" alt="Transcript with speakers"></p>

### Export and share

- **PDF**: A4 layout that keeps the fishbone's problem, branches and notes, ready to send.
- **Word**: an editable .docx.
- **SRT subtitles**: sentence by sentence with speakers, for meeting recordings.
- **Obsidian**: one note per meeting with date, duration and participants as properties; fishbones are embedded as Excalidraw drawings. Can be saved automatically after the minutes are written.
- **Feishu / DingTalk / WeCom group bots**: paste a webhook and share to the group in one click.
- **Markdown**: copy and paste anywhere.

Exported fishbones leave out the quotes, citation counts and timestamps, and PDF and Word leave out the names & terms table. They stay in the app for checking and search.

<p align="center"><img src="docs/screenshots/pdf.png" width="820" alt="PDF export layout preview; current exports omit evidence details and terms"></p>

### Minutes history

Regenerating or editing the minutes keeps the previous version, and a failed generation never overwrites the current one. Open **History** to browse versions by time and preview, export, copy or share any of them. Historical exports get their own filenames; SRT always uses the meeting transcript. Deleting a historical version is permanent and leaves the current minutes unchanged.

### Stay organized

- **Tags and archive**: tag meetings by project or type and filter by tag; archive finished ones (they stay searchable).
- **Bulk actions**: select several meetings to tag, archive or delete them at once.
- **Rename** any meeting without touching its folder.
- **Undoable deletes**: deleting a meeting or only its recording moves it to the Trash.
- **Storage**: see what takes space and remove recordings older than N days (optionally only archived ones) while keeping transcripts and minutes.

### Swappable speech models

Pick the recognizer that fits your meetings in Settings; all of them run locally:

| Model | Best for |
|---|---|
| **SenseVoice** | English, Japanese, Korean, Cantonese, and meetings that mix languages. The default when the app is in English |
| **Paraformer-large** | Chinese meetings: fast, accurate and light on memory. The default when the app is in Chinese |
| **Fun-ASR-Nano** | The most accurate Chinese recognition, and it applies your hotwords (names, project code names) while recognizing; about 1/4 the speed, about 2 GB of memory |
| **Apple Speech** | macOS 26+: the system's on-device recognizer for English or Chinese, with language assets managed by macOS |

Speaker separation always uses Tingo's own voice model, whichever recognizer you pick.

<p align="center"><img src="docs/screenshots/settings.png" width="820" alt="Settings: speech models"></p>

## Install

### Desktop app (recommended)

Download `Tingo_x.y.z_aarch64.dmg` from [Releases](https://github.com/tancheng33/tingo/releases/latest) and drag it into Applications. It runs on Apple silicon Macs (M1 and later) with macOS 13+; recording system audio needs macOS 15+.

A short tour on first launch downloads the speech model, test-records 3 seconds to check permissions, connects an LLM and shows a built-in sample meeting. The speech models are a one-time download of a few hundred MB (from GitHub and ModelScope, resumable); after that, recognition works fully offline.

Official releases are Developer ID-signed and Apple-notarized: open the DMG and drag Tingo into Applications, with no right-click workaround or `xattr` command. Builds made from source are not notarized.

The first recording asks for **Microphone** and **Screen & System Audio Recording** permission (the latter captures the other side of the call).

New versions are announced next to the version number in the sidebar. Download, let the signature check pass, then choose **Restart and update**; meetings, history and settings are preserved. You can also replace the app with a newer DMG.

### Command line

The command-line version is built on Python (FunASR) and suits scripts and batch processing. It needs Python 3.11+ and ffmpeg:

```bash
brew install ffmpeg
uv tool install git+https://github.com/tancheng33/tingo      # or pipx install git+…
tingo doctor                                                 # check the environment
```

```bash
tingo record -t standup -w "Lin Zhou,signup funnel"   # record system audio + mic, Ctrl+C to stop, minutes follow
tingo run ~/Downloads/design-review.mp4                # process an existing recording or screen capture
tingo summarize ~/Documents/Tingo/2026-10-28_1400_retro -s "Lin Zhou,Chen Mo"   # regenerate after renaming speakers
tingo push ~/Documents/Tingo/2026-10-28_1400_retro     # share to a group chat
tingo ui                                               # open the GUI
```

Each meeting is a folder, by default under `~/Documents/Tingo/`, shared by the app and the command line. In the app, **Settings → Storage** moves the library to another folder without restarting: by default recordings, transcripts, minutes and history are copied to a new empty folder and verified, and the original stays in place. You can also just switch folders and switch back later. Speech models stay where they are.

```
2026-10-28_1400_signup-funnel-retro/
├── 录音.m4a          # recording, 16 kHz mono, about 30 MB per hour
├── 转写.md           # transcript
├── transcript.json   # structured transcript
└── 纪要.md           # minutes
```

## Connect an LLM

Transcription doesn't need an LLM; the minutes do. Pick one in Settings and test the connection:

| Choice | Notes |
|---|---|
| OpenAI, Claude, Gemini, DeepSeek, Qwen (Bailian) | Paste an API key. On a 76-minute meeting, DeepSeek V4 Flash writes the minutes in about 50 seconds |
| Local relays (Magpie, CC Switch) | Detected automatically when running; pick a model from the list |
| Local Ollama | Nothing leaves your Mac and no key is needed. Local models have shorter contexts, so long meetings are summarized in parts and merged |
| Custom | Any OpenAI-compatible URL, or an Anthropic (Claude) API |

Some models only accept a fixed temperature (Kimi only accepts 1, for example). Tingo handles that automatically, and the temperature can be adjusted in Settings.

The config file is `~/.config/tingo/config.toml`; the environment variables `TINGO_LLM_BASE_URL`, `TINGO_LLM_API_KEY` and `TINGO_LLM_MODEL` work too.

## Privacy

- Recordings and transcripts stay on your Mac, and the speech models run locally.
- When generating minutes, the **transcript text** is sent to the LLM endpoint you configured. With local Ollama, that step stays on your Mac too.
- Sharing to a group chat, saving to Obsidian and exporting PDFs only happen when you click.
- The app's built-in server listens on 127.0.0.1 only. No analytics, no telemetry.

## How it works

```
Recording ─┬─ system audio (ScreenCaptureKit) ─┐
           └─ microphone ──────────────────────┴─ 16 kHz mono, chunked at pauses while recording
                                                    │
         VAD → speech recognition (SenseVoice / Paraformer / Fun-ASR-Nano / Apple Speech) → punctuation → CAM++ voiceprints
                                                    │
                 spectral clustering separates speakers (once per meeting, so labels stay consistent)
                                                    │
     LLM writes the minutes (long meetings in parts, then merged; dates resolved; a glossary fixes homophones)
                                                    │
          every fishbone citation is checked against the transcript → minutes → PDF / Obsidian / group chat
```

A few details:

- **Desktop app**: a Tauri shell around a Rust core. [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx) runs the FunASR models locally; speaker clustering was ported from the Python version and matches it on 93.6% of seconds. Notes on choosing between 8 recognition engines: [`spike/sherpa/RESULTS.md`](spike/sherpa/RESULTS.md) (in Chinese).
- **Speaker echo**: with speakers instead of headphones, the microphone picks up the other side again. Tingo ducks the microphone while the system audio has speech and uses it when the other side is quiet; when both talk at once, the local side is drowned out. **Headphones work best.**
- **Metadata isn't left to the model**: the date, duration and participants in the minutes are filled in by the program; the model only writes the content, so it can't make them up.
- **Reasoning models**: the thinking of DeepSeek-R1, Qwen3 and similar models is filtered out of the minutes.

## Compared with other tools

| | Tingo | Cloud services (Otter, Fireflies, Feishu Minutes…) | Open-source assistants (Meetily…) |
|---|---|---|---|
| Where recordings live | Only on your Mac | Uploaded to the cloud | On your machine |
| Chinese recognition | FunASR, built for Chinese | Varies | Mostly Whisper / Parakeet, English-first |
| Recording online meetings | Captures system audio directly, no virtual audio device | Usually a bot that joins the call, or their own platform | Supported |
| LLM for the minutes | Your choice, can be fully local | Chosen by the platform | Your choice |
| Tracing conclusions to quotes | Every line timestamped; fishbone citations verified | Partial | — |
| Price | Free and open source | Subscription or per-minute | Free and open source |

## FAQ

**How do I record and transcribe a Zoom / Teams / Google Meet call on a Mac?**
Hit record in Tingo. It captures the system audio (the other side) and your microphone at the same time; no BlackHole or other virtual audio device needed. You can also drop in recordings exported by the meeting app (m4a, mp4, mp3, wav, …).

**Is my audio uploaded?**
No. Speech recognition and speaker separation run on your Mac. Only the transcript text goes to the LLM you choose; with local Ollama, nothing leaves the machine and it works offline.

**Does it write minutes and action items automatically?**
Yes: a one-line summary, topics, decisions, action items (owner, due date) and risks, each with a timestamp that plays the original words. Phrases like "next Wednesday" become actual dates.

**Which LLMs are supported?**
Any OpenAI-compatible API (OpenAI, Gemini, DeepSeek, Qwen, Kimi, Zhipu, local Ollama, …) and the Anthropic (Claude) API.

**Can it tell different speakers apart?**
Yes, by voice. Rename a speaker and the minutes are regenerated with the new names.

**Which languages does it understand?**
With SenseVoice: English, Chinese, Japanese, Korean and Cantonese, including mixed-language meetings. The app interface is in English or Chinese, and the minutes can be written in English or Chinese regardless of the meeting's language.

**Does it run on Windows or Linux?**
The desktop app is macOS-only (Apple silicon) for now. The command-line version records the microphone on Linux; on Windows, record with the meeting app and import the file.

## Known limitations

- The desktop app is macOS-only (Apple silicon); recording system audio needs macOS 15+. The command line records only the microphone on Linux.
- When people talk over each other or have similar voices, speakers may be mixed up; rename them on the meeting page and regenerate the minutes.
- With speakers, echo handling ducks the microphone rather than truly cancelling echo, so a word or two may be misrecognized at speaker changes.

## Roadmap

- [ ] Real acoustic echo cancellation (AEC), so speakers work as well as headphones
- [ ] Quick recording from the menu bar, with a reminder when a meeting app starts
- [ ] Windows version (WASAPI system audio)
- [ ] Export to Feishu Docs / DingTalk Docs

## Development

```bash
git clone https://github.com/tancheng33/tingo && cd tingo

# Command line / Python backend
uv sync
uv run pytest -m "not slow"          # unit tests that don't download models

# Interface (TypeScript + Vite), built into src/tingo/web and shared by the desktop app and `tingo ui`
cd app && pnpm install
pnpm dev                             # http://localhost:1420, API proxied to the backend on port 8765

# Desktop app (Tauri + Rust)
pnpm tauri dev                       # development window
pnpm tauri build                     # build .app / .dmg
cd engine && cargo test              # Rust core tests
cargo run --release --example serve -- <models dir> <recorder> 8765   # backend only, for pnpm dev
```

Official releases are built with `cd app && npm run release:mac`, then notarized and stapled; see [the macOS release guide](app/scripts/README.md). Regular source builds don't need a distribution certificate.

Issues and pull requests are welcome, in English or Chinese.

## Acknowledgements

- [FunASR](https://github.com/modelscope/FunASR): speech recognition, VAD, punctuation and speaker models
- [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx): local inference in the desktop app
- [Tauri](https://tauri.app), [Excalidraw](https://excalidraw.com)
- [Meetily](https://github.com/Zackriya-Solutions/meetily), [Hyprnote](https://github.com/fastrepl/anarlog): ideas for local meeting assistants

## Former name

The project was first called jiyao (纪要, "minutes"). The old `jiyao` command, `JIYAO_*` environment variables, `~/.config/jiyao/` config folder and `~/Documents/jiyao/` meeting folder all keep working; no data needs to move.

## License

[MIT](LICENSE)
