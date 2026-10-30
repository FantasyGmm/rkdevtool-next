use crate::model::{HistoryEntry, OperationEvent, OperationKind};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex as AsyncMutex;

const HISTORY_LIMIT: usize = 100;

pub struct ActiveJob {
    pub id: String,
    pub cancel: Arc<AtomicBool>,
}

pub struct Backend {
    pub usb_gate: Arc<AsyncMutex<()>>,
    pub active: Mutex<Option<ActiveJob>>,
    history: Mutex<Vec<HistoryEntry>>,
    directory: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct Journal {
    entry: HistoryEntry,
}

impl Backend {
    pub fn load(directory: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&directory).map_err(|e| format!("无法创建应用数据目录：{e}"))?;
        let mut history: Vec<HistoryEntry> =
            read_json(&directory.join("history.json"))?.unwrap_or_default();
        if let Some(mut journal) = read_json::<Journal>(&directory.join("pending-job.json"))? {
            if !history.iter().any(|e| e.id == journal.entry.id) {
                journal.entry.status = "failed".into();
                journal.entry.message = if journal.entry.operation == OperationKind::Unpack {
                    "上次本地解包被中断；输出目录可能保留未完成的 rk-unpacked-* 子目录，请核对后清理或重新解包".into()
                } else {
                    "上次任务被应用或系统中断；设备可能仅部分写入，请重新连接并恢复烧录".into()
                };
                history.insert(0, journal.entry);
            }
            history.truncate(HISTORY_LIMIT);
            atomic_json(&directory.join("history.json"), &history)?;
            fs::remove_file(directory.join("pending-job.json"))
                .map_err(|e| format!("无法清理任务恢复记录：{e}"))?;
        }
        history.truncate(HISTORY_LIMIT);
        Ok(Self {
            usb_gate: Arc::new(AsyncMutex::new(())),
            active: Mutex::new(None),
            history: Mutex::new(history),
            directory,
        })
    }

    pub fn is_busy(&self) -> bool {
        self.active.lock().map(|j| j.is_some()).unwrap_or(true)
    }

    pub fn begin(&self, entry: &HistoryEntry, cancel: Arc<AtomicBool>) -> Result<(), String> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| "任务状态锁异常，请重启应用")?;
        if active.is_some() {
            return Err("已有设备任务正在运行".into());
        }
        if self.directory.join("pending-job.json").exists() {
            return Err(
                "上次任务记录尚未安全保存；请检查应用数据目录的可写空间并重启应用后继续".into(),
            );
        }
        atomic_json(
            &self.directory.join("pending-job.json"),
            &Journal {
                entry: entry.clone(),
            },
        )?;
        *active = Some(ActiveJob {
            id: entry.id.clone(),
            cancel,
        });
        Ok(())
    }

    pub fn finish(&self, entry: HistoryEntry) -> Result<(), String> {
        let mut history = self.history.lock().map_err(|_| "历史记录锁异常")?;
        history.retain(|e| e.id != entry.id);
        history.insert(0, entry);
        history.truncate(HISTORY_LIMIT);
        let result = atomic_json(&self.directory.join("history.json"), &*history).and_then(|_| {
            fs::remove_file(self.directory.join("pending-job.json"))
                .map_err(|e| format!("任务完成，但恢复记录清理失败：{e}"))
        });
        // USB ownership is released by the task after its terminal event. The
        // journal remains recoverable if persisting history failed.
        *self.active.lock().map_err(|_| "任务状态锁异常")? = None;
        result
    }

    pub fn history(&self) -> Result<Vec<HistoryEntry>, String> {
        self.history
            .lock()
            .map(|v| v.clone())
            .map_err(|_| "无法读取历史记录".into())
    }

    pub fn cancel(&self, id: &str) -> Result<(), String> {
        let active = self.active.lock().map_err(|_| "任务状态锁异常")?;
        match active.as_ref() {
            Some(job) if job.id == id => {
                job.cancel.store(true, Ordering::Release);
                Ok(())
            }
            _ => Err("此任务已结束，或不是当前正在运行的任务".into()),
        }
    }

    pub fn warn_close(&self, app: &AppHandle) {
        if let Ok(active) = self.active.lock() {
            if let Some(job) = active.as_ref() {
                let _ = app.emit(
                    "operation-event",
                    OperationEvent {
                        job_id: job.id.clone(),
                        status: "running".into(),
                        stage: "close-protected".into(),
                        progress: None,
                        message: "任务运行期间不能退出。请先取消并等待任务到达安全边界并完成清理"
                            .into(),
                        level: "warning".into(),
                        timestamp: Utc::now().to_rfc3339(),
                        bytes_done: None,
                        bytes_total: None,
                    },
                );
            }
        }
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    match fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("无法读取任务记录：{e}")),
        Ok(meta) if meta.len() > 2 * 1024 * 1024 => {
            return Err("任务记录异常过大；请备份应用数据目录后检查记录文件".into());
        }
        _ => {}
    }
    let file = File::open(path).map_err(|e| format!("无法打开任务记录：{e}"))?;
    serde_json::from_reader(file).map(Some).map_err(|e| {
        format!(
            "任务记录损坏（{}）：{e}。请备份并移走此文件后重试",
            path.display()
        )
    })
}

fn atomic_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let temp = path.with_extension("json.tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temp)
        .map_err(|e| format!("无法保存任务记录：{e}"))?;
    serde_json::to_writer_pretty(&mut file, value)
        .map_err(|e| format!("无法序列化任务记录：{e}"))?;
    file.write_all(b"\n")
        .and_then(|_| file.sync_all())
        .map_err(|e| format!("无法同步任务记录：{e}"))?;
    drop(file);
    fs::rename(&temp, path).map_err(|e| format!("无法提交任务记录：{e}"))
}
