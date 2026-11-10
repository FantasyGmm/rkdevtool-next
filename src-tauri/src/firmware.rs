//! Bounded, read-only inspection of Rockchip containers and raw disk images.
//!
//! RKFW/RKAF field offsets, high-dword markers and legacy 4 GiB recovery follow
//! upstream/rkdeveloptool-gui/rkfw.py. RKCRC is Rockchip's afptool variant, not
//! the IEEE CRC used by GPT. Payload offsets are always absolute file offsets.

use crate::model::{FirmwareInfo, FirmwarePartition};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const RKFW_HEADER_SIZE: u64 = 102;
const RKAF_HEADER_SIZE: u64 = 2048;
const RKAF_PART_SIZE: usize = 112;
const RKAF_MAX_PARTS: usize = 16;
const ARCHIVE_SECTOR: u64 = 2048;
const DISK_SECTOR: u64 = 512;
const U32_RANGE: u64 = 1 << 32;
const SIGNED_TAIL_MIN: u64 = 160;
const SIGNED_TAIL_MAX: u64 = 1 << 20;
const MAX_PARAMETER_SIZE: u64 = 1 << 20;
const COPY_CHUNK: usize = 1 << 20;

pub struct Image {
    pub info: FirmwareInfo,
    pub package: Option<Package>,
}

pub struct Package {
    pub archive_offset: u64,
    /// RKAF bytes covered by RKCRC; excludes the following four-byte checksum.
    pub archive_length: u64,
    pub loader_offset: Option<u64>,
    pub loader_size: u64,
    /// Flashable members; `bootloader` is the special, unpartitioned IDB member.
    pub parts: Vec<Part>,
    pub parameter: String,
    members: Vec<Member>,
}

pub struct Part {
    pub name: String,
    pub offset: u64,
    pub size: u64,
    pub start_sector: Option<u64>,
    /// None means a final grow partition, or the special `bootloader` member.
    pub sectors: Option<u64>,
}

#[derive(Clone)]
struct Member {
    name: String,
    offset: u64,
    size: u64,
}

struct Layout {
    name: String,
    start: u64,
    sectors: Option<u64>,
}

pub fn inspect(path: &str) -> Result<Image, String> {
    let canonical = Path::new(path)
        .canonicalize()
        .map_err(|e| format!("Cannot open firmware: {e}"))?;
    let path = canonical
        .to_str()
        .ok_or("Firmware path is not valid UTF-8")?;
    let mut file = File::open(&canonical).map_err(|e| format!("Cannot open firmware: {e}"))?;
    let metadata = file
        .metadata()
        .map_err(|e| format!("Cannot inspect firmware: {e}"))?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err("Choose a nonempty regular firmware file".into());
    }
    let size = metadata.len();
    let prefix = read_at(&mut file, 0, size.min(4096) as usize, size)?;
    reject_encoded_image(&prefix)?;
    let mut info = FirmwareInfo {
        path: path.to_owned(),
        name: canonical
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("firmware")
            .to_owned(),
        size,
        kind: "raw".into(),
        model: None,
        manufacturer: None,
        partitions: Vec::new(),
        warnings: Vec::new(),
    };
    let package = if prefix.starts_with(b"RKFW") {
        info.kind = "rkfw".into();
        let (offset, length, loader_offset, loader_size, tail) = parse_rkfw(&mut file, size)?;
        if tail >= SIGNED_TAIL_MIN {
            info.warnings.push(
                "This image has a signature trailer; signature authenticity is not verified".into(),
            );
        }
        Some(parse_archive(
            &mut file,
            size,
            offset,
            Some(length),
            Some(loader_offset),
            loader_size,
            &mut info,
        )?)
    } else if prefix.starts_with(b"RKAF") {
        info.kind = "rkfw".into();
        info.warnings
            .push("Standalone RKAF archive: no RAM Loader is included".into());
        Some(parse_archive(
            &mut file,
            size,
            0,
            Some(size),
            None,
            0,
            &mut info,
        )?)
    } else if prefix.starts_with(b"BOOT") || prefix.starts_with(b"LDR ") {
        inspect_loader(&mut file, size)?;
        info.kind = "loader".into();
        None
    } else {
        // An unknown archive must not become a plausible raw write by extension.
        let extension = canonical.extension().and_then(|e| e.to_str()).unwrap_or("");
        let extension = extension.to_ascii_lowercase();
        if matches!(
            extension.as_str(),
            "gz" | "gzip" | "tgz" | "xz" | "bz2" | "zip" | "zst" | "zstd" | "lz4" | "7z" | "tar"
        ) {
            return Err("Compressed archives must be extracted before flashing".into());
        }
        if matches!(
            extension.as_str(),
            "qcow" | "qcow2" | "vmdk" | "vdi" | "vhd" | "vhdx" | "sparseimage"
        ) {
            return Err(
                "Virtual or sparse disk containers must be exported as raw disk images".into(),
            );
        }
        inspect_raw(&mut file, &prefix, size, &mut info)?;
        None
    };
    Ok(Image { info, package })
}

/// Stream the complete archive before any destructive operation. The callback
/// receives the number of bytes consumed by this chunk and may cancel the scan.
pub fn verify_archive(
    file: &mut File,
    package: &Package,
    mut on_chunk: impl FnMut(u64) -> Result<(), String>,
) -> Result<(), String> {
    let file_size = file
        .metadata()
        .map_err(|e| format!("Cannot inspect firmware: {e}"))?
        .len();
    if package.archive_length < RKAF_HEADER_SIZE {
        return Err("RKAF archive length is invalid".into());
    }
    let crc_offset = range_end(
        package.archive_offset,
        package.archive_length,
        file_size,
        "RKAF archive",
    )?;
    let stored = read_at(file, crc_offset, 4, file_size)?;
    file.seek(SeekFrom::Start(package.archive_offset))
        .map_err(|e| format!("Cannot seek firmware: {e}"))?;
    let mut remaining = package.archive_length;
    let mut buffer = vec![0u8; COPY_CHUNK];
    let mut crc = 0;
    while remaining != 0 {
        let count = remaining.min(buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..count])
            .map_err(|e| format!("Cannot read RKAF archive during checksum verification: {e}"))?;
        crc = rkcrc32(crc, &buffer[..count]);
        remaining -= count as u64;
        on_chunk(count as u64)?;
    }
    if crc != le32(&stored, 0) {
        return Err("RKAF checksum mismatch; the firmware is damaged or incomplete".into());
    }
    Ok(())
}

