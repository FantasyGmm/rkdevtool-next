use crate::mcp::Controller;
use serde::Deserialize;
#[cfg(any(target_os = "macos", windows))]
use std::time::Duration;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
};
use tauri::{AppHandle, State, Window};
#[cfg(any(target_os = "macos", windows))]
use tauri::{Manager, WebviewWindow};
use tauri_plugin_dialog::{DialogExt, FilePath};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};
use tokio_util::sync::CancellationToken;

struct Picker {
    closed: CancellationToken,
    cancelled: CancellationToken,
}

pub(crate) struct DesktopDialogs {
    priority: Arc<Semaphore>,
    picker: Mutex<Weak<Picker>>,
}

impl Default for DesktopDialogs {
    fn default() -> Self {
        Self {
            priority: Arc::new(Semaphore::new(1)),
            picker: Mutex::default(),
        }
    }
}

impl DesktopDialogs {
    fn reserve_priority(&self) -> Result<OwnedSemaphorePermit, String> {
        self.priority
            .clone()
            .try_acquire_owned()
            .map_err(|_| "MCP 请求正在处理，请完成当前操作后重试。".into())
    }

    fn active_picker(&self) -> Result<std::sync::MutexGuard<'_, Weak<Picker>>, String> {
        self.picker.lock().map_err(|_| "文件窗口状态异常。".into())
    }

    fn begin_picker(&self) -> Result<Arc<Picker>, String> {
        // Register before releasing priority so MCP can also cancel a picker
        // that has been scheduled but is not visible yet.
        let _priority = self.reserve_priority()?;
        let mut active = self.active_picker()?;
        if active
            .upgrade()
            .is_some_and(|picker| !picker.closed.is_cancelled())
        {
            return Err("已有文件选择窗口打开。".into());
        }
        let picker = Arc::new(Picker {
            closed: CancellationToken::new(),
            cancelled: CancellationToken::new(),
        });
        *active = Arc::downgrade(&picker);
        Ok(picker)
    }

    pub(crate) async fn prioritize_mcp(
        &self,
        app: &AppHandle,
    ) -> Result<OwnedSemaphorePermit, String> {
        let priority = self.reserve_priority()?;
        let picker = self.active_picker()?.upgrade();
        if let Some(picker) = picker.filter(|picker| !picker.closed.is_cancelled()) {
            dismiss_picker(app, picker).await?;
        }
        Ok(priority)
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum FileDialog {
    Open,
    Loader,
    Directory,
    Save { name: PathBuf, log: bool },
}

#[tauri::command]
pub(crate) async fn choose_path(
    request: FileDialog,
    window: Window,
    controller: State<'_, Controller>,
) -> Result<Option<PathBuf>, String> {
    let picker = controller.dialogs.begin_picker()?;
    let dialog = window.dialog().file();
    #[cfg(any(target_os = "macos", windows))]
    let dialog = dialog.set_parent(&window);
    let (tx, rx) = oneshot::channel();
    let complete = move |path: Option<FilePath>| {
        picker.closed.cancel();
        let path = if picker.cancelled.is_cancelled() {
            None
        } else {
            path
        };
        let _ = tx.send(path);
    };
    match request {
        FileDialog::Open => dialog.pick_file(complete),
        FileDialog::Loader => dialog.add_filter("Loader", &["bin"]).pick_file(complete),
        FileDialog::Directory => dialog.pick_folder(complete),
        FileDialog::Save { name, log } => {
            let name: PathBuf = name.components().collect();
            let mut dialog = if log {
                dialog.add_filter("日志", &["log"])
            } else {
                dialog
            };
            if name.is_dir() {
                dialog = dialog.set_directory(name);
            } else {
                if let Some(parent) = name
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                {
                    dialog = dialog.set_directory(parent);
                }
                if let Some(name) = name.file_name() {
                    dialog = dialog.set_file_name(name.to_string_lossy());
                }
            }
            dialog.save_file(complete);
        }
    }
    rx.await
        .map_err(|_| "文件选择窗口已关闭。".to_string())?
        .map(FilePath::into_path)
        .transpose()
        .map_err(|e| format!("无法读取所选文件路径：{e}"))
}

#[cfg(any(target_os = "macos", windows))]
async fn dismiss_picker(app: &AppHandle, picker: Arc<Picker>) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or("找不到文件选择窗口。")?;
    picker.cancelled.cancel();
    // The picker might not yet have attached to its owner. Retry until the
    // native cancel is posted, and wait for the picker callback before asking.
    tokio::time::timeout(Duration::from_secs(3), async {
        while !picker.closed.is_cancelled() {
            let (tx, rx) = oneshot::channel();
            let (window, active) = (window.clone(), picker.clone());
            app.run_on_main_thread(move || {
                let posted = !active.closed.is_cancelled()
                    && !tx.is_closed()
                    && close_native_picker(&window);
                let _ = tx.send(posted);
            })
            .map_err(|e| format!("无法取消文件选择：{e}"))?;
            if rx.await.map_err(|_| "文件选择窗口已关闭。".to_string())? {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        picker.closed.cancelled().await;
        Ok(())
    })
    .await
    .map_err(|_| "无法自动关闭文件选择窗口；请先关闭窗口，再重试 MCP 请求。".to_string())?
}

#[cfg(not(any(target_os = "macos", windows)))]
async fn dismiss_picker(_app: &AppHandle, _picker: Arc<Picker>) -> Result<(), String> {
    Err("请先关闭文件选择窗口，再重试 MCP 请求。".into())
}

#[cfg(target_os = "macos")]
fn close_native_picker(window: &WebviewWindow) -> bool {
    use objc2_app_kit::{NSSavePanel, NSWindow};

    let Some(owner) = window
        .ns_window()
        .ok()
        .and_then(|ptr| unsafe { ptr.cast::<NSWindow>().as_ref() })
    else {
        return false;
    };
    let Some(sheet) = owner.attachedSheet() else {
        return false;
    };
    let Some(panel) = sheet.downcast_ref::<NSSavePanel>() else {
        return false;
    };
    // Called on the AppKit main thread; NSOpenPanel inherits NSSavePanel.
    unsafe { panel.cancel(None) };
    true
}

#[cfg(windows)]
fn close_native_picker(window: &WebviewWindow) -> bool {
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM},
        UI::WindowsAndMessaging::{
            EnumWindows, GW_OWNER, GetClassNameW, GetWindow, GetWindowThreadProcessId,
            PostMessageW, WM_CLOSE,
        },
    };

    struct Search {
        owner: HWND,
        closed: bool,
    }

    unsafe extern "system" fn visit(hwnd: HWND, param: LPARAM) -> i32 {
        // EnumWindows calls this synchronously with our live search record.
        let search = unsafe { &mut *(param as *mut Search) };
        let mut pid = 0;
        let mut class = [0u16; 16];
        unsafe {
            if GetWindow(hwnd, GW_OWNER) == search.owner {
                GetWindowThreadProcessId(hwnd, &mut pid);
                let len = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32);
                if pid == std::process::id()
                    && len == 6
                    && class[..6].iter().copied().eq("#32770".encode_utf16())
                {
                    search.closed = PostMessageW(hwnd, WM_CLOSE, 0, 0) != 0;
                    if search.closed {
                        return 0;
                    }
                }
            }
        }
        1
    }

    let Ok(owner) = window.hwnd() else {
        return false;
    };
    let mut search = Search {
        owner: owner.0,
        closed: false,
    };
    unsafe { EnumWindows(Some(visit), &mut search as *mut Search as LPARAM) };
    search.closed
}
