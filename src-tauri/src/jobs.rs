use crate::host::Host;
use crate::{
    archive,
    firmware::{self, Image},
    gpt, loader,
    model::*,
    state::{Backend, JobOrigin},
    usb::{self, CHUNK_BYTES, Session, Target},
};
use chrono::Utc;
use futures_util::FutureExt;
use rockusb::{operation, protocol::StorageIndex};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::OwnedMutexGuard;

const CANCELLED: &str = "__RK_STUDIO_CANCELLED_AT_SAFE_BOUNDARY__";
type SourceStamp = (u64, Option<std::time::SystemTime>);

struct PreparedWrite {
    name: String,
    start_sector: u64,
    file: File,
    stamp: SourceStamp,
}

fn source_stamp(file: &File) -> Result<SourceStamp, String> {
    let meta = file
        .metadata()
        .map_err(|e| format!("无法检查源文件：{e}"))?;
    Ok((meta.len(), meta.modified().ok()))
}

fn check_source(file: &File, stamp: SourceStamp) -> Result<(), String> {
    if source_stamp(file)? != stamp {
        return Err("源固件文件发生变化；已停止，请重新选择文件".into());
    }
    Ok(())
}

fn validate_batch_ranges(ranges: &mut [(u64, u64)], capacity: u64) -> Result<(), String> {
    for &(start, sectors) in ranges.iter() {
        checked_range(start, sectors, capacity)?;
    }
    ranges.sort_unstable_by_key(|range| range.0);
    if ranges
        .windows(2)
        .any(|pair| pair[0].0 + pair[0].1 > pair[1].0)
    {
        return Err("批量写入条目的目标扇区重叠；未进行写入".into());
    }
    Ok(())
}

fn validate_partition_target(
    name: &str,
    start: u64,
    sectors: Option<u64>,
    partitions: &[PartitionInfo],
) -> Result<(), String> {
    let partition = partitions
        .iter()
        .find(|p| p.name == name)
        .ok_or("所选分区已不存在，请重新读取分区表")?;
    if partition.start_sector != start || Some(partition.sectors) != sectors {
        return Err("分区布局已改变；未执行操作，请重新读取并确认分区".into());
    }
    Ok(())
}

pub struct Prepared {
    request: OperationRequest,
    target: Target,
    session: Option<Session>,
    image: Option<Image>,
    source: Option<File>,
    source_stamp: Option<(u64, Option<std::time::SystemTime>)>,
    capacity: Option<u64>,
    writes: Vec<PreparedWrite>,
    extracted: Option<archive::ExtractedImage>,
}

fn checked_range(start: u64, sectors: u64, capacity: u64) -> Result<(), String> {
    if sectors == 0
        || start
            .checked_add(sectors)
            .is_none_or(|end| end > capacity || end > u32::MAX as u64 + 1)
    {
        return Err("目标扇区范围为空、溢出或超出设备实际容量".into());
    }
    Ok(())
}

fn format_bytes(bytes: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < units.len() {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.2} {}", units[unit])
    }
}

fn required_path(path: &Option<String>, label: &str) -> Result<String, String> {
    path.as_ref()
        .filter(|p| !p.trim().is_empty())
        .cloned()
        .ok_or_else(|| format!("请选择{label}"))
}

