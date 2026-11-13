//! Opt-in terminal maintenance. Uses the native RockUSB session, not a child
//! process or the desktop/MCP command registries.
mod sparse;

use crate::{
    firmware, gpt, loader,
    model::PartitionInfo,
    usb::{self, CHUNK_BYTES, Session},
};
use rockusb::{operation, protocol::StorageIndex};
use std::{
    ffi::OsString,
    fs::{File, OpenOptions},
    io::{self, Read, Seek, Write},
    path::Path,
};

const SECTOR: usize = 512;
const ROCKUSB_SECTORS: u64 = u32::MAX as u64 + 1;
const PARAM_LBA: u64 = 8192;
const PARAM_SECTORS: u64 = 512;

const HELP: &str = "rkdevtool-maintenance [--allow-device-write] COMMAND

  ppt                        List partitions from the primary GPT or legacy parameter
  rl LBA SECTORS OUT         Read an exact device range into a new file
  wl LBA IMAGE               Write RAW or Android sparse by address
  wlx PARTITION IMAGE        Write RAW or Android sparse within a named partition
  ul LOADER                  Persist an unsigned MiniLoader on eMMC/SD
  gpt PARAMETER_OR_GPT_IMG   Write GPT from parameters or a 67-sector image
  prm PARAMETER              Write a legacy PARM block on a non-GPT device
  pack TEMPLATE DIR OUT      Repack an unsigned MiniLoader from unpacked entries
  unpack LOADER NEW_DIR      Extract validated MiniLoader entries
  tagspl TAG SPL OUT         Prefix a tag into a new SPL file

All device writes require --allow-device-write. Close the desktop application
and connect exactly one device in Loader or Maskrom mode.";

pub fn run(arguments: impl IntoIterator<Item = OsString>) -> Result<(), String> {
    let mut args = arguments.into_iter().peekable();
    let allow_write = args.peek().is_some_and(|arg| arg == "--allow-device-write");
    if allow_write {
        args.next();
    }
    let command = args.next();
    let args: Vec<_> = args.collect();
    match (
        command.as_deref().and_then(|name| name.to_str()),
        args.as_slice(),
    ) {
        (None | Some("--help" | "-h"), []) => {
            println!("{HELP}");
            Ok(())
        }
        (Some("pack"), [template, directory, output]) => {
            let (mut file, size) = input(Path::new(template))?;
            let bytes = loader::repack_container(&mut file, size, Path::new(directory))?;
            let mut output = new_file(Path::new(output))?;
            output
                .write_all(&bytes)
                .and_then(|_| output.sync_all())
                .map_err(io_error)
        }
        (Some("unpack"), [path, output]) => {
            let (mut file, size) = input(Path::new(path))?;
            loader::unpack_container(&mut file, size, Path::new(output))
        }
        (Some("tagspl"), [tag, path, output]) => {
            let tag = tag
                .to_str()
                .filter(|tag| !tag.is_empty())
                .ok_or("A nonempty UTF-8 SPL tag is required")?;
            let (mut file, _) = input(Path::new(path))?;
            let mut output = new_file(Path::new(output))?;
            output.write_all(tag.as_bytes()).map_err(io_error)?;
            io::copy(&mut file, &mut output).map_err(io_error)?;
            output.sync_all().map_err(io_error)
        }
        (Some("ppt"), []) => device_task(async {
            let mut session = open_session().await?;
            let (kind, partitions) = partition_layout(&mut session, None).await?;
            println!("{kind}\tSTART_LBA\tSECTORS");
            for part in partitions {
                println!("{}\t{}\t{}", part.name, part.start_sector, part.sectors);
            }
            Ok(())
        }),
        (Some("rl"), [start, sectors, output]) => device_task(async {
            let start = number(start, "LBA")?;
            let sectors = number(sectors, "sector count")?;
            check_address(start, sectors)?;
            let mut session = open_session().await?;
            let mut out = new_file(Path::new(output))?;
            let mut data = vec![0; CHUNK_BYTES];
            let mut done = 0;
            while done < sectors {
                let count = (sectors - done).min((CHUNK_BYTES / SECTOR) as u64);
                let data = &mut data[..count as usize * SECTOR];
                session.read(start + done, data).await?;
                out.write_all(data).map_err(io_error)?;
                done += count;
            }
            out.sync_all().map_err(io_error)
        }),
        (Some("wl"), [start, path]) => {
            require_write(allow_write)?;
            let start = number(start, "LBA")?;
            device_task(async {
                let (mut session, capacity) = open_device().await?;
                let available = capacity
                    .checked_sub(start)
                    .ok_or("Start LBA is outside the device")?;
                write_image(&mut session, start, available, Path::new(path)).await
            })
        }
        (Some("wlx"), [name, path]) => {
            require_write(allow_write)?;
            let name = name.to_str().ok_or("Partition name must be UTF-8")?;
            device_task(async {
                let (mut session, capacity) = open_device().await?;
                let (kind, parts) = partition_layout(&mut session, Some(capacity)).await?;
                let mut matches = parts.iter().filter(|part| {
                    if kind == "PARM" {
                        part.name.eq_ignore_ascii_case(name)
                    } else {
                        part.name == name
                    }
                });
                let target = matches
                    .next()
                    .ok_or("Partition not found; use the name shown by ppt")?;
                if matches.next().is_some() {
                    return Err(format!("Partition name is ambiguous: {name}"));
                }
                check_range(target.start_sector, target.sectors, capacity)?;
                if kind == "GPT" && target.start_sector < gpt::DEVICE_PRIMARY_SECTORS {
                    return Err("Selected partition overlaps the primary GPT sectors".into());
                }
                ensure_no_overlap(&parts, target.start_sector, target.sectors, Some(target))?;
                write_image(
                    &mut session,
                    target.start_sector,
                    target.sectors,
                    Path::new(path),
                )
                .await
            })
        }
        (Some("gpt"), [path]) => {
            require_write(allow_write)?;
            device_task(write_gpt(Path::new(path)))
        }
        (Some("prm"), [path]) => {
            require_write(allow_write)?;
            device_task(write_parameter(Path::new(path)))
        }
        (Some("ul"), [path]) => {
            require_write(allow_write)?;
            device_task(write_loader(Path::new(path)))
        }
        _ => Err(format!("Invalid command or arguments.\n\n{HELP}")),
    }
}