fn parse_rkfw(file: &mut File, file_size: u64) -> Result<(u64, u64, u64, u64, u64), String> {
    let header = read_at(file, 0, RKFW_HEADER_SIZE as usize, file_size)?;
    let loader_offset = le32(&header, 25) as u64;
    let loader_size = le32(&header, 29) as u64;
    let archive_offset = le32(&header, 33) as u64;
    let mut archive_size = le32(&header, 37) as u64;
    if let Some(high) = high_dword(&header, 55, b"HI") {
        archive_size |= high << 32;
    } else {
        let low_end = range_end(archive_offset, archive_size, file_size, "RKFW archive")?;
        let remainder = file_size - low_end;
        let tail = remainder % U32_RANGE;
        if remainder > SIGNED_TAIL_MAX && !valid_tail(tail) {
            return Err("RKFW size does not match the container layout (truncated image?)".into());
        }
        if remainder >= U32_RANGE {
            archive_size = archive_size
                .checked_add(remainder - tail)
                .ok_or("RKFW archive size overflow")?;
        }
    }
    if loader_offset < RKFW_HEADER_SIZE || loader_size == 0 {
        return Err("RKFW Loader offset or size is invalid".into());
    }
    let loader_end = range_end(loader_offset, loader_size, file_size, "RKFW Loader")?;
    if archive_offset < RKFW_HEADER_SIZE || archive_size < RKAF_HEADER_SIZE + 4 {
        return Err("RKFW archive offset or size is invalid".into());
    }
    let archive_end = range_end(archive_offset, archive_size, file_size, "RKFW archive")?;
    if loader_offset < archive_end && archive_offset < loader_end {
        return Err("RKFW Loader and archive overlap".into());
    }
    let tail = file_size - archive_end;
    if !valid_tail(tail) {
        return Err("RKFW has an invalid or truncated trailing data region".into());
    }
    Ok((
        archive_offset,
        archive_size,
        loader_offset,
        loader_size,
        tail,
    ))
}

fn parse_archive(
    file: &mut File,
    file_size: u64,
    archive_offset: u64,
    container_size: Option<u64>,
    loader_offset: Option<u64>,
    loader_size: u64,
    info: &mut FirmwareInfo,
) -> Result<Package, String> {
    let header = read_at(file, archive_offset, RKAF_HEADER_SIZE as usize, file_size)?;
    if !header.starts_with(b"RKAF") {
        return Err("RKFW payload is not an RKAF partition archive".into());
    }
    let mut length = le32(&header, 4) as u64;
    if let Some(container_size) = container_size {
        let expected = container_size
            .checked_sub(4)
            .ok_or("RKAF is missing its checksum")?;
        if expected >= U32_RANGE {
            if expected % U32_RANGE != length {
                return Err("RKAF size does not match the container layout".into());
            }
            length = expected;
        }
        if length != expected {
            return Err("RKAF length does not match its container".into());
        }
    }
    if length < RKAF_HEADER_SIZE {
        return Err("RKAF length is smaller than its header".into());
    }
    let crc_offset = range_end(archive_offset, length, file_size, "RKAF archive")?;
    range_end(crc_offset, 4, file_size, "RKAF checksum")?;
    info.model = optional_metadata(&header[8..42], "model")?;
    // Validate even the metadata not currently shown by the UI.
    optional_metadata(&header[42..72], "machine ID")?;
    info.manufacturer = optional_metadata(&header[72..128], "manufacturer")?;
    let count = le32(&header, 136) as usize;
    if count == 0 || count > RKAF_MAX_PARTS {
        return Err(format!(
            "RKAF partition count must be between 1 and {RKAF_MAX_PARTS}"
        ));
    }
    let mut names = HashSet::new();
    let mut members = Vec::new();
    let mut ranges = Vec::new();
    let mut previous_end = RKAF_HEADER_SIZE;
    let mut known_layouts = HashMap::new();
    for index in 0..count {
        let base = 140 + index * RKAF_PART_SIZE;
        let name = ascii_cstr(&header[base..base + 32], "RKAF member name")?;
        validate_name(&name)?;
        if !names.insert(name.clone()) {
            return Err(format!("RKAF contains duplicate member '{name}'"));
        }
        let file_field = &header[base + 32..base + 92];
        let pos_high = high_dword(file_field, 50, b"H");
        let size_high = high_dword(file_field, 55, b"H");
        // High-word markers occupy the end of the filename field, not text.
        let filename_end = if pos_high.is_some() {
            50
        } else if size_high.is_some() {
            55
        } else {
            60
        };
        let filename = ascii_cstr(&file_field[..filename_end], "RKAF filename")?;
        if filename.is_empty() {
            return Err(format!("RKAF member '{name}' has no filename"));
        }
        let mut position = le32(&header, base + 96) as u64;
        let mut size = le32(&header, base + 108) as u64;
        let padded = le32(&header, base + 104) as u64;
        if let Some(high) = pos_high {
            position |= high << 32;
        }
        if let Some(high) = size_high {
            size |= high << 32;
        }
        if filename == "SELF" {
            continue;
        }
        if length >= U32_RANGE && (pos_high.is_none() || size_high.is_none()) {
            (position, size) = recover_layout(
                &name,
                position,
                size,
                padded,
                pos_high.is_some(),
                size_high.is_some(),
                previous_end,
                &mut known_layouts,
                length,
            )?;
        }
        if position < RKAF_HEADER_SIZE || position % ARCHIVE_SECTOR != 0 || size == 0 {
            return Err(format!(
                "RKAF member '{name}' has an invalid offset or size"
            ));
        }
        let end = range_end(position, size, length, &format!("RKAF member '{name}'"))?;
        let allocation_end = range_end(
            position,
            align_up(size, ARCHIVE_SECTOR)?,
            length,
            "RKAF member allocation",
        )?;
        previous_end = previous_end.max(allocation_end);
        ranges.push((position, end, name.clone()));
        let offset = archive_offset
            .checked_add(position)
            .ok_or("RKAF member offset overflow")?;
        members.push(Member { name, offset, size });
    }
    reject_overlaps(&mut ranges, "RKAF members")?;
    let parameter_member = members
        .iter()
        .find(|m| m.name == "parameter")
        .ok_or("RKAF has no parameter member; partition addresses cannot be determined")?;
    let parameter = read_parameter(file, file_size, parameter_member)?;
    let layout = parse_parameter(&parameter, &mut info.warnings)?;
    let mut parts = Vec::new();
    for member in &members {
        if member.name == "parameter" || member.name == "package-file" {
            continue;
        }
        let prefix = read_at(
            file,
            member.offset,
            member.size.min(512) as usize,
            file_size,
        )?;
        reject_encoded_image(&prefix).map_err(|e| format!("RKAF member '{}': {e}", member.name))?;
        let (start_sector, sectors) = if member.name == "bootloader" {
            (None, None)
        } else {
            let partition = layout
                .iter()
                .find(|p| p.name == member.name)
                .ok_or_else(|| {
                    format!(
                        "RKAF member '{}' has no partition in parameter",
                        member.name
                    )
                })?;
            if let Some(sectors) = partition.sectors {
                let bytes = sectors
                    .checked_mul(DISK_SECTOR)
                    .ok_or("Partition size overflow")?;
                if member.size > bytes {
                    return Err(format!(
                        "Image '{}' is larger than its target partition",
                        member.name
                    ));
                }
            }
            (Some(partition.start), partition.sectors)
        };
        parts.push(Part {
            name: member.name.clone(),
            offset: member.offset,
            size: member.size,
            start_sector,
            sectors,
        });
    }
    if parts.is_empty() {
        return Err("RKAF contains no flashable payloads".into());
    }
    info.partitions = layout
        .iter()
        .map(|partition| FirmwarePartition {
            name: partition.name.clone(),
            size: partition
                .sectors
                .map(|n| n * DISK_SECTOR)
                .unwrap_or_else(|| {
                    parts
                        .iter()
                        .find(|p| p.name == partition.name)
                        .map_or(0, |p| p.size)
                }),
            start_sector: Some(partition.start),
        })
        .collect();
    info.warnings
        .push("Archive integrity is verified before flashing".into());
    Ok(Package {
        archive_offset,
        archive_length: length,
        loader_offset,
        loader_size,
        parts,
        parameter,
        members,
    })
}

