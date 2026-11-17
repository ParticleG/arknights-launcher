use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use reqwest::blocking::{Client, Response};
use reqwest::header::{ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE, RANGE, REFERER};
use reqwest::StatusCode;

use crate::work::Control;

const REQUEST_BYTES: u64 = 8 * 1024 * 1024;

pub struct Volume {
    pub url: String,
    pub size: u64,
    start: u64,
}

pub struct Source {
    client: Client,
    volumes: Vec<Volume>,
    pub len: u64,
}

pub struct Metadata {
    pub start: u64,
    pub bytes: Box<[u8]>,
}

pub fn client() -> Result<Client, String> {
    Client::builder()
        .user_agent("arknights-launcher/0.1.0")
        .https_only(true)
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(120))
        .retry(reqwest::retry::never())
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .build()
        .map_err(|error| format!("Cannot initialize HTTPS: {error}"))
}

impl Source {
    pub fn new(client: Client, packs: Vec<(String, u64)>) -> Result<Self, String> {
        if packs.is_empty() {
            return Err("The release has no ZIP volumes".into());
        }
        let mut len = 0_u64;
        let mut volumes = Vec::with_capacity(packs.len());
        for (url, size) in packs {
            let parsed = reqwest::Url::parse(&url).map_err(|error| error.to_string())?;
            if parsed.scheme() != "https" || parsed.host_str().is_none() || size == 0 {
                return Err("The release contains an invalid HTTPS ZIP volume".into());
            }
            volumes.push(Volume { url, size, start: len });
            len = len.checked_add(size).ok_or("ZIP volume sizes overflow")?;
        }
        Ok(Self { client, volumes, len })
    }

    fn locate(&self, position: u64) -> Option<&Volume> {
        locate_volume(&self.volumes, self.len, position)
    }

    fn request(&self, start: u64, end: u64) -> io::Result<Response> {
        let volume = self.locate(start).ok_or_else(|| invalid("Range starts outside the ZIP"))?;
        let local_start = start - volume.start;
        if end <= start || end > volume.start + volume.size {
            return Err(invalid("Range crosses a ZIP volume boundary"));
        }
        let local_end = end - volume.start - 1;
        let response = self.client
            .get(&volume.url)
            .header(RANGE, format!("bytes={local_start}-{local_end}"))
            .header(ACCEPT_ENCODING, "identity")
            .header(REFERER, "https://ak.hypergryph.com/")
            .send()
            .map_err(io::Error::other)?;
        if response.status() != StatusCode::PARTIAL_CONTENT {
            return Err(io::Error::other(format!(
                "ZIP server returned {} instead of HTTP 206", response.status()
            )));
        }
        let expected = format!("bytes {local_start}-{local_end}/{}", volume.size);
        let actual = response.headers().get(CONTENT_RANGE).and_then(|value| value.to_str().ok());
        if actual != Some(expected.as_str()) {
            return Err(invalid("ZIP server returned an incorrect Content-Range"));
        }
        if let Some(length) = response.headers().get(CONTENT_LENGTH) {
            if length.to_str().ok().and_then(|value| value.parse::<u64>().ok()) != Some(end - start) {
                return Err(invalid("ZIP server returned an incorrect Content-Length"));
            }
        }
        if response.headers().get(CONTENT_ENCODING).is_some_and(|value| value != "identity") {
            return Err(invalid("ZIP server unexpectedly encoded a byte range"));
        }
        Ok(response)
    }
}

struct Stream {
    response: Response,
    end: u64,
}

pub struct HttpRangeReader {
    source: Arc<Source>,
    metadata: Option<Arc<Metadata>>,
    control: Arc<Control>,
    end_limit: Arc<AtomicU64>,
    position: u64,
    stream: Option<Stream>,
}

impl HttpRangeReader {
    pub fn new(source: Arc<Source>, metadata: Option<Arc<Metadata>>, control: Arc<Control>) -> Self {
        let len = source.len;
        Self {
            source,
            metadata,
            control,
            end_limit: Arc::new(AtomicU64::new(len)),
            position: 0,
            stream: None,
        }
    }

