# Windows 版

和 macOS 版同一套代码。界面、识别、说话人区分、纪要都一样，差别只在录音和导出用的系统组件：

| 能力 | macOS | Windows |
| --- | --- | --- |
| 录麦克风 | Swift 录音组件（AVFoundation） | WASAPI，引擎进程里直接录 |
| 录系统声音（会议里对方的声音） | ScreenCaptureKit，需要「屏幕与系统录音」权限，macOS 15+ | WASAPI 回环录音，不需要权限 |
| 解码导入的音视频 | AVFoundation | Media Foundation |
| 录音压成 m4a | afconvert，AAC 48 kbps | Media Foundation，AAC 96 kbps（系统编码器最低就是这个码率） |
| 导出 PDF | WebKit | 无界面的 Microsoft Edge |
| 导出 Word | textutil | 引擎自己生成 .docx |
| 删除 | 废纸篓 | 回收站 |
| 识别模型目录 | ~/Library/Application Support/io.github.tancheng33.tingo/models | %LOCALAPPDATA%\io.github.tancheng33.tingo\models |

会议默认存在 `%USERPROFILE%\Documents\Tingo`，配置在 `%USERPROFILE%\.config\tingo\config.toml`。

系统要求：Windows 10 1809 及以上或 Windows 11，64 位（x86_64）。Windows N 版能打开，但录完压缩录音、导入音视频都要先装「媒体功能包」，否则会提示缺少媒体组件。

## 安装

安装包还没有代码签名。第一次运行时 SmartScreen 会提示「Windows 已保护你的电脑」：点「更多信息」→「仍要运行」。
安装到当前用户目录，不需要管理员权限。缺 WebView2 运行时（极少数没更新过的 Windows 10）时安装程序会自动下载。

## 打安装包

在 Windows 上：

```
cd app
pnpm install
pnpm release:windows
```

在 macOS 上交叉编译（不需要 Windows 机器）：

```
rustup target add x86_64-pc-windows-msvc
cargo install cargo-xwin
brew install llvm lld makensis
cd app && pnpm release:windows
```

产物在 `app/src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/`（Windows 上直接构建时没有中间那层 `x86_64-pc-windows-msvc`）。
CI 的 `windows` 任务也会构建一份，在 Actions 的产物 `tingo-windows-installer` 里。

只检查能不能编译（macOS 上）：

```
cd app/engine
PATH="$(brew --prefix llvm)/bin:$(brew --prefix lld)/bin:$PATH" cargo xwin check --target x86_64-pc-windows-msvc --all-targets
```

## 实机测试清单

CI 的 Windows 机器上已经跑过：Media Foundation 压缩再解码、Edge 打印 A4 PDF、生成 Word。CI 机器没有声卡，录音只能在真机上测。

1. 安装：SmartScreen 点「更多信息 → 仍要运行」，装完能打开，界面正常、中文字体正常。
2. 新手引导：下载识别模型（国内源、GitHub 源各试一次更好）。
3. 新手引导的「试录 3 秒」：一边说话一边放段视频，「系统声音」「麦克风」都应该是「正常」。
4. 麦克风权限：到「设置 → 隐私和安全性 → 麦克风」关掉「允许桌面应用访问你的麦克风」，再试录，应提示「没有权限」并给出设置路径；打开后恢复。
5. 正式录一场（5 分钟以上，中间留一段一分钟左右完全不放声音）：
   - 用腾讯会议 / 飞书 / Teams / Zoom 开个会或放视频，确认对方的声音被录到、边录边转有字；
   - 没声音的那段过后，转写和录音里的时间要对得上（回放时点时间戳跳到的就是那句话）；
   - 录音中途拔插耳机、切换默认播放设备，录音不应中断（切换那一两秒可能是静音）。
6. 停止后：生成 `录音.m4a`，能在详情页回放，能拖动进度条；转写、说话人、纪要正常。
7. 导入：拖入一个 mp3、一个 m4a、一个 mp4 视频，都能转写。
8. 导出：PDF（A4、中文正常、打开后资源管理器选中文件）、Word（用 Word 或 WPS 打开，标题、列表、表格正常）、SRT。
9. 设置 → 存储：「选择目录」弹出系统的选文件夹对话框，切换目录、搬迁会议正常。
10. 删除会议、删除录音：进回收站，能从回收站还原。
11. 自动停止：开着会议软件录音，退出会议软件后能自动结束录音（如果开了这个选项）。
12. 「在资源管理器中显示」、打开 Obsidian（装了的话）。

有问题时把界面提示、以及 `%USERPROFILE%\Documents\Tingo` 里那场会议的文件列表发回来。

## 还没做的

- 代码签名（SmartScreen 提示）
- 自动更新：要在发布时给 Windows 安装包签更新签名（`createUpdaterArtifacts`），并把 `windows-x86_64` 写进 `latest.json`
- ARM64 版
