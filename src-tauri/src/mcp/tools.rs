use super::{Approval, Scope, write_new_file};
#[cfg(feature = "desktop")]
use crate::dialogs::DesktopDialogs;
use crate::{
    advanced,
    host::Host,
    local_jobs,
    model::{OperationKind, OperationRequest, WriteEntry},
    state::{Backend, JobOrigin},
};
use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig},
    schemars, tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
#[cfg(feature = "desktop")]
use std::time::Duration;
use std::{fs, path::Path, sync::Arc};
#[cfg(feature = "desktop")]
use tauri::Manager;
#[cfg(feature = "desktop")]
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
#[cfg(feature = "desktop")]
use tokio::sync::{OwnedSemaphorePermit, oneshot};

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct FileArgs {
    /// Absolute file path on the computer running RKDevTool Next.
    path: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeviceArgs {
    /// Current device ID from scan_devices. Scan again after reconnecting.
    device_id: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct QueryArgs {
    device_id: String,
    /// One of flashId, flashInfo, chipInfo, capability, testDevice, currentStorage.
    query: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StorageArgs {
    device_id: String,
    /// Named medium: nand, emmc, sd0, sd1, spi-nor, spi-nand, ram, mtd-blk-nand, mtd-blk-spi-nand, mtd-blk-spi-nor, sata, pcie or ufs.
    storage: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UnpackArgs {
    /// Absolute path to the RKFW/RKAF container.
    path: String,
    /// Existing absolute directory. A fresh child directory is created.
    output_dir: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct JobArgs {
    job_id: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LogArgs {
    job_id: Option<String>,
    /// Maximum entries to return (1–1000, default 100).
    limit: Option<u16>,
}

#[derive(Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum DeviceOperation {
    Flash,
    Backup,
    Erase,
    Loader,
    Reboot,
    Maskrom,
}

impl From<DeviceOperation> for OperationKind {
    fn from(kind: DeviceOperation) -> Self {
        match kind {
            DeviceOperation::Flash => Self::Flash,
            DeviceOperation::Backup => Self::Backup,
            DeviceOperation::Erase => Self::Erase,
            DeviceOperation::Loader => Self::Loader,
            DeviceOperation::Reboot => Self::Reboot,
            DeviceOperation::Maskrom => Self::Maskrom,
        }
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StartArgs {
    /// Current device ID from scan_devices.
    device_id: String,
    kind: DeviceOperation,
    /// Absolute path to a firmware, archive, or RAM Loader.
    firmware_path: Option<String>,
    /// Absolute path to a new file for a backup.
    output_path: Option<String>,
    /// Zero-based LBA, in 512-byte sectors. Omit for package flash, Loader and reset.
    start_sector: Option<u64>,
    /// Number of 512-byte sectors. Required for backup/erase; optional bound for RAW flash.
    sectors: Option<u64>,
    /// Reboot after a successful flash (default false). Flash only.
    reboot: Option<bool>,
    /// Exact GPT partition name. Include startSector and sectors from read_partitions.
    partition_name: Option<String>,
    /// Exact member name from inspect_archive. firmwarePath is the archive itself.
    archive_member: Option<String>,
    /// 1–128 non-overlapping RAW images. Flash only; replaces firmwarePath and single-target range.
    writes: Option<Vec<WriteEntry>>,
}

impl StartArgs {
    fn request(self) -> Result<OperationRequest, String> {
        let kind = OperationKind::from(self.kind);
        let flash = kind == OperationKind::Flash;
        let backup = kind == OperationKind::Backup;
        let range_required =
            backup || kind == OperationKind::Erase || self.partition_name.is_some();
        let batch = self.writes.is_some();
        let needs_firmware = (flash && !batch) || kind == OperationKind::Loader;
        if needs_firmware != self.firmware_path.is_some() || backup != self.output_path.is_some() {
            return Err(
                "单镜像 flash/loader 需要 firmwarePath；backup 需要 outputPath；批量 flash 只用 writes。"
                    .into(),
            );
        }
        if !flash && (batch || self.reboot.is_some() || self.archive_member.is_some()) {
            return Err("writes、archiveMember 和 reboot 仅用于 flash。".into());
        }
        if self.partition_name.is_some()
            && !matches!(
                kind,
                OperationKind::Flash | OperationKind::Backup | OperationKind::Erase
            )
        {
            return Err("partitionName 仅用于 flash、backup 和 erase。".into());
        }
        if range_required && self.sectors.is_none()
            || !flash && !range_required && (self.start_sector.is_some() || self.sectors.is_some())
            || self.sectors == Some(0)
        {
            return Err(
                "backup、erase 和分区操作需要正数 sectors；Loader 和重启不接受扇区参数。".into(),
            );
        }
        if let Some(writes) = &self.writes {
            if !(1..=128).contains(&writes.len())
                || self.sectors.is_some()
                || self.start_sector.is_some()
                || self.partition_name.is_some()
                || self.archive_member.is_some()
            {
                return Err("批量 flash 需要 1–128 个 RAW 条目；不接受单镜像目标参数。".into());
            }
            for entry in writes {
                input_file(&entry.path)?;
            }
        }
        if let Some(path) = &self.firmware_path {
            input_file(path)?;
        }
        if let Some(path) = &self.output_path {
            absolute_path(path)?;
        }
        Ok(OperationRequest {
            device_id: self.device_id,
            kind,
            firmware_path: self.firmware_path,
            output_path: self.output_path,
            start_sector: self.start_sector.unwrap_or(0),
            sectors: self.sectors,
            _legacy_verify: None,
            reboot: flash && self.reboot.unwrap_or(false),
            confirmed: false,
            writes: self.writes,
            archive_member: self.archive_member,
            partition_name: self.partition_name,
        })
    }
}

#[derive(Clone)]
pub(super) struct Tools {
    host: Host,
    backend: Arc<Backend>,
    scope: Scope,
    #[cfg(feature = "desktop")]
    dialogs: Arc<DesktopDialogs>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl Tools {
    pub(super) fn new(
        host: Host,
        backend: Arc<Backend>,
        scope: Scope,
        #[cfg(feature = "desktop")] dialogs: Arc<DesktopDialogs>,
    ) -> Self {
        Self {
            host,
            backend,
            scope,
            #[cfg(feature = "desktop")]
            dialogs,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "List connected Rockchip devices, mode, USB identity, and Loader chip and capacity (bytes)."
    )]
    async fn scan_devices(&self) -> CallToolResult {
        result(crate::scan(&self.backend).await)
    }

    #[tool(description = "Inspect a local RAW, RKFW, RKAF or MiniLoader file. Does not flash it.")]
    async fn inspect_firmware(&self, Parameters(args): Parameters<FileArgs>) -> CallToolResult {
        if let Err(error) = input_file(&args.path) {
            return failure(error);
        }
        result(crate::inspect_firmware(args.path).await)
    }

    #[tool(
        description = "Inspect a local ZIP or compressed firmware and list its supported member names."
    )]
    async fn inspect_archive(&self, Parameters(args): Parameters<FileArgs>) -> CallToolResult {
        if let Err(error) = input_file(&args.path) {
            return failure(error);
        }
        result(crate::inspect_archive(args.path).await)
    }

    #[tool(
        description = "Read Loader diagnostics: Flash ID, flash geometry, Chip ID, capabilities, USB response time or current storage."
    )]
    async fn query_device(&self, Parameters(args): Parameters<QueryArgs>) -> CallToolResult {
        result(advanced::read_query(&self.backend, &args.device_id, &args.query).await)
    }

    #[tool(
        description = "Switch the Loader's active storage. Desktop only; approval required unless full unattended access is enabled. Discard previous partition results and scan again."
    )]
    async fn switch_storage(&self, Parameters(args): Parameters<StorageArgs>) -> CallToolResult {
        result(self.switch(args).await)
    }

    #[tool(
        description = "Read the device GPT in Loader mode. Returns partition starts and lengths in 512-byte sectors."
    )]
    async fn read_partitions(&self, Parameters(args): Parameters<DeviceArgs>) -> CallToolResult {
        result(crate::partitions(&self.backend, &args.device_id).await)
    }

    #[tool(
        description = "Request a device job: flash RKFW/RKAF, RAW, archive member, or RAW batch; back up a whole device, partition or LBA range; erase a range or partition; load a RAM Loader, reboot, or enter Maskrom. Use read_partitions for partition ranges; whole device is startSector=0 and sectors=capacity/512. Desktop approval depends on local settings; headless mode only permits flash jobs with --allow-flash. Returns jobId; poll get_operation."
    )]
    async fn start_operation(&self, Parameters(args): Parameters<StartArgs>) -> CallToolResult {
        result(self.start(args).await.map(|id| json!({ "jobId": id })))
    }

    #[tool(
        description = "Validate and unpack RKFW/RKAF members into a new child of an existing local directory. Desktop only; approval required unless full unattended access is enabled. Returns jobId."
    )]
    async fn start_unpack(&self, Parameters(args): Parameters<UnpackArgs>) -> CallToolResult {
        result(self.unpack(args).await.map(|id| json!({ "jobId": id })))
    }

    #[tool(
        description = "Poll an operation by jobId. Returns current progress or the terminal history entry."
    )]
    fn get_operation(&self, Parameters(args): Parameters<JobArgs>) -> CallToolResult {
        result(self.operation(&args.job_id))
    }

    #[tool(
        description = "Request cancellation at the next safe block boundary. Desktop only; approval required unless full unattended access is enabled. Already written sectors are not rolled back."
    )]
    async fn cancel_operation(&self, Parameters(args): Parameters<JobArgs>) -> CallToolResult {
        let outcome = async {
            if !self.active(&args.job_id)? {
                return Err("任务已结束或不是当前任务。".into());
            }
            let approval = self
                .approve(
                    false,
                    format!(
                        "MCP 客户端请求停止任务 {}。已写入的数据不会回滚。",
                        args.job_id
                    ),
                )
                .await?;
            self.scope
                .commit(approval, || self.backend.cancel(&args.job_id))?;
            Ok(json!({ "cancellationRequested": true }))
        }
        .await;
        result(outcome)
    }

    #[tool(description = "Read persisted RKDevTool Next job history, newest first.")]
    fn get_history(&self) -> CallToolResult {
        result(self.backend.history())
    }

    #[tool(
        description = "Read recent backend task logs from this RKDevTool Next session, optionally filtered by jobId."
    )]
    fn get_logs(&self, Parameters(args): Parameters<LogArgs>) -> CallToolResult {
        let limit = args.limit.unwrap_or(100);
        if !(1..=1000).contains(&limit) {
            return failure("limit 必须为 1 至 1000。".into());
        }
        result(
            self.backend
                .operation_events(args.job_id.as_deref(), usize::from(limit)),
        )
    }

    #[tool(
        description = "Export this session's recent backend task logs to a new local file. Desktop only; approval required unless full unattended access is enabled. Never overwrites an existing file."
    )]
    async fn export_logs(&self, Parameters(args): Parameters<FileArgs>) -> CallToolResult {
        let outcome = async {
            absolute_path(&args.path)?;
            let events = self.backend.operation_events(None, 1000)?;
            let contents: String = events
                .iter()
                .map(|event| {
                    format!(
                        "{} [{}] {} {}\n",
                        event.timestamp,
                        event.level.to_uppercase(),
                        event.job_id,
                        event.message
                    )
                })
                .collect();
            let approval = self
                .approve(
                    false,
                    format!(
                        "MCP 客户端请求将 {} 条任务日志保存到：\n{:?}",
                        events.len(),
                        args.path
                    ),
                )
                .await?;
            let scope = self.scope.clone();
            let path = args.path.clone();
            tokio::task::spawn_blocking(move || {
                scope.commit(approval, || {
                    write_new_file(Path::new(&path), contents.as_bytes())
                })
            })
            .await
            .map_err(|e| format!("日志导出任务异常：{e}"))??;
            Ok(json!({ "path": args.path, "entries": events.len() }))
        }
        .await;
        result(outcome)
    }

    #[tool(description = "Report the RKDevTool Next version, OS, architecture and USB backend.")]
    fn get_runtime_info(&self) -> CallToolResult {
        result(Ok(crate::get_runtime_info()))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Tools {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
                    .with_title("RKDevTool Next"),
            )
            .with_instructions(if self.host.is_headless() {
                "Headless RKDevTool Next. Scan before addressing a device. The server is read-only unless started with --allow-flash, which permits flash jobs only. Poll get_operation for job progress."
            } else {
                "Local RKDevTool Next desktop tools. Scan before addressing a device. By default, changes and exports require desktop approval. The local policy can allow only flash jobs, or all supported operations, without prompts. Poll get_operation for job progress."
            }.to_string())
    }
}