pub async fn prepare(request: OperationRequest) -> Result<Prepared, String> {
    if request.kind == OperationKind::Unpack {
        return Err("本地解包请使用 start_unpack 接口".into());
    }
    if !request.confirmed && request.kind != OperationKind::Backup {
        return Err("操作尚未确认；请重新核对设备与目标范围".into());
    }
    if request.device_id.is_empty() {
        return Err("请先选择目标设备".into());
    }
    let is_batch = request.writes.is_some();
    if let Some(writes) = &request.writes {
        if request.kind != OperationKind::Flash
            || writes.is_empty()
            || writes.len() > 128
            || request.firmware_path.is_some()
            || request.sectors.is_some()
            || request.start_sector != 0
            || request.partition_name.is_some()
            || request.archive_member.is_some()
        {
            return Err(
                "批量写入须包含 1–128 个 RAW 条目，不能混用单文件、分区或压缩包参数".into(),
            );
        }
    }
    if request.partition_name.is_some()
        && !matches!(
            request.kind,
            OperationKind::Flash | OperationKind::Backup | OperationKind::Erase
        )
    {
        return Err("此操作不支持分区目标".into());
    }
    if let Some(member) = &request.archive_member {
        if request.kind != OperationKind::Flash {
            return Err("仅固件写入支持压缩包成员".into());
        }
        let path = required_path(&request.firmware_path, "压缩包")?;
        let archive = archive::inspect_archive(&path)?;
        if !archive.entries.iter().any(|entry| &entry.name == member) {
            return Err("压缩包中不存在所选镜像，请重新选择".into());
        }
    }
    let target = usb::resolve(&request.device_id).await?;
    let image = if !is_batch
        && request.archive_member.is_none()
        && matches!(request.kind, OperationKind::Flash | OperationKind::Loader)
    {
        Some(firmware::inspect(&required_path(
            &request.firmware_path,
            "固件文件",
        )?)?)
    } else {
        None
    };
    if request.kind == OperationKind::Loader {
        if target.info.mode != "maskrom" {
            return Err(
                "RAM Loader 下载仅适用于 Maskrom 模式；当前 Loader 不会被替换或持久化".into(),
            );
        }
        if image.as_ref().is_none_or(|i| i.info.kind != "loader") {
            return Err("请选择 BOOT/LDR 格式的 Rockchip MiniLoader 文件".into());
        }
        if request.reboot {
            return Err("RAM Loader 下载不支持自动重启".into());
        }
    }
    if request.kind == OperationKind::Flash && !is_batch && request.archive_member.is_none() {
        let img = image.as_ref().ok_or("缺少固件")?;
        if img.info.kind == "loader" {
            return Err("MiniLoader 不能作为磁盘镜像写入；请使用下载 Loader 功能".into());
        }
        if img.package.is_some()
            && (request.start_sector != 0
                || request.sectors.is_some()
                || request.partition_name.is_some())
        {
            return Err("RKFW/RKAF 升级包必须按完整分区布局烧录，不能指定原始扇区范围".into());
        }
    }
    if target.info.mode == "maskrom"
        && !matches!(request.kind, OperationKind::Loader | OperationKind::Flash)
    {
        return Err("当前为 Maskrom 模式；请先下载适配此板卡的 Loader".into());
    }
    if target.info.mode == "maskrom"
        && request.kind == OperationKind::Flash
        && request.archive_member.is_none()
        && image
            .as_ref()
            .is_none_or(|i| i.package.as_ref().is_none_or(|p| p.loader_offset.is_none()))
    {
        return Err(
            "Maskrom 烧录需要 RKFW 内置 Loader；原始镜像或独立 RKAF 请先下载 Loader".into(),
        );
    }
    if request.kind == OperationKind::Backup {
        let path = PathBuf::from(required_path(&request.output_path, "备份保存位置")?);
        if !path.is_absolute() || path.file_name().is_none() {
            return Err("备份路径必须为完整文件路径".into());
        }
        if path.exists() {
            return Err("备份目标已存在；请选择新文件名，原文件不会被覆盖".into());
        }
        if !path.parent().is_some_and(Path::is_dir) {
            return Err("备份目标目录不存在".into());
        }
    }
    let source = if let Some(image) = image.as_ref() {
        Some(File::open(&image.info.path).map_err(|e| format!("无法打开固件：{e}"))?)
    } else if request.archive_member.is_some() {
        Some(
            File::open(required_path(&request.firmware_path, "压缩包")?)
                .map_err(|e| format!("无法打开压缩包：{e}"))?,
        )
    } else {
        None
    };
    let source_metadata_stamp = source
        .as_ref()
        .map(|f| f.metadata().map(|m| (m.len(), m.modified().ok())))
        .transpose()
        .map_err(|e| e.to_string())?;
    if let (Some(image), Some((size, _))) = (&image, source_metadata_stamp) {
        if image.info.size != size {
            return Err("固件在检查后发生变化，请重新选择".into());
        }
    }
    let mut session = Session::open(&target).await?;
    let capacity = if target.info.mode == "loader"
        && matches!(
            request.kind,
            OperationKind::Flash | OperationKind::Backup | OperationKind::Erase
        ) {
        Some(session.capacity().await?)
    } else {
        None
    };
    if let Some(capacity) = capacity {
        match request.kind {
            OperationKind::Flash if image.as_ref().is_some_and(|i| i.package.is_none()) => {
                let sectors = image.as_ref().unwrap().info.size.div_ceil(512);
                if request.sectors.is_some_and(|limit| sectors > limit) {
                    return Err("镜像大于所选目标分区；未进行写入".into());
                }
                checked_range(
                    request.start_sector,
                    request.sectors.unwrap_or(sectors),
                    capacity,
                )?;
            }
            OperationKind::Backup | OperationKind::Erase => checked_range(
                request.start_sector,
                request.sectors.ok_or("请指定正数扇区长度")?,
                capacity,
            )?,
            _ => {}
        }
    }
    if let Some(name) = &request.partition_name {
        let capacity = capacity.ok_or("分区操作需要 Loader 模式和有效容量")?;
        let partitions = usb::partitions(&mut session, capacity).await?;
        validate_partition_target(name, request.start_sector, request.sectors, &partitions)?;
    }
    let mut prepared_writes = Vec::new();
    if let Some(writes) = &request.writes {
        let capacity = capacity.ok_or("批量写入需要 Loader 模式和有效容量")?;
        let mut ranges = Vec::new();
        for entry in writes {
            let file =
                File::open(&entry.path).map_err(|e| format!("无法打开 {}：{e}", entry.name))?;
            let stamp = source_stamp(&file)?;
            let image = firmware::inspect(&entry.path)?;
            check_source(&file, stamp)?;
            if image.info.kind != "raw" || image.info.size != stamp.0 {
                return Err(format!("批量条目 {} 必须为未改变的 RAW 镜像", entry.name));
            }
            ranges.push((entry.start_sector, stamp.0.div_ceil(512)));
            prepared_writes.push(PreparedWrite {
                name: entry.name.clone(),
                start_sector: entry.start_sector,
                file,
                stamp,
            });
        }
        validate_batch_ranges(&mut ranges, capacity)?;
    }
    Ok(Prepared {
        request,
        target,
        session: Some(session),
        image,
        source,
        source_stamp: source_metadata_stamp,
        capacity,
        writes: prepared_writes,
        extracted: None,
    })
}

