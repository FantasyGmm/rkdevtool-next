//! 512-byte-sector GPT handling and Rockchip parameter-file conversion.

use std::collections::{BTreeMap, HashSet};

use uuid::Uuid;

use crate::model::PartitionInfo;

const SECTOR_SIZE: usize = 512;
const HEADER_SIZE: usize = 92;
const ENTRY_SIZE: usize = 128;
const ENTRY_COUNT: usize = 128;
const TABLE_SIZE: usize = ENTRY_SIZE * ENTRY_COUNT;
const TABLE_SECTORS: u64 = (TABLE_SIZE / SECTOR_SIZE) as u64;
const FIRST_USABLE: u64 = 34;
// U-Boot's default partition type, also used for raw Rockchip boot partitions.
const BASIC_DATA_GUID: [u8; 16] = [
    0xa2, 0xa0, 0xd0, 0xeb, 0xe5, 0xb9, 0x33, 0x44, 0x87, 0xc0, 0x68, 0xb6, 0xb7, 0x26, 0x99, 0xc7,
];

pub struct GptImage {
    /// Protective MBR, primary header, and entry array: LBAs 0 through 33.
    pub primary: Vec<u8>,
    /// Entry array and backup header: the final 33 sectors of the device.
    pub backup: Vec<u8>,
    pub partitions: Vec<PartitionInfo>,
}

struct Header {
    table_lba: u64,
    table_bytes: usize,
    entry_size: usize,
    first_usable: u64,
    last_usable: u64,
    table_crc: u32,
}

/// Validates the primary header read from LBA 1 and returns its entry array's
/// starting LBA and exact byte length, not sector count.
/// A device read may round the length up to whole sectors; CRC32 excludes padding.
pub fn entry_table_spec(header: &[u8], capacity_sectors: u64) -> Result<(u64, usize), String> {
    let header = validate_header(header, capacity_sectors)?;
    Ok((header.table_lba, header.table_bytes))
}

pub fn parse(
    header: &[u8],
    entries: &[u8],
    capacity_sectors: u64,
) -> Result<Vec<PartitionInfo>, String> {
    let header = validate_header(header, capacity_sectors)?;
    let entries = entries
        .get(..header.table_bytes)
        .ok_or("GPT partition entry array is truncated")?;
    if crc32(entries) != header.table_crc {
        return Err("GPT partition entry array CRC32 does not match".into());
    }

    let mut partitions = Vec::new();
    let mut names = HashSet::new();
    let mut identifiers = HashSet::new();
    for (index, entry) in entries.chunks_exact(header.entry_size).enumerate() {
        if entry[..16].iter().all(|byte| *byte == 0) {
            continue;
        }
        let identifier: [u8; 16] = entry[16..32].try_into().unwrap();
        if identifier == [0; 16] || !identifiers.insert(identifier) {
            return Err(format!(
                "GPT partition {} has a zero or duplicate UUID",
                index + 1
            ));
        }
        let start = read_u64(entry, 32);
        let end = read_u64(entry, 40);
        if start < header.first_usable || end > header.last_usable || start > end {
            return Err(format!(
                "GPT partition {} is outside the usable disk range",
                index + 1
            ));
        }
        let encoded_name: Vec<u16> = entry[56..128]
            .chunks_exact(2)
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
            .take_while(|unit| *unit != 0)
            .collect();
        let name = String::from_utf16(&encoded_name)
            .map_err(|_| format!("GPT partition {} has an invalid UTF-16 name", index + 1))?;
        if name.is_empty() || name.chars().any(char::is_control) || !names.insert(name.clone()) {
            return Err(format!(
                "GPT partition {} has an empty, invalid, or duplicate name",
                index + 1
            ));
        }
        let sectors = end - start + 1;
        let size = sectors
            .checked_mul(SECTOR_SIZE as u64)
            .ok_or_else(|| format!("GPT partition {name} byte size overflows"))?;
        partitions.push(PartitionInfo {
            name,
            start_sector: start,
            sectors,
            size,
        });
    }
    validate_no_overlap(&partitions)?;
    Ok(partitions)
}

