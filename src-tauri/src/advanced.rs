//! Read-only diagnostics and explicit storage selection using rockusb's public
//! protocol operations. No CLI process, guessed command, or synthetic device data.
use crate::{state::Backend, usb};
use rockusb::{operation, protocol::StorageIndex};
use serde::Serialize;
#[cfg(feature = "desktop")]
use std::sync::Arc;
use std::time::Instant;
#[cfg(feature = "desktop")]
use tauri::State;

#[derive(Debug, Serialize)]
pub struct QueryReport {
    pub title: String,
    pub values: Vec<(String, String)>,
    #[serde(rename = "currentStorage", skip_serializing_if = "Option::is_none")]
    pub current_storage: Option<String>,
}

fn report(title: &str, values: Vec<(&str, String)>) -> QueryReport {
    QueryReport {
        title: title.into(),
        current_storage: None,
        values: values.into_iter().map(|(k, v)| (k.into(), v)).collect(),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn format_capacity(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{bytes} B | {value:.2} {}", UNITS[unit])
    }
}

fn supported(value: bool) -> String {
    if value { "支持" } else { "未报告支持" }.into()
}

fn validate_query(query: &str) -> Result<(), String> {
    match query {
        "flashId" | "flashInfo" | "chipInfo" | "capability" | "testDevice" | "currentStorage" => {
            Ok(())
        }
        _ => Err("不支持的设备查询".into()),
    }
}

pub(crate) fn storage_index(storage: &str) -> Result<StorageIndex, String> {
    // Only named, documented BOOT_TYPE indices. Never pass through arbitrary
    // numeric values or Unknown; capability does not enumerate installed media.
    match storage {
        "nand" => Ok(StorageIndex::Nand),
        "emmc" => Ok(StorageIndex::Emmc),
        "sd0" => Ok(StorageIndex::Sd0),
        "sd1" => Ok(StorageIndex::Sd1),
        "spi-nor" => Ok(StorageIndex::SpiNor),
        "spi-nand" => Ok(StorageIndex::SpiNand),
        "ram" => Ok(StorageIndex::Ram),
        "mtd-blk-nand" => Ok(StorageIndex::MtdBlkNand),
        "mtd-blk-spi-nand" => Ok(StorageIndex::MtdBlkSpiNand),
        "mtd-blk-spi-nor" => Ok(StorageIndex::MtdBlkSpiNor),
        "sata" => Ok(StorageIndex::Sata),
        "pcie" => Ok(StorageIndex::Pcie),
        "ufs" => Ok(StorageIndex::Ufs),
        _ => Err("存储类型无效：只能选择协议定义的命名存储介质".into()),
    }
}

async fn loader_session(device_id: &str) -> Result<usb::Session, String> {
    let target = usb::resolve(device_id).await?;
    if target.info.mode != "loader" {
        return Err("此操作需要 Loader 模式；请先下载适配设备的 Loader".into());
    }
    usb::Session::open(&target).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn query_device(
    device_id: String,
    query: String,
    state: State<'_, Arc<Backend>>,
) -> Result<QueryReport, String> {
    read_query(&state, &device_id, &query).await
}

pub async fn read_query(
    backend: &Backend,
    device_id: &str,
    query: &str,
) -> Result<QueryReport, String> {
    validate_query(query)?;
    let _gate = backend
        .usb_gate
        .clone()
        .try_lock_owned()
        .map_err(|_| "USB 设备正忙；请等待当前任务或扫描完成")?;
    if backend.is_busy() {
        return Err("设备任务运行中，暂不能查询设备".into());
    }
    let mut session = loader_session(device_id).await?;
    match query {
        "flashId" => {
            let id = session.execute(operation::flash_id()).await?;
            // FlashId exposes a lossy string and Debug, but no raw-byte accessor
            // in rockusb 0.4. Debug preserves all five returned numeric bytes.
            Ok(report("Flash ID", vec![("设备响应", format!("{id:?}"))]))
        }
        "flashInfo" => {
            let info = session.flash_info().await?;
            Ok(report(
                "Flash 信息",
                vec![
                    ("容量", format_capacity(info.size())),
                    ("扇区数（512 字节）", info.sectors().to_string()),
                    ("块大小（扇区）", info.block_size_sectors().to_string()),
                    ("原始响应", hex(info.inner())),
                ],
            ))
        }
        "chipInfo" => {
            let chip = session.chip_info().await?;
            let data = chip.inner();
            let raw = hex(data);
            let value = match usb::chip_model(data) {
                Some(model) => format!("{raw} | {model}"),
                None => raw,
            };
            Ok(report("芯片信息", vec![("Chip ID", value)]))
        }
        "capability" => {
            let cap = session.execute(operation::capability()).await?;
            Ok(report(
                "Loader 能力",
                vec![
                    ("直接 LBA", supported(cap.direct_lba())),
                    ("Vendor Storage", supported(cap.vendor_storage())),
                    ("前 4 MiB 访问", supported(cap.first_4m_access())),
                    ("读取 LBA", supported(cap.read_lba())),
                    ("读取串口日志", supported(cap.read_com_log())),
                    ("读取 IDB 配置", supported(cap.read_idb_config())),
                    ("读取安全模式", supported(cap.read_secure_mode())),
                    ("新版 IDB", supported(cap.new_idb())),
                    ("切换存储", supported(cap.switch_storage())),
                    ("原始响应", hex(cap.inner())),
                ],
            ))
        }
        "testDevice" => {
            let started = Instant::now();
            let chip = session.chip_info().await?;
            Ok(report(
                "设备连接测试",
                vec![
                    ("结果", "连接正常".into()),
                    ("耗时（毫秒）", started.elapsed().as_millis().to_string()),
                    ("Chip ID", hex(chip.inner())),
                ],
            ))
        }
        "currentStorage" => {
            let storage = session.execute(operation::storage()).await?;
            let mut result = report("当前存储", vec![("存储介质", storage.to_string())]);
            result.current_storage = Some(storage.to_string());
            Ok(result)
        }
        _ => unreachable!("query validated before opening USB"),
    }
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn switch_storage(
    device_id: String,
    storage: String,
    state: State<'_, Arc<Backend>>,
) -> Result<QueryReport, String> {
    select_storage(&state, &device_id, &storage).await
}

#[cfg(feature = "desktop")]
pub async fn select_storage(
    backend: &Backend,
    device_id: &str,
    storage: &str,
) -> Result<QueryReport, String> {
    let requested = storage_index(storage)?;
    let _gate = reserve_storage_change(backend)?;
    switch_medium(device_id, requested).await
}

pub fn reserve_storage_change(
    backend: &Backend,
) -> Result<tokio::sync::OwnedMutexGuard<()>, String> {
    let gate = backend
        .usb_gate
        .clone()
        .try_lock_owned()
        .map_err(|_| "USB 设备正忙；请等待当前任务或扫描完成")?;
    if backend.is_busy() {
        return Err("设备任务运行中，禁止切换存储".into());
    }
    Ok(gate)
}

pub async fn switch_medium(
    device_id: &str,
    requested: StorageIndex,
) -> Result<QueryReport, String> {
    let mut session = loader_session(device_id).await?;
    let capability = session.execute(operation::capability()).await?;
    if !capability.switch_storage() {
        return Err("当前 Loader 未报告切换存储能力；未发送切换命令".into());
    }
    session
        .execute(operation::switch_storage(requested))
        .await
        .map_err(|e| format!("切换存储未确认成功，当前介质状态未知；请刷新设备后再操作：{e}"))?;
    let actual = session.execute(operation::storage()).await.map_err(|e| {
        format!("已发送切换命令，但无法确认当前存储；请刷新设备，勿使用旧分区信息：{e}")
    })?;
    if actual != requested {
        return Err(format!(
            "存储切换校验失败：请求 {requested}，设备返回 {actual}；请刷新设备，勿使用旧分区信息"
        ));
    }
    let sectors = session.capacity().await.map_err(|e| {
        format!("已切换到 {actual}，但读取容量失败；请刷新设备，勿使用旧分区信息：{e}")
    })?;
    let mut result = report(
        "存储切换完成",
        vec![
            ("当前存储", actual.to_string()),
            ("容量（字节）", (sectors * 512).to_string()),
            ("扇区数（512 字节）", sectors.to_string()),
            (
                "提示",
                "请重新读取分区；原存储的分区与容量不可继续使用".into(),
            ),
        ],
    );
    result.current_storage = Some(actual.to_string());
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_queries_before_usb() {
        assert!(validate_query("erase").is_err());
        assert!(validate_query("testDevice").is_ok());
    }

    #[test]
    fn only_documented_named_storage_indices() {
        for name in [
            "nand",
            "emmc",
            "sd0",
            "sd1",
            "spi-nor",
            "spi-nand",
            "ram",
            "mtd-blk-nand",
            "mtd-blk-spi-nand",
            "mtd-blk-spi-nor",
            "sata",
            "pcie",
            "ufs",
        ] {
            assert_eq!(storage_index(name).unwrap().to_string(), name);
        }
        for name in ["", "0", "255", "unknown", "nvme", "EMMC"] {
            assert!(storage_index(name).is_err());
        }
    }

    #[test]
    fn byte_report_is_lossless() {
        assert_eq!(hex(&[0, 0x8f, 0xff]), "00 8F FF");
    }
}