pub fn launch(
    app: Host,
    backend: Arc<Backend>,
    prepared: Prepared,
    gate: OwnedMutexGuard<()>,
    origin: JobOrigin,
) -> Result<String, String> {
    launch_task(app, backend, prepared, gate, origin).map(|(id, _)| id)
}

pub(crate) fn launch_task(
    app: Host,
    backend: Arc<Backend>,
    mut prepared: Prepared,
    gate: OwnedMutexGuard<()>,
    origin: JobOrigin,
) -> Result<(String, tokio::task::JoinHandle<Result<String, String>>), String> {
    let id = uuid::Uuid::new_v4().to_string();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut entry = HistoryEntry {
        id: id.clone(),
        device: format!("{} · {}", prepared.target.info.name, prepared.target.port),
        operation: prepared.request.kind,
        file: if prepared.request.kind == OperationKind::Backup {
            prepared.request.output_path.clone()
        } else if let Some(writes) = &prepared.request.writes {
            Some(
                writes
                    .iter()
                    .map(|entry| entry.path.as_str())
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        } else {
            prepared.request.firmware_path.clone()
        },
        status: "failed".into(),
        started_at: Utc::now().to_rfc3339(),
        duration_ms: 0,
        message: "任务在完成前中断，请检查设备状态".into(),
    };
    backend.begin(&entry, cancel.clone(), origin)?;
    let job_id = id.clone();
    let task = tokio::spawn(async move {
        let start = Instant::now();
        let mut report = Reporter::new(app, backend.clone(), origin, job_id, cancel);
        report.phase("preflight", "正在检查目标设备与固件", 0, 0.0, 0.0);
        let result = std::panic::AssertUnwindSafe(prepared.run(&mut report))
            .catch_unwind()
            .await;
        let device_may_be_partial = matches!(
            prepared.request.kind,
            OperationKind::Flash | OperationKind::Erase
        );
        if matches!(&result, Ok(Ok(())))
            && (matches!(
                prepared.request.kind,
                OperationKind::Reboot | OperationKind::Maskrom
            ) || (prepared.request.kind == OperationKind::Flash && prepared.request.reboot))
        {
            // A package flash may have reconnected to a different Loader ID.
            // Record the reset session before releasing USB or emitting success.
            backend
                .expect_reboot_disconnect(prepared.target.info.id.clone())
                .await;
        }
        let (status, message, level) = match result {
            Ok(Ok(())) => ("completed", match prepared.request.kind {
                OperationKind::Loader => "Loader 已载入 RAM，设备已进入 Loader 模式".to_string(),
                OperationKind::Backup => "备份完成，文件已同步保存".to_string(),
                OperationKind::Erase => format!("擦除完成：{}", prepared.erase_target()),
                _ => "操作完成".to_string(),
            }, "success"),
            Ok(Err(e)) if e == CANCELLED => (
                "cancelled",
                report.cancel_notice.clone().unwrap_or_else(|| if device_may_be_partial {
                    "已在完整 USB 块传输后取消；已写入的数据不会回滚，请勿直接启动部分烧录的设备".into()
                } else { "任务已在安全边界取消".into() }),
                "warning",
            ),
            Ok(Err(e)) => (
                "failed",
                if device_may_be_partial { format!("操作失败：{e}。设备可能仅部分写入；请保持连接并按需要重新烧录") }
                else { format!("操作失败：{e}") },
                "error",
            ),
            Err(_) => (
                "failed",
                "后台任务异常终止；请重新连接设备并恢复烧录".into(),
                "error",
            ),
        };
        entry.status = status.into();
        entry.duration_ms = start.elapsed().as_millis().min(u64::MAX as u128) as u64;
        entry.message = message.clone();
        // Release the claimed interface while the USB gate is still held.
        // A terminal event allows the UI to scan or start another operation.
        drop(prepared.session.take());
        drop(prepared.source.take());
        prepared.writes.clear();
        drop(prepared.extracted.take());
        let history_result = backend.finish(entry);
        let message = match history_result {
            Ok(()) => message,
            Err(e) => format!("{message}；历史保存失败：{e}"),
        };
        drop(gate);
        report.terminal(status, &message, level);
        if status == "completed" {
            Ok(message)
        } else {
            Err(message)
        }
    });
    Ok((id, task))
}

struct Reporter {
    app: Host,
    backend: Arc<Backend>,
    origin: JobOrigin,
    id: String,
    cancel: Arc<AtomicBool>,
    stage: String,
    message: String,
    done: u64,
    total: u64,
    base: f64,
    span: f64,
    last: Instant,
    cancel_notice: Option<String>,
}

impl Reporter {
    fn new(
        app: Host,
        backend: Arc<Backend>,
        origin: JobOrigin,
        id: String,
        cancel: Arc<AtomicBool>,
    ) -> Self {
        Self {
            app,
            backend,
            origin,
            id,
            cancel,
            stage: String::new(),
            message: String::new(),
            done: 0,
            total: 0,
            base: 0.0,
            span: 100.0,
            last: Instant::now(),
            cancel_notice: None,
        }
    }
    fn check(&self) -> Result<(), String> {
        if self.cancel.load(Ordering::Acquire) {
            Err(CANCELLED.into())
        } else {
            Ok(())
        }
    }
    fn phase(&mut self, stage: &str, message: &str, total: u64, base: f64, span: f64) {
        self.stage = stage.into();
        self.message = message.into();
        self.done = 0;
        self.total = total;
        self.base = base;
        self.span = span;
        self.emit("running", "info", false);
    }
    fn label(&mut self, stage: &str, message: &str) {
        self.stage = stage.into();
        self.message = message.into();
        self.emit("running", "info", false);
    }
    fn advance(&mut self, delta: u64) {
        self.done = self.done.saturating_add(delta);
        if self.last.elapsed() >= Duration::from_millis(80) || self.done >= self.total {
            self.emit("running", "info", false);
        }
    }
    fn emit(&mut self, status: &str, level: &str, complete: bool) {
        let progress = if complete {
            Some(100.0)
        } else if self.total > 0 {
            Some(
                (self.base + self.span * self.done.min(self.total) as f64 / self.total as f64)
                    .min(99.9),
            )
        } else {
            None
        };
        self.backend.publish_event(
            &self.app,
            self.origin,
            OperationEvent {
                job_id: self.id.clone(),
                status: status.into(),
                stage: self.stage.clone(),
                progress,
                message: self.message.clone(),
                level: level.into(),
                timestamp: Utc::now().to_rfc3339(),
                bytes_done: (self.total > 0).then_some(self.done),
                bytes_total: (self.total > 0).then_some(self.total),
            },
        );
        self.last = Instant::now();
    }
    fn terminal(&mut self, status: &str, message: &str, level: &str) {
        self.stage = status.into();
        self.message = message.into();
        self.emit(status, level, status == "completed");
    }
}

impl Prepared {
    #[cfg(feature = "headless-cli")]
    pub(crate) fn is_firmware_package(&self) -> bool {
        self.image
            .as_ref()
            .is_some_and(|image| image.package.is_some())
    }

    async fn run(&mut self, report: &mut Reporter) -> Result<(), String> {
        report.check()?;
        self.source_unchanged()?;
        if let Some(member) = self.request.archive_member.clone() {
            let path = required_path(&self.request.firmware_path, "压缩包")?;
            let max_bytes = self
                .capacity
                .map(|capacity| capacity * 512)
                .unwrap_or(128 * 1024 * 1024 * 1024)
                .min(128 * 1024 * 1024 * 1024);
            report.phase("extracting", "正在解压所选镜像", 0, 0.0, 0.0);
            let extracted = archive::extract_selected(&path, &member, max_bytes, |bytes| {
                report.check()?;
                report.advance(bytes);
                Ok(())
            })?;
            self.source_unchanged()?;
            let path = extracted.path().to_str().ok_or("解压路径不是有效 UTF-8")?;
            let image = firmware::inspect(path)?;
            if image.info.kind == "loader" {
                return Err("压缩包中的 MiniLoader 不能作为磁盘镜像写入".into());
            }
            if image.package.is_some()
                && (self.request.start_sector != 0
                    || self.request.sectors.is_some()
                    || self.request.partition_name.is_some())
            {
                return Err("升级包不能按 RAW 地址或分区写入".into());
            }
            if self.target.info.mode == "maskrom"
                && image
                    .package
                    .as_ref()
                    .is_none_or(|p| p.loader_offset.is_none())
            {
                return Err("Maskrom 烧录需要内置 Loader 的 RKFW；请先下载适配的 Loader".into());
            }
            if image.package.is_none() {
                let sectors = image.info.size.div_ceil(512);
                if self.request.sectors.is_some_and(|limit| sectors > limit) {
                    return Err("解压镜像大于所选分区；未进行写入".into());
                }
                checked_range(
                    self.request.start_sector,
                    self.request.sectors.unwrap_or(sectors),
                    self.capacity.ok_or("未读取设备容量")?,
                )?;
            }
            let file = File::open(path).map_err(|e| format!("无法打开解压镜像：{e}"))?;
            self.source_stamp = Some(source_stamp(&file)?);
            self.source = Some(file);
            self.image = Some(image);
            self.extracted = Some(extracted);
        }
        match self.request.kind {
            OperationKind::Unpack => return Err("本地解包不能作为设备任务运行".into()),
            OperationKind::Flash => {
                if !self.writes.is_empty() {
                    let total = self
                        .writes
                        .iter()
                        .try_fold(0u64, |sum, entry| {
                            sum.checked_add(entry.stamp.0.div_ceil(512) * 512)
                        })
                        .ok_or("批量写入总长度溢出")?;
                    report.phase("writing", "正在批量写入 RAW 镜像", total, 0.0, 100.0);
                    // All files were inspected and all ranges checked before
                    // launch. Recheck every source before the first write.
                    for entry in &self.writes {
                        check_source(&entry.file, entry.stamp)?;
                    }
                    for entry in &mut self.writes {
                        report.check()?;
                        check_source(&entry.file, entry.stamp)?;
                        report.label(
                            "writing",
                            &format!("正在写入 {} · LBA {}", entry.name, entry.start_sector),
                        );
                        write_file(
                            self.session.as_mut().ok_or("USB 会话已关闭")?,
                            &mut entry.file,
                            0,
                            entry.stamp.0,
                            entry.start_sector,
                            report,
                        )
                        .await?;
                        check_source(&entry.file, entry.stamp)?;
                    }
                } else if self.image.as_ref().is_some_and(|i| i.package.is_some()) {
                    self.flash_package(report).await?;
                } else {
                    let size = self.image.as_ref().ok_or("缺少固件信息")?.info.size;
                    let padded = size.div_ceil(512) * 512;
                    report.phase("writing", "正在写入原始镜像", padded, 0.0, 100.0);
                    write_file(
                        self.session.as_mut().ok_or("USB 会话已关闭")?,
                        self.source.as_mut().ok_or("缺少固件文件")?,
                        0,
                        size,
                        self.request.start_sector,
                        report,
                    )
                    .await?;
                }
                self.source_unchanged()?;
                if self.request.reboot {
                    report.check()?;
                    report.label("rebooting", "写入完成，正在重启设备");
                    self.session
                        .as_mut()
                        .ok_or("USB 会话已关闭")?
                        .reboot(false)
                        .await?;
                }
            }
            OperationKind::Loader => {
                let size = self.image.as_ref().ok_or("缺少 Loader")?.info.size;
                let blobs =
                    loader::ram_blobs(self.source.as_mut().ok_or("缺少 Loader 文件")?, 0, size)?;
                let total = blobs.iter().map(|b| b.data.len() as u64).sum();
                report.phase("loading-loader", "正在向 RAM 下载 Loader", total, 0.0, 95.0);
                upload_loader(
                    self.session.as_mut().ok_or("USB 会话已关闭")?,
                    blobs,
                    report,
                )
                .await?;
                self.reconnect(report).await?;
            }
            OperationKind::Backup => self.backup(report).await?,
            OperationKind::Erase => {
                let sectors = self.request.sectors.ok_or("缺少擦除范围")?;
                report.phase(
                    "erasing",
                    &format!("开始擦除：{}", self.erase_target()),
                    sectors * 512,
                    0.0,
                    100.0,
                );
                let mut done = 0;
                while done < sectors {
                    report.check()?;
                    let count = (sectors - done).min(256) as u16;
                    self.session
                        .as_mut()
                        .ok_or("USB 会话已关闭")?
                        .erase(self.request.start_sector + done, count)
                        .await?;
                    done += u64::from(count);
                    report.advance(u64::from(count) * 512);
                }
            }
            OperationKind::Reboot | OperationKind::Maskrom => {
                report.label(
                    "rebooting",
                    if self.request.kind == OperationKind::Maskrom {
                        "正在请求进入 Maskrom"
                    } else {
                        "正在重启设备"
                    },
                );
                self.session
                    .as_mut()
                    .ok_or("USB 会话已关闭")?
                    .reboot(self.request.kind == OperationKind::Maskrom)
                    .await?;
            }
        }
        Ok(())
    }

    fn erase_target(&self) -> String {
        if let Some(name) = &self.request.partition_name {
            return format!("分区 {name}");
        }
        // prepare checked this non-empty range against the actual capacity.
        let sectors = self.request.sectors.expect("validated erase range");
        let start = self.request.start_sector;
        if start == 0 && self.capacity == Some(sectors) {
            format!("全部存储（{}）", format_bytes(sectors * 512))
        } else {
            let end = start + sectors - 1;
            format!("扇区 LBA {start}–{end}（含首尾，共 {sectors} 个扇区）")
        }
    }

    fn source_unchanged(&self) -> Result<(), String> {
        for entry in &self.writes {
            check_source(&entry.file, entry.stamp)?;
        }
        if let (Some(file), Some(stamp)) = (&self.source, &self.source_stamp) {
            let meta = file
                .metadata()
                .map_err(|e| format!("无法检查源文件：{e}"))?;
            if (meta.len(), meta.modified().ok()) != *stamp {
                return Err("源固件文件在操作期间发生变化；已停止".into());
            }
        }
        Ok(())
    }

    async fn reconnect(&mut self, report: &mut Reporter) -> Result<(), String> {
        report.label(
            "reconnecting",
            "正在等待同一 USB 端口的 Loader，并读取芯片信息确认",
        );
        // Drop the old claim before opening the re-enumerated interface. The
        // loader may retain its enumeration identity on some boards.
        drop(self.session.take());
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut last_seen = None;
        let mut last_error = format!("USB 端口 {} 尚未出现 Rockchip 设备", self.target.port);
        while Instant::now() < deadline {
            report.check()?;
            tokio::time::sleep(Duration::from_millis(500)).await;
            report.check()?;
            if Instant::now() >= deadline {
                break;
            }
            let matches: Vec<_> = usb::targets()
                .await?
                .into_iter()
                // PID, serial strings and bcdUSB may change (or stay the same)
                // when BootROM hands off to firmware. Keep the physical port
                // constraint and require a valid read-only protocol response.
                .filter(|d| d.port == self.target.port)
                .collect();
            if matches.len() > 1 {
                return Err("重枚举设备身份不唯一；未继续写入".into());
            }
            if let Some(mut target) = matches.into_iter().next() {
                let description = target.description();
                if last_seen.as_ref() != Some(&description) {
                    report.label(
                        "reconnecting",
                        &format!("检测到 {description}；正在读取芯片信息"),
                    );
                    last_seen = Some(description.clone());
                }
                // Bound each attempt, not regular Loader commands. Dropping a
                // timed-out probe also drops its claim and pending transfers.
                let probe = tokio::time::timeout(
                    Duration::from_secs(2).min(deadline.saturating_duration_since(Instant::now())),
                    usb::probe_loader(&mut target),
                )
                .await
                .unwrap_or_else(|_| Err("读取 Loader 芯片信息超时".into()));
                match probe {
                    Ok(session) => {
                        report.check()?;
                        report.label(
                            "reconnecting",
                            &format!("Loader 已响应：{description} · {}", target.info.chip),
                        );
                        self.session = Some(session);
                        self.target = target;
                        return Ok(());
                    }
                    Err(error) => last_error = format!("{description}；{error}"),
                }
            }
        }
        report.check()?;
        Err(format!(
            "等待 Loader 就绪超时（30 秒）；最后检测结果：{last_error}。请检查 Loader 适配、供电与 USB 连接"
        ))
    }

    async fn flash_package(&mut self, report: &mut Reporter) -> Result<(), String> {
        let package = self
            .image
            .as_ref()
            .and_then(|i| i.package.as_ref())
            .ok_or("缺少升级包布局")?;
        report.phase(
            "verifying-package",
            "正在校验升级包 RKCRC-32（写入前必须通过）",
            package.archive_length,
            0.0,
            20.0,
        );
        firmware::verify_archive(
            self.source.as_mut().ok_or("缺少升级包文件")?,
            package,
            |bytes| {
                report.check()?;
                report.advance(bytes);
                Ok(())
            },
        )?;
        report.check()?;
        self.source_unchanged()?;
        if self.target.info.mode == "maskrom" {
            let offset = package.loader_offset.ok_or("升级包没有可用的内置 Loader")?;
            let blobs =
                loader::ram_blobs(self.source.as_mut().unwrap(), offset, package.loader_size)?;
            let total = blobs.iter().map(|b| b.data.len() as u64).sum();
            report.phase(
                "loading-loader",
                "正在向 RAM 下载内置 Loader",
                total,
                20.0,
                5.0,
            );
            upload_loader(
                self.session.as_mut().ok_or("USB 会话已关闭")?,
                blobs,
                report,
            )
            .await?;
            self.reconnect(report).await?;
        }
        // Resolve the full persistent plan before changing any flash sector.
        report.check()?;
        self.session
            .as_mut()
            .ok_or("USB 会话已关闭")?
            .chip_id()
            .await?;
        let capacity = self
            .session
            .as_mut()
            .ok_or("USB 会话已关闭")?
            .capacity()
            .await?;
        self.capacity = Some(capacity);
        let package = self
            .image
            .as_ref()
            .and_then(|i| i.package.as_ref())
            .unwrap();
        let disk = gpt::build(&package.parameter, capacity)?;
        let boot = package.parts.iter().find(|p| p.name == "bootloader");
        let idb_range = boot
            .map(|p| (p.offset, p.size))
            .or_else(|| package.loader_offset.map(|o| (o, package.loader_size)))
            .ok_or("升级包缺少可持久化 MiniLoader；未修改设备")?;
        let storage = self
            .session
            .as_mut()
            .ok_or("USB 会话已关闭")?
            .execute(operation::storage())
            .await?;
        if !matches!(
            storage,
            StorageIndex::Emmc | StorageIndex::Sd0 | StorageIndex::Sd1
        ) {
            return Err(format!(
                "完整升级包的 IDB 持久化目前仅支持 eMMC/SD，当前介质为 {storage}；未修改设备"
            ));
        }
        let new_idb = self
            .session
            .as_mut()
            .ok_or("USB 会话已关闭")?
            .execute(operation::capability())
            .await?
            .new_idb();
        let idb = loader::idb_writes(
            self.source.as_mut().unwrap(),
            idb_range.0,
            idb_range.1,
            new_idb,
        )?;
        if idb.is_empty() {
            return Err("MiniLoader 没有生成可写的 IDB".into());
        }
        for write in &idb {
            checked_range(write.sector, write.data.len() as u64 / 512, capacity)?;
            let end = write.sector + write.data.len() as u64 / 512;
            if disk
                .partitions
                .iter()
                .any(|p| write.sector < p.start_sector + p.sectors && p.start_sector < end)
                || write.sector < 34
                || end > capacity - 33
            {
                return Err("IDB 区域与 GPT/数据分区冲突；未修改设备".into());
            }
        }
        for part in package.parts.iter().filter(|p| p.name != "bootloader") {
            let dest = disk
                .partitions
                .iter()
                .find(|p| p.name == part.name)
                .ok_or_else(|| format!("升级包分区 {} 不在 GPT 中", part.name))?;
            if part.start_sector != Some(dest.start_sector)
                || part.size.div_ceil(512) > dest.sectors
            {
                return Err(format!(
                    "分区 {} 的镜像大小或地址与 GPT 不一致；未修改设备",
                    part.name
                ));
            }
        }
        self.source_unchanged()?;
        let metadata_bytes = (disk.primary.len() + disk.backup.len()) as u64
            + idb.iter().map(|w| w.data.len() as u64).sum::<u64>();
        let payload_bytes = package
            .parts
            .iter()
            .filter(|p| p.name != "bootloader")
            .map(|p| p.size.div_ceil(512) * 512)
            .sum::<u64>();
        let bytes = metadata_bytes + payload_bytes;
        report.phase(
            "writing-gpt",
            "已完成写入前检查，正在写入 GPT",
            bytes,
            25.0,
            75.0,
        );
        // Backup GPT first: interrupted writes leave the old primary readable.
        write_memory(
            self.session.as_mut().ok_or("USB 会话已关闭")?,
            capacity - 33,
            &disk.backup,
            report,
        )
        .await?;
        write_memory(
            self.session.as_mut().ok_or("USB 会话已关闭")?,
            0,
            &disk.primary,
            report,
        )
        .await?;
        report.label("writing-idb", "正在持久化 MiniLoader 到 IDB 引导区");
        for write in idb {
            write_memory(
                self.session.as_mut().ok_or("USB 会话已关闭")?,
                write.sector,
                &write.data,
                report,
            )
            .await?;
        }
        for part in package.parts.iter().filter(|p| p.name != "bootloader") {
            report.label("writing", &format!("正在写入分区 {}", part.name));
            let start = part.start_sector.ok_or("分区缺少写入地址")?;
            write_file(
                self.session.as_mut().ok_or("USB 会话已关闭")?,
                self.source.as_mut().unwrap(),
                part.offset,
                part.size,
                start,
                report,
            )
            .await?;
        }
        Ok(())
    }

    async fn backup(&mut self, report: &mut Reporter) -> Result<(), String> {
        let path = PathBuf::from(required_path(&self.request.output_path, "备份位置")?);
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("无法创建备份（不会覆盖现有文件）：{e}"))?;
        let sectors = self.request.sectors.ok_or("缺少备份范围")?;
        report.phase("reading", "正在备份所选扇区范围", sectors * 512, 0.0, 100.0);
        let result = async {
            let mut done = 0;
            while done < sectors {
                report.check()?;
                let count = (sectors - done).min((CHUNK_BYTES / 512) as u64);
                let mut data = vec![0; count as usize * 512];
                self.session
                    .as_mut()
                    .ok_or("USB 会话已关闭")?
                    .read(self.request.start_sector + done, &mut data)
                    .await?;
                output
                    .write_all(&data)
                    .map_err(|e| format!("备份磁盘写入失败：{e}"))?;
                done += count;
                report.advance(count * 512);
            }
            output
                .sync_all()
                .map_err(|e| format!("备份文件同步失败：{e}"))?;
            Ok::<(), String>(())
        }
        .await;
        drop(output);
        if result.is_err() {
            let partial = path.with_file_name(format!(
                "{}.partial-{}",
                path.file_name().unwrap().to_string_lossy(),
                report.id
            ));
            if let Err(e) = fs::rename(&path, &partial) {
                return Err(format!(
                    "备份未完成，{} 是不完整文件，重命名失败：{e}",
                    path.display()
                ));
            }
            if let Err(reason) = &result {
                if reason != CANCELLED {
                    return Err(format!("{reason}；不完整备份保留为 {}", partial.display()));
                }
                report.cancel_notice = Some(format!(
                    "备份已取消；不完整文件保留为 {}",
                    partial.display()
                ));
            }
        }
        result
    }
}

