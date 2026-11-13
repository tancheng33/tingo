<p align="center"><img src="brand/tingo-icon.svg" width="120" alt="Tingo"></p>

<h1 align="center">Tingo 听过</h1>

<p align="center"><b>开过的会，都听过。</b><br>本地录音、本地转写的中文会议纪要工具，每条结论都对得上原话。</p>

<p align="center">
  <a href="https://github.com/tancheng33/tingo/releases/latest"><img src="https://img.shields.io/badge/%E4%B8%8B%E8%BD%BD-macOS%20App-1f4bff?logo=apple" alt="下载"></a>
  <img src="https://img.shields.io/badge/macOS-13%2B-000?logo=apple" alt="macOS 13+">
  <img src="https://img.shields.io/badge/license-MIT-34c6f4" alt="MIT">
  <a href="https://tancheng33.github.io/tingo/"><img src="https://img.shields.io/badge/%E4%B8%BB%E9%A1%B5-tancheng33.github.io%2Ftingo-555" alt="主页"></a>
  <a href="README.en.md"><img src="https://img.shields.io/badge/README-English-555" alt="English"></a>
</p>

<p align="center"><img src="docs/screenshots/demo.gif" width="880" alt="演示：跨会议搜索、点回原话、鱼骨图展开引用、看着纪要一节一节写出来"></p>

录下腾讯会议、飞书、Zoom 里的声音和你自己的麦克风，在你的电脑上转成文字、分清谁说了什么，再交给你自己选的大模型整理成纪要：议题、决议、带负责人和截止日期的待办、风险，讨论了"为什么"时还会画一张**每个分支都绑着原话的鱼骨图**。

- **录音不出本机**：识别、区分说话人都在本地完成，不上传录音。纪要可以用 DeepSeek、通义千问这类接口，也可以用本地 Ollama，全程断网。
- **为中文会议做的**：阿里达摩院 FunASR 的中文识别模型，中文标点、数字、人名都比通用的英文方案好；时间说法（"下周三""月底"）按会议日期换算成具体日子。
- **结论有出处**：纪要里每一条都带时间戳，点一下跳回录音那一秒；鱼骨图会统计每个分支引用了多少原文，一句都对不上的直接标出来，一眼看出谁在说空话。
- **中英双语**：界面、纪要、导出都支持中文和英文；英文会议用 SenseVoice 识别，中文会议也能直接出英文纪要。
- **桌面安装包约 13 MB**：Tauri + Rust，不用装 Python 和 ffmpeg。1 小时的会议结束后几秒出转写，76 分钟的录音导入后约 3.5 分钟转完。

如果你在找 **Otter.ai、Granola、Fireflies、飞书妙记、通义听悟、讯飞听见** 的本地、开源、免费替代，或者想要一个**不上传录音的 AI 会议记录工具**，Tingo 就是为这个做的。

> 截图里的会议、人名和数据都是虚构的演示内容。

## v0.5.5 · 实时说话人更准，窄窗口也好用

- 录音中区分说话人时会对全场重新聚类，并修正之前的编号。实测两场 3 人会议，之前会出现 50～90 多个临时说话人，现在录到最后是 3 位，和录完后的结果一致。
- 侧边栏可以收成图标栏。窗口较窄时会自动收起，展开后浮在内容上方；最小窗口宽度从 1000 降到 720。
- 收起时，环境异常和后台任务各用一个图标提示，点击会进入对应设置页或任务。

