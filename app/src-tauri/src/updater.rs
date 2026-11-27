//! Native updater: verified download first, explicit installation only while the engine is idle.
use std::sync::{Arc, Mutex};
use std::time::Duration;
use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_updater::{Update, UpdaterExt};
use crate::Backend;

#[derive(Clone, Default, Serialize)]
pub struct Status {
    current: String,
    latest: Option<String>,
    newer: bool,
    notes: String,
    url: String,
    native: bool,
    phase: String,
    downloaded: u64,
    total: Option<u64>,
    error: Option<String>,
}

#[derive(Default)]
struct Inner {
    status: Status,
    update: Option<Update>,
    bytes: Option<Vec<u8>>,
}

#[derive(Default)]
pub struct NativeUpdater {
    inner: Mutex<Inner>,
    operation: Arc<tokio::sync::Mutex<()>>,
}

impl NativeUpdater {
    fn status(&self, app: &AppHandle) -> Status {
        let mut status = self.inner.lock().unwrap().status.clone();
        status.current = app.package_info().version.to_string();
        status.native = true;
        status
    }

    fn fail(&self, error: String) -> String {
        let mut inner = self.inner.lock().unwrap();
        inner.status.phase = if inner.bytes.is_some() { "ready" } else { "available" }.into();
        inner.status.error = Some(error.clone());
        error
    }
}

#[tauri::command]
pub fn native_update_status(app: AppHandle, state: State<'_, Arc<NativeUpdater>>) -> Status {
    state.status(&app)
}

#[tauri::command]
pub async fn native_update_check(app: AppHandle, state: State<'_, Arc<NativeUpdater>>) -> Result<Status, String> {
    let Ok(_operation) = state.operation.clone().try_lock_owned() else { return Ok(state.status(&app)); };
    // A downloaded, verified release remains available even if the network goes offline.
    if state.inner.lock().unwrap().bytes.is_some() { return Ok(state.status(&app)); }
    let result = app.updater_builder().timeout(Duration::from_secs(20))
        .header("Cache-Control", "no-cache").map_err(|e| e.to_string())?
        .build().map_err(|e| e.to_string())?.check().await;
    match result {
        Ok(update) => {
            let mut inner = state.inner.lock().unwrap();
            inner.status = Status {
                latest: update.as_ref().map(|u| u.version.clone()),
                newer: update.is_some(),
                notes: update.as_ref().and_then(|u| u.body.clone()).unwrap_or_default(),
                url: update.as_ref().map(|u| format!("https://github.com/tancheng33/tingo/releases/tag/v{}", u.version)).unwrap_or_default(),
                phase: if update.is_some() { "available" } else { "idle" }.into(),
                ..Default::default()
            };
            inner.update = update;
        }
        Err(error) => return Err(state.fail(error.to_string())),
    }
    Ok(state.status(&app))
}

#[tauri::command]
pub async fn native_update_download(app: AppHandle, state: State<'_, Arc<NativeUpdater>>) -> Result<Status, String> {
    let _operation = state.operation.clone().try_lock_owned().map_err(|_| "更新操作正在进行 / Update already in progress")?;
    if state.inner.lock().unwrap().bytes.is_some() { return Ok(state.status(&app)); }
    let mut update = {
        let mut inner = state.inner.lock().unwrap();
        let update = inner.update.clone().ok_or("请先检查更新 / Check for updates first")?;
        if update.download_url.scheme() != "https" { return Err("更新下载必须使用 HTTPS / HTTPS is required".into()); }
        inner.status.phase = "downloading".into();
        inner.status.error = None;
        inner.status.downloaded = 0;
        inner.status.total = None;
        update
    };
    update.timeout = Some(Duration::from_secs(20 * 60));
    let bytes = update.download(|size, total| {
        let mut inner = state.inner.lock().unwrap();
        inner.status.downloaded += size as u64;
        inner.status.total = total;
    }, || {
        state.inner.lock().unwrap().status.phase = "verifying".into();
    }).await.map_err(|e| state.fail(format!("下载或签名校验失败 / Download or verification failed: {e}")))?;
    {
        let mut inner = state.inner.lock().unwrap();
        inner.bytes = Some(bytes);
        inner.status.phase = "ready".into();
        inner.status.error = None;
    }
    Ok(state.status(&app))
}