fn validate_header(bytes: &[u8], capacity: u64) -> Result<Header, String> {
    if capacity < 68 {
        return Err("Device is too small for a 512-byte-sector GPT".into());
    }
    let bytes = bytes
        .get(..SECTOR_SIZE)
        .ok_or("GPT header sector is truncated")?;
    if &bytes[..8] != b"EFI PART" {
        return Err("No GPT signature at the requested header".into());
    }
    if read_u32(bytes, 8) != 0x0001_0000 {
        return Err("Unsupported GPT header revision".into());
    }
    let header_size = read_u32(bytes, 12) as usize;
    if !(HEADER_SIZE..=SECTOR_SIZE).contains(&header_size) {
        return Err("Invalid GPT header size".into());
    }
    if read_u32(bytes, 20) != 0 || bytes[header_size..].iter().any(|byte| *byte != 0) {
        return Err("GPT header reserved bytes are nonzero".into());
    }
    let mut checked = bytes[..header_size].to_vec();
    checked[16..20].fill(0);
    if crc32(&checked) != read_u32(bytes, 16) {
        return Err("GPT header CRC32 does not match".into());
    }
    let current_lba = read_u64(bytes, 24);
    let alternate_lba = read_u64(bytes, 32);
    if current_lba != 1 || alternate_lba != capacity - 1 {
        return Err("Primary GPT header locations do not match the device capacity".into());
    }
    if bytes[56..72].iter().all(|byte| *byte == 0) {
        return Err("GPT disk UUID is zero".into());
    }
    let count = read_u32(bytes, 80) as usize;
    let entry_size = read_u32(bytes, 84) as usize;
    if !(1..=4096).contains(&count)
        || !(ENTRY_SIZE..=1024).contains(&entry_size)
        || !entry_size.is_power_of_two()
    {
        return Err(
            "Unsupported GPT entry array dimensions (maximum 4096 entries, 128–1024 bytes each)"
                .into(),
        );
    }
    let table_bytes = count * entry_size;
    let table_sectors = (table_bytes as u64 + SECTOR_SIZE as u64 - 1) / SECTOR_SIZE as u64;
    let reserved_sectors = table_sectors.max(TABLE_SECTORS);
    let first_usable = read_u64(bytes, 40);
    let last_usable = read_u64(bytes, 48);
    let minimum_first = reserved_sectors + 2;
    let maximum_last = capacity
        .checked_sub(reserved_sectors + 2)
        .ok_or("GPT entry array does not fit on this device")?;
    if first_usable < minimum_first || last_usable > maximum_last || first_usable > last_usable {
        return Err("GPT usable range overlaps its metadata or exceeds the device capacity".into());
    }
    let table_lba = read_u64(bytes, 72);
    let table_end = table_lba
        .checked_add(table_sectors)
        .ok_or("GPT entry array location overflows")?;
    if table_lba < 2 || table_end > first_usable {
        return Err(
            "GPT partition entry array overlaps a header, partition space, or disk boundary".into(),
        );
    }
    Ok(Header {
        table_lba,
        table_bytes,
        entry_size,
        first_usable,
        last_usable,
        table_crc: read_u32(bytes, 88),
    })
}

/// Bounded reads needed to verify both GPT copies. A smaller image is accepted
/// only by `parse_verified`, never by the primary-only legacy parser.
pub struct ReadPlan {
    pub table_lba: u64,
    pub table_bytes: usize,
    pub backup_header_lba: u64,
    /// Informational only: no metadata or partition is automatically rewritten.
    pub backup_is_not_at_device_end: bool,
}

pub fn read_plan(primary: &[u8], capacity: u64) -> Result<ReadPlan, String> {
    let (header, declared_capacity) = validate_embedded_primary(primary, capacity)?;
    Ok(ReadPlan {
        table_lba: header.table_lba,
        table_bytes: header.table_bytes,
        backup_header_lba: declared_capacity - 1,
        backup_is_not_at_device_end: declared_capacity != capacity,
    })
}

fn validate_embedded_primary(primary: &[u8], capacity: u64) -> Result<(Header, u64), String> {
    let primary = primary
        .get(..SECTOR_SIZE)
        .ok_or("GPT header sector is truncated")?;
    let declared_capacity = read_u64(primary, 32)
        .checked_add(1)
        .ok_or("GPT backup header location overflows")?;
    if declared_capacity > capacity {
        return Err("GPT backup header is beyond the physical device".into());
    }
    // Use the image's declared end as the stricter boundary for metadata and
    // partitions, not the larger physical medium's end.
    Ok((
        validate_header(primary, declared_capacity)?,
        declared_capacity,
    ))
}

