#!/bin/bash
# 打 Windows 安装包（NSIS，未签名）。在 Windows 上直接构建；在 macOS / Linux 上用 cargo-xwin 交叉编译，
# 需要先装好：rustup target add x86_64-pc-windows-msvc、cargo install cargo-xwin、llvm 和 lld（macOS：brew install llvm lld makensis）
set -euo pipefail
cd "$(dirname "$0")/.."
target=x86_64-pc-windows-msvc
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*|Windows_NT)
    pnpm tauri build --target "$target" "$@"
    ;;
  *)
    if command -v brew >/dev/null; then
      export PATH="$(brew --prefix llvm)/bin:$(brew --prefix lld)/bin:$PATH"
    fi
    pnpm tauri build --runner cargo-xwin --target "$target" "$@"
    ;;
esac
ls -l src-tauri/target/"$target"/release/bundle/nsis/*.exe
