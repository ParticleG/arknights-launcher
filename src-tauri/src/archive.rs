use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use zip::ZipArchive;

use crate::files::archive_path;
use crate::ranges::{self, HttpRangeReader, Metadata, Source};
use crate::types::ReleaseSummary;
use crate::work::{Control, WorkError};

const API: &str = "https://launcher.hypergryph.com/api/proxy/batch_proxy";
const MAX_DIRECTORY_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Deserialize)]
struct ApiResponse {
    proxy_rsps: Vec<ProxyResponse>,
}

#[derive(Deserialize)]
struct ProxyResponse {
    get_latest_game_rsp: Option<LatestGame>,
}

#[derive(Deserialize)]
struct LatestGame {
    version: String,
    client_version: String,
    pkg: Package,
}

#[derive(Deserialize)]
struct Package {
    packs: Vec<Pack>,
}

#[derive(Deserialize)]
struct Pack {
    url: String,
    package_size: String,
}

pub struct Entry {
    pub index: usize,
    pub path: PathBuf,
    pub size: u64,
    pub compressed_size: u64,
    pub crc32: u32,
    pub header_start: u64,
}

pub struct Release {
    pub summary: ReleaseSummary,
    pub entries: Vec<Entry>,
    pub directories: Vec<PathBuf>,
    pub source: Arc<Source>,
    pub metadata: Arc<Metadata>,
}

impl Release {
    pub fn archive(&self, control: Arc<Control>) -> Result<(ZipArchive<HttpRangeReader>, Arc<std::sync::atomic::AtomicU64>), WorkError> {
        control.check()?;
        let reader = HttpRangeReader::new(self.source.clone(), Some(self.metadata.clone()), control.clone());
        let limit = reader.limit_handle();
        let archive = ZipArchive::new(reader).map_err(|error| control.error(error))?;
        Ok((archive, limit))
    }
}

pub fn fetch(control: Arc<Control>) -> Result<Release, WorkError> {
    control.check()?;
    let client = ranges::client()?;
    let response: ApiResponse = client.post(API)
        .json(&serde_json::json!({
            "seq": "1",
            "proxy_reqs": [{
                "kind": "get_latest_game",
                "get_latest_game_req": {
                    "version": "",
                    "appcode": "GzD1CpaWgmSq1wew",
                    "channel": "1",
                    "sub_channel": "1",
                    "device_id": "arknights-launcher"
                }
            }]
        }))
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .and_then(reqwest::blocking::Response::json)
        .map_err(|error| control.error(format!("Cannot retrieve the official Arknights CN release: {error}")))?;
    control.check()?;
    let latest = response.proxy_rsps.into_iter().next()
        .and_then(|response| response.get_latest_game_rsp)
        .ok_or_else(|| WorkError::Failed("The official API returned no game release".into()))?;
    if latest.version.is_empty() || latest.client_version.is_empty() {
        return Err("The official release has an empty version".to_string().into());
    }
    let packs = latest.pkg.packs.into_iter().map(|pack| {
        let size = pack.package_size.parse::<u64>()
            .map_err(|_| "The official API returned an invalid ZIP volume size".to_string())?;
        Ok((pack.url, size))
    }).collect::<Result<Vec<_>, String>>()?;
    let source = Arc::new(Source::new(client, packs)?);
    let metadata = load_metadata(source.clone(), control.clone())?;
    let reader = HttpRangeReader::new(source.clone(), Some(metadata.clone()), control.clone());
    let archive = ZipArchive::new(reader)
        .map_err(|error| control.error(format!("Cannot open the release ZIP directory: {error}")))?;
    if archive.central_directory_start() != metadata.start {
        return Err("The ZIP directory offset disagrees with its footer".to_string().into());
    }
    let (entries, directories, installed_bytes) = directory_entries(&archive, &metadata)?;
    if entries.is_empty() || !entries.iter().any(|entry| entry.path == std::path::Path::new("Arknights.exe")) {
        return Err("The official package does not contain a root Arknights.exe".to_string().into());
    }
    control.check()?;
    Ok(Release {
        summary: ReleaseSummary {
            version: latest.version,
            client_version: latest.client_version,
            file_count: entries.len(),
            compressed_bytes: source.len,
            installed_bytes,
        },
        entries,
        directories,
        source,
        metadata,
    })
}