impl Tools {
    async fn switch(&self, args: StorageArgs) -> Result<advanced::QueryReport, String> {
        let requested = advanced::storage_index(&args.storage)?;
        let target = crate::usb::resolve(&args.device_id).await?;
        if target.info.mode != "loader" || self.backend.is_busy() {
            return Err("仅可在设备空闲且处于 Loader 模式时切换存储。".into());
        }
        let approval = self
            .approve(
                false,
                format!(
                    "MCP 客户端请求切换存储。\n目标：{}\n新介质：{}\n切换后需要重新读取分区。",
                    target.description(),
                    args.storage
                ),
            )
            .await?;
        let backend = self.backend.clone();
        let host = self.host.clone();
        // Keep the switch and its UI invalidation alive after admission, even if
        // the HTTP client disconnects after the USB command has been sent.
        let task = self.scope.commit(approval, || {
            let gate = advanced::reserve_storage_change(&backend)?;
            Ok(tokio::spawn(async move {
                host.emit("mcp-storage-state", json!({ "running": true }));
                let result = advanced::switch_medium(&args.device_id, requested).await;
                drop(gate);
                host.emit("mcp-storage-state", json!({
                    "running": false,
                    "currentStorage": result.as_ref().ok().and_then(|report| report.current_storage.as_deref()),
                    "error": result.as_ref().err(),
                }));
                result
            }))
        })?;
        task.await.map_err(|e| format!("存储切换任务异常：{e}"))?
    }