/// Verify the backup header before permitting any reads from its entry array.
pub fn backup_table_spec(
    primary: &[u8],
    backup: &[u8],
    capacity: u64,
) -> Result<(u64, usize), String> {
    let (header, declared_capacity) = validate_embedded_primary(primary, capacity)?;
    let backup = backup
        .get(..SECTOR_SIZE)
        .ok_or("Backup GPT header sector is truncated")?;
    let header_size = read_u32(backup, 12) as usize;
    if &backup[..8] != b"EFI PART"
        || read_u32(backup, 8) != 0x0001_0000
        || !(HEADER_SIZE..=SECTOR_SIZE).contains(&header_size)
    {
        return Err("Invalid backup GPT signature, revision, or header size".into());
    }
    if read_u32(backup, 20) != 0 || backup[header_size..].iter().any(|byte| *byte != 0) {
        return Err("Backup GPT header reserved bytes are nonzero".into());
    }
    let mut checked = backup[..header_size].to_vec();
    checked[16..20].fill(0);
    if crc32(&checked) != read_u32(backup, 16) {
        return Err("Backup GPT header CRC32 does not match".into());
    }
    if read_u64(backup, 24) != declared_capacity - 1 || read_u64(backup, 32) != 1 {
        return Err("Backup GPT header locations contradict the primary header".into());
    }
    // Compare usable range, disk GUID, dimensions, table checksum, and any
    // header extension. Current/alternate/table LBAs and header CRC differ.
    if backup[8..16] != primary[8..16]
        || backup[40..72] != primary[40..72]
        || backup[80..header_size] != primary[80..header_size]
    {
        return Err("Primary and backup GPT headers disagree".into());
    }
    let table_lba = read_u64(backup, 72);
    let sectors = (header.table_bytes as u64).div_ceil(SECTOR_SIZE as u64);
    let table_end = table_lba
        .checked_add(sectors)
        .ok_or("Backup GPT entry array location overflows")?;
    if table_lba <= header.last_usable || table_end > declared_capacity - 1 {
        return Err("Backup GPT entry array overlaps usable space or its header".into());
    }
    Ok((table_lba, header.table_bytes))
}

/// Read-only compatibility with RAW GPT images copied onto larger media.
/// Both copies must be valid and identical; this does not repair, relocate,
/// expand, or otherwise modify anything on disk.
pub fn parse_verified(
    primary: &[u8],
    entries: &[u8],
    backup: &[u8],
    backup_entries: &[u8],
    capacity: u64,
) -> Result<Vec<PartitionInfo>, String> {
    let (_, bytes) = backup_table_spec(primary, backup, capacity)?;
    let (_, declared_capacity) = validate_embedded_primary(primary, capacity)?;
    let partitions = parse(primary, entries, declared_capacity)?;
    let entries = entries
        .get(..bytes)
        .ok_or("GPT partition entry array is truncated")?;
    let backup_entries = backup_entries
        .get(..bytes)
        .ok_or("Backup GPT partition entry array is truncated")?;
    if crc32(backup_entries) != read_u32(backup, 88) {
        return Err("Backup GPT partition entry array CRC32 does not match".into());
    }
    if entries != backup_entries {
        return Err("Primary and backup GPT entry arrays disagree".into());
    }
    Ok(partitions)
}

struct ParameterPartition {
    name: String,
    start: u64,
    sectors: Option<u64>,
    bootable: bool,
}