fn device_task(future: impl Future<Output = Result<(), String>>) -> Result<(), String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Cannot initialize the USB runtime: {e}"))?
        .block_on(future)
}

fn require_write(allowed: bool) -> Result<(), String> {
    if allowed {
        Ok(())
    } else {
        Err("Device writes require --allow-device-write before the command".into())
    }
}

async fn open_session() -> Result<Session, String> {
    let mut devices = usb::targets().await?;
    if devices.len() != 1 {
        return Err("Connect exactly one Rockchip device".into());
    }
    let target = devices.pop().unwrap();
    if !matches!(target.info.mode.as_str(), "loader" | "maskrom") {
        return Err("Put the device in Loader or Maskrom mode before maintenance".into());
    }
    let session = Session::open(&target).await?;
    eprintln!("{}", target.description());
    Ok(session)
}

async fn open_device() -> Result<(Session, u64), String> {
    let mut session = open_session().await?;
    let capacity = session.capacity().await?;
    Ok((session, capacity))
}

async fn require_sd_emmc(session: &mut Session) -> Result<(), String> {
    let medium = session.execute(operation::storage()).await?;
    if !matches!(
        medium,
        StorageIndex::Emmc | StorageIndex::Sd0 | StorageIndex::Sd1
    ) {
        return Err(format!(
            "This operation is limited to eMMC or SD (current: {medium})"
        ));
    }
    Ok(())
}

async fn primary_metadata(session: &mut Session) -> Result<[u8; 1024], String> {
    let mut metadata = [0; 1024];
    session.read(0, &mut metadata).await?;
    Ok(metadata)
}

async fn has_gpt(session: &mut Session) -> Result<bool, String> {
    let primary = primary_metadata(session).await?;
    Ok(primary[512..520] == *b"EFI PART"
        || (primary[510..512] == [0x55, 0xaa]
            && primary[446..510]
                .chunks_exact(16)
                .any(|entry| entry[4] == 0xee)))
}