    async fn unpack(&self, args: UnpackArgs) -> Result<String, String> {
        input_file(&args.path)?;
        let directory = Path::new(&args.output_dir);
        if !directory.is_absolute() || !directory.is_dir() {
            return Err("请选择已存在的绝对输出目录。".into());
        }
        if self.backend.is_busy() {
            return Err("已有任务正在运行，请勿重复提交。".into());
        }
        let approval = self
            .approve(
                false,
                format!(
                    "MCP 客户端请求解包固件：\n{:?}\n在此目录下新建子目录：\n{:?}",
                    args.path, args.output_dir
                ),
            )
            .await?;
        self.scope.commit(approval, || {
            local_jobs::launch(
                local_jobs::UnpackRequest {
                    path: args.path,
                    output_dir: args.output_dir,
                },
                self.host.clone(),
                self.backend.clone(),
                JobOrigin::Mcp,
            )
        })
    }

    async fn start(&self, args: StartArgs) -> Result<String, String> {
        let mut request = args.request()?;
        let flash = request.kind == OperationKind::Flash;
        if self.host.is_headless() && !self.scope.allows_without_prompt(flash)? {
            return Err(if flash {
                "CLI 的 MCP 默认只读。要允许自动烧录，请用 --allow-flash 启动服务。"
            } else {
                "CLI 的 MCP 不允许这项操作；请在桌面版批准，或使用对应的 CLI 命令。"
            }
            .into());
        }
        if self.backend.is_busy() {
            return Err("已有任务正在运行，请勿重复提交。".into());
        }
        let target = crate::usb::resolve(&request.device_id).await?;
        let kind = match request.kind {
            OperationKind::Flash => "烧录固件（覆盖设备数据）",
            OperationKind::Backup => "备份设备数据",
            OperationKind::Erase => "擦除设备数据（无法撤销）",
            OperationKind::Loader => "下载 Loader 到 RAM",
            OperationKind::Reboot => "重启设备",
            OperationKind::Maskrom => "重启并进入 Maskrom",
            OperationKind::Unpack => return Err("请使用 start_unpack。".into()),
        };
        let mut message = format!(
            "MCP 客户端请求：{kind}\n目标：{} · {}",
            target.description(),
            target.info.mode
        );
        if let Some(path) = request.firmware_path.as_ref() {
            message.push_str(&format!("\n固件：{path:?}"));
        }
        if let Some(path) = request.output_path.as_ref() {
            message.push_str(&format!("\n保存到：{path:?}"));
        }
        if let Some(name) = &request.partition_name {
            message.push_str(&format!("\n分区：{name:?}"));
        }
        if let Some(member) = &request.archive_member {
            message.push_str(&format!("\n压缩包成员：{member:?}"));
        }
        if let Some(writes) = &request.writes {
            let mut first = u64::MAX;
            let mut end = 0;
            for entry in writes {
                let count = input_file(&entry.path)?.len().div_ceil(512);
                if count == 0 {
                    return Err("批量镜像不能为空。".into());
                }
                first = first.min(entry.start_sector);
                end = end.max(
                    entry
                        .start_sector
                        .checked_add(count)
                        .ok_or("批量写入范围溢出")?,
                );
            }
            message.push_str(&format!("\n{} 个 RAW 镜像，最小 LBA {}，最大 LBA {}。各条目不一定连续，请在客户端核对文件和地址。",
                writes.len(), first, end - 1));
        } else if let Some(sectors) = request.sectors {
            message.push_str(&format!(
                "\n起始 LBA：{}，长度：{sectors} 个 512 字节扇区",
                request.start_sector
            ));
        } else if request.kind == OperationKind::Flash {
            message.push_str(&format!(
                "\nRAW 从 LBA {} 写入；固件包按自身分区布局写入。",
                request.start_sector
            ));
        }
        if request.kind == OperationKind::Flash {
            message.push_str(&format!(
                "\n完成后重启：{}",
                if request.reboot { "是" } else { "否" }
            ));
        }
        let approval = self.approve(flash, message).await?;
        request.confirmed = true;
        let (prepared, gate) = crate::prepare_operation(&self.backend, request).await?;
        self.scope.commit(approval, || {
            crate::jobs::launch(
                self.host.clone(),
                self.backend.clone(),
                prepared,
                gate,
                JobOrigin::Mcp,
            )
        })
    }