// zip 2 exposes names and total size, but not central-entry CRCs without
// opening local headers. Read these fixed central-directory fields directly so
// a release check never fetches thousands of unrelated file payload ranges.
// zip itself remains responsible for ZIP/ZIP64 structure and decompression.
fn directory_entries(
    archive: &ZipArchive<HttpRangeReader>,
    metadata: &Metadata,
) -> Result<(Vec<Entry>, Vec<PathBuf>, u64), WorkError> {
    let mut bytes = metadata.bytes.as_ref();
    let mut entries = Vec::with_capacity(archive.len());
    let mut directories = Vec::new();
    let mut paths = HashMap::with_capacity(archive.len());
    let mut total = 0_u64;
    for index in 0..archive.len() {
        let header = take(&mut bytes, 46)?;
        if u32_at(header, 0) != 0x0201_4b50 {
            return Err("Invalid ZIP central-directory entry".to_string().into());
        }
        let flags = u16_at(header, 8);
        let method = u16_at(header, 10);
        if flags & 1 != 0 || !matches!(method, 0 | 8 | 9) {
            return Err("The ZIP contains encrypted or unsupported compression entries".to_string().into());
        }
        let name_len = usize::from(u16_at(header, 28));
        let extra_len = usize::from(u16_at(header, 30));
        let comment_len = usize::from(u16_at(header, 32));
        take(&mut bytes, name_len)?;
        let extra = take(&mut bytes, extra_len)?;
        take(&mut bytes, comment_len)?;
        let name = archive.name_for_index(index).ok_or("Missing ZIP entry name".to_string())?;
        let path = archive_path(name)?;
        let external = u32_at(header, 38);
        let unix_type = (external >> 16) & 0xf000;
        if !matches!(unix_type, 0 | 0x4000 | 0x8000) {
            return Err(format!("Refusing a symbolic-link or special ZIP entry: {name}").into());
        }
        let is_dir = name.ends_with('/') || external & 0x10 != 0 || unix_type == 0x4000;
        if paths.insert(path.clone(), is_dir).is_some() {
            return Err(format!("Duplicate ZIP destination: {}", path.display()).into());
        }
        let mut size = u64::from(u32_at(header, 24));
        let mut compressed_size = u64::from(u32_at(header, 20));
        let mut offset = u64::from(u32_at(header, 42));
        let mut disk = u32::from(u16_at(header, 34));
        if size == u64::from(u32::MAX)
            || compressed_size == u64::from(u32::MAX)
            || offset == u64::from(u32::MAX)
            || disk == u32::from(u16::MAX)
        {
            let mut zip64 = zip64_extra(extra)?;
            if size == u64::from(u32::MAX) { size = read_u64(&mut zip64)?; }
            if compressed_size == u64::from(u32::MAX) { compressed_size = read_u64(&mut zip64)?; }
            if offset == u64::from(u32::MAX) { offset = read_u64(&mut zip64)?; }
            if disk == u32::from(u16::MAX) { disk = u32_at(take(&mut zip64, 4)?, 0); }
        }
        if disk != 0 {
            return Err("The package is not a concatenated single-disk ZIP".to_string().into());
        }
        let header_start = offset.checked_add(archive.offset()).ok_or("ZIP local-header offset overflows".to_string())?;
        if header_start.checked_add(30).is_none_or(|end| end > metadata.start) {
            return Err("A ZIP local header overlaps the central directory".to_string().into());
        }
        if is_dir {
            if size != 0 {
                return Err(format!("ZIP directory has unexpected data: {name}").into());
            }
            directories.push(path);
        } else {
            total = total.checked_add(size).ok_or("ZIP installed size overflows".to_string())?;
            entries.push(Entry {
                index,
                path,
                size,
                compressed_size,
                crc32: u32_at(header, 16),
                header_start,
            });
        }
    }
    if bytes.len() >= 4 && u32_at(bytes, 0) == 0x0201_4b50 {
        return Err("The ZIP contains duplicate entry names".to_string().into());
    }
    for path in paths.keys() {
        for ancestor in path.ancestors().skip(1) {
            if paths.get(ancestor) == Some(&false) {
                return Err(format!("ZIP file is also a parent directory: {}", ancestor.display()).into());
            }
        }
    }
    Ok((entries, directories, total))
}

fn zip64_extra(mut bytes: &[u8]) -> Result<&[u8], WorkError> {
    while !bytes.is_empty() {
        let header = take(&mut bytes, 4)?;
        let data = take(&mut bytes, usize::from(u16_at(header, 2)))?;
        if u16_at(header, 0) == 1 {
            return Ok(data);
        }
    }
    Err("Missing ZIP64 extended information".to_string().into())
}

fn read_u64(bytes: &mut &[u8]) -> Result<u64, WorkError> {
    let value = take(bytes, 8)?;
    Ok(u64::from_le_bytes(value.try_into().expect("Eight bytes were checked")))
}