async fn legacy_block(session: &mut Session) -> Result<Vec<u8>, String> {
    let mut bytes = vec![0; PARAM_SECTORS as usize * SECTOR];
    session.read(PARAM_LBA, &mut bytes).await?;
    Ok(bytes)
}

async fn partition_layout(
    session: &mut Session,
    capacity: Option<u64>,
) -> Result<(&'static str, Vec<PartitionInfo>), String> {
    if let Some(partitions) = usb::primary_partitions(session).await? {
        return Ok(("GPT", partitions));
    }
    let legacy = firmware::LegacyLayout::from_device(&legacy_block(session).await?)?;
    let capacity = if capacity.is_none() && legacy.has_grow() {
        Some(session.capacity().await?)
    } else {
        capacity
    };
    Ok(("PARM", legacy.partitions(capacity)?))
}

async fn write_image(
    session: &mut Session,
    start: u64,
    available: u64,
    path: &Path,
) -> Result<(), String> {
    let (mut file, mut size) = input(path)?;
    let mut prefix = vec![0; size.min(SECTOR as u64) as usize];
    file.read_exact(&mut prefix).map_err(io_error)?;
    file.rewind().map_err(io_error)?;
    if prefix.starts_with(&sparse::MAGIC) {
        (file, size) = sparse::expand(&mut file, size, available * SECTOR as u64)?;
        prefix.resize(SECTOR, 0);
        file.read_exact(&mut prefix).map_err(io_error)?;
        file.rewind().map_err(io_error)?;
    }
    if prefix.starts_with(b"UBI#") {
        return Err(
            "UBI/NAND images require erase-block and bad-block handling; not supported".into(),
        );
    }
    if [b"RKFW", b"RKAF", b"BOOT", b"LDR "]
        .iter()
        .any(|magic| prefix.starts_with(*magic))
    {
        return Err("A firmware or loader container must not be written as a RAW image".into());
    }
    firmware::reject_encoded_image(&prefix)?;
    if size.div_ceil(SECTOR as u64) > available {
        return Err("Image exceeds the target range".into());
    }
    let mut bytes = vec![0; CHUNK_BYTES];
    let mut done = 0;
    while done < size {
        let len = (size - done).min(CHUNK_BYTES as u64) as usize;
        let data = &mut bytes[..len.next_multiple_of(SECTOR)];
        data.fill(0);
        file.read_exact(&mut data[..len]).map_err(|error| {
            format!("Read image: {error}; previous chunks may already be written")
        })?;
        write_lba(session, start + done / SECTOR as u64, data).await?;
        done += len as u64;
    }
    Ok(())
}

async fn write_gpt(path: &Path) -> Result<(), String> {
    let (file, size) = input(path)?;
    if size > 64 * 1024 {
        return Err("Expected GPT parameter text or a 67-sector GPT image".into());
    }
    let mut bytes = Vec::new();
    file.take(size).read_to_end(&mut bytes).map_err(io_error)?;
    let (mut session, capacity) = open_device().await?;
    let disk = if bytes.get(SECTOR..SECTOR + 8) == Some(&b"EFI PART"[..]) {
        gpt::resize_image(&bytes, capacity)?
    } else {
        let parameter = std::str::from_utf8(&bytes)
            .map_err(|_| "Expected GPT parameter text or a valid GPT image")?;
        gpt::build(parameter, capacity)?
    };
    write_lba(&mut session, 0, &disk.primary).await?;
    write_lba(&mut session, capacity - 33, &disk.backup).await
}

