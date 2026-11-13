//! RockUSB's sans-I/O protocol over nusb, with bounded, exact USB transfers.
use crate::model::DeviceInfo;
use nusb::transfer::{Buffer, Bulk, ControlOut, ControlType, In, Out, Recipient};
use nusb::{DeviceInfo as UsbInfo, Endpoint, Interface};
use rockusb::{
    operation::{self, OperationSteps, UsbStep},
    protocol::{
        COMMAND_BLOCK_BYTES, COMMAND_STATUS_BYTES, ChipInfo, CommandBlock, CommandStatus,
        FlashInfo, ResetOpcode, Status,
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    sync::Mutex,
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(15);
const HANDOFF_WINDOW: Duration = Duration::from_secs(30);
pub const CHUNK_BYTES: usize = 128 * 512;

// A Loader can keep the same descriptors as Maskrom. Remember a successful
// handshake only for that USB enumeration, not for the PID or physical port.
static CONFIRMED_LOADERS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
static PENDING_HANDOFFS: Mutex<BTreeMap<String, Instant>> = Mutex::new(BTreeMap::new());

#[derive(Clone)]
pub struct Target {
    pub usb: UsbInfo,
    pub info: DeviceInfo,
    pub port: String,
}

fn is_rockusb_candidate(class: u8, subclass: u8, protocol: u8) -> bool {
    // ADB is also vendor-specific and has two bulk endpoints.
    class == 0xff && (subclass, protocol) != (0x42, 0x01)
}

pub async fn targets() -> Result<Vec<Target>, String> {
    let devices = rockusb::nusb::devices()
        .await
        .map_err(|e| format!("无法枚举 USB 设备：{e}"))?;
    let mut targets = Vec::new();
    for usb in devices {
        let has_candidate = {
            let mut interfaces = usb.interfaces().peekable();
            interfaces.peek().is_some().then(|| {
                interfaces.any(|i| is_rockusb_candidate(i.class(), i.subclass(), i.protocol()))
            })
        };
        if has_candidate == Some(false) {
            continue;
        }
        // WinUSB may omit the interface summary for non-composite devices.
        // Read descriptors from the hub without claiming an interface.
        #[cfg(target_os = "windows")]
        if has_candidate.is_none()
            && let Ok(device) = usb.open().await
            && let Some(config) = device
                .active_configuration()
                .ok()
                .or_else(|| device.configurations().next())
            && !config
                .interface_alt_settings()
                .any(|i| is_rockusb_candidate(i.class(), i.subclass(), i.protocol()))
        {
            continue;
        }
        let id = identity(&usb);
        let port = format!("{}:{:?}", usb.bus_id(), usb.port_chain());
        // bcdUSB is only a hint: Rockchip's USB-PLUG deliberately keeps
        // 0x0200, unlike legacy Loader's 0x0201. Its standard gadget name
        // lets a fresh app discover it without probing a real Maskrom's
        // bulk endpoints on every scan. A recently downloaded Loader can
        // also be recognized by a read-only LBA response on the same port.
        let mode =
            if usb.usb_version() & 1 != 0 || usb.product_string() == Some("USB download gadget") {
                "loader"
            } else {
                "maskrom"
            };
        let info = DeviceInfo {
            id,
            // VID/PID identifies a USB product, not the board's SoC. In
            // Maskrom no chip-info command is available before loading RAM.
            name: "Rockchip USB 设备".into(),
            chip: String::new(),
            chip_model: None,
            mode: mode.into(),
            vid: "2207".into(),
            pid: format!("{:04x}", usb.product_id()),
            connection: format!("USB · {} · {:?}", port, usb.speed()),
            capacity: None,
        };
        targets.push(Target { usb, info, port });
    }
    let mut confirmed = CONFIRMED_LOADERS.lock().unwrap();
    confirmed.retain(|id| targets.iter().any(|target| &target.info.id == id));
    for target in &mut targets {
        if confirmed.contains(&target.info.id) {
            target.info.mode = "loader".into();
        }
    }
    Ok(targets)
}

impl Target {
    pub fn description(&self) -> String {
        format!(
            "2207:{:04x} · {} · bcdUSB=0x{:04x} · bcdDevice=0x{:04x} · {}",
            self.usb.product_id(),
            self.port,
            self.usb.usb_version(),
            self.usb.device_version(),
            self.usb.product_string().unwrap_or("无产品名称")
        )
    }
}

/// Record a successful RAM download without claiming the new Loader has booted.
pub fn remember_handoff(target: &Target) {
    if target.usb.port_chain().is_empty() {
        return;
    }
    let now = Instant::now();
    let mut pending = PENDING_HANDOFFS.lock().unwrap();
    pending.retain(|_, expires| *expires > now);
    pending.insert(target.port.clone(), now + HANDOFF_WINDOW);
}

/// Remember a bulk command that succeeded after the Maskrom handoff.
pub fn mark_loader(target: &mut Target) {
    PENDING_HANDOFFS.lock().unwrap().remove(&target.port);
    if target.info.mode == "maskrom" {
        CONFIRMED_LOADERS
            .lock()
            .unwrap()
            .insert(target.info.id.clone());
    }
    target.info.mode = "loader".into();
}

pub async fn detect_handoff(target: &mut Target) {
    if target.usb.port_chain().is_empty() {
        return;
    }
    let pending = {
        let now = Instant::now();
        let mut pending = PENDING_HANDOFFS.lock().unwrap();
        pending.retain(|_, expires| *expires > now);
        pending.contains_key(&target.port)
    };
    if !pending {
        return;
    }
    if target.info.mode == "loader" {
        PENDING_HANDOFFS.lock().unwrap().remove(&target.port);
        return;
    }
    let probe = tokio::time::timeout(Duration::from_secs(1), async {
        let mut session = Session::open(target).await?;
        session.read(0, &mut [0; 512]).await
    })
    .await;
    if matches!(probe, Ok(Ok(()))) {
        mark_loader(target);
    }
}

fn identity(d: &UsbInfo) -> String {
    // Enumeration identity intentionally changes after unplug/replug. A stale UI
    // selection must not silently target another board on the same USB port.
    format!(
        "2207:{:04x}:{}:{}:{:?}",
        d.product_id(),
        d.bus_id(),
        d.device_address(),
        d.id()
    )
}

pub async fn resolve(id: &str) -> Result<Target, String> {
    targets()
        .await?
        .into_iter()
        .find(|d| d.info.id == id)
        .ok_or_else(|| "目标设备已断开或重新连接，请刷新设备列表并重新确认".into())
}

pub struct Session {
    _device: nusb::Device,
    device_id: String,
    interface: Interface,
    input: Endpoint<Bulk, In>,
    output: Endpoint<Bulk, Out>,
}

impl Session {
    pub async fn open(target: &Target) -> Result<Self, String> {
        let device = target.usb.open().await.map_err(|e| format!("无法打开 USB 设备：{e}。Windows 请将目标 RockUSB 设备绑定到 WinUSB；关闭其他烧录工具"))?;
        // macOS does not automatically select a configuration for every
        // vendor-class device. Select only the descriptor's first configuration.
        if device.active_configuration().is_err() {
            let config = device.configurations().next().ok_or("设备没有 USB 配置")?;
            device
                .set_configuration(config.configuration_value())
                .await
                .map_err(|e| format!("无法配置 USB 设备：{e}"))?;
        }
        let config = device
            .active_configuration()
            .map_err(|e| format!("无法读取 USB 配置：{e}"))?;
        for alt in config.interface_alt_settings() {
            // RockUSB uses a vendor-class interface with bulk IN and OUT.
            if !is_rockusb_candidate(alt.class(), alt.subclass(), alt.protocol()) {
                continue;
            }
            let input = alt.endpoints().find(|e| {
                e.direction() == nusb::transfer::Direction::In
                    && e.transfer_type() == nusb::descriptors::TransferType::Bulk
            });
            let output = alt.endpoints().find(|e| {
                e.direction() == nusb::transfer::Direction::Out
                    && e.transfer_type() == nusb::descriptors::TransferType::Bulk
            });
            if let (Some(input), Some(output)) = (input, output) {
                let interface = device
                    .claim_interface(alt.interface_number())
                    .await
                    .map_err(|e| format!("无法独占 RockUSB 接口：{e}"))?;
                if alt.alternate_setting() != 0 {
                    interface
                        .set_alt_setting(alt.alternate_setting())
                        .await
                        .map_err(|e| format!("无法选择 USB 接口：{e}"))?;
                }
                let input = interface
                    .endpoint::<Bulk, In>(input.address())
                    .map_err(|e| e.to_string())?;
                let output = interface
                    .endpoint::<Bulk, Out>(output.address())
                    .map_err(|e| e.to_string())?;
                return Ok(Self {
                    _device: device.clone(),
                    device_id: target.info.id.clone(),
                    interface,
                    input,
                    output,
                });
            }
        }
        Err("未找到 RockUSB 双向批量接口；该设备可能不是烧录模式".into())
    }

    pub async fn execute<O, T>(&mut self, op: O) -> Result<T, String>
    where
        O: OperationSteps<T>,
    {
        self.execute_operation(op, false).await
    }

    async fn execute_operation<O, T>(&mut self, mut op: O, lba_transfer: bool) -> Result<T, String>
    where
        O: OperationSteps<T>,
    {
        loop {
            match op.step() {
                UsbStep::WriteBulk { data } => self.bulk_out(data).await?,
                UsbStep::ReadBulk { data } => {
                    self.bulk_in(data).await?;
                    // rkdeveloptool trusts the actual USB byte count and CSW
                    // status, not the LBA residue field. rockusb otherwise
                    // subtracts it from the already-checked transfer length.
                    // LBA payloads are sector-sized, so this is only the CSW.
                    if lba_transfer
                        && data.len() == COMMAND_STATUS_BYTES
                        && data.starts_with(b"USBS")
                    {
                        data[8..12].fill(0);
                    }
                }
                UsbStep::WriteControl {
                    request_type,
                    request,
                    value,
                    index,
                    data,
                } => {
                    if request_type != 0x40 {
                        return Err("不支持的 RockUSB 控制请求".into());
                    }
                    let control = ControlOut {
                        control_type: ControlType::Vendor,
                        recipient: Recipient::Device,
                        request,
                        value,
                        index,
                        data,
                    };
                    self.interface
                        .control_out(control, TIMEOUT)
                        .await
                        .map_err(|e| format!("Loader USB 控制传输失败：{e}"))?;
                }
                UsbStep::Finished(result) => {
                    return result.map_err(|e| format!("RockUSB 协议错误：{e}"));
                }
            }
        }
    }

    async fn bulk_out(&mut self, data: &[u8]) -> Result<(), String> {
        self.output.submit(Buffer::from(data.to_vec()));
        let completion = tokio::time::timeout(TIMEOUT, self.output.next_complete())
            .await
            .map_err(|_| "USB 写入超时，设备状态未知；请重新连接后恢复烧录".to_string())?;
        let transferred = completion.actual_len;
        completion
            .into_result()
            .map_err(|e| format!("USB 写入失败：{e}"))?;
        if transferred != data.len() {
            return Err(format!("USB 短写：{transferred}/{} 字节", data.len()));
        }
        Ok(())
    }

    async fn bulk_in(&mut self, data: &mut [u8]) -> Result<(), String> {
        let buffer = self.receive_bulk(data.len()).await?;
        if buffer.len() != data.len() {
            return Err(format!(
                "USB 短读或多余响应：{}/{} 字节",
                buffer.len(),
                data.len()
            ));
        }
        data.copy_from_slice(&buffer);
        Ok(())
    }

    async fn receive_bulk(&mut self, size: usize) -> Result<Buffer, String> {
        self.input.submit(Buffer::new(
            size.next_multiple_of(self.input.max_packet_size()),
        ));
        tokio::time::timeout(TIMEOUT, self.input.next_complete())
            .await
            .map_err(|_| "USB 读取超时，操作已停止；请检查连接".to_string())?
            .into_result()
            .map_err(|e| format!("USB 读取失败：{e}"))
    }

    pub async fn flash_info(&mut self) -> Result<FlashInfo, String> {
        // rkdeveloptool advertises 11 bytes in the CBW but accepts a FlashInfo
        // reply up to 512 bytes. Only its fixed 11-byte prefix is interpreted.
        let command = CommandBlock::flash_info();
        let mut cbw = [0; COMMAND_BLOCK_BYTES];
        command.to_bytes(&mut cbw);
        self.bulk_out(&cbw).await?;
        let reply = self.receive_bulk(512).await?;
        if !(11..=512).contains(&reply.len()) {
            return Err(format!("FlashInfo 响应长度无效：{} 字节", reply.len()));
        }
        let mut csw = [0; COMMAND_STATUS_BYTES];
        self.bulk_in(&mut csw).await?;
        let status =
            CommandStatus::from_bytes(&csw).map_err(|e| format!("FlashInfo 响应状态无效：{e}"))?;
        if status.tag != command.tag() || status.status != Status::SUCCESS {
            return Err("FlashInfo 响应标签不匹配或命令失败".into());
        }
        Ok(FlashInfo::from_bytes(reply[..11].try_into().unwrap()))
    }
    pub async fn chip_info(&mut self) -> Result<ChipInfo, String> {
        self.execute(operation::chip_info()).await
    }
    pub async fn describe_chip(&mut self, target: &mut Target) -> Result<(), String> {
        let chip = self.chip_info().await?;
        let data = chip.inner();
        let raw = format!(
            "Chip ID {}",
            data.iter()
                .map(|b| format!("{b:02X}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        target.info.chip = raw;
        target.info.chip_model = chip_model(data).map(str::to_owned);
        Ok(())
    }
    pub async fn capacity(&mut self) -> Result<u64, String> {
        let sectors = u64::from(self.flash_info().await?.sectors());
        if sectors == 0 {
            return Err("设备报告的容量为零；未进行写入".into());
        }
        Ok(sectors)
    }
    pub async fn idb_write(
        &mut self,
        file: &mut File,
        offset: u64,
        size: u64,
    ) -> Result<crate::loader::IdbWrite, String> {
        let loader = crate::loader::PersistentLoader::read(file, offset, size)?;
        let write = loader.idb_write()?;
        // Like rkdeveloptool ul, only FlashHead needs the new-IDB capability.
        if loader.has_flash_head()? && !self.execute(operation::capability()).await?.new_idb() {
            return Err(
                "MiniLoader contains FlashHead, but the device does not support the new IDB format"
                    .into(),
            );
        }
        Ok(write)
    }
    pub async fn read(&mut self, sector: u64, data: &mut [u8]) -> Result<(), String> {
        check_transfer(sector, data.len())?;
        self.execute_operation(operation::read_lba(sector as u32, data), true)
            .await?;
        Ok(())
    }
    pub async fn write(&mut self, sector: u64, data: &[u8]) -> Result<(), String> {
        check_transfer(sector, data.len())?;
        self.execute_operation(operation::write_lba(sector as u32, data), true)
            .await?;
        Ok(())
    }
    pub async fn erase(&mut self, sector: u64, sectors: u16) -> Result<(), String> {
        check_transfer(sector, sectors as usize * 512)?;
        self.execute(operation::erase_lba(sector as u32, sectors))
            .await
    }
    pub async fn reboot(&mut self, maskrom: bool) -> Result<(), String> {
        // The device might reset even if the final USB status is lost.
        CONFIRMED_LOADERS.lock().unwrap().remove(&self.device_id);
        self.execute(operation::reset_device(if maskrom {
            ResetOpcode::Maskrom
        } else {
            ResetOpcode::Reset
        }))
        .await
    }
}

pub(crate) fn chip_model(data: &[u8]) -> Option<&'static str> {
    // Rockchip U-Boot's rockchip_rockusb_get_chip_info returns a little-endian
    // u32. Legacy fixups and shared family IDs require an explicit whitelist.
    let id = u32::from_le_bytes(data.get(..4)?.try_into().ok()?);
    Some(match id {
        0x33303341 => "RK3036",
        0x33313241 => "RK3126 / RK3128",
        0x33323241 => "RK322x",
        0x33323248 => "RK3328",
        0x33333043 => "RK3399",
        0x31383041 => "RK1808",
        0x31313236 => "RV1126",
        0x31313046 => "RV1126B",
        0x31313036 => "RV1106",
        0x31313045 => "RV1103B",
        0x33333038 => "RK3308",
        0x33333236 => "RK3326",
        0x50583330 => "PX30",
        0x33353238 => "RK3528",
        0x33353632 => "RK3562",
        0x33353638 => "RK356x",
        0x33353736 => "RK3576",
        0x33353838 => "RK3588",
        0x33353046 => "RK3506",
        _ => return None,
    })
}

fn check_transfer(sector: u64, bytes: usize) -> Result<(), String> {
    if bytes == 0
        || bytes % 512 != 0
        || bytes / 512 > u16::MAX as usize
        || sector
            .checked_add(bytes as u64 / 512)
            .is_none_or(|end| end > u32::MAX as u64 + 1)
    {
        return Err("USB 传输范围无效或超出 RockUSB 32 位 LBA 地址能力".into());
    }
    Ok(())
}

pub async fn primary_partitions(
    session: &mut Session,
) -> Result<Option<Vec<crate::model::PartitionInfo>>, String> {
    let mut metadata = [0; crate::gpt::DEVICE_PRIMARY_BYTES];
    session.read(0, &mut metadata).await?;
    crate::gpt::parse_device_primary(&metadata)
}

pub async fn partitions(session: &mut Session) -> Result<Vec<crate::model::PartitionInfo>, String> {
    primary_partitions(session)
        .await?
        .ok_or_else(|| "No primary GPT signature at LBA 1".into())
}
