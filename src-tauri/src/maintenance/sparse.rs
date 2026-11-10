//! Android sparse v1 decoder. Expand into a scratch file before a device write.
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
};

pub(super) const MAGIC: [u8; 4] = [0x3a, 0xff, 0x26, 0xed];

pub(super) fn expand(input: &mut File, file_size: u64, limit: u64) -> Result<(File, u64), String> {
    input.seek(SeekFrom::Start(0)).map_err(io)?;
    let mut header = [0; 28];
    input.read_exact(&mut header).map_err(io)?;
    let file_header = u64::from(le16(&header, 8));
    let chunk_header = u64::from(le16(&header, 10));
    let block = u64::from(le32(&header, 12));
    let blocks = u64::from(le32(&header, 16));
    let chunks = u64::from(le32(&header, 20));
    let checksum = le32(&header, 24);
    if header[..4] != MAGIC
        || le16(&header, 4) != 1
        || !(28..=file_size).contains(&file_header)
        || chunk_header < 12
        || block == 0
        || block % 512 != 0
        || blocks == 0
        || chunks == 0
        || chunks > (file_size - file_header) / chunk_header
    {
        return Err("Invalid Android sparse header".into());
    }
    let expanded = block
        .checked_mul(blocks)
        .ok_or("Sparse expanded length overflows")?;
    if expanded > limit {
        return Err("Sparse expanded image exceeds the target range".into());
    }

    let mut output = tempfile::tempfile().map_err(io)?;
    let mut crc = crc32fast::Hasher::new();
    let mut buffer = vec![0; 128 * 1024];
    let mut file_pos = file_header;
    let mut logical = 0_u64;
    for _ in 0..chunks {
        input.seek(SeekFrom::Start(file_pos)).map_err(io)?;
        let mut raw = [0; 12];
        input.read_exact(&mut raw).map_err(io)?;
        let kind = le16(&raw, 0);
        let chunk_blocks = u64::from(le32(&raw, 4));
        let total = u64::from(le32(&raw, 8));
        let bytes = chunk_blocks
            .checked_mul(block)
            .ok_or("Sparse chunk length overflows")?;
        let payload = match kind {
            0xcac1 if bytes != 0 => bytes,
            0xcac2 if bytes != 0 => 4,
            0xcac3 if bytes != 0 => 0,
            0xcac4 if bytes == 0 => 4,
            _ => return Err("Unsupported or empty Android sparse chunk".into()),
        };
        if chunk_header.checked_add(payload) != Some(total) {
            return Err("Invalid Android sparse chunk size".into());
        }
        let data_pos = file_pos
            .checked_add(chunk_header)
            .ok_or("Sparse header overflows")?;
        file_pos = file_pos
            .checked_add(total)
            .filter(|&pos| pos <= file_size)
            .ok_or("Android sparse chunk extends past end of file")?;
        logical = logical
            .checked_add(bytes)
            .filter(|&pos| pos <= expanded)
            .ok_or("Android sparse chunks exceed declared expanded size")?;
        input.seek(SeekFrom::Start(data_pos)).map_err(io)?;

        if kind == 0xcac4 {
            let mut expected = [0; 4];
            input.read_exact(&mut expected).map_err(io)?;
            if crc.clone().finalize() != u32::from_le_bytes(expected) {
                return Err("Android sparse CRC32 chunk does not match".into());
            }
            continue;
        }
        if kind == 0xcac2 {
            let mut pattern = [0; 4];
            input.read_exact(&mut pattern).map_err(io)?;
            for bytes in buffer.chunks_exact_mut(4) {
                bytes.copy_from_slice(&pattern);
            }
        } else if kind == 0xcac3 {
            buffer.fill(0);
        }
        let mut remaining = bytes;
        while remaining != 0 {
            let count = remaining.min(buffer.len() as u64) as usize;
            let data = &mut buffer[..count];
            if kind == 0xcac1 {
                input.read_exact(data).map_err(io)?;
            }
            crc.update(data);
            if kind != 0xcac3 {
                output.write_all(data).map_err(io)?;
            }
            remaining -= count as u64;
        }
        if kind == 0xcac3 {
            output.seek(SeekFrom::Start(logical)).map_err(io)?;
        }
    }
    if logical != expanded || file_pos != file_size {
        return Err("Android sparse image has missing blocks or trailing bytes".into());
    }
    if checksum != 0 && crc.finalize() != checksum {
        return Err("Android sparse image checksum does not match".into());
    }
    output.set_len(expanded).map_err(io)?;
    output.rewind().map_err(io)?;
    Ok((output, expanded))
}

fn io(error: std::io::Error) -> String {
    format!("Decode Android sparse: {error}")
}

fn le16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn le32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
