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

pub const DEVICE_PRIMARY_SECTORS: u64 = FIRST_USABLE;
pub const DEVICE_PRIMARY_BYTES: usize = DEVICE_PRIMARY_SECTORS as usize * SECTOR_SIZE;

/// rkdeveloptool's device commands read the first 34 sectors. Loader access
/// to that area does not imply access to the disk tail or a valid backup GPT.
pub fn parse_device_primary(
    metadata: &[u8; DEVICE_PRIMARY_BYTES],
) -> Result<Option<Vec<PartitionInfo>>, String> {
    let header = &metadata[SECTOR_SIZE..2 * SECTOR_SIZE];
    if !header.starts_with(b"EFI PART") {
        return Ok(None);
    }
    let count = read_u32(header, 80) as usize;
    let entry_size = read_u32(header, 84) as usize;
    if count == 0
        || count > ENTRY_COUNT
        || !(ENTRY_SIZE..=1024).contains(&entry_size)
        || !entry_size.is_power_of_two()
        || count * entry_size > TABLE_SIZE
        || read_u64(header, 72) != 2
    {
        return Err("Primary GPT entry array is not within the 34-sector Rockchip layout".into());
    }
    let mut partitions = Vec::new();
    for entry in metadata[2 * SECTOR_SIZE..]
        .chunks_exact(entry_size)
        .take(count)
    {
        if entry.iter().all(|&byte| byte == 0) {
            continue;
        }
        let start = read_u64(entry, 32);
        let end = read_u64(entry, 40);
        let sectors = end
            .checked_sub(start)
            .and_then(|len| len.checked_add(1))
            .ok_or("Invalid partition range in primary GPT")?;
        let size = sectors
            .checked_mul(SECTOR_SIZE as u64)
            .ok_or("Primary GPT partition size overflows")?;
        let name: Vec<_> = entry[56..128]
            .chunks_exact(2)
            .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
            .take_while(|&unit| unit != 0)
            .collect();
        let name = String::from_utf16(&name).map_err(|_| "Invalid UTF-16 name in primary GPT")?;
        if name.is_empty() || name.chars().any(char::is_control) {
            return Err("Empty or control-character name in primary GPT".into());
        }
        partitions.push(PartitionInfo {
            name,
            start_sector: start,
            sectors,
            size,
        });
    }
    Ok(Some(partitions))
}

/// Rebuild a Rockchip 67-sector metadata image for the actual target capacity.
/// The source primary must be internally valid; the supplied backup is replaced.
#[cfg(feature = "maintenance-cli")]
pub fn resize_image(bytes: &[u8], capacity: u64) -> Result<GptImage, String> {
    if bytes.len() != DEVICE_PRIMARY_BYTES + TABLE_SIZE + SECTOR_SIZE {
        return Err("Expected a 67-sector Rockchip GPT metadata image".into());
    }
    if capacity < 2 * FIRST_USABLE {
        return Err("Device is too small for a 512-byte-sector GPT".into());
    }
    let mut primary = bytes[..DEVICE_PRIMARY_BYTES].to_vec();
    let (mbr, metadata) = primary.split_at_mut(SECTOR_SIZE);
    let (header, entries) = metadata.split_at_mut(SECTOR_SIZE);
    let old_capacity = read_u64(header, 32)
        .checked_add(1)
        .ok_or("GPT backup header location overflows")?;
    let old = validate_header(header, old_capacity)?;
    let mbr_length = read_u32(mbr, 458);
    if old.table_lba != 2
        || old.table_bytes > TABLE_SIZE
        || mbr[510..512] != [0x55, 0xaa]
        || mbr[450] != 0xee
        || read_u32(mbr, 454) != 1
        || !(mbr_length == u32::MAX || u64::from(mbr_length) == old_capacity - 1)
        || mbr[462..510].iter().any(|&byte| byte != 0)
    {
        return Err("GPT image has a nonstandard protective MBR or entry array".into());
    }
    parse(header, entries, old_capacity)?;

    let last_usable = capacity - FIRST_USABLE;
    // Like rkdeveloptool, a last entry ending immediately before the old
    // backup GPT is the grow partition. Other entries keep their exact bounds.
    if let Some(last) = entries[..old.table_bytes]
        .chunks_exact_mut(old.entry_size)
        .rfind(|entry| entry[..16].iter().any(|&byte| byte != 0))
    {
        if read_u64(last, 40) == old_capacity - FIRST_USABLE {
            write_u64(last, 40, last_usable);
        }
    }
    write_u32(mbr, 458, (capacity - 1).min(u64::from(u32::MAX)) as u32);
    write_u64(header, 32, capacity - 1);
    write_u64(header, 48, last_usable);
    write_u32(header, 88, crc32(&entries[..old.table_bytes]));
    update_header_checksum(header);
    let partitions = parse(header, entries, capacity)?;

    let mut backup = entries.to_vec();
    let mut backup_header = header.to_vec();
    write_u64(&mut backup_header, 24, capacity - 1);
    write_u64(&mut backup_header, 32, 1);
    write_u64(&mut backup_header, 72, capacity - TABLE_SECTORS - 1);
    update_header_checksum(&mut backup_header);
    backup.extend_from_slice(&backup_header);
    Ok(GptImage {
        primary,
        backup,
        partitions,
    })
}

