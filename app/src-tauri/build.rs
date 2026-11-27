use std::path::Path;
use std::process::Command;

fn main() {
    // Windows 上录音、解码都在引擎进程里做（WASAPI、Media Foundation），没有 Swift 录音组件
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        build_recorder();
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // Windows N 版没有 Media Foundation：延迟加载，缺了也能启动，用到时引擎先检查再报错（engine/src/win.rs）
        for dll in ["mfplat.dll", "mfreadwrite.dll"] {
            println!("cargo:rustc-link-arg-bins=/DELAYLOAD:{dll}");
        }
        println!("cargo:rustc-link-arg-bins=delayimp.lib");
    }
    tauri_build::build()
}

fn build_recorder() {
    // 把 Swift 录音组件（录系统声音、麦克风，解码音视频）编译好，作为资源打进 App（tauri.macos.conf.json）
    let src = Path::new("../../src/tingo/recorder.swift");
    let out = Path::new("bin/tingo-recorder");
    println!("cargo:rerun-if-changed={}", src.display());
    std::fs::create_dir_all("bin").unwrap();
    let fresh = out.exists()
        && out.metadata().and_then(|m| m.modified()).ok()
            >= src.metadata().and_then(|m| m.modified()).ok();
    if !fresh {
        let status = Command::new("swiftc")
            // 最低系统和 App 一致（tauri.conf.json 的 minimumSystemVersion）。不写的话 swiftc 默认按本机 SDK 的最新版本，
            // 在别人较老的系统上录音组件根本起不来（"录音组件意外退出"）
            .args(["-O", "-target", "arm64-apple-macos13.0", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
            .status()
            .expect("需要 Xcode Command Line Tools（swiftc）来编译录音组件");
        assert!(status.success(), "录音组件编译失败");
    }
    // 公证也检查 Resources 中的 Swift 可执行文件，因此必须在封装 App 前签名。
    println!("cargo:rerun-if-env-changed=APPLE_SIGNING_IDENTITY");
    println!("cargo:rerun-if-changed=Entitlements.plist");
    if let Ok(identity) = std::env::var("APPLE_SIGNING_IDENTITY") {
        if !identity.is_empty() && identity != "-" {
            let status = Command::new("codesign")
                .args(["--force", "--sign", &identity, "--timestamp", "--options", "runtime",
                    "--identifier", "io.github.tancheng33.tingo.recorder",
                    "--entitlements", "Entitlements.plist", out.to_str().unwrap()])
                .status()
                .expect("无法启动 codesign");
            assert!(status.success(), "录音组件正式签名失败");
        }
    }
}