fn take<'a>(bytes: &mut &'a [u8], count: usize) -> Result<&'a [u8], WorkError> {
    if bytes.len() < count {
        return Err("Truncated ZIP central directory".to_string().into());
    }
    let (value, remainder) = bytes.split_at(count);
    *bytes = remainder;
    Ok(value)
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("Four bytes were checked"))
}

fn load_metadata(source: Arc<Source>, control: Arc<Control>) -> Result<Arc<Metadata>, WorkError> {
    // EOCD plus the maximum legal ZIP comment. This is metadata only, even
    // when the game payload is tens of gigabytes and spans many HTTP volumes.
    let tail_start = source.len.saturating_sub(65_557);
    let tail = Arc::new(Metadata {
        start: tail_start,
        bytes: read_region(source.clone(), None, control.clone(), tail_start, source.len - tail_start)?,
    });
    let bytes = tail.bytes.as_ref();
    let end = (0..bytes.len().saturating_sub(21)).rev().find(|offset| {
        u32_at(bytes, *offset) == 0x0605_4b50
            && *offset + 22 + usize::from(u16_at(bytes, *offset + 20)) == bytes.len()
    }).ok_or("The ZIP end-of-directory record was not found".to_string())?;
    let footer = &bytes[end..];
    if u16_at(footer, 4) != 0 || u16_at(footer, 6) != 0 {
        return Err("The package declares a multi-disk ZIP instead of raw concatenated volumes".to_string().into());
    }
    let mut start = u64::from(u32_at(footer, 16));
    let mut size = u64::from(u32_at(footer, 12));
    let mut boundary = tail_start + end as u64;
    if start == u64::from(u32::MAX)
        || size == u64::from(u32::MAX)
        || u16_at(footer, 8) == u16::MAX
        || u16_at(footer, 10) == u16::MAX
    {
        let locator_start = boundary.checked_sub(20).ok_or("Missing ZIP64 locator".to_string())?;
        let locator = read_region(source.clone(), Some(tail.clone()), control.clone(), locator_start, 20)?;
        if u32_at(&locator, 0) != 0x0706_4b50 || u32_at(&locator, 4) != 0 || u32_at(&locator, 16) != 1 {
            return Err("Invalid single-disk ZIP64 locator".to_string().into());
        }
        boundary = u64_at(&locator, 8);
        let record = read_region(source.clone(), Some(tail.clone()), control.clone(), boundary, 56)?;
        if u32_at(&record, 0) != 0x0606_4b50
            || u64_at(&record, 4) < 44
            || u32_at(&record, 16) != 0
            || u32_at(&record, 20) != 0
            || u64_at(&record, 24) != u64_at(&record, 32)
        {
            return Err("Invalid single-disk ZIP64 directory".to_string().into());
        }
        size = u64_at(&record, 40);
        start = u64_at(&record, 48);
    } else if u16_at(footer, 8) != u16_at(footer, 10) {
        return Err("The ZIP directory entry counts disagree".to_string().into());
    }
    if start.checked_add(size).is_none_or(|end| end > boundary) {
        return Err("The ZIP directory exceeds its footer boundary".to_string().into());
    }
    let length = source.len.checked_sub(start).ok_or("Invalid ZIP central-directory offset".to_string())?;
    if length > MAX_DIRECTORY_BYTES {
        return Err("The ZIP central directory exceeds the 64 MiB metadata safety limit".to_string().into());
    }
    let bytes = read_region(source, Some(tail), control, start, length)?;
    Ok(Arc::new(Metadata { start, bytes }))
}

fn read_region(
    source: Arc<Source>,
    metadata: Option<Arc<Metadata>>,
    control: Arc<Control>,
    start: u64,
    length: u64,
) -> Result<Box<[u8]>, WorkError> {
    let end = start.checked_add(length)
        .filter(|end| *end <= source.len)
        .ok_or("ZIP metadata range is outside the archive".to_string())?;
    if length > MAX_DIRECTORY_BYTES {
        return Err("ZIP metadata range exceeds its memory limit".to_string().into());
    }
    let mut reader = HttpRangeReader::new(source, metadata, control.clone());
    reader.limit_handle().store(end, std::sync::atomic::Ordering::Release);
    reader.seek(SeekFrom::Start(start)).map_err(|error| control.error(error))?;
    let mut bytes = vec![0; length as usize].into_boxed_slice();
    reader.read_exact(&mut bytes).map_err(|error| control.error(error))?;
    Ok(bytes)
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("Eight bytes were checked"))
}