pub fn build(parameter: &str, capacity_sectors: u64) -> Result<GptImage, String> {
    if capacity_sectors < 68 {
        return Err("Device is too small for a 512-byte-sector GPT".into());
    }
    if parameter.contains('\0') {
        return Err("Parameter file contains a NUL byte".into());
    }
    let (definitions, mut uuids) = parse_parameter(parameter)?;
    let last_usable = capacity_sectors - 34;
    let mut partitions = Vec::new();
    let mut entries = vec![0_u8; TABLE_SIZE];
    let mut names = HashSet::new();
    let mut identifiers = HashSet::new();
    for (index, definition) in definitions.iter().enumerate() {
        if !names.insert(definition.name.clone()) {
            return Err(format!("Duplicate partition name: {}", definition.name));
        }
        if definition.start < FIRST_USABLE || definition.start > last_usable {
            return Err(format!(
                "Partition {} starts outside the usable disk range",
                definition.name
            ));
        }
        let sectors = match definition.sectors {
            Some(sectors) => sectors,
            None if index + 1 == definitions.len() => last_usable - definition.start + 1,
            None => return Err("Only the final partition may grow to the end of the device".into()),
        };
        let end = definition
            .start
            .checked_add(sectors - 1)
            .ok_or_else(|| format!("Partition {} end sector overflows", definition.name))?;
        if end > last_usable {
            return Err(format!(
                "Partition {} extends into the backup GPT or beyond the device",
                definition.name
            ));
        }
        let size = sectors
            .checked_mul(SECTOR_SIZE as u64)
            .ok_or_else(|| format!("Partition {} byte size overflows", definition.name))?;
        let identifier = uuids
            .remove(&definition.name)
            .unwrap_or_else(Uuid::new_v4)
            .to_bytes_le();
        if !identifiers.insert(identifier) {
            return Err(format!(
                "Partition {} has a duplicate UUID",
                definition.name
            ));
        }
        let entry = &mut entries[index * ENTRY_SIZE..(index + 1) * ENTRY_SIZE];
        entry[..16].copy_from_slice(&BASIC_DATA_GUID);
        entry[16..32].copy_from_slice(&identifier);
        write_u64(entry, 32, definition.start);
        write_u64(entry, 40, end);
        write_u64(entry, 48, if definition.bootable { 1 << 2 } else { 0 });
        for (index, unit) in definition.name.encode_utf16().enumerate() {
            entry[56 + index * 2..58 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        partitions.push(PartitionInfo {
            name: definition.name.clone(),
            start_sector: definition.start,
            sectors,
            size,
        });
    }
    if let Some(name) = uuids.keys().next() {
        return Err(format!(
            "UUID directive refers to an unknown partition: {name}"
        ));
    }
    validate_no_overlap(&partitions)?;

    let disk_guid = Uuid::new_v4().to_bytes_le();
    let table_crc = crc32(&entries);
    let primary_header = build_header(
        1,
        capacity_sectors - 1,
        2,
        last_usable,
        &disk_guid,
        table_crc,
    );
    let backup_header = build_header(
        capacity_sectors - 1,
        1,
        capacity_sectors - 33,
        last_usable,
        &disk_guid,
        table_crc,
    );
    let mut primary = vec![0_u8; 34 * SECTOR_SIZE];
    // Protective MBR: one 0xEE partition spanning all addressable sectors after LBA 0.
    primary[447..450].copy_from_slice(&[0, 2, 0]);
    primary[450] = 0xee;
    primary[451..454].fill(0xff);
    write_u32(&mut primary, 454, 1);
    write_u32(
        &mut primary,
        458,
        (capacity_sectors - 1).min(u32::MAX as u64) as u32,
    );
    primary[510..512].copy_from_slice(&[0x55, 0xaa]);
    primary[SECTOR_SIZE..2 * SECTOR_SIZE].copy_from_slice(&primary_header);
    primary[2 * SECTOR_SIZE..].copy_from_slice(&entries);
    let mut backup = entries;
    backup.extend_from_slice(&backup_header);
    Ok(GptImage {
        primary,
        backup,
        partitions,
    })
}

fn parse_parameter(
    parameter: &str,
) -> Result<(Vec<ParameterPartition>, BTreeMap<String, Uuid>), String> {
    let mut definitions = None;
    let mut uuids = BTreeMap::new();
    let mut saw_type = false;
    for raw_line in parameter.trim_start_matches('\u{feff}').lines() {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(separator) = line.find([':', '=']) {
            let key = line[..separator].trim();
            let value = line[separator + 1..].trim();
            if key.eq_ignore_ascii_case("type") {
                if saw_type || !value.eq_ignore_ascii_case("gpt") {
                    return Err("Parameter TYPE must be GPT and may appear only once".into());
                }
                saw_type = true;
                continue;
            }
            if key.eq_ignore_ascii_case("uuid") {
                if line.as_bytes()[separator] != b':' {
                    return Err("Use uuid:<partition-name>=<UUID> for partition UUIDs".into());
                }
                let (name, identifier) = value
                    .split_once('=')
                    .ok_or("UUID directive must use uuid:<partition-name>=<UUID>")?;
                let name = name.trim();
                validate_name(name)?;
                let identifier = Uuid::parse_str(identifier.trim())
                    .map_err(|_| format!("Invalid UUID for partition {name}"))?;
                if identifier.is_nil() || uuids.insert(name.to_string(), identifier).is_some() {
                    return Err(format!(
                        "Zero UUID or duplicate UUID directive for partition {name}"
                    ));
                }
                continue;
            }
            let normalized_key = key.to_ascii_lowercase();
            if normalized_key.contains("uuid") || normalized_key.contains("guid") {
                return Err(format!("Unsupported UUID directive: {key}"));
            }
        }
        let lower = line.to_ascii_lowercase();
        if lower.contains("uuid:") || lower.contains("uuid_disk=") {
            return Err(
                "UUID directives must use a separate uuid:<partition-name>=<UUID> line".into(),
            );
        }
        if let Some(position) = lower.find("mtdparts=") {
            if definitions.is_some() || lower[position + 9..].contains("mtdparts=") {
                return Err("Parameter must contain exactly one mtdparts definition".into());
            }
            if position > 0 {
                let before = line.as_bytes()[position - 1];
                if !before.is_ascii_whitespace() && before != b':' {
                    return Err("Invalid mtdparts command-line token".into());
                }
            }
            definitions = Some(&line[position + 9..]);
        }
    }
    Ok((
        parse_mtdparts(
            definitions.ok_or("Parameter has no mtdparts definition")?,
            saw_type,
        )?,
        uuids,
    ))
}

fn parse_mtdparts(value: &str, explicit_gpt: bool) -> Result<Vec<ParameterPartition>, String> {
    let (device, mut remaining) = value
        .trim_start()
        .split_once(':')
        .ok_or("mtdparts must name a device followed by a colon")?;
    if (device.is_empty() && !(explicit_gpt && value.starts_with(':')))
        || device.chars().any(|ch| ch.is_whitespace() || ch == ';')
    {
        return Err("Invalid mtdparts device name".into());
    }
    let mut result = Vec::new();
    loop {
        remaining = remaining.trim_start();
        let close = remaining
            .find(')')
            .ok_or("Partition definition has no closing parenthesis")?;
        let definition = &remaining[..close];
        let (range, name_flags) = definition
            .split_once('(')
            .ok_or("Partition definition must use size@offset(name)")?;
        let (size, start) = range
            .split_once('@')
            .ok_or("Partition definition must use size@offset(name)")?;
        let start = parse_sector_count(start.trim())?;
        let mut flags = name_flags.split(':');
        let name = flags.next().unwrap_or("").trim();
        validate_name(name)?;
        let mut grow = false;
        let mut bootable = false;
        for flag in flags {
            match flag.trim() {
                "grow" if !grow => grow = true,
                "bootable" if !bootable => bootable = true,
                flag => return Err(format!("Unsupported or duplicate partition flag: {flag}")),
            }
        }
        let sectors = if size.trim() == "-" {
            None
        } else {
            let sectors = parse_sector_count(size.trim())?;
            if sectors == 0 {
                return Err(format!("Partition {name} has zero size"));
            }
            if grow { None } else { Some(sectors) }
        };
        result.push(ParameterPartition {
            name: name.into(),
            start,
            sectors,
            bootable,
        });
        if result.len() > ENTRY_COUNT {
            return Err("GPT supports at most 128 partitions".into());
        }
        let tail = &remaining[close + 1..];
        let trimmed = tail.trim_start();
        if let Some(next) = trimmed.strip_prefix(',') {
            remaining = next;
        } else if trimmed.is_empty() {
            break;
        } else if trimmed.starts_with(';') || tail.len() == trimmed.len() {
            return Err("Unsupported trailing mtdparts syntax or multiple devices".into());
        } else {
            // mtdparts is one kernel command-line argument; remaining arguments are unrelated.
            break;
        }
    }
    Ok(result)
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.encode_utf16().count() > 36
        || name
            .chars()
            .any(|ch| ch.is_control() || matches!(ch, ':' | ',' | ';' | '(' | ')' | '@'))
    {
        return Err(format!(
            "Invalid GPT partition name: {name:?} (maximum 36 UTF-16 code units)"
        ));
    }
    Ok(())
}

fn parse_sector_count(value: &str) -> Result<u64, String> {
    let number = if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        if hex.is_empty() || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!("Invalid hexadecimal sector count: {value}"));
        }
        u64::from_str_radix(hex, 16)
    } else {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(format!("Invalid decimal sector count: {value}"));
        }
        value.parse()
    };
    number.map_err(|_| format!("Sector count exceeds 64 bits: {value}"))
}

