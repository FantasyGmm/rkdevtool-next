//! MiniLoader download and eMMC/SD IDB preparation, with no USB side effects.
//!
//! Layout and wire-byte rules follow Rockchip's `rkdeveloptool`:
//! https://github.com/rockchip-linux/rkdeveloptool/blob/master/main.cpp
//! (`upgrade_loader`, `MakeIDBlockData`) and RKDevice.cpp (`DownloadBoot`).

use rockfile::boot::{RkBootEntry, RkBootHeader, RkBootHeaderEntry};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;

const HEADER_SIZE: usize = 102;
const ENTRY_SIZE: usize = 57;
const SECTOR_SIZE: usize = 512;
const ENTRY_ALIGN: usize = 2048;
const IDB_SECTOR: u64 = 64;
const MAX_LOADER_BYTES: u64 = 64 * 1024 * 1024;
// Reject unreasonable file-controlled waits before sending any RAM entry.
const MAX_RAM_ENTRY_DELAY_MS: u32 = 60_000;
// The supported eMMC/SD boot layout reserves LBA 64..8192 for the IDB.
const MAX_IDB_BYTES: usize = (8192 - IDB_SECTOR as usize) * SECTOR_SIZE;

pub struct LoaderBlob {
    pub area: u16,
    pub data: Vec<u8>,
    pub delay_ms: u32,
}

pub struct IdbWrite {
    pub sector: u64,
    pub data: Vec<u8>,
}

struct Loader {
    header: RkBootHeader,
    bytes: Vec<u8>,
    ram_471: Vec<RkBootEntry>,
    ram_472: Vec<RkBootEntry>,
    flash: Vec<RkBootEntry>,
}

/// Prepare 0x471 SRAM blobs followed by 0x472 DDR blobs in file order.
///
/// BOOT/LDR entries are already RC4-encoded by the loader packer. Even when
/// `rc4_flag` disables RC4 for *flash boot*, DownloadBoot sends the RAM entries
/// unchanged. `write_maskrom_area` must add the transfer CRC, not another RC4
/// layer. The caller observes every delay and waits for USB re-enumeration.
pub fn ram_blobs(file: &mut File, offset: u64, size: u64) -> Result<Vec<LoaderBlob>, String> {
    let loader = Loader::read(file, offset, size)?;
    let mut blobs = Vec::new();
    for (area, entries) in [(0x471, &loader.ram_471), (0x472, &loader.ram_472)] {
        for entry in entries {
            if entry.data_size != 0 {
                if entry.data_delay > MAX_RAM_ENTRY_DELAY_MS {
                    return Err(format!(
                        "MiniLoader RAM entry delay exceeds the supported {} ms limit",
                        MAX_RAM_ENTRY_DELAY_MS
                    ));
                }
                blobs.push(LoaderBlob {
                    area,
                    data: loader.data(entry).to_vec(),
                    delay_ms: entry.data_delay,
                });
            }
        }
    }
    if !blobs.iter().any(|blob| blob.area == 0x472) {
        return Err("MiniLoader has no nonempty 0x472 USB loader entry".into());
    }
    Ok(blobs)
}