    fn active(&self, id: &str) -> Result<bool, String> {
        let active = self.backend.active.lock().map_err(|_| "无法读取活动任务")?;
        Ok(active.as_ref().is_some_and(|job| job.id == id))
    }

    fn operation(&self, id: &str) -> Result<serde_json::Value, String> {
        // An existing history entry is terminal, even if the final event has
        // not been published or the event buffer has since rolled over.
        let active = self.active(id)?;
        if let Some(entry) = self
            .backend
            .history()?
            .into_iter()
            .find(|entry| entry.id == id)
        {
            return Ok(json!({ "jobId": id, "status": entry.status, "history": entry }));
        }
        if let Some(event) = self.backend.operation_events(Some(id), 1)?.pop() {
            return Ok(json!(event));
        }
        if active {
            return Ok(json!({ "jobId": id, "status": "running" }));
        }
        Err("找不到此任务。".into())
    }

    async fn approve(&self, flash: bool, message: String) -> Result<Approval, String> {
        self.scope.check()?;
        #[cfg(feature = "desktop")]
        if let Some(app) = self.host.app() {
            let permit = self.dialogs.prioritize_mcp(app).await?;
            self.scope.check()?;
            if self.scope.allows_without_prompt(flash)? {
                return Ok(Approval::Unattended {
                    flash,
                    _desktop: Some(permit),
                });
            }
            self.approve_desktop(app, message, permit).await?;
            return Ok(Approval::Interactive);
        }
        let _ = message;
        if self.scope.allows_without_prompt(flash)? {
            Ok(Approval::Unattended {
                flash,
                #[cfg(feature = "desktop")]
                _desktop: None,
            })
        } else {
            Err("此操作需要在桌面版批准，CLI 的 MCP 不支持。".into())
        }
    }