fn validate_no_overlap(partitions: &[PartitionInfo]) -> Result<(), String> {
    let mut ordered: Vec<_> = partitions.iter().collect();
    ordered.sort_unstable_by_key(|partition| partition.start_sector);
    for pair in ordered.windows(2) {
        let end = pair[0]
            .start_sector
            .checked_add(pair[0].sectors - 1)
            .ok_or("Partition end sector overflows")?;
        if end >= pair[1].start_sector {
            return Err(format!(
                "Partitions {} and {} overlap",
                pair[0].name, pair[1].name
            ));
        }
    }
    Ok(())
}

fn build_header(
    current: u64,
    alternate: u64,
    table_lba: u64,
    last_usable: u64,
    disk_guid: &[u8; 16],
    table_crc: u32,
) -> [u8; SECTOR_SIZE] {
    let mut header = [0_u8; SECTOR_SIZE];
    header[..8].copy_from_slice(b"EFI PART");
    write_u32(&mut header, 8, 0x0001_0000);
    write_u32(&mut header, 12, HEADER_SIZE as u32);
    write_u64(&mut header, 24, current);
    write_u64(&mut header, 32, alternate);
    write_u64(&mut header, 40, FIRST_USABLE);
    write_u64(&mut header, 48, last_usable);
    header[56..72].copy_from_slice(disk_guid);
    write_u64(&mut header, 72, table_lba);
    write_u32(&mut header, 80, ENTRY_COUNT as u32);
    write_u32(&mut header, 84, ENTRY_SIZE as u32);
    write_u32(&mut header, 88, table_crc);
    let checksum = crc32(&header[..HEADER_SIZE]);
    write_u32(&mut header, 16, checksum);
    header
}