#[allow(clippy::too_many_arguments)]
fn recover_layout(
    name: &str,
    mut position: u64,
    mut size: u64,
    padded: u64,
    position_exact: bool,
    size_exact: bool,
    previous_end: u64,
    known_layouts: &mut HashMap<(u64, u64, u64), u64>,
    length: u64,
) -> Result<(u64, u64), String> {
    if !size_exact {
        let allocation = padded
            .checked_mul(ARCHIVE_SECTOR)
            .ok_or("RKAF allocation overflow")?;
        if allocation < size {
            return Err(format!(
                "RKAF member '{name}' allocation is smaller than its size"
            ));
        }
        size += ((allocation - size) / U32_RANGE) * U32_RANGE;
        if size == 0 || allocation - size >= ARCHIVE_SECTOR {
            return Err(format!(
                "RKAF member '{name}' has an inconsistent large-file allocation"
            ));
        }
    }
    if !position_exact {
        let key = (position, size, padded);
        if let Some(previous) = known_layouts.get(&key) {
            // Retain upstream wrap reconstruction; the overlap check subsequently
            // rejects aliases rather than flashing a shared payload twice.
            position = *previous;
        } else {
            if position < previous_end {
                let wraps = (previous_end - position).div_ceil(U32_RANGE);
                position = position
                    .checked_add(
                        wraps
                            .checked_mul(U32_RANGE)
                            .ok_or("RKAF position overflow")?,
                    )
                    .ok_or("RKAF position overflow")?;
            }
            known_layouts.insert(key, position);
        }
    }
    if position % ARCHIVE_SECTOR != 0 {
        return Err(format!(
            "RKAF member '{name}' is not archive-sector aligned"
        ));
    }
    range_end(
        position,
        align_up(size, ARCHIVE_SECTOR)?,
        length,
        &format!("RKAF member '{name}'"),
    )?;
    Ok((position, size))
}

fn read_parameter(file: &mut File, file_size: u64, member: &Member) -> Result<String, String> {
    if member.size < 12 || member.size > MAX_PARAMETER_SIZE + 12 {
        return Err("Rockchip parameter member has an invalid size".into());
    }
    let payload = read_at(file, member.offset, member.size as usize, file_size)?;
    decode_parameter(&payload)
}