#[tauri::command]
pub async fn native_update_install(app: AppHandle, state: State<'_, Arc<NativeUpdater>>, backend: State<'_, Arc<Backend>>) -> Result<(), String> {
    let operation = state.operation.clone().try_lock_owned().map_err(|_| "更新操作正在进行 / Update already in progress")?;
    let engine = backend.engine.lock().unwrap().clone().ok_or("服务尚未就绪 / Engine not ready")?;
    let idle = engine.prepare_app_update()?;
    #[cfg(target_os = "macos")]
    {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let bundle = exe.parent().and_then(|p| p.parent()).and_then(|p| p.parent());
        if !bundle.is_some_and(|p| p.extension().is_some_and(|e| e == "app") && !p.starts_with("/Volumes")) {
            return Err("请先将 Tingo 安装到应用程序后再更新 / Install Tingo in Applications before updating".into());
        }
    }
    let (update, bytes) = {
        let mut inner = state.inner.lock().unwrap();
        let update = inner.update.clone().ok_or("请先下载更新 / Download the update first")?;
        let bytes = inner.bytes.take().ok_or("请先下载更新 / Download the update first")?;
        inner.status.phase = "installing".into();
        inner.status.error = None;
        (update, bytes)
    };
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        // Hold both guards until restart, even if the webview closes or cancels its request.
        let (_operation, _idle) = (operation, idle);
        match update.install(&bytes) {
            Ok(()) => app.restart(),
            Err(error) => {
                state.inner.lock().unwrap().bytes = Some(bytes);
                Err(state.fail(format!("安装失败，未重启 / Installation failed: {error}")))
            }
        }
    }).await.map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run with the actual signed release artifact; only a temporary fixture app is replaced.
    #[test]
    #[ignore = "requires TINGO_UPDATE_TEST_ARCHIVE and TINGO_UPDATE_TEST_MANIFEST"]
    fn signed_archive_install_and_tamper_rejection() {
        use std::path::PathBuf;
        let archive = PathBuf::from(std::env::var("TINGO_UPDATE_TEST_ARCHIVE").unwrap());
        let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(std::env::var("TINGO_UPDATE_TEST_MANIFEST").unwrap()).unwrap()).unwrap();
        let config: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        context.config_mut().plugins.0.insert("updater".into(), config["plugins"]["updater"].clone());
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_updater::Builder::new().build())
            .build(context).unwrap();
        let fixture = std::env::temp_dir().join(format!("tingo-updater-test-{}", std::process::id()));
        assert!(!fixture.exists());
        let bundle = fixture.join("Tingo.app");
        let exe = bundle.join("Contents/MacOS/tingo");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"old app fixture").unwrap();
        let updater = app.updater_builder()
            .pubkey(config["plugins"]["updater"]["pubkey"].as_str().unwrap())
            .executable_path(&exe).target("darwin-aarch64")
            .version_comparator(|_, _| true) // fixture only; production uses strict upgrades
            .build().unwrap();
        let update = updater.restore_update(manifest.clone()).unwrap().unwrap();
        let bytes = std::fs::read(archive).unwrap();
        update.verify(&bytes).unwrap();
        let mut damaged = bytes.clone();
        damaged[100] ^= 1;
        assert!(update.install(&damaged).is_err());
        assert_eq!(std::fs::read(&exe).unwrap(), b"old app fixture");
        let mut wrong_version = manifest;
        wrong_version["version"] = serde_json::json!("99.0.0");
        assert!(updater.restore_update(wrong_version).unwrap().unwrap().verify(&bytes).is_err());
        update.install(bytes).unwrap();
        assert_ne!(std::fs::read(&exe).unwrap(), b"old app fixture");
        assert!(std::process::Command::new("codesign").args(["--verify", "--deep", "--strict"]).arg(&bundle).status().unwrap().success());
        assert!(std::process::Command::new("xcrun").args(["stapler", "validate"]).arg(&bundle).status().unwrap().success());
        std::fs::remove_dir_all(fixture).unwrap();
    }
}