    pub fn limit_handle(&self) -> Arc<AtomicU64> {
        self.end_limit.clone()
    }
}

impl Read for HttpRangeReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.control.check().map_err(io::Error::other)?;
        if buffer.is_empty() || self.position >= self.source.len {
            return Ok(0);
        }
        let limit = self.end_limit.load(Ordering::Acquire).min(self.source.len);
        if self.position >= limit {
            return Err(invalid("ZIP reader reached its permitted file boundary"));
        }
        if let Some(metadata) = &self.metadata {
            if self.position >= metadata.start {
                let offset = (self.position - metadata.start) as usize;
                if offset < metadata.bytes.len() {
                    let count = buffer.len()
                        .min(metadata.bytes.len() - offset)
                        .min((limit - self.position) as usize);
                    buffer[..count].copy_from_slice(&metadata.bytes[offset..offset + count]);
                    self.position += count as u64;
                    return Ok(count);
                }
            }
        }
        if self.stream.as_ref().is_none_or(|stream| self.position >= stream.end) {
            let volume = self.source.locate(self.position).ok_or_else(|| invalid("Invalid ZIP position"))?;
            let cache_boundary = self.metadata.as_ref()
                .filter(|metadata| self.position < metadata.start)
                .map_or(self.source.len, |metadata| metadata.start);
            let end = (volume.start + volume.size)
                .min(self.position.saturating_add(REQUEST_BYTES))
                .min(limit)
                .min(cache_boundary);
            self.stream = Some(Stream {
                response: self.source.request(self.position, end)?,
                end,
            });
        }
        let stream = self.stream.as_mut().expect("The range stream was initialized");
        let count = buffer.len().min((stream.end - self.position).min(limit - self.position) as usize);
        let received = stream.response.read(&mut buffer[..count])?;
        if received == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "ZIP byte range ended early"));
        }
        self.position += received as u64;
        Ok(received)
    }
}

impl Seek for HttpRangeReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let next = seek_position(self.position, self.source.len, from)?;
        if next != self.position {
            self.stream = None;
            self.position = next;
        }
        Ok(next)
    }
}

fn seek_position(current: u64, len: u64, from: SeekFrom) -> io::Result<u64> {
    let next = match from {
        SeekFrom::Start(position) => i128::from(position),
        SeekFrom::Current(delta) => i128::from(current) + i128::from(delta),
        SeekFrom::End(delta) => i128::from(len) + i128::from(delta),
    };
    u64::try_from(next).map_err(|_| invalid("Seek before the ZIP start or beyond u64"))
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_offsets_cross_volumes_and_handle_signed_seeks() {
        let volumes = vec![
            Volume { url: String::new(), size: 4, start: 0 },
            Volume { url: String::new(), size: 7, start: 4 },
        ];
        assert_eq!(locate_volume(&volumes, 11, 3).unwrap().start, 0);
        assert_eq!(locate_volume(&volumes, 11, 4).unwrap().start, 4);
        assert_eq!(locate_volume(&volumes, 11, 10).unwrap().size, 7);
        assert!(locate_volume(&volumes, 11, 11).is_none());
        assert_eq!(seek_position(4, 11, SeekFrom::Current(-1)).unwrap(), 3);
        assert_eq!(seek_position(0, 11, SeekFrom::End(-7)).unwrap(), 4);
        assert_eq!(seek_position(0, 11, SeekFrom::End(2)).unwrap(), 13);
        assert!(seek_position(0, 11, SeekFrom::Current(-1)).is_err());
        assert!(seek_position(u64::MAX, 11, SeekFrom::Current(1)).is_err());
    }
}

fn locate_volume(volumes: &[Volume], len: u64, position: u64) -> Option<&Volume> {
    if position >= len {
        return None;
    }
    let index = volumes.partition_point(|volume| volume.start <= position).checked_sub(1)?;
    volumes.get(index)
}