fn decode_parameter(payload: &[u8]) -> Result<String, String> {
    if payload.len() < 12 {
        return Err("Rockchip parameter member has an invalid size".into());
    }
    if !payload.starts_with(b"PARM") {
        return Err("Rockchip parameter member is missing its PARM header".into());
    }
    let text_len = le32(&payload, 4) as usize;
    if text_len != payload.len() - 12 {
        return Err("Rockchip parameter length does not match its member size".into());
    }
    let text = &payload[8..8 + text_len];
    if rkcrc32(0, text) != le32(&payload, 8 + text_len) {
        return Err("Rockchip parameter checksum mismatch".into());
    }
    let text = match text.iter().position(|b| *b == 0) {
        Some(end) => {
            if text[end..].iter().any(|b| *b != 0) {
                return Err("Rockchip parameter contains embedded NUL data".into());
            }
            &text[..end]
        }
        None => text,
    };
    if !text
        .iter()
        .all(|b| b.is_ascii_graphic() || matches!(*b, b' ' | b'\t' | b'\r' | b'\n'))
    {
        return Err("Rockchip parameter must contain ASCII text".into());
    }
    Ok(std::str::from_utf8(text)
        .map_err(|_| "Rockchip parameter is not ASCII")?
        .trim()
        .to_owned())
}

fn parse_parameter(parameter: &str, warnings: &mut Vec<String>) -> Result<Vec<Layout>, String> {
    parse_layout(parameter, warnings, false)
}

fn parse_layout(
    parameter: &str,
    warnings: &mut Vec<String>,
    legacy: bool,
) -> Result<Vec<Layout>, String> {
    let mut partition_type = None;
    let mut mtdparts = None;
    for line in parameter
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let field = if legacy {
            line.split_once([':', '='])
        } else {
            line.split_once(':')
        };
        if let Some((key, value)) = field {
            if key.trim().eq_ignore_ascii_case("TYPE") {
                if partition_type.replace(value.trim()).is_some() {
                    return Err("Parameter contains more than one TYPE declaration".into());
                }
            }
            if key.trim().eq_ignore_ascii_case("CMDLINE") {
                for token in value.split_ascii_whitespace() {
                    if let Some(value) = token.strip_prefix("mtdparts=") {
                        if mtdparts.replace(value).is_some() {
                            return Err("Parameter contains more than one mtdparts layout".into());
                        }
                    }
                }
            }
        }
    }
    let expected_type = if legacy { "MTD" } else { "GPT" };
    match partition_type {
        Some(kind) if !kind.eq_ignore_ascii_case(expected_type) => {
            return Err(format!(
                "Only {expected_type} parameter layouts are supported"
            ));
        }
        None if !legacy => warnings.push(
            "Parameter has no TYPE declaration; interpreting its mtdparts layout as GPT".into(),
        ),
        _ => (),
    }
    let definition = mtdparts.ok_or("Parameter has no CMDLINE mtdparts layout")?;
    let (device, definition) = definition
        .split_once(':')
        .ok_or("Invalid mtdparts device declaration")?;
    // Some GPT-only RKFW packages omit the mtdparts device name. Keep the
    // existing name requirement when GPT is only inferred.
    if (device.is_empty() && (legacy || partition_type.is_none()))
        || definition.is_empty()
        || definition.contains(';')
    {
        return Err("Exactly one nonempty mtdparts device layout is supported".into());
    }
    let entries: Vec<_> = definition.split(',').collect();
    if entries.is_empty() || entries.len() > 128 {
        return Err("GPT parameter must contain between 1 and 128 partitions".into());
    }
    let mut names = HashSet::new();
    let mut layouts = Vec::new();
    let mut previous_end = 34;
    for (index, entry) in entries.iter().enumerate() {
        let (extent, name) = entry
            .split_once('(')
            .ok_or("Invalid mtdparts partition syntax")?;
        let name = name
            .strip_suffix(')')
            .ok_or("Invalid mtdparts partition name")?;
        let (size, start) = extent
            .split_once('@')
            .ok_or("Each mtdparts partition must specify an address")?;
        let mut attributes = name.split(':');
        let name = attributes.next().unwrap_or("").trim();
        validate_name(name)?;
        let mut grow = size == "-";
        let mut seen_attributes = HashSet::new();
        for attribute in attributes {
            if !seen_attributes.insert(attribute) {
                return Err(format!("Duplicate parameter attribute on '{name}'"));
            }
            match attribute {
                "grow" if legacy && size != "-" => {
                    return Err(format!(
                        "Legacy grow partition '{name}' must use '-' for size"
                    ));
                }
                "grow" => grow = true,
                "bootable" => (),
                _ => {
                    return Err(format!(
                        "Unsupported parameter attribute '{attribute}' on '{name}'"
                    ));
                }
            }
        }
        let name_key = if legacy {
            name.to_ascii_lowercase()
        } else {
            name.to_owned()
        };
        if !names.insert(name_key) {
            return Err(format!("Duplicate parameter partition '{name}'"));
        }
        let start = parse_sector(start)?;
        let declared_size = if size == "-" {
            None
        } else {
            Some(parse_sector(size)?)
        };
        if declared_size == Some(0) || start < previous_end {
            return Err(format!(
                "Parameter partition '{name}' is empty, overlaps another partition or GPT metadata"
            ));
        }
        let sectors = if grow { None } else { declared_size };
        if grow && index + 1 != entries.len() {
            return Err("Only the final parameter partition may grow to fill the disk".into());
        }
        if let Some(sectors) = sectors {
            previous_end = start
                .checked_add(sectors)
                .ok_or("Parameter partition end overflow")?;
            sectors
                .checked_mul(DISK_SECTOR)
                .ok_or("Parameter partition byte size overflow")?;
        }
        start
            .checked_mul(DISK_SECTOR)
            .ok_or("Parameter partition byte offset overflow")?;
        layouts.push(Layout {
            name: name.to_owned(),
            start,
            sectors,
        });
    }
    Ok(layouts)
}

#[cfg(feature = "maintenance-cli")]
pub(crate) fn decode_legacy_sectors(sectors: &[u8]) -> Result<String, String> {
    if sectors.len() < 12 || !sectors.starts_with(b"PARM") {
        return Err("No legacy Rockchip PARM block at LBA 8192".into());
    }
    let end = (le32(sectors, 4) as usize)
        .checked_add(12)
        .ok_or("Legacy parameter length overflows")?;
    decode_parameter(
        sectors
            .get(..end)
            .ok_or("Legacy parameter block is truncated")?,
    )
}

