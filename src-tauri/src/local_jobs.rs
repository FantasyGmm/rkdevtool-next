//! Local-only unpack jobs share cancellation, history and close protection, not USB.
use crate::{
    firmware,
    model::{HistoryEntry, OperationEvent, OperationKind},
    state::Backend,
};
use chrono::Utc;
use serde::Deserialize;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, State};
const CANCELLED: &str = "__RK_LOCAL_CANCELLED__";
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnpackRequest {
    pub path: String,
    pub output_dir: String,
}
fn event(app: &AppHandle, id: &str, status: &str, message: &str, level: &str, bytes: u64) {
    let _ = app.emit(
        "operation-event",
        OperationEvent {
            job_id: id.into(),
            status: status.into(),
            stage: "unpack".into(),
            progress: if status == "completed" {
                Some(100.0)
            } else {
                None
            },
            message: message.into(),
            level: level.into(),
            timestamp: Utc::now().to_rfc3339(),
            bytes_done: Some(bytes),
            bytes_total: None,
        },
    );
}
#[tauri::command]
pub fn start_unpack(
    request: UnpackRequest,
    app: AppHandle,
    state: State<'_, Arc<Backend>>,
) -> Result<String, String> {
    if request.path.trim().is_empty() || request.output_dir.trim().is_empty() {
        return Err("请选择固件和解包输出目录".into());
    }
    let backend = state.inner().clone();
    let id = uuid::Uuid::new_v4().to_string();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut entry = HistoryEntry {
        id: id.clone(),
        device: "本地文件（无需 USB）".into(),
        operation: OperationKind::Unpack,
        file: Some(request.path.clone()),
        status: "failed".into(),
        started_at: Utc::now().to_rfc3339(),
        duration_ms: 0,
        message: "本地解包任务被中断".into(),
    };
    backend.begin(&entry, cancel.clone())?;
    let job_id = id.clone();
    tauri::async_runtime::spawn(async move {
        let start = Instant::now();
        let worker_app = app.clone();
        let worker_id = job_id.clone();
        let result = tauri::async_runtime::spawn_blocking(move || {
            let mut bytes = 0u64;
            let mut last = Instant::now();
            event(
                &worker_app,
                &worker_id,
                "running",
                "正在校验容器并解包；取消时会清理本次输出",
                "info",
                0,
            );
            let result = firmware::unpack(&request.path, &request.output_dir, |n| {
                if cancel.load(Ordering::Acquire) {
                    return Err(CANCELLED.into());
                }
                bytes = bytes.saturating_add(n);
                if last.elapsed() >= Duration::from_millis(150) {
                    event(
                        &worker_app,
                        &worker_id,
                        "running",
                        "正在校验/解包固件",
                        "info",
                        bytes,
                    );
                    last = Instant::now();
                }
                Ok(())
            });
            (result, bytes)
        })
        .await;
        let (status, message, level, bytes) = match result {
            Ok((Ok(path), bytes)) => ("completed", format!("解包完成：{path}"), "success", bytes),
            Ok((Err(e), bytes)) if e == CANCELLED => (
                "cancelled",
                "解包已取消，本次临时输出已清理".into(),
                "warning",
                bytes,
            ),
            Ok((Err(e), bytes)) => (
                "failed",
                format!("解包失败：{e}；本次输出已清理"),
                "error",
                bytes,
            ),
            Err(e) => ("failed", format!("本地解包任务异常：{e}"), "error", 0),
        };
        entry.status = status.into();
        entry.message = message.clone();
        entry.duration_ms = start.elapsed().as_millis().min(u64::MAX as u128) as u64;
        let message = match backend.finish(entry) {
            Ok(()) => message,
            Err(e) => format!("{message}；历史保存失败：{e}"),
        };
        event(&app, &job_id, status, &message, level, bytes);
    });
    Ok(id)
}