/// Prepare the persistent IDB, not the BOOT/LDR container itself.
///
/// The caller must first confirm that the target is eMMC or SD, never NAND,
/// SPI flash, RAM, or an unknown medium. `new_idb` is the device capability bit,
/// not a request to convert a legacy loader. A FlashHead entry requires it.
/// Write the returned data with native WriteLBA (0x15, image subcode 0), then
/// read it back before resetting. This is the official `upgrade_loader` LBA
/// 64 layout; it does not synthesize NAND pages, ECC, or redundant IDB copies.
pub fn idb_writes(
    file: &mut File,
    offset: u64,
    size: u64,
    new_idb: bool,
) -> Result<Vec<IdbWrite>, String> {
    let loader = Loader::read(file, offset, size)?;
    for entry in &loader.flash {
        let name = entry_name(entry)?;
        if !["FlashData", "FlashBoot", "FlashHead"]
            .iter()
            .any(|known| name.eq_ignore_ascii_case(known))
        {
            return Err(format!(
                "Unsupported persistent loader entry {name:?}; only the eMMC/SD FlashData/FlashBoot layout is supported"
            ));
        }
    }

    let flash_data = loader.named("FlashData")?;
    let flash_boot = loader.named("FlashBoot")?;
    let flash_head = loader.optional_named("FlashHead")?;
    let mut data = loader.flash_data(flash_data)?;
    let mut boot = loader.flash_data(flash_boot)?;
    let data_aligned = aligned_len(data.len())?;
    let boot_aligned = aligned_len(boot.len())?;
    let rc4_disabled = loader.header.rc4_flag != 0;

    // The container stores loader blobs encoded in independent 512B RC4
    // sectors. A nonzero ucRc4Flag means the flash payload must be plaintext.
    if rc4_disabled {
        decode_sectors(&mut data);
        decode_sectors(&mut boot);
    }

    let idb = if let Some(head_entry) = flash_head {
        if !new_idb {
            return Err(
                "MiniLoader contains FlashHead, but the device does not support the new IDB format"
                    .into(),
            );
        }
        let mut head = loader.flash_data(head_entry)?;
        if rc4_disabled {
            decode_sectors(&mut head);
        }
        // FlashHead is opaque to the official upgrader. Check its decoded
        // envelope so unknown/signed variants do not become arbitrary writes.
        let mut plain_head = head.clone();
        if !rc4_disabled {
            decode_sectors(&mut plain_head);
        }
        validate_new_head(&plain_head)?;
        let head_aligned = aligned_len(head.len())?;
        let total = checked_idb_len(&[head_aligned, data_aligned, boot_aligned])?;
        let mut idb = vec![0; total];
        idb[..head.len()].copy_from_slice(&head);
        idb[head_aligned..head_aligned + data.len()].copy_from_slice(&data);
        idb[head_aligned + data_aligned..head_aligned + data_aligned + boot.len()]
            .copy_from_slice(&boot);
        idb
    } else {
        legacy_idb(&data, &boot, data_aligned, boot_aligned, rc4_disabled)?
    };

    Ok(vec![IdbWrite {
        sector: IDB_SECTOR,
        data: idb,
    }])
}

impl Loader {
    fn read(file: &mut File, offset: u64, size: u64) -> Result<Self, String> {
        if !(HEADER_SIZE as u64 + 4..=MAX_LOADER_BYTES).contains(&size) {
            return Err("MiniLoader size is outside the supported 106-byte to 64-MiB range".into());
        }
        let end = offset
            .checked_add(size)
            .ok_or("MiniLoader file range overflows")?;
        let file_len = file
            .metadata()
            .map_err(|e| format!("Read loader size: {e}"))?
            .len();
        if end > file_len {
            return Err("MiniLoader extends beyond the selected file".into());
        }
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| format!("Seek MiniLoader: {e}"))?;
        let mut bytes = vec![0; size as usize];
        file.read_exact(&mut bytes)
            .map_err(|e| format!("Read MiniLoader: {e}"))?;
        let header_bytes = bytes[..HEADER_SIZE]
            .try_into()
            .map_err(|_| "Truncated MiniLoader header")?;
        let header = RkBootHeader::from_bytes(header_bytes)
            .ok_or("MiniLoader must have a BOOT or LDR header")?;
        if header.size as usize != HEADER_SIZE {
            return Err(format!(
                "Unsupported MiniLoader header size {}",
                header.size
            ));
        }
        if header.sign_flag != 0 {
            return Err("Signed or secured MiniLoader containers are not supported".into());
        }
        if header.rc4_flag > 1 {
            return Err(format!(
                "Unsupported MiniLoader RC4 flag {}",
                header.rc4_flag
            ));
        }
        let payload_end = bytes.len() - 4;
        let expected = u32::from_le_bytes(bytes[payload_end..].try_into().unwrap());
        if rockchip_crc32(&bytes[..payload_end]) != expected {
            return Err("MiniLoader CRC32 mismatch; select an intact loader or update.img".into());
        }