#[cfg(feature = "maintenance-cli")]
pub(crate) fn legacy_partitions(
    parameter: &str,
    capacity: u64,
) -> Result<Vec<crate::model::PartitionInfo>, String> {
    parse_layout(parameter, &mut Vec::new(), true)?
        .into_iter()
        .map(|part| {
            let available = capacity
                .checked_sub(part.start)
                .filter(|&length| length != 0)
                .ok_or_else(|| {
                    format!("Legacy partition {} starts outside the device", part.name)
                })?;
            let sectors = part.sectors.unwrap_or(available);
            if sectors > available {
                return Err(format!(
                    "Legacy partition {} exceeds the device capacity",
                    part.name
                ));
            }
            Ok(crate::model::PartitionInfo {
                name: part.name,
                start_sector: part.start,
                sectors,
                size: sectors
                    .checked_mul(DISK_SECTOR)
                    .ok_or("Legacy partition byte size overflows")?,
            })
        })
        .collect()
}

#[cfg(feature = "maintenance-cli")]
pub(crate) fn encode_legacy_parameter(text: &[u8]) -> Result<Vec<u8>, String> {
    if text.is_empty() || text.len() > 512 * 512 - 12 || !text.is_ascii() || text.contains(&0) {
        return Err("Legacy parameter must be nonempty ASCII text fitting 512 sectors".into());
    }
    let mut block = b"PARM".to_vec();
    block.extend_from_slice(&(text.len() as u32).to_le_bytes());
    block.extend_from_slice(text);
    block.extend_from_slice(&rkcrc32(0, text).to_le_bytes());
    block.resize(block.len().next_multiple_of(512), 0);
    Ok(block)
}

fn parse_sector(value: &str) -> Result<u64, String> {
    let number = if let Some(digits) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!(
                "Invalid hexadecimal Rockchip sector value '{value}'"
            ));
        }
        u64::from_str_radix(digits, 16)
    } else {
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!("Invalid decimal Rockchip sector value '{value}'"));
        }
        value.parse()
    };
    number.map_err(|_| format!("Rockchip sector value '{value}' is out of range"))
}

fn inspect_loader(file: &mut File, file_size: u64) -> Result<(), String> {
    // BOOT/LDR tags and packed offsets match rockfile 0.1.2 src/boot.rs.
    let header = read_at(file, 0, 102, file_size)?;
    let header_size = u16::from_le_bytes([header[4], header[5]]) as u64;
    if header_size < 102 || header_size > file_size {
        return Err("Rockchip Loader header size is invalid".into());
    }
    let mut total_entries = 0;
    for descriptor in [25, 31, 37] {
        let count = header[descriptor] as usize;
        let table_offset = le32(&header, descriptor + 1) as u64;
        let record_size = header[descriptor + 5] as usize;
        if count == 0 {
            continue;
        }
        if record_size < 57 || table_offset < header_size {
            return Err("Rockchip Loader entry table is invalid".into());
        }
        let table = read_at(file, table_offset, count * record_size, file_size)?;
        for record in table.chunks_exact(record_size) {
            if record[0] as usize != record_size {
                return Err("Rockchip Loader entry size is inconsistent".into());
            }
            let offset = le32(record, 45) as u64;
            let size = le32(record, 49) as u64;
            if offset < header_size || size == 0 {
                return Err("Rockchip Loader payload offset or size is invalid".into());
            }
            range_end(offset, size, file_size, "Rockchip Loader payload")?;
            total_entries += 1;
        }
    }
    if total_entries == 0 {
        return Err("Rockchip Loader has no payload entries".into());
    }
    Ok(())
}

fn inspect_raw(
    file: &mut File,
    prefix: &[u8],
    file_size: u64,
    info: &mut FirmwareInfo,
) -> Result<(), String> {
    let has_gpt = prefix.get(512..520) == Some(&b"EFI PART"[..]);
    let has_volume = is_volume_boot_record(prefix);
    let has_mbr = prefix.get(510..512) == Some(&[0x55, 0xaa][..]) && !has_volume;
    if (has_gpt || has_mbr) && file_size % DISK_SECTOR != 0 {
        return Err("Raw disk images must contain a whole number of 512-byte sectors".into());
    }
    if file_size % DISK_SECTOR != 0 {
        info.warnings
            .push("The final partial 512-byte sector will be zero-padded when writing".into());
    }
    if file_size >= DISK_SECTOR
        && read_at(file, file_size - DISK_SECTOR, 8, file_size)?.starts_with(b"conectix")
    {
        return Err("VHD containers must be exported as raw disk images before flashing".into());
    }
    if has_gpt {
        inspect_gpt(file, file_size, info)?;
    } else if has_volume {
        info.warnings
            .push("Raw filesystem image: no disk partition table is included".into());
    } else if has_mbr {
        let mut ranges = Vec::new();
        let mut extended = false;
        for index in 0..4 {
            let base = 446 + index * 16;
            let kind = prefix[base + 4];
            let start = le32(prefix, base + 8) as u64;
            let sectors = le32(prefix, base + 12) as u64;
            if kind == 0 {
                if start != 0 || sectors != 0 {
                    return Err("Raw image has an inconsistent MBR partition entry".into());
                }
                continue;
            }
            if kind == 0xee {
                return Err(
                    "Raw image has a protective MBR but no valid primary GPT header".into(),
                );
            }
            if !matches!(prefix[base], 0 | 0x80) || start == 0 || sectors == 0 {
                return Err("Raw image has an invalid MBR partition entry".into());
            }
            let end = range_end(start, sectors, file_size / DISK_SECTOR, "MBR partition")?;
            let name = format!("partition{}", index + 1);
            ranges.push((start, end, name.clone()));
            extended |= matches!(kind, 0x05 | 0x0f | 0x85);
            info.partitions.push(FirmwarePartition {
                name,
                size: sectors * DISK_SECTOR,
                start_sector: Some(start),
            });
        }
        reject_overlaps(&mut ranges, "MBR partitions")?;
        if extended {
            info.warnings.push(
                "Extended MBR containers are shown; logical partitions are not enumerated".into(),
            );
        }
        if info.partitions.is_empty() {
            info.warnings
                .push("Raw image contains no MBR partitions".into());
        }
    } else {
        info.warnings.push(
            "No disk partition table detected; this file will be written as raw sectors".into(),
        );
    }
    Ok(())
}

