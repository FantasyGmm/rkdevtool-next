//! Native, bounded extraction. Archive names are metadata, never output paths.
use serde::Serialize;
use std::{
    collections::HashSet,
    fs::{File, Metadata, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

pub const MAX_EXTRACTED_BYTES: u64 = 128 * 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveInfo {
    pub path: String,
    pub format: String,
    pub entries: Vec<ArchiveEntry>,
}
#[derive(Debug, Serialize)]
pub struct ArchiveEntry {
    pub name: String,
    pub size: Option<u64>,
}
pub struct ExtractedImage {
    _directory: tempfile::TempDir,
    path: PathBuf,
}
impl ExtractedImage {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn err(e: impl std::fmt::Display) -> String {
    format!("Archive: {e}")
}
fn open(path: &str) -> Result<(File, Metadata, PathBuf), String> {
    let path = Path::new(path).canonicalize().map_err(err)?;
    let file = File::open(&path).map_err(err)?;
    let meta = file.metadata().map_err(err)?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_EXTRACTED_BYTES {
        return Err("Archive must be a nonempty regular file no larger than 128 GiB".into());
    }
    Ok((file, meta, path))
}
pub(crate) fn unchanged(before: &Metadata, after: &Metadata) -> Result<(), String> {
    let mut same = before.len() == after.len() && before.modified().ok() == after.modified().ok();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        same &= before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec();
    }
    if same {
        Ok(())
    } else {
        Err("Source file changed during processing; select it again".into())
    }
}
fn format(file: &mut File) -> Result<&'static str, String> {
    let mut b = [0; 8];
    let n = file.read(&mut b).map_err(err)?;
    file.seek(SeekFrom::Start(0)).map_err(err)?;
    let b = &b[..n];
    if b.starts_with(b"PK\x03\x04") || b.starts_with(b"PK\x05\x06") {
        Ok("zip")
    } else if b.starts_with(&[31, 139]) {
        Ok("gzip")
    } else if b.starts_with(b"\xfd7zXZ\0") {
        Ok("xz")
    } else if b.starts_with(b"BZh") {
        Ok("bzip2")
    } else {
        Err("Unsupported archive format (native support: ZIP, gzip, xz, bzip2; 7z/zstd/tar are not supported)".into())
    }
}
fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 1024
        && !name.starts_with('/')
        && !name.contains('\\')
        && !name.contains(':')
        && !name.chars().any(char::is_control)
        && name
            .trim_end_matches('/')
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..")
}
fn image_name(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".img")
}
// Bound the central directory before the ZIP crate allocates its index.
fn zip_index_limits(file: &mut File) -> Result<(), String> {
    let length = file.metadata().map_err(err)?.len();
    let tail_len = length.min(65557) as usize;
    file.seek(SeekFrom::Start(length - tail_len as u64))
        .map_err(err)?;
    let mut tail = vec![0; tail_len];
    file.read_exact(&mut tail).map_err(err)?;
    let at = (0..tail_len.saturating_sub(21))
        .rev()
        .find(|&i| {
            tail.get(i..i + 4) == Some(b"PK\x05\x06")
                && i + 22 + u16::from_le_bytes([tail[i + 20], tail[i + 21]]) as usize == tail_len
        })
        .ok_or("Invalid ZIP end record")?;
    let u16at = |i| u16::from_le_bytes([tail[i], tail[i + 1]]);
    let u32at = |i| u32::from_le_bytes(tail[i..i + 4].try_into().unwrap());
    if u16at(at + 4) != 0 || u16at(at + 6) != 0 {
        return Err("Multi-disk ZIP is not supported".into());
    }
    let mut count = u16at(at + 10) as u64;
    let mut size = u32at(at + 12) as u64;
    if count == 65535 || size == u32::MAX as u64 || u32at(at + 16) == u32::MAX {
        let absolute = length - tail_len as u64 + at as u64;
        let locator_offset = absolute.checked_sub(20).ok_or("Invalid ZIP64 locator")?;
        file.seek(SeekFrom::Start(locator_offset)).map_err(err)?;
        let mut loc = [0; 20];
        file.read_exact(&mut loc).map_err(err)?;
        if &loc[..4] != b"PK\x06\x07" || loc[4..8] != [0; 4] || loc[16..20] != [1, 0, 0, 0] {
            return Err("Invalid or multi-disk ZIP64".into());
        }
        let offset = u64::from_le_bytes(loc[8..16].try_into().unwrap());
        if offset
            .checked_add(56)
            .is_none_or(|end| end > locator_offset)
        {
            return Err("Invalid ZIP64 index bounds".into());
        }
        file.seek(SeekFrom::Start(offset)).map_err(err)?;
        let mut record = [0; 56];
        file.read_exact(&mut record).map_err(err)?;
        if &record[..4] != b"PK\x06\x06" || record[16..24] != [0; 8] {
            return Err("Invalid ZIP64 end record".into());
        }
        count = u64::from_le_bytes(record[32..40].try_into().unwrap());
        size = u64::from_le_bytes(record[40..48].try_into().unwrap());
    }
    if count > MAX_ENTRIES as u64 || size > 16 * 1024 * 1024 {
        return Err("ZIP directory exceeds the 4096-member / 16 MiB metadata limit".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(err)?;
    Ok(())
}
fn inspect_open(file: &mut File, path: &Path) -> Result<ArchiveInfo, String> {
    let kind = format(file)?;
    let entries = if kind == "zip" {
        zip_index_limits(file)?;
        let mut zip = zip::ZipArchive::new(file).map_err(err)?;
        if zip.len() > MAX_ENTRIES {
            return Err("ZIP contains too many members (limit 4096)".into());
        }
        let mut entries = Vec::new();
        let mut names = HashSet::new();
        for i in 0..zip.len() {
            let member = zip.by_index(i).map_err(err)?;
            if !safe_name(member.name()) || !names.insert(member.name().to_owned()) {
                return Err("ZIP contains an unsafe or duplicate member name".into());
            }
            if let Some(mode) = member.unix_mode() {
                let kind = mode & 0o170000;
                if kind != 0 && kind != 0o100000 && kind != 0o040000 {
                    return Err("ZIP links and special files are not supported".into());
                }
            }
            if member.size() > MAX_EXTRACTED_BYTES {
                return Err("ZIP member exceeds the 128 GiB extraction limit".into());
            }
            if !member.is_dir() && image_name(member.name()) {
                entries.push(ArchiveEntry {
                    name: member.name().into(),
                    size: Some(member.size()),
                });
            }
        }
        entries
    } else {
        let name = path
            .file_stem()
            .and_then(|v| v.to_str())
            .ok_or("Invalid archive filename")?;
        if !safe_name(name) || !image_name(name) {
            return Err(
                "Single-stream archives must be named *.img.gz, *.img.xz or *.img.bz2".into(),
            );
        }
        vec![ArchiveEntry {
            name: name.into(),
            size: None,
        }]
    };
    if entries.is_empty() {
        return Err("Archive contains no selectable .img firmware".into());
    }
    Ok(ArchiveInfo {
        path: path.to_str().ok_or("Archive path is not UTF-8")?.into(),
        format: kind.into(),
        entries,
    })
}
pub fn inspect_archive(path: &str) -> Result<ArchiveInfo, String> {
    let (mut file, before, canonical) = open(path)?;
    let info = inspect_open(&mut file, &canonical)?;
    unchanged(&before, &file.metadata().map_err(err)?)?;
    Ok(info)
}
fn copy_bounded(
    reader: &mut impl Read,
    output: &mut File,
    limit: u64,
    callback: &mut impl FnMut(u64) -> Result<(), String>,
) -> Result<u64, String> {
    let mut buf = vec![0; 1024 * 1024];
    let mut total = 0u64;
    loop {
        callback(0)?;
        let n = reader.read(&mut buf).map_err(err)?;
        if n == 0 {
            break;
        }
        total = total
            .checked_add(n as u64)
            .filter(|n| *n <= limit)
            .ok_or("Expanded image exceeds device capacity or extraction quota")?;
        output.write_all(&buf[..n]).map_err(err)?;
        callback(n as u64)?;
    }
    if total == 0 {
        return Err("Extracted image is empty".into());
    }
    Ok(total)
}
pub fn extract_selected(
    path: &str,
    member: &str,
    max_bytes: u64,
    mut on_chunk: impl FnMut(u64) -> Result<(), String>,
) -> Result<ExtractedImage, String> {
    on_chunk(0)?;
    let limit = max_bytes.min(MAX_EXTRACTED_BYTES);
    if limit == 0 {
        return Err("Extraction quota is zero".into());
    }
    let (mut file, before, canonical) = open(path)?;
    let info = inspect_open(&mut file, &canonical)?;
    let selected = info
        .entries
        .iter()
        .find(|e| e.name == member)
        .ok_or("Selected archive member no longer exists")?;
    if selected.size.is_some_and(|n| n > limit) {
        return Err("Selected image exceeds device capacity or extraction quota".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(err)?;
    let directory = tempfile::Builder::new()
        .prefix("rkdevtool-next-image-")
        .tempdir()
        .map_err(err)?;
    let target = directory.path().join("firmware.img");
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
        .map_err(err)?;
    match info.format.as_str() {
        "zip" => {
            let mut zip = zip::ZipArchive::new(&mut file).map_err(err)?;
            let mut input = zip.by_name(member).map_err(err)?;
            let actual = copy_bounded(&mut input, &mut output, limit, &mut on_chunk)?;
            if Some(actual) != selected.size {
                return Err("ZIP member size mismatch".into());
            }
        }
        "gzip" => {
            copy_bounded(
                &mut flate2::read::MultiGzDecoder::new(&mut file),
                &mut output,
                limit,
                &mut on_chunk,
            )?;
        }
        "xz" => {
            let stream = xz2::stream::Stream::new_stream_decoder(
                256 * 1024 * 1024,
                xz2::stream::CONCATENATED,
            )
            .map_err(err)?;
            copy_bounded(
                &mut xz2::read::XzDecoder::new_stream(&mut file, stream),
                &mut output,
                limit,
                &mut on_chunk,
            )?;
        }
        "bzip2" => {
            copy_bounded(
                &mut bzip2::read::MultiBzDecoder::new(&mut file),
                &mut output,
                limit,
                &mut on_chunk,
            )?;
        }
        _ => unreachable!(),
    }
    output.sync_all().map_err(err)?;
    drop(output);
    unchanged(&before, &file.metadata().map_err(err)?)?;
    unchanged(&before, &std::fs::metadata(&canonical).map_err(err)?)?;
    on_chunk(0)?;
    Ok(ExtractedImage {
        _directory: directory,
        path: target,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn path_validation() {
        for n in [
            "../a.img",
            "/a.img",
            "C:a.img",
            "a\\b.img",
            "a/../b.img",
            "a//b.img",
            "./a.img",
        ] {
            assert!(!safe_name(n), "{n}");
        }
        assert!(safe_name("images/root.img"));
    }
    #[test]
    fn gzip_quota_cancel_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("disk.img.gz");
        let mut enc = flate2::write::GzEncoder::new(
            File::create(&path).unwrap(),
            flate2::Compression::default(),
        );
        enc.write_all(&[42; 4096]).unwrap();
        enc.finish().unwrap();
        let p = path.to_str().unwrap();
        assert_eq!(inspect_archive(p).unwrap().entries[0].name, "disk.img");
        assert!(extract_selected(p, "disk.img", 1024, |_| Ok(())).is_err());
        assert!(extract_selected(p, "disk.img", 8192, |_| Err("cancelled".into())).is_err());
        let extracted = extract_selected(p, "disk.img", 8192, |_| Ok(())).unwrap();
        let out = extracted.path().to_owned();
        assert_eq!(std::fs::read(&out).unwrap(), vec![42; 4096]);
        drop(extracted);
        assert!(!out.exists());
    }
}