        let tables = [
            table_range(&header.entry_471, payload_end)?,
            table_range(&header.entry_472, payload_end)?,
            table_range(&header.entry_loader, payload_end)?,
        ];
        for (index, first) in tables.iter().enumerate() {
            for second in &tables[index + 1..] {
                if overlaps(first, second) {
                    return Err("MiniLoader entry tables overlap".into());
                }
            }
        }
        let ram_471 = read_entries(&bytes, &tables, &header.entry_471, 1)?;
        let ram_472 = read_entries(&bytes, &tables, &header.entry_472, 2)?;
        let flash = read_entries(&bytes, &tables, &header.entry_loader, 4)?;
        Ok(Self {
            header,
            bytes,
            ram_471,
            ram_472,
            flash,
        })
    }

    fn data(&self, entry: &RkBootEntry) -> &[u8] {
        let start = entry.data_offset as usize;
        &self.bytes[start..start + entry.data_size as usize]
    }

    fn flash_data(&self, entry: &RkBootEntry) -> Result<Vec<u8>, String> {
        if entry.data_size == 0 || entry.data_size as usize % SECTOR_SIZE != 0 {
            return Err(format!(
                "{} must be nonempty and 512-byte aligned",
                entry_name(entry)?
            ));
        }
        Ok(self.data(entry).to_vec())
    }

    fn optional_named(&self, wanted: &str) -> Result<Option<&RkBootEntry>, String> {
        let mut found = None;
        for entry in &self.flash {
            if entry_name(entry)?.eq_ignore_ascii_case(wanted) {
                if found.is_some() {
                    return Err(format!("MiniLoader has duplicate {wanted} entries"));
                }
                found = Some(entry);
            }
        }
        Ok(found)
    }

    fn named(&self, wanted: &str) -> Result<&RkBootEntry, String> {
        self.optional_named(wanted)?
            .ok_or_else(|| format!("MiniLoader has no {wanted} persistent loader entry"))
    }
}

fn table_range(table: &RkBootHeaderEntry, payload_end: usize) -> Result<Range<usize>, String> {
    if table.count == 0 {
        return Ok(HEADER_SIZE..HEADER_SIZE);
    }
    if table.size as usize != ENTRY_SIZE {
        return Err(format!("Unsupported MiniLoader entry size {}", table.size));
    }
    let start = table.offset as usize;
    let end = start
        .checked_add(table.count as usize * ENTRY_SIZE)
        .ok_or("MiniLoader table range overflows")?;
    if start < HEADER_SIZE || end > payload_end {
        return Err("MiniLoader entry table is outside its payload".into());
    }
    Ok(start..end)
}

fn read_entries(
    bytes: &[u8],
    tables: &[Range<usize>; 3],
    table: &RkBootHeaderEntry,
    expected_type: u32,
) -> Result<Vec<RkBootEntry>, String> {
    let mut entries = Vec::new();
    for index in 0..table.count as usize {
        let start = table.offset as usize + index * ENTRY_SIZE;
        let encoded = bytes[start..start + ENTRY_SIZE]
            .try_into()
            .map_err(|_| "Truncated MiniLoader entry")?;
        let entry = RkBootEntry::from_bytes(encoded);
        if entry.size as usize != ENTRY_SIZE || entry.type_ != expected_type {
            return Err("MiniLoader entry has an unsupported size or type".into());
        }
        let start = entry.data_offset as usize;
        let end = start
            .checked_add(entry.data_size as usize)
            .ok_or("MiniLoader data range overflows")?;
        let range = start..end;
        if start < HEADER_SIZE
            || end > bytes.len() - 4
            || tables.iter().any(|table| overlaps(&range, table))
        {
            return Err("MiniLoader entry data is outside its payload or overlaps metadata".into());
        }
        entries.push(entry);
    }
    Ok(entries)
}

fn entry_name(entry: &RkBootEntry) -> Result<String, String> {
    let end = entry
        .name
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(entry.name.len());
    String::from_utf16(&entry.name[..end])
        .map_err(|_| "MiniLoader entry name is not valid UTF-16".into())
}

fn overlaps(first: &Range<usize>, second: &Range<usize>) -> bool {
    first.start < first.end
        && second.start < second.end
        && first.start < second.end
        && second.start < first.end
}

fn aligned_len(len: usize) -> Result<usize, String> {
    len.checked_add(ENTRY_ALIGN - 1)
        .map(|value| value / ENTRY_ALIGN * ENTRY_ALIGN)
        .ok_or_else(|| "IDB aligned length overflows".into())
}

fn checked_idb_len(parts: &[usize]) -> Result<usize, String> {
    let len = parts
        .iter()
        .try_fold(0usize, |total, part| total.checked_add(*part))
        .ok_or("IDB length overflows")?;
    if len > MAX_IDB_BYTES {
        return Err("IDB exceeds the supported eMMC/SD boot region before LBA 8192".into());
    }
    Ok(len)
}

fn validate_new_head(head: &[u8]) -> Result<(), String> {
    // U-Boot tools/rkcommon.c documents the unsigned v2 RKNS envelope.
    // Other magic/flag forms require secure-boot-specific handling.
    if head.len() < ENTRY_ALIGN || &head[..4] != b"RKNS" {
        return Err("Unsupported FlashHead format; an unsigned RKNS IDB header is required".into());
    }
    let flags = u32::from_le_bytes(head[12..16].try_into().unwrap());
    if flags > 2 {
        return Err("Signed, secured, or unknown RKNS IDB flags are not supported".into());
    }
    Ok(())
}