fn is_volume_boot_record(bytes: &[u8]) -> bool {
    bytes.get(510..512) == Some(&[0x55, 0xaa][..])
        && matches!(bytes[0], 0xe9 | 0xeb)
        && (bytes.get(3..11) == Some(&b"EXFAT   "[..])
            || bytes.get(3..11) == Some(&b"NTFS    "[..])
            || bytes.get(54..62) == Some(&b"FAT12   "[..])
            || bytes.get(54..62) == Some(&b"FAT16   "[..])
            || bytes.get(82..90) == Some(&b"FAT32   "[..]))
}

fn inspect_gpt(file: &mut File, file_size: u64, info: &mut FirmwareInfo) -> Result<(), String> {
    let header = read_at(file, DISK_SECTOR, DISK_SECTOR as usize, file_size)?;
    let header_size = le32(&header, 12) as usize;
    if le32(&header, 8) != 0x0001_0000
        || !(92..=512).contains(&header_size)
        || le32(&header, 20) != 0
    {
        return Err("Raw image has an invalid GPT header".into());
    }
    let stored_crc = le32(&header, 16);
    let mut header_for_crc = header[..header_size].to_vec();
    header_for_crc[16..20].fill(0);
    if ieee_crc32(&header_for_crc) != stored_crc {
        return Err("Raw image GPT header checksum mismatch".into());
    }
    let total_sectors = file_size / DISK_SECTOR;
    let current = le64(&header, 24);
    let backup = le64(&header, 32);
    let first_usable = le64(&header, 40);
    let last_usable = le64(&header, 48);
    let table_sector = le64(&header, 72);
    let entries = le32(&header, 80) as u64;
    let entry_size = le32(&header, 84) as u64;
    if current != 1
        || backup <= current
        || backup >= total_sectors
        || first_usable < 3
        || last_usable < first_usable
        || last_usable >= backup
        || entries == 0
        || entries > 4096
        || entry_size < 128
        || entry_size > 4096
        || entry_size % 128 != 0
        || table_sector < 2
    {
        return Err("Raw image GPT geometry is invalid".into());
    }
    let table_size = entries
        .checked_mul(entry_size)
        .ok_or("GPT table size overflow")?;
    let table_offset = table_sector
        .checked_mul(DISK_SECTOR)
        .ok_or("GPT table offset overflow")?;
    let table_end = range_end(table_offset, table_size, file_size, "GPT partition array")?;
    if table_end
        > first_usable
            .checked_mul(DISK_SECTOR)
            .ok_or("GPT geometry overflow")?
    {
        return Err("GPT partition array overlaps the usable disk area".into());
    }
    let table = read_at(file, table_offset, table_size as usize, file_size)?;
    if ieee_crc32(&table) != le32(&header, 88) {
        return Err("Raw image GPT partition-array checksum mismatch".into());
    }
    let mut ranges = Vec::new();
    let mut ids = HashSet::new();
    for (index, entry) in table.chunks_exact(entry_size as usize).enumerate() {
        if entry[..16].iter().all(|b| *b == 0) {
            continue;
        }
        if entry[16..32].iter().all(|b| *b == 0) || !ids.insert(entry[16..32].to_vec()) {
            return Err("Raw image GPT contains an empty or duplicate partition identifier".into());
        }
        let start = le64(entry, 32);
        let end = le64(entry, 40);
        if start < first_usable || end < start || end > last_usable {
            return Err("Raw image GPT partition lies outside the usable disk area".into());
        }
        let name_units: Vec<_> = entry[56..128]
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .take_while(|n| *n != 0)
            .collect();
        let name = String::from_utf16(&name_units)
            .map_err(|_| "Raw image GPT has an invalid UTF-16 partition name")?;
        if name.chars().any(char::is_control) {
            return Err("Raw image GPT has control characters in a partition name".into());
        }
        let name = if name.trim().is_empty() {
            format!("partition{}", index + 1)
        } else {
            name.trim().to_owned()
        };
        ranges.push((start, end + 1, name.clone()));
        info.partitions.push(FirmwarePartition {
            name,
            size: (end - start + 1) * DISK_SECTOR,
            start_sector: Some(start),
        });
    }
    reject_overlaps(&mut ranges, "GPT partitions")?;
    if backup + 1 != total_sectors {
        info.warnings
            .push("The raw image contains trailing sectors after its GPT backup location".into());
    }
    Ok(())
}