    #[cfg(feature = "desktop")]
    async fn approve_desktop(
        &self,
        app: &tauri::AppHandle,
        message: String,
        permit: OwnedSemaphorePermit,
    ) -> Result<(), String> {
        let window = app
            .get_webview_window("main")
            .ok_or("请先打开 RKDevTool Next 主窗口。")?;
        let (tx, rx) = oneshot::channel();
        app.dialog()
            .message(message)
            .parent(&window)
            .title("批准 RKDevTool Next MCP 请求？")
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom(
                "批准".into(),
                "取消".into(),
            ))
            .show(move |allowed| {
                drop(permit);
                let _ = tx.send(allowed);
            });
        let approved = tokio::select! {
            _ = self.scope.stopped.cancelled() => return Err("MCP 服务已关闭。".into()),
            decision = tokio::time::timeout(Duration::from_secs(90), rx) => {
                decision.map_err(|_| "等待桌面确认超时，请关闭对话框后重试。")?
                    .map_err(|_| "桌面确认窗口已关闭。")?
            }
        };
        if !approved {
            return Err("用户取消了 MCP 请求。".into());
        }
        self.scope.check()
    }
}

fn absolute_path(path: &str) -> Result<std::path::PathBuf, String> {
    let path = Path::new(path);
    if !path.is_absolute() || path.file_name().is_none() {
        return Err("请提供运行 RKDevTool Next 的计算机上的绝对文件路径。".into());
    }
    Ok(path.to_path_buf())
}

fn input_file(path: &str) -> Result<fs::Metadata, String> {
    let file = absolute_path(path)?;
    let metadata = fs::metadata(&file).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            format!("服务端找不到固件文件 {file:?}；请检查该路径及文件是否已移动。")
        }
        _ => format!("服务端无法读取固件文件 {file:?}：{e}"),
    })?;
    if !metadata.is_file() {
        return Err(format!("服务端路径 {file:?} 不是普通固件文件。"));
    }
    Ok(metadata)
}

fn failure(error: String) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(error)])
}

fn result(value: Result<impl Serialize, String>) -> CallToolResult {
    match value.and_then(|data| serde_json::to_string(&data).map_err(|e| e.to_string())) {
        Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
        Err(error) => failure(error),
    }
}
