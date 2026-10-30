mod advanced;
mod archive;
mod firmware;
mod gpt;
mod jobs;
mod loader;
mod local_jobs;
mod model;
mod state;
mod usb;

use model::*;
use state::Backend;
use std::{fs::OpenOptions, io::Write, path::Path, sync::Arc};
use tauri::{AppHandle, Manager, State};

#[tauri::command]
async fn scan_devices(state: State<'_, Arc<Backend>>) -> Result<Vec<DeviceInfo>, String> {
    let _gate = state
        .usb_gate
        .clone()
        .try_lock_owned()
        .map_err(|_| "设备任务运行中，扫描已暂停")?;
    let targets = usb::targets().await?;
    let connected_ids: Vec<_> = targets
        .iter()
        .map(|target| target.info.id.as_str())
        .collect();
    let rebooting_ids = state.rebooting_device_ids(&connected_ids).await;
    let mut devices = Vec::new();
    for mut target in targets {
        if rebooting_ids.contains(&target.info.id) {
            continue;
        }
        if target.info.mode == "loader" {
            let detected = format!(
                "已检测到 Loader 候选设备（{}），但读取失败",
                target.description()
            );
            let mut session = usb::probe_loader(&mut target)
                .await
                .map_err(|e| format!("{detected}：{e}"))?;
            target.info.capacity = Some(
                session
                    .capacity()
                    .await
                    .map_err(|e| format!("{detected}：{e}"))?
                    * 512,
            );
        }
        devices.push(target.info);
    }
    Ok(devices)
}

#[tauri::command]
async fn inspect_firmware(path: String) -> Result<FirmwareInfo, String> {
    tauri::async_runtime::spawn_blocking(move || firmware::inspect(&path).map(|image| image.info))
        .await
        .map_err(|e| format!("固件检查任务异常：{e}"))?
}

#[tauri::command]
async fn inspect_archive(path: String) -> Result<archive::ArchiveInfo, String> {
    tauri::async_runtime::spawn_blocking(move || archive::inspect_archive(&path))
        .await
        .map_err(|e| format!("压缩包检查任务异常：{e}"))?
}

#[tauri::command]
async fn read_partitions(
    device_id: String,
    state: State<'_, Arc<Backend>>,
) -> Result<Vec<PartitionInfo>, String> {
    let _gate = state
        .usb_gate
        .clone()
        .try_lock_owned()
        .map_err(|_| "设备任务运行中，暂不能读取分区")?;
    let target = usb::resolve(&device_id).await?;
    if target.info.mode != "loader" {
        return Err("读取分区需要 Loader 模式；请先下载 Loader".into());
    }
    let mut session = usb::Session::open(&target).await?;
    let capacity = session.capacity().await?;
    usb::partitions(&mut session, capacity).await
}

#[tauri::command]
async fn start_operation(
    request: OperationRequest,
    app: AppHandle,
    state: State<'_, Arc<Backend>>,
) -> Result<String, String> {
    let gate = state
        .usb_gate
        .clone()
        .try_lock_owned()
        .map_err(|_| "USB 设备正忙；请等待当前任务或扫描完成")?;
    if state.is_busy() {
        return Err("已有任务正在运行，请勿重复提交".into());
    }
    let prepared = jobs::prepare(request).await?;
    jobs::launch(app, state.inner().clone(), prepared, gate)
}

#[tauri::command]
fn cancel_operation(job_id: String, state: State<'_, Arc<Backend>>) -> Result<(), String> {
    state.cancel(&job_id)
}

#[tauri::command]
fn get_history(state: State<'_, Arc<Backend>>) -> Result<Vec<HistoryEntry>, String> {
    state.history()
}

#[tauri::command]
async fn export_logs(path: String, contents: String) -> Result<(), String> {
    if contents.len() > 16 * 1024 * 1024 {
        return Err("日志超过 16 MiB，请减少导出范围".into());
    }
    if !Path::new(&path).is_absolute() {
        return Err("请选择完整的日志保存路径".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .map_err(|e| format!("无法创建日志：{e}"))?;
        file.write_all(contents.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|e| format!("日志保存失败：{e}"))
    })
    .await
    .map_err(|e| format!("日志导出任务异常：{e}"))?
}

#[tauri::command]
fn get_runtime_info() -> RuntimeInfo {
    RuntimeInfo {
        version: env!("CARGO_PKG_VERSION").into(),
        platform: format!("{} / {}", std::env::consts::OS, std::env::consts::ARCH),
        backend: "RockUSB 0.4 · nusb 原生 USB（无需命令行工具）".into(),
    }
}

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let backend =
                Backend::load(app.path().app_data_dir()?).map_err(std::io::Error::other)?;
            app.manage(Arc::new(backend));
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<Arc<Backend>>();
                if state.is_busy() {
                    api.prevent_close();
                    state.warn_close(window.app_handle());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            advanced::query_device,
            advanced::switch_storage,
            local_jobs::start_unpack,
            scan_devices,
            inspect_firmware,
            inspect_archive,
            read_partitions,
            start_operation,
            cancel_operation,
            get_history,
            export_logs,
            get_runtime_info
        ])
        .build(tauri::generate_context!())
        .expect("RKDevTool Next failed to initialize");
    app.run(|app, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            let state = app.state::<Arc<Backend>>();
            if state.is_busy() {
                api.prevent_exit();
                state.warn_close(app);
            }
        }
    });
}