pub(crate) fn reject_encoded_image(bytes: &[u8]) -> Result<(), String> {
    if bytes.starts_with(&[0x3a, 0xff, 0x26, 0xed]) {
        return Err("Android sparse images are not supported; convert to a raw image first".into());
    }
    if bytes.starts_with(&[0x1f, 0x8b])
        || bytes.starts_with(b"PK\x03\x04")
        || bytes.starts_with(b"PK\x05\x06")
        || bytes.starts_with(b"BZh")
        || bytes.starts_with(b"\xfd7zXZ\0")
        || bytes.starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
        || bytes.starts_with(&[0x04, 0x22, 0x4d, 0x18])
        || bytes.starts_with(&[0x02, 0x21, 0x4c, 0x18])
        || bytes.starts_with(b"7z\xbc\xaf\x27\x1c")
        || bytes.get(257..262) == Some(&b"ustar"[..])
    {
        return Err("Compressed archives must be extracted before flashing".into());
    }
    if bytes.starts_with(b"QFI\xfb")
        || bytes.starts_with(b"KDMV")
        || bytes.starts_with(b"# Disk DescriptorFile")
        || bytes.starts_with(b"sprs")
        || bytes.starts_with(b"vhdxfile")
        || bytes.starts_with(b"<<< Oracle VM VirtualBox Disk Image >>>")
    {
        return Err(
            "Virtual or sparse disk containers are not supported; export a raw disk image first"
                .into(),
        );
    }
    Ok(())
}

fn reject_overlaps(ranges: &mut [(u64, u64, String)], label: &str) -> Result<(), String> {
    ranges.sort_by_key(|r| r.0);
    for pair in ranges.windows(2) {
        if pair[0].1 > pair[1].0 {
            return Err(format!(
                "{label} '{}' and '{}' overlap",
                pair[0].2, pair[1].2
            ));
        }
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 32
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        || name == "."
        || name == ".."
    {
        return Err(format!("Invalid Rockchip partition name '{name}'"));
    }
    Ok(())
}

fn optional_metadata(bytes: &[u8], label: &str) -> Result<Option<String>, String> {
    let value = ascii_cstr(bytes, label)?;
    Ok(if value.is_empty() { None } else { Some(value) })
}

fn ascii_cstr(bytes: &[u8], label: &str) -> Result<String, String> {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    let bytes = &bytes[..end];
    if !bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        return Err(format!("{label} must contain printable ASCII"));
    }
    Ok(std::str::from_utf8(bytes)
        .map_err(|_| format!("{label} is not ASCII"))?
        .trim()
        .to_owned())
}

fn high_dword(bytes: &[u8], at: usize, marker: &[u8]) -> Option<u64> {
    (bytes.get(at..at + marker.len()) == Some(marker))
        .then(|| le32(bytes, at + marker.len()) as u64)
}

fn valid_tail(size: u64) -> bool {
    size == 0 || size == 32 || (SIGNED_TAIL_MIN..=SIGNED_TAIL_MAX).contains(&size)
}

fn range_end(offset: u64, length: u64, limit: u64, label: &str) -> Result<u64, String> {
    offset
        .checked_add(length)
        .filter(|end| *end <= limit)
        .ok_or_else(|| format!("{label} lies outside the file or its containing region"))
}

fn align_up(value: u64, alignment: u64) -> Result<u64, String> {
    value
        .checked_add(alignment - 1)
        .map(|n| n / alignment * alignment)
        .ok_or_else(|| "Aligned size overflow".into())
}

fn read_at(file: &mut File, offset: u64, length: usize, file_size: u64) -> Result<Vec<u8>, String> {
    range_end(offset, length as u64, file_size, "Firmware data")?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| format!("Cannot seek firmware: {e}"))?;
    let mut bytes = vec![0; length];
    file.read_exact(&mut bytes)
        .map_err(|e| format!("Cannot read firmware: {e}"))?;
    Ok(bytes)
}

fn le32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn le64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}