async fn upload_loader(
    session: &mut Session,
    blobs: Vec<loader::LoaderBlob>,
    report: &mut Reporter,
) -> Result<(), String> {
    // Cancellation happens between complete boot entries, never in the middle
    // of a control-transfer stream (which would leave an invalid RAM loader).
    for blob in blobs {
        report.check()?;
        report.label(
            "loading-loader",
            &format!(
                "正在下载 RAM 条目 0x{:03x}（{} 字节，延迟 {} ms）",
                blob.area,
                blob.data.len(),
                blob.delay_ms
            ),
        );
        session
            .execute(operation::write_area(blob.area, &blob.data))
            .await?;
        report.advance(blob.data.len() as u64);
        if blob.delay_ms > 0 {
            // This complete entry is a safe cancellation boundary, unlike
            // the middle of its USB control-transfer stream.
            let deadline = Instant::now() + Duration::from_millis(blob.delay_ms as u64);
            loop {
                report.check()?;
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                tokio::time::sleep(remaining.min(Duration::from_millis(100))).await;
            }
        }
    }
    Ok(())
}

async fn write_file(
    session: &mut Session,
    file: &mut File,
    offset: u64,
    size: u64,
    start: u64,
    report: &mut Reporter,
) -> Result<(), String> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| format!("无法定位固件数据：{e}"))?;
    let mut done = 0;
    while done < size {
        report.check()?;
        let bytes = (size - done).min(CHUNK_BYTES as u64) as usize;
        let mut data = vec![0; bytes.next_multiple_of(512)];
        file.read_exact(&mut data[..bytes])
            .map_err(|e| format!("固件读取失败：{e}"))?;
        write_memory(session, start + done / 512, &data, report).await?;
        done += bytes as u64;
    }
    Ok(())
}