async fn write_parameter(path: &Path) -> Result<(), String> {
    let (file, size) = input(path)?;
    if size > PARAM_SECTORS * SECTOR as u64 - 12 {
        return Err("Legacy parameter file is too large".into());
    }
    let mut text = Vec::new();
    file.take(size).read_to_end(&mut text).map_err(io_error)?;
    let block = firmware::encode_legacy_parameter(&text)?;
    let parameter = std::str::from_utf8(&text).map_err(|_| "Legacy parameter must be ASCII")?;
    let (mut session, capacity) = open_device().await?;
    if has_gpt(&mut session).await? {
        return Err("prm cannot replace or coexist with a GPT disk layout".into());
    }
    let sector_count = (block.len() / SECTOR) as u64;
    check_range(PARAM_LBA, sector_count, capacity)?;
    let partitions =
        firmware::LegacyLayout::from_parameter(parameter)?.partitions(Some(capacity))?;
    ensure_no_overlap(&partitions, PARAM_LBA, sector_count, None)?;
    let previous = legacy_block(&mut session).await?;
    if previous.starts_with(b"PARM") {
        let partitions =
            firmware::LegacyLayout::from_device(&previous)?.partitions(Some(capacity))?;
        ensure_no_overlap(&partitions, PARAM_LBA, sector_count, None)?;
    } else if previous[..block.len()].iter().any(|&b| b != 0) {
        return Err("The target PARM sectors contain unrecognized data; no write performed".into());
    }
    write_lba(&mut session, PARAM_LBA, &block).await
}

async fn write_loader(path: &Path) -> Result<(), String> {
    let (mut file, size) = input(path)?;
    let (mut session, capacity) = open_device().await?;
    require_sd_emmc(&mut session).await?;
    let write = session.idb_write(&mut file, 0, size).await?;
    let sectors = (write.data.len() / SECTOR) as u64;
    check_range(write.sector, sectors, capacity)?;
    if write.sector < gpt::DEVICE_PRIMARY_SECTORS
        || write.sector + sectors > capacity.saturating_sub(33)
    {
        return Err("IDB would overlap reserved GPT sectors".into());
    }
    write_lba(&mut session, write.sector, &write.data).await
}

fn ensure_no_overlap(
    parts: &[PartitionInfo],
    start: u64,
    sectors: u64,
    except: Option<&PartitionInfo>,
) -> Result<(), String> {
    for part in parts {
        if except.is_some_and(|target| std::ptr::eq(part, target)) {
            continue;
        }
        if start < part.start_sector.saturating_add(part.sectors)
            && part.start_sector < start + sectors
        {
            return Err(format!(
                "Maintenance write overlaps partition {}",
                part.name
            ));
        }
    }
    Ok(())
}

async fn write_lba(session: &mut Session, start: u64, bytes: &[u8]) -> Result<(), String> {
    for (index, data) in bytes.chunks(CHUNK_BYTES).enumerate() {
        let lba = start + (index * CHUNK_BYTES / SECTOR) as u64;
        session
            .write(lba, data)
            .await
            .map_err(|e| format!("Write at LBA {lba}: {e}; device may be partially written"))?;
    }
    Ok(())
}

fn check_range(start: u64, sectors: u64, capacity: u64) -> Result<(), String> {
    check_address(start, sectors)?;
    if start + sectors > capacity {
        return Err(format!(
            "LBA range is outside the device capacity of {capacity} sectors"
        ));
    }
    Ok(())
}

fn check_address(start: u64, sectors: u64) -> Result<(), String> {
    if sectors == 0
        || start
            .checked_add(sectors)
            .is_none_or(|end| end > ROCKUSB_SECTORS)
    {
        return Err("LBA range is empty or exceeds RockUSB's 32-bit addressing".into());
    }
    Ok(())
}

fn input(path: &Path) -> Result<(File, u64), String> {
    let metadata = path.metadata().map_err(io_error)?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(format!(
            "Expected a nonempty regular file: {}",
            path.display()
        ));
    }
    let file = File::open(path).map_err(io_error)?;
    Ok((file, metadata.len()))
}

fn new_file(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|error| format!("Create new file {}: {error}", path.display()))
}

fn number(value: &OsString, label: &str) -> Result<u64, String> {
    let value = value.to_str().ok_or_else(|| format!("Invalid {label}"))?;
    let parsed = match value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => value.parse(),
    };
    parsed.map_err(|_| format!("Invalid {label}: expected an unsigned integer"))
}

fn io_error(error: io::Error) -> String {
    error.to_string()
}
