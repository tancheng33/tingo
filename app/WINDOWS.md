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

系统要求：Windows 10 1809 及以上或 Windows 11，64 位（x86_64）。Windows N 版要先装「媒体功能包」，否则导入录音和压缩录音会报错。

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

## 还没做的

- 代码签名（SmartScreen 提示）
- 自动更新：要在发布时给 Windows 安装包签更新签名（`createUpdaterArtifacts`），并把 `windows-x86_64` 写进 `latest.json`
- ARM64 版