[下载 v0.5.5](https://github.com/tancheng33/tingo/releases/tag/v0.5.5) · v0.5.2 及以上支持应用内更新。

## v0.5.4 · 实时转写与录音工作台

- 录音时逐句显示转写和临时说话人编号，可回看整场文字，并标记重点。
- 识别前自动提升低音量音轨，减少轻声说话时的漏转。
- 切换设置只刷新右侧内容，大模型探测在后台进行。

[下载 v0.5.4](https://github.com/tancheng33/tingo/releases/tag/v0.5.4) · v0.5.2 及以上支持应用内更新。

## v0.5.3：流动背景与更新检查修复

- **首页渐变持续流动**：蓝、青、紫色光团缓慢移动，录音卡片保持稳定；遵循系统「减少动态效果」设置。
- **更新检查备用通道**：旧更新接口遇到 GitHub API 限流或网络错误时，自动读取发布清单，并保留安装包 SHA-256 校验。

[下载 v0.5.3](https://github.com/tancheng33/tingo/releases/tag/v0.5.3) · v0.5.2 用户可以在应用内更新；更早版本请手动安装本次更新。

## v0.5.2：应用内自动更新

- **不用再拖拽安装**：新版下载并通过签名校验后，点击「重启并更新」即可自动替换应用并重新打开，会议、配置和模型保留。
- **进度与安装分开**：界面显示下载进度和校验状态，可以稍后再安装；录音、转写和纪要等任务未完成时不能安装更新。
- **手动检查立即刷新**：修复「检查更新」仍使用后台旧缓存的问题。
- **正式签名与公证**：安装包及更新包均使用 Developer ID 签名并通过苹果公证；更新包另有绑定版本号的签名校验。

[下载 v0.5.2](https://github.com/tancheng33/tingo/releases/tag/v0.5.2) · v0.5.1 及更早版本需要手动安装这次更新；装上 v0.5.2 后，后续版本可在应用内完成更新。支持 Apple 芯片 Mac。

## 功能

### 原因分析鱼骨图：一眼看出哪条是空话

会上讨论了"为什么"——转化率为什么掉了、项目为什么延期——纪要里就会多一张鱼骨图。每个分支都绑着它依据的原话，点开能看到是谁在几分几秒说的，点时间戳跳回去听。

每个分支会统计引用了几段、多少字的原文：大模型给的时间戳在转写里对不上的，标成**无原文依据**（红色）；引用太少的标成**依据很少**（橙色）。会上有人凭感觉下的结论，在图上一眼就能看出来。

<p align="center"><img src="docs/screenshots/fishbone.png" width="820" alt="鱼骨图：每个分支绑着原话"></p>

鱼骨图可以一键导出成 [Excalidraw](https://excalidraw.com) 画板，接着改、拿去汇报：

<p align="center"><img src="docs/screenshots/excalidraw.png" width="720" alt="导出到 Excalidraw 的鱼骨图"></p>

### 不用等：录完就能开下一场

结束录音后，转写和纪要在后台进行，首页马上可以开下一场会。侧边栏能看到每个任务进行到哪一步；点进会议，能看着大模型一节一节写纪要——推理模型动笔前要先想一两分钟，这时也会显示"已思考多少字"，不会像卡住了一样。

<p align="center"><img src="docs/screenshots/home.png" width="49%" alt="首页：后台任务"> <img src="docs/screenshots/generating.png" width="49%" alt="纪要生成过程"></p>

### 搜得到会上说过的每一句话

在会议列表里搜关键词，会把所有会议的转写都翻一遍，列出命中的原话，点一句就跳到那场会的那一秒。纪要末尾有一张「人名与术语」表，记下正确写法和识别错的写法（比如"历史工单"被听成"历史中单"），搜索时会连错的写法一起搜出来；一键就能把它们加进热词。

### 谁说了什么

边录边转：录音时就在识别，结束后几秒出完整转写。按声纹区分说话人；不想一个个改名，可以让 AI 根据"王总你看呢"这类称呼和自我介绍猜出谁是谁，每个名字都附上依据。猜名时显示状态和实时耗时；保留已填写的名字，确认后保存，也可选择保存并重新生成纪要。

忘了点结束也没关系：会议软件退出、或者连续 10 分钟没人说话（可以调），录音会自动结束并截掉末尾的空白。点任意一句跳到那里回听，播放时当前发言会高亮。

<p align="center"><img src="docs/screenshots/transcript.png" width="820" alt="转写：区分说话人"></p>

### 导出与分享

- **PDF**：A4 排版，保留鱼骨图的问题、分支和说明，适合直接分享。
- **Word**：可以接着改的 .docx，发给领导最常用的格式。
- **SRT 字幕**：带说话人的逐句字幕，可以配进会议录屏。
- **Obsidian**：一场会一篇笔记，日期、时长、参会人写在属性里，鱼骨图存成 Excalidraw 画板嵌在笔记里；可以设成生成纪要后自动存。
- **飞书 / 钉钉 / 企业微信**：贴一个群机器人的 webhook，一键推送到群里。
- **Markdown**：复制粘贴到任何地方。

导出的鱼骨图不附带引用原话、引用统计和引用时间戳；PDF、Word 也不附带「人名与术语」。这些信息仍保留在应用内，方便核对和搜索。

<p align="center"><img src="docs/screenshots/pdf.png" width="820" alt="导出 PDF（旧版排版示意，当前导出已精简引用和术语）"></p>

### 纪要历史版本

重新生成成功或编辑保存时，旧纪要会自动保留。点击「历史版本」，在左侧选择保存时间，右侧查看内容；生成失败不会覆盖原来的纪要。

历史版本也支持 PDF、Word、存到 Obsidian、复制 Markdown 和推送到群。导出文件带版本标识，不会覆盖当前版；SRT 使用整场会议的转写，不随纪要版本变化。可单独删除不需要的旧版，确认删除后无法恢复，不影响当前纪要。

历史从启用此功能后开始保留，无法找回此前已覆盖的内容。

### 会议多了也好管

- **标签和归档**：按项目、会议类型打标签，会议列表按标签筛选；开完的会归档收起，搜索时照样能找到。
- **批量操作**：在会议列表里多选，一次加标签、归档或删除。
- **改标题**：随手改成好记的名字，不影响会议文件夹。
- **删除可恢复**：删除会议、只删录音都是移到废纸篓，后悔了还能拿回来。
- **管理空间**：设置里能看到录音占了多少、哪几场会最占地方；可以一键删掉 N 天前（或只删已归档）会议的录音，转写和纪要都保留。

### 识别模型可以换

默认的 Paraformer-large 适合大多数中文会议。会上中英文混着说，可以换成 SenseVoice；要更准、想让人名和项目代号在识别时就认对，可以换成 Fun-ASR-Nano（会直接用上你填的热词）。在设置里下载、切换，都在本地运行。

<p align="center"><img src="docs/screenshots/settings.png" width="820" alt="设置：识别模型"></p>

## 安装

### 桌面 App（推荐）

到 [Releases](https://github.com/tancheng33/tingo/releases/latest) 下载 `Tingo_x.y.z_aarch64.dmg`，拖进「应用程序」。目前只有 Apple 芯片（M1 及以后）的版本，需要 macOS 13 以上；录系统声音需要 macOS 15 以上。

第一次打开会有一个新手引导：下载识别模型、试录 3 秒确认权限、接上大模型，再看一场内置的示例会议。识别模型（约 330 MB：识别模型来自 ModelScope，国内直连；标点和声纹模型来自 GitHub，网络中断会自动续传），之后完全离线可用。

**v0.5.0 起，官方安装包已完成 Developer ID 签名和苹果公证。** 用户无需自行签名、右键绕过提示或运行 `xattr` 命令。请使用 GitHub Releases 中的官方 DMG；自行从源码构建的临时签名版本不属于公证发布包。

v0.5.2 及以后可在应用内下载并点击「重启并更新」。更早版本先退出旧版，再拖进「应用程序」替换；会议、历史版本和配置保留。首次从旧临时签名版升级后，如录音权限失效，请在「系统设置 → 隐私与安全性」重新授权麦克风、屏幕与系统录音。

第一次录音时，macOS 会请求「麦克风」和「屏幕与系统录音」权限（录对方的声音要用到后者），允许即可。有新版本时侧边栏的版本号旁边会亮起提示（侧边栏收起时显示在「设置」图标上），点击可下载更新，签名校验通过后选择「重启并更新」。

### 命令行

命令行版基于 Python（FunASR），适合放进脚本、批量处理录音。需要 Python 3.11+ 和 ffmpeg：

```bash
brew install ffmpeg
uv tool install git+https://github.com/tancheng33/tingo      # 或 pipx install git+…
tingo doctor                                                 # 检查环境
```

```bash
tingo record -t 周会 -w "林舟,注册转化"     # 录音（系统声音 + 麦克风），Ctrl+C 结束，自动出纪要
tingo run ~/Downloads/项目评审.mp4          # 处理已有的录音或录屏
tingo summarize ~/Documents/Tingo/2026-10-28_1400_复盘 -s "林舟,陈默"   # 改说话人名字后重新生成纪要
tingo push ~/Documents/Tingo/2026-10-28_1400_复盘                       # 推送到群
tingo ui                                                     # 打开图形界面
```

每场会议一个文件夹，默认在 `~/Documents/Tingo/`，桌面 App 和命令行共用。桌面版可在 **设置 → 存储** 选择其他目录，保存后立即生效，无需重启。默认将录音、转写、纪要及历史版本复制到新的空文件夹，显示进度并校验文件，原目录保留；也可以只切换目录，之后切回查看旧会议。正在录音或处理会议时不能切换。识别模型的位置不变。

目录结构：

```
2026-10-28_1400_注册转化复盘/
├── 录音.m4a          # 16 kHz 单声道，1 小时约 30 MB
├── 转写.md
├── transcript.json   # 结构化转写
└── 纪要.md
```

## 配置大模型

转写不需要大模型，生成纪要需要。任何 OpenAI 兼容接口都可以，在设置里点选，带连接测试：

| 选择 | 说明 |
|---|---|
| DeepSeek、通义千问（百炼） | 填 API Key 即可。实测 76 分钟的会，DeepSeek V4 Flash 约 50 秒出纪要 |
| 本机中转（Magpie、CC Switch） | 打开后自动探测，一键选用，模型从列表里挑 |
| 本地 Ollama | 全程不出本机，不需要 Key。本地模型上下文较短，长会议会自动分段提炼再合并 |
| 自定义 | 任意 OpenAI 兼容地址 |

配置文件在 `~/.config/tingo/config.toml`，也可以用环境变量 `TINGO_LLM_BASE_URL`、`TINGO_LLM_API_KEY`、`TINGO_LLM_MODEL`。

## 隐私

- 录音和转写只在你的电脑上，识别模型在本地运行。
- 生成纪要时，**转写文字**会发给你配置的大模型接口；用本地 Ollama 的话这一步也不出本机。
- 推送到群、存到 Obsidian、导出 PDF 都是你点了才发生。
- App 内的服务只监听 127.0.0.1，不对外开放。没有任何统计和上报。

## 工作原理

```
录音 ─┬─ 系统声音（ScreenCaptureKit）─┐
      └─ 麦克风 ──────────────────────┴─ 16 kHz 单声道，边录边按停顿切段
                                              │
          VAD 切分 → 识别（Paraformer / SenseVoice / Fun-ASR-Nano）→ 标点 → CAM++ 声纹
                                              │
                    谱聚类区分说话人（全场一次，说话人编号前后一致）
                                              │
          大模型写纪要（长会议分段提炼再合并；日期按会议日换算；术语表纠正同音字）
                                              │
               鱼骨图的每个引用对照转写核实 → 纪要.md → PDF / Obsidian / 群
```

几个细节：

- **桌面版**：Tauri 外壳 + Rust 核心库，sherpa-onnx 运行 FunASR 官方 Paraformer-large int8 模型，声纹聚类从 Python 版移植，与之逐秒对比 93.6% 一致。选型过程（比较了 8 套识别引擎）见 [`spike/sherpa/RESULTS.md`](spike/sherpa/RESULTS.md)。
- **外放回声**：开着扬声器开会时，麦克风会把对方的声音再录一遍。Tingo 在系统声音有人说话时压低麦克风，只在对方安静时用麦克风；双方同时说话时本地一侧会被盖掉，**戴耳机效果最好**。
- **元信息不交给模型**：纪要里的日期、时长、参会人由程序填写，模型只写内容，避免编造。
- **推理模型**：DeepSeek-R1、Qwen3 等的思考过程会被过滤，不进纪要。

## 和其他工具比

| | Tingo | 飞书妙记 / 通义听悟等云服务 | Meetily 等开源会议助手 |
|---|---|---|---|
| 录音存放 | 只在本机 | 上传到云端 | 本机 |
| 中文识别 | FunASR（中文专用） | 好 | 以 Whisper / Parakeet 为主，面向英文 |
| 录线上会议 | 直接录系统声音，不用装虚拟声卡 | 主要服务自家会议平台 | 支持 |
| 纪要用的模型 | 自己选，可以完全本地 | 平台指定 | 自己选 |
| 结论能否追溯原话 | 每条带时间戳，鱼骨图核实引用 | 部分支持 | — |
| 费用 | 免费开源 | 按时长或会员收费 | 免费开源 |

## 常见问题

**怎么把腾讯会议、飞书、Zoom 的会议录下来并转成文字？**
打开 Tingo 点录音，它会同时录下系统声音（对方的声音）和你的麦克风，不用装 BlackHole 之类的虚拟声卡。会议软件自己导出的录音、录屏（m4a、mp4、mp3、wav 等）也可以直接拖进窗口。

**会议录音会上传吗？**
不会。语音识别、区分说话人都在你的 Mac 上完成。生成纪要时只把转写文字发给你选的大模型；用本地 Ollama 的话这一步也不出本机，可以断网使用。

**能自动生成会议纪要和待办吗？**
能。纪要按一句话总结、议题、决议、待办（负责人、截止时间）、风险整理，每条带时间戳，点一下跳回录音原话。"下周三"这类说法会换算成具体日期。

**支持哪些大模型？**
任何 OpenAI 兼容接口（DeepSeek、通义千问、OpenAI、Gemini、Kimi、智谱、本地 Ollama 等）和 Anthropic（Claude）接口。

**能区分不同的人说的话吗？**
能，按声纹自动区分说话人，改个名字纪要会按新名字重新生成。

**英文会议能用吗？**
能。界面和纪要都支持英文，英文会议在设置里把识别模型换成 SenseVoice；中文会议也可以直接出英文纪要。

**Windows 能用吗？**
桌面 App 目前只有 macOS（Apple 芯片）版。命令行版可以在 Linux 上用麦克风录音，Windows 可以先用会议软件录制再导入。

## 已知限制

- 桌面 App 目前只有 Apple 芯片的 macOS 版；录系统声音需要 macOS 15+。命令行版在 Linux 上只能录麦克风，Windows 请先用会议软件录制再导入。
- 多人抢话、声音相近时说话人可能分错，可以在详情页改名后重新生成纪要。
- 外放时的回声处理是"让路"而不是真正的回声消除，换人处偶尔有一两个字识别错。

## 路线图

- [ ] 真正的回声消除（AEC），外放也能达到耳机的效果
- [ ] 菜单栏快捷录音，会议软件启动时提醒
- [ ] Windows 版（WASAPI 录系统声音）
- [ ] 导出到飞书文档 / 钉钉文档
- [x] App 正式签名与苹果公证（v0.5.0）

## 参与开发

```bash
git clone https://github.com/tancheng33/tingo && cd tingo

# 命令行 / Python 后端
uv sync
uv run pytest -m "not slow"          # 不下载模型的单元测试

# 界面（TypeScript + Vite），构建产物输出到 src/tingo/web，桌面版和 `tingo ui` 共用
cd app && pnpm install
pnpm dev                             # 浏览器打开 http://localhost:1420，接口转给 8765 端口的后端

# 桌面版（Tauri + Rust）
pnpm tauri dev                       # 开发窗口
pnpm tauri build                     # 打包 .app / .dmg
cd engine && cargo test              # Rust 核心库测试
cargo run --release --example serve -- <模型目录> <录音组件> 8765   # 只起后端，配合 pnpm dev
```

官方发布使用 `cd app && npm run release:mac`，之后提交公证并附加票据。完整步骤见 [macOS 正式发布流程](app/scripts/README.md)；普通源码构建无需发布证书。

欢迎提 Issue 和 PR。

## 致谢

- [FunASR](https://github.com/modelscope/FunASR)：语音识别、VAD、标点、说话人模型
- [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx)：桌面版的本地推理
- [Tauri](https://tauri.app)、[Excalidraw](https://excalidraw.com)
- [Meetily](https://github.com/Zackriya-Solutions/meetily)、[Hyprnote](https://github.com/fastrepl/anarlog)：本地会议助手的思路

## 改名说明

项目早期叫 jiyao（纪要）。改名后旧的命令 `jiyao`、环境变量 `JIYAO_*`、配置目录 `~/.config/jiyao/`、会议目录 `~/Documents/jiyao/` 都继续可用，不用搬数据。

## 许可

[MIT](LICENSE)

### Apple 系统语音识别（macOS 26+）

桌面版可在「设置 → 识别模型」选择 **Apple Speech**。先将会议语言明确设为「中文」或「英文」，再点击「检查／安装」准备系统语言模型及共享基础模型；已准备好时点击「使用」。模型由 macOS 下载、管理，音频在本机识别。系统不支持或语言模型不可用时会提示原因，可随时切回原有引擎。说话人区分仍由 Tingo 的声纹模型完成；此选项不改变大模型纪要生成方式。