#[cfg(feature = "maintenance-cli")]
fn update_header_checksum(header: &mut [u8]) {
    let size = read_u32(header, 12) as usize;
    write_u32(header, 16, 0);
    let checksum = crc32(&header[..size]);
    write_u32(header, 16, checksum);
}

#[cfg(any(test, feature = "maintenance-cli"))]
struct Header {
    #[cfg(feature = "maintenance-cli")]
    table_lba: u64,
    table_bytes: usize,
    entry_size: usize,
    first_usable: u64,
    last_usable: u64,
    table_crc: u32,
}

#[cfg(any(test, feature = "maintenance-cli"))]
fn parse(
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

#[cfg(any(test, feature = "maintenance-cli"))]
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
        #[cfg(feature = "maintenance-cli")]
        table_lba,
        table_bytes,
        entry_size,
        first_usable,
        last_usable,
        table_crc: read_u32(bytes, 88),
    })
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
mod tests {
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

    fn fixture() -> (Vec<u8>, Vec<u8>) {
        let image = build(
            "TYPE: GPT\nCMDLINE: mtdparts=rk:64@34(boot),64@98(root)",
            512,
        )
        .unwrap();
        (
            image.primary[512..1024].to_vec(),
            image.primary[1024..].to_vec(),
        )
    }

    fn checksum(header: &mut [u8]) {
        write_u32(header, 16, 0);
        let crc = crc32(&header[..HEADER_SIZE]);
        write_u32(header, 16, crc);
    }

    fn table_checksum(primary: &mut [u8], entries: &[u8]) {
        let crc = crc32(entries);
        write_u32(primary, 88, crc);
        checksum(primary);
    }

    #[test]
    fn accepts_valid_primary_at_its_declared_capacity() {
        let (h, e) = fixture();
        let partitions = parse(&h, &e, 512).unwrap();
        assert_eq!(partitions.len(), 2);
        assert_eq!(partitions[1].start_sector, 98);
        assert_eq!(partitions[1].sectors, 64);
        assert!(
            parse(&h, &e, 1024).is_err(),
            "Local primary GPT validation must remain strict"
        );
        assert!(parse(&h, &e, 511).is_err());
    }

    #[test]
    fn rejects_corrupt_headers_and_entry_arrays() {
        let (h, e) = fixture();
        let mut bad = h.clone();
        bad[56] ^= 1;
        assert!(parse(&bad, &e, 512).is_err());
        let mut bad = e.clone();
        bad[0] ^= 1;
        assert!(parse(&h, &bad, 512).is_err());
    }

    #[test]
    fn rejects_partition_overlap_and_bounds_even_with_valid_crcs() {
        let (h, e) = fixture();
        for (start, end) in [(97, 161), (478, 479), (600, 601), (33, 34), (100, 99)] {
            let (mut h, mut e) = (h.clone(), e.clone());
            write_u64(&mut e[ENTRY_SIZE..], 32, start);
            write_u64(&mut e[ENTRY_SIZE..], 40, end);
            table_checksum(&mut h, &e);
            assert!(parse(&h, &e, 512).is_err());
        }
    }

    #[test]
    fn rejects_declared_usable_space_past_embedded_backup() {
        let (mut h, e) = fixture();
        write_u64(&mut h, 48, 700);
        checksum(&mut h);
        assert!(parse(&h, &e, 512).is_err());
    }
}