async fn write_memory(
    session: &mut Session,
    start: u64,
    data: &[u8],
    report: &mut Reporter,
) -> Result<(), String> {
    if data.is_empty() || data.len() % 512 != 0 {
        return Err("写入数据不是完整的 512 字节扇区".into());
    }
    for (index, chunk) in data.chunks(CHUNK_BYTES).enumerate() {
        report.check()?;
        let lba = start + (index * CHUNK_BYTES / 512) as u64;
        session.write(lba, chunk).await?;
        report.advance(chunk.len() as u64);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_ranges_reject_zero_overflow_and_device_end() {
        assert!(checked_range(0, 0, 100).is_err());
        assert!(checked_range(u64::MAX, 1, u64::MAX).is_err());
        assert!(checked_range(99, 2, 100).is_err());
        assert!(checked_range(u32::MAX as u64, 2, u64::MAX).is_err());
        assert!(checked_range(0, 100, 100).is_ok());
    }

    #[test]
    fn batch_allows_adjacency_but_rejects_overlaps() {
        assert!(validate_batch_ranges(&mut [(10, 4), (0, 10)], 100).is_ok());
        assert!(validate_batch_ranges(&mut [(10, 4), (0, 11)], 100).is_err());
        assert!(validate_batch_ranges(&mut [(0, 1), (0, 1)], 100).is_err());
        assert!(validate_batch_ranges(&mut [(99, 2)], 100).is_err());
    }

    #[test]
    fn optional_target_fields_remain_backwards_compatible() {
        let request: OperationRequest = serde_json::from_str(
            r#"{
            "deviceId":"device", "kind":"erase", "startSector":0,
            "sectors":100, "verify":false, "reboot":false, "confirmed":true
        }"#,
        )
        .unwrap();
        assert!(request.writes.is_none());
        assert!(request.partition_name.is_none());
        assert!(request.archive_member.is_none());
    }

    #[test]
    fn partition_target_rejects_stale_layout_and_unknown_name() {
        let partitions = [PartitionInfo {
            name: "boot".into(),
            start_sector: 8192,
            sectors: 1024,
            size: 1024 * 512,
        }];
        assert!(validate_partition_target("boot", 8192, Some(1024), &partitions).is_ok());
        assert!(validate_partition_target("boot", 4096, Some(1024), &partitions).is_err());
        assert!(validate_partition_target("boot", 8192, Some(2048), &partitions).is_err());
        assert!(validate_partition_target("boot", 8192, None, &partitions).is_err());
        assert!(validate_partition_target("rootfs", 8192, Some(1024), &partitions).is_err());
    }
}