/// IEEE CRC32, as used by GPT headers and partition entry arrays.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in data {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0_u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod verified_tests {
    use super::*;

    #[test]
    fn anonymous_mtdparts_builds_only_an_explicit_valid_gpt() {
        let definition = "CMDLINE:mtdparts=:0x10@0x40(rootfs),-@0x50(userdata:grow)";
        let image = build(&format!("{definition}\nTYPE: GPT"), 512).unwrap();
        assert_eq!(image.partitions.len(), 2);
        assert_eq!(
            (
                image.partitions[0].start_sector,
                image.partitions[0].sectors
            ),
            (64, 16)
        );
        assert_eq!(
            (
                image.partitions[1].start_sector,
                image.partitions[1].sectors
            ),
            (80, 399)
        );

        for parameter in [
            definition.to_owned(),
            format!("TYPE: MTD\n{definition}"),
            "TYPE: GPT\nCMDLINE:mtdparts=:".into(),
            "TYPE: GPT\nCMDLINE:mtdparts=:16@64(rootfs);rk:16@80(other)".into(),
            "TYPE: GPT\nCMDLINE:mtdparts=:16@64(rootfs),16@72(other)".into(),
            "TYPE: GPT\nCMDLINE:mtdparts=:-@64(rootfs:grow),16@80(other)".into(),
        ] {
            assert!(build(&parameter, 512).is_err(), "{parameter}");
        }
        assert!(build(&format!("TYPE: GPT\n{definition}"), 112).is_err());
    }

    fn fixture() -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
        let image = build(
            "TYPE: GPT\nCMDLINE: mtdparts=rk:64@34(boot),64@98(root)",
            512,
        )
        .unwrap();
        (
            image.primary[512..1024].to_vec(),
            image.primary[1024..].to_vec(),
            image.backup[TABLE_SIZE..].to_vec(),
            image.backup[..TABLE_SIZE].to_vec(),
        )
    }

    fn checksum(header: &mut [u8]) {
        write_u32(header, 16, 0);
        let crc = crc32(&header[..HEADER_SIZE]);
        write_u32(header, 16, crc);
    }

    fn tables_checksum(primary: &mut [u8], backup: &mut [u8], entries: &[u8]) {
        let crc = crc32(entries);
        write_u32(primary, 88, crc);
        write_u32(backup, 88, crc);
        checksum(primary);
        checksum(backup);
    }

    #[test]
    fn accepts_verified_smaller_image_without_expanding_partitions() {
        let (h, e, b, be) = fixture();
        let plan = read_plan(&h, 1024).unwrap();
        assert!(plan.backup_is_not_at_device_end);
        assert_eq!(plan.backup_header_lba, 511);
        assert_eq!(backup_table_spec(&h, &b, 1024).unwrap(), (479, TABLE_SIZE));
        let partitions = parse_verified(&h, &e, &b, &be, 1024).unwrap();
        assert_eq!(partitions.len(), 2);
        assert_eq!(partitions[1].start_sector, 98);
        assert_eq!(partitions[1].sectors, 64);
        assert!(!read_plan(&h, 512).unwrap().backup_is_not_at_device_end);
        assert!(
            parse(&h, &e, 1024).is_err(),
            "Primary-only API must remain strict"
        );
    }

    #[test]
    fn rejects_corrupt_headers_and_entry_arrays() {
        let (h, e, b, be) = fixture();
        let mut bad = h.clone();
        bad[56] ^= 1;
        assert!(read_plan(&bad, 1024).is_err());
        let mut bad = b.clone();
        bad[56] ^= 1;
        assert!(parse_verified(&h, &e, &bad, &be, 1024).is_err());
        let mut bad = e.clone();
        bad[0] ^= 1;
        assert!(parse_verified(&h, &bad, &b, &be, 1024).is_err());
        let mut bad = be.clone();
        bad[0] ^= 1;
        assert!(parse_verified(&h, &e, &b, &bad, 1024).is_err());
    }

    #[test]
    fn rejects_correctly_checksummed_contradictory_backup() {
        let (h, e, mut b, be) = fixture();
        write_u64(&mut b, 32, 2);
        checksum(&mut b);
        assert!(parse_verified(&h, &e, &b, &be, 1024).is_err());
        let (_, _, mut b, _) = fixture();
        b[56] ^= 1;
        checksum(&mut b);
        assert!(parse_verified(&h, &e, &b, &be, 1024).is_err());
    }

    #[test]
    fn rejects_backup_array_overlaps_and_out_of_bounds() {
        let (h, _, b, _) = fixture();
        for table_lba in [34, 478, 500, 1024, u64::MAX] {
            let mut bad = b.clone();
            write_u64(&mut bad, 72, table_lba);
            checksum(&mut bad);
            assert!(backup_table_spec(&h, &bad, 1024).is_err());
        }
        assert!(read_plan(&h, 511).is_err());
        let mut bad = h.clone();
        write_u64(&mut bad, 32, u64::MAX);
        checksum(&mut bad);
        assert!(read_plan(&bad, 1024).is_err());
    }

    #[test]
    fn rejects_partition_overlap_and_bounds_even_with_valid_crcs() {
        let (h, e, b, _) = fixture();
        for (start, end) in [(97, 161), (478, 479), (600, 601), (33, 34), (100, 99)] {
            let (mut h, mut e, mut b) = (h.clone(), e.clone(), b.clone());
            write_u64(&mut e[ENTRY_SIZE..], 32, start);
            write_u64(&mut e[ENTRY_SIZE..], 40, end);
            tables_checksum(&mut h, &mut b, &e);
            assert!(parse_verified(&h, &e, &b, &e, 1024).is_err());
        }
    }

    #[test]
    fn rejects_declared_usable_space_past_embedded_backup() {
        let (mut h, _, _, _) = fixture();
        write_u64(&mut h, 48, 700);
        checksum(&mut h);
        assert!(read_plan(&h, 1024).is_err());
    }
}
