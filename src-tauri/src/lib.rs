mod advanced;
mod archive;
#[cfg(feature = "headless-cli")]
pub mod cli;
#[cfg(feature = "desktop")]
mod dialogs;
mod firmware;
mod gpt;
mod host;
mod jobs;
mod loader;
mod local_jobs;
#[cfg(feature = "maintenance-cli")]
pub mod maintenance;
mod mcp;
mod model;
mod state;
mod usb;

use model::*;
use state::Backend;
#[cfg(feature = "desktop")]
use state::JobOrigin;
#[cfg(feature = "desktop")]
use std::{fs::OpenOptions, io::Write, path::Path, sync::Arc};
#[cfg(feature = "desktop")]
use tauri::{AppHandle, Manager, State};

#[cfg(feature = "desktop")]
#[tauri::command]
async fn scan_devices(state: State<'_, Arc<Backend>>) -> Result<Vec<DeviceInfo>, String> {
    scan(&state).await
}

async fn scan(backend: &Backend) -> Result<Vec<DeviceInfo>, String> {
    let _gate = backend
        .usb_gate
        .clone()
        .try_lock_owned()
        .map_err(|_| "设备任务运行中，扫描已暂停")?;
    let mut targets = usb::targets().await?;
    let connected_ids: Vec<_> = targets
        .iter()
        .map(|target| target.info.id.as_str())
        .collect();
    let rebooting_ids = backend.rebooting_device_ids(&connected_ids).await;
    targets.retain(|target| !rebooting_ids.contains(&target.info.id));
    probe_targets(targets).await
}

async fn probe_targets(targets: Vec<usb::Target>) -> Result<Vec<DeviceInfo>, String> {
    let mut devices = Vec::new();
    for mut target in targets {
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

#[cfg_attr(feature = "desktop", tauri::command)]
async fn inspect_firmware(path: String) -> Result<FirmwareInfo, String> {
    tokio::task::spawn_blocking(move || firmware::inspect(&path).map(|image| image.info))
        .await
        .map_err(|e| format!("固件检查任务异常：{e}"))?
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn inspect_archive(path: String) -> Result<archive::ArchiveInfo, String> {
    tokio::task::spawn_blocking(move || archive::inspect_archive(&path))
        .await
        .map_err(|e| format!("压缩包检查任务异常：{e}"))?
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn read_partitions(
    device_id: String,
    state: State<'_, Arc<Backend>>,
) -> Result<Vec<PartitionInfo>, String> {
    partitions(&state, &device_id).await
}

async fn partitions(backend: &Backend, device_id: &str) -> Result<Vec<PartitionInfo>, String> {
    let _gate = backend
        .usb_gate
        .clone()
        .try_lock_owned()
        .map_err(|_| "设备任务运行中，暂不能读取分区")?;
    let target = usb::resolve(device_id).await?;
    if target.info.mode != "loader" {
        return Err("读取分区需要 Loader 模式；请先下载 Loader".into());
    }
    let mut session = usb::Session::open(&target).await?;
    let capacity = session.capacity().await?;
    usb::partitions(&mut session, capacity).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn start_operation(
    request: OperationRequest,
    app: AppHandle,
    state: State<'_, Arc<Backend>>,
) -> Result<String, String> {
    let (prepared, gate) = prepare_operation(&state, request).await?;
    jobs::launch(
        host::Host::desktop(app),
        state.inner().clone(),
        prepared,
        gate,
        JobOrigin::Desktop,
    )
}

async fn prepare_operation(
    backend: &Backend,
    request: OperationRequest,
) -> Result<(jobs::Prepared, tokio::sync::OwnedMutexGuard<()>), String> {
    let gate = backend
        .usb_gate
        .clone()
        .try_lock_owned()
        .map_err(|_| "USB 设备正忙；请等待当前任务或扫描完成")?;
    if backend.is_busy() {
        return Err("已有任务正在运行，请勿重复提交".into());
    }
    Ok((jobs::prepare(request).await?, gate))
}

#[cfg(feature = "desktop")]
#[tauri::command]
fn cancel_operation(job_id: String, state: State<'_, Arc<Backend>>) -> Result<(), String> {
    state.cancel(&job_id)
}

#[cfg(feature = "desktop")]
#[tauri::command]
fn get_history(state: State<'_, Arc<Backend>>) -> Result<Vec<HistoryEntry>, String> {
    state.history()
}

#[cfg(feature = "desktop")]
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

#[cfg_attr(feature = "desktop", tauri::command)]
fn get_runtime_info() -> RuntimeInfo {
    RuntimeInfo {
        version: env!("CARGO_PKG_VERSION").into(),
        platform: format!("{} / {}", std::env::consts::OS, std::env::consts::ARCH),
        backend: "RockUSB 0.4 · nusb 原生 USB（无需命令行工具）".into(),
    }
}

#[cfg(feature = "desktop")]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let directory = app.path().app_data_dir()?;
            let backend =
                Arc::new(Backend::load(directory.clone()).map_err(std::io::Error::other)?);
            let mcp = tauri::async_runtime::block_on(mcp::Controller::restore(
                directory.join("mcp-settings.json"),
                app.handle().clone(),
                backend.clone(),
            ));
            app.manage(backend);
            app.manage(mcp);
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
            dialogs::choose_path,
            local_jobs::start_unpack,
            scan_devices,
            inspect_firmware,
            inspect_archive,
            read_partitions,
            start_operation,
            cancel_operation,
            get_history,
            export_logs,
            get_runtime_info,
            mcp::mcp_status,
            mcp::save_mcp_settings
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