fn legacy_idb(
    data: &[u8],
    boot: &[u8],
    data_aligned: usize,
    boot_aligned: usize,
    rc4_disabled: bool,
) -> Result<Vec<u8>, String> {
    let total = checked_idb_len(&[ENTRY_ALIGN, data_aligned, boot_aligned])?;
    let data_sectors =
        u16::try_from(data_aligned / SECTOR_SIZE).map_err(|_| "FlashData is too large")?;
    let code_sectors = u16::try_from((data_aligned + boot_aligned) / SECTOR_SIZE)
        .map_err(|_| "Persistent loader is too large")?;
    let mut idb = vec![0; total];
    put_u32(&mut idb, 0, 0x0ff0aa55);
    put_u32(&mut idb, 8, u32::from(rc4_disabled));
    put_u16(&mut idb, 12, 4);
    put_u16(&mut idb, 14, 4);
    put_u16(&mut idb, 506, data_sectors);
    put_u16(&mut idb, 508, code_sectors);

    put_u16(&mut idb, SECTOR_SIZE, 0x0c);
    put_u16(&mut idb, SECTOR_SIZE + 2, 0xffff);
    put_u32(&mut idb, SECTOR_SIZE + 10, 0x38324b52);
    idb[ENTRY_ALIGN..ENTRY_ALIGN + data.len()].copy_from_slice(data);
    idb[ENTRY_ALIGN + data_aligned..ENTRY_ALIGN + data_aligned + boot.len()].copy_from_slice(boot);

    let sec0_crc = crc16(&idb[..SECTOR_SIZE]);
    let sec1_crc = crc16(&idb[SECTOR_SIZE..SECTOR_SIZE * 2]);
    let sec3_crc = crc16(&idb[SECTOR_SIZE * 3..ENTRY_ALIGN]);
    let code_crc = rockchip_crc32(&idb[ENTRY_ALIGN..]);
    let sec2 = SECTOR_SIZE * 2;
    idb[sec2 + 491..sec2 + 493].copy_from_slice(b"VC");
    put_u16(&mut idb, sec2 + 494, sec0_crc);
    put_u16(&mut idb, sec2 + 496, sec1_crc);
    put_u32(&mut idb, sec2 + 498, code_crc);
    idb[sec2 + 506..sec2 + 509].copy_from_slice(b"CRC");
    put_u16(&mut idb, sec2 + 510, sec3_crc);
    // Header sectors 0, 2, and 3 are RC4 encoded regardless of payload flag.
    for sector in [0, 2, 3] {
        rc4(&mut idb[sector * SECTOR_SIZE..(sector + 1) * SECTOR_SIZE]);
    }
    Ok(idb)
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn decode_sectors(bytes: &mut [u8]) {
    for sector in bytes.chunks_exact_mut(SECTOR_SIZE) {
        rc4(sector);
    }
}

fn rc4(bytes: &mut [u8]) {
    const KEY: [u8; 16] = [124, 78, 3, 4, 85, 5, 9, 7, 45, 44, 123, 56, 23, 13, 23, 17];
    let mut state = [0u8; 256];
    for (index, value) in state.iter_mut().enumerate() {
        *value = index as u8;
    }
    let mut j = 0usize;
    for i in 0..256 {
        j = (j + state[i] as usize + KEY[i % KEY.len()] as usize) & 255;
        state.swap(i, j);
    }
    let mut i = 0usize;
    j = 0;
    for byte in bytes {
        i = (i + 1) & 255;
        j = (j + state[i] as usize) & 255;
        state.swap(i, j);
        *byte ^= state[(state[i] as usize + state[j] as usize) & 255];
    }
}

fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0u16;
    for byte in bytes {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn rockchip_crc32(bytes: &[u8]) -> u32 {
    // Rockchip CRC_32 is MSB-first, init=0, xorout=0, with the vendor
    // polynomial 0x04c10db7 (not the usual Ethernet polynomial 0x04c11db7).
    let mut table = [0u32; 256];
    for (index, entry) in table.iter_mut().enumerate() {
        let mut value = (index as u32) << 24;
        for _ in 0..8 {
            value = if value & 0x80000000 != 0 {
                (value << 1) ^ 0x04c10db7
            } else {
                value << 1
            };
        }
        *entry = value;
    }
    bytes.iter().fold(0u32, |crc, byte| {
        (crc << 8) ^ table[((crc >> 24) as u8 ^ *byte) as usize]
    })
}