const fn rkcrc_table() -> [u32; 256] {
    let mut table = [0; 256];
    let mut index = 0;
    while index < 256 {
        let mut value = (index as u32) << 24;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 0x8000_0000 != 0 {
                (value << 1) ^ 0x04c1_0db7
            } else {
                value << 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

const RKCRC_TABLE: [u32; 256] = rkcrc_table();

fn rkcrc32(mut crc: u32, bytes: &[u8]) -> u32 {
    for byte in bytes {
        crc = (crc << 8) ^ RKCRC_TABLE[((crc >> 24) as u8 ^ *byte) as usize];
    }
    crc
}

fn ieee_crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Unpack validated RKFW/RKAF members into a fresh private child directory.
/// Never writes member paths or overwrites existing files. On error/cancel the
/// entire newly-owned directory is removed; successful files remain for the user.
pub fn unpack(
    path: &str,
    output_parent: &str,
    mut on_chunk: impl FnMut(u64) -> Result<(), String>,
) -> Result<String, String> {
    use std::io::Write;
    on_chunk(0)?;
    let canonical = Path::new(path).canonicalize().map_err(|e| e.to_string())?;
    let mut source = File::open(&canonical).map_err(|e| e.to_string())?;
    let stamp = source.metadata().map_err(|e| e.to_string())?;
    let image = inspect(canonical.to_str().ok_or("Firmware path is not UTF-8")?)?;
    let package = image
        .package
        .ok_or("Local unpack requires an RKFW or RKAF container")?;
    crate::archive::unchanged(
        &stamp,
        &std::fs::metadata(&canonical).map_err(|e| e.to_string())?,
    )?;
    let mut manifest: Vec<(String, u64, u64)> = package
        .members
        .iter()
        .map(|m| (format!("{}.img", m.name), m.offset, m.size))
        .collect();
    if let Some(offset) = package.loader_offset {
        manifest.push(("MiniLoader.bin".into(), offset, package.loader_size));
    }
    let mut total = 0u64;
    let mut names = HashSet::new();
    for (name, offset, size) in &manifest {
        if !names.insert(name.to_ascii_lowercase()) {
            return Err("Unpack output names collide".into());
        }
        range_end(*offset, *size, stamp.len(), "Unpack member")?;
        total = total
            .checked_add(*size)
            .filter(|n| *n <= crate::archive::MAX_EXTRACTED_BYTES)
            .ok_or("Unpack exceeds the 128 GiB output quota")?;
    }
    // Validate integrity before making any filesystem changes.
    verify_archive(&mut source, &package, &mut on_chunk)?;
    crate::archive::unchanged(&stamp, &source.metadata().map_err(|e| e.to_string())?)?;
    let parent = Path::new(output_parent)
        .canonicalize()
        .map_err(|e| format!("Cannot open output directory: {e}"))?;
    if !parent.is_dir() {
        return Err("Choose an existing output directory".into());
    }
    on_chunk(0)?;
    let directory = tempfile::Builder::new()
        .prefix("rk-unpacked-")
        .tempdir_in(parent)
        .map_err(|e| e.to_string())?;
    let mut buffer = vec![0; COPY_CHUNK];
    for (name, offset, size) in manifest {
        on_chunk(0)?;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.path().join(name))
            .map_err(|e| e.to_string())?;
        source
            .seek(SeekFrom::Start(offset))
            .map_err(|e| e.to_string())?;
        let mut remaining = size;
        while remaining > 0 {
            on_chunk(0)?;
            let n = remaining.min(buffer.len() as u64) as usize;
            source
                .read_exact(&mut buffer[..n])
                .map_err(|e| e.to_string())?;
            output.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
            remaining -= n as u64;
            on_chunk(n as u64)?;
        }
        output.sync_all().map_err(|e| e.to_string())?;
    }
    crate::archive::unchanged(&stamp, &source.metadata().map_err(|e| e.to_string())?)?;
    crate::archive::unchanged(
        &stamp,
        &std::fs::metadata(&canonical).map_err(|e| e.to_string())?,
    )?;
    on_chunk(0)?;
    let result = directory
        .path()
        .to_str()
        .ok_or("Output path is not UTF-8")?
        .to_owned();
    let _ = directory.keep();
    Ok(result)
}

#[cfg(test)]
mod unpack_tests {
    use super::*;

    #[test]
    fn anonymous_mtdparts_requires_explicit_gpt_and_a_valid_single_layout() {
        let definition = "CMDLINE:mtdparts=:0x10@0x40(rootfs),-@0x50(userdata:grow)";
        let layout = parse_parameter(&format!("TYPE: GPT\n{definition}"), &mut vec![]).unwrap();
        assert_eq!(layout.len(), 2);
        assert_eq!((layout[0].start, layout[0].sectors), (64, Some(16)));
        assert_eq!((layout[1].start, layout[1].sectors), (80, None));

        for parameter in [
            definition.to_owned(),
            format!("TYPE: MTD\n{definition}"),
            "TYPE: GPT\nCMDLINE:mtdparts=:".into(),
            "TYPE: GPT\nCMDLINE:mtdparts=:16@64(rootfs);rk:16@80(other)".into(),
            "TYPE: GPT\nCMDLINE:mtdparts=:16@64(rootfs),16@72(other)".into(),
            "TYPE: GPT\nCMDLINE:mtdparts=:-@64(rootfs:grow),16@80(other)".into(),
        ] {
            assert!(
                parse_parameter(&parameter, &mut vec![]).is_err(),
                "{parameter}"
            );
        }
    }

    fn fixture() -> Vec<u8> {
        let text = b"TYPE: GPT\nCMDLINE: mtdparts=rk:0x10@0x40(rootfs)\n";
        let mut parm = b"PARM".to_vec();
        parm.extend_from_slice(&(text.len() as u32).to_le_bytes());
        parm.extend_from_slice(text);
        parm.extend_from_slice(&rkcrc32(0, text).to_le_bytes());
        let mut bytes = vec![0; 6144];
        bytes[..4].copy_from_slice(b"RKAF");
        bytes[4..8].copy_from_slice(&6144u32.to_le_bytes());
        bytes[136..140].copy_from_slice(&2u32.to_le_bytes());
        for (i, (name, pos, data)) in [
            ("parameter", 2048, parm.as_slice()),
            ("rootfs", 4096, &[42u8; 512][..]),
        ]
        .iter()
        .enumerate()
        {
            let base = 140 + i * 112;
            bytes[base..base + name.len()].copy_from_slice(name.as_bytes());
            bytes[base + 32..base + 32 + name.len()].copy_from_slice(name.as_bytes());
            bytes[base + 96..base + 100].copy_from_slice(&(*pos as u32).to_le_bytes());
            bytes[base + 104..base + 108].copy_from_slice(&1u32.to_le_bytes());
            bytes[base + 108..base + 112].copy_from_slice(&(data.len() as u32).to_le_bytes());
            bytes[*pos..*pos + data.len()].copy_from_slice(data);
        }
        bytes.extend_from_slice(&rkcrc32(0, &bytes).to_le_bytes());
        bytes
    }
    #[test]
    fn unpack_streams_members_and_cleans_cancelled_output() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("update.img");
        std::fs::write(&source, fixture()).unwrap();
        let parent = dir.path().join("output");
        std::fs::create_dir(&parent).unwrap();
        let output = unpack(source.to_str().unwrap(), parent.to_str().unwrap(), |_| {
            Ok(())
        })
        .unwrap();
        assert_eq!(
            std::fs::read(Path::new(&output).join("rootfs.img")).unwrap(),
            vec![42; 512]
        );
        assert!(Path::new(&output).join("parameter.img").exists());
        std::fs::remove_dir_all(output).unwrap();
        let mut consumed = 0;
        assert!(
            unpack(source.to_str().unwrap(), parent.to_str().unwrap(), |n| {
                consumed += n;
                if consumed > 6144 {
                    Err("cancel".into())
                } else {
                    Ok(())
                }
            })
            .is_err()
        );
        assert_eq!(std::fs::read_dir(&parent).unwrap().count(), 0);
        let mut bad = fixture();
        bad[4096] ^= 1;
        std::fs::write(&source, bad).unwrap();
        assert!(
            unpack(source.to_str().unwrap(), parent.to_str().unwrap(), |_| Ok(
                ()
            ))
            .is_err()
        );
        assert_eq!(std::fs::read_dir(parent).unwrap().count(), 0);
    }
}
