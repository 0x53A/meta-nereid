//! HTTP and bounded range reads with a compile-time TLS backend.
use crate::model::{Server, Source, Track};
use anyhow::{bail, Context, Result};
use reqwest::{
    blocking::{Client, Response},
    header, Url,
};
use std::io::{self, Read, Seek, SeekFrom};
use std::time::Duration;

pub fn client() -> Result<Client> {
    let builder = Client::builder();
    #[cfg(feature = "tls-system")]
    let builder = builder.use_native_tls();
    #[cfg(feature = "tls-rustcrypto")]
    let builder = {
        let _ = rustls_rustcrypto::provider().install_default();
        builder.use_rustls_tls()
    };
    Ok(builder
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}
fn clean_error(_: reqwest::Error) -> anyhow::Error {
    anyhow::anyhow!("Network request failed (connection, TLS, or timeout)")
}
pub fn endpoint(server: &Server, method: &str) -> Result<Url> {
    let mut url = Url::parse(&server.url).context("Invalid Navidrome URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        bail!("Use an HTTP(S) server URL without embedded credentials");
    }
    url.set_query(None);
    url.set_fragment(None);
    let base = url.path().trim_end_matches('/').to_string();
    url.set_path(&format!("{base}/rest/{method}.view"));
    let mut random = [0u8; 16];
    getrandom::getrandom(&mut random).map_err(|_| anyhow::anyhow!("Random source unavailable"))?;
    let salt = random
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let token = format!("{:x}", md5::compute(format!("{}{salt}", server.password)));
    url.query_pairs_mut().extend_pairs([
        ("u", server.username.as_str()),
        ("s", &salt),
        ("t", &token),
        ("v", "1.16.1"),
        ("c", "hoki-music"),
        ("f", "json"),
    ]);
    Ok(url)
}
pub fn server_library(server: &Server) -> Result<Vec<Track>> {
    let client = client()?;
    let mut tracks = Vec::new();
    for offset in (0..10_000).step_by(250) {
        let mut url = endpoint(server, "search3")?;
        url.query_pairs_mut().extend_pairs([
            ("query", ""),
            ("artistCount", "0"),
            ("albumCount", "0"),
            ("songCount", "250"),
            ("songOffset", &offset.to_string()),
        ]);
        let mut response = client
            .get(url)
            .send()
            .map_err(clean_error)?
            .error_for_status()
            .map_err(clean_error)?;
        let mut bytes = Vec::new();
        response
            .by_ref()
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| anyhow::anyhow!("Navidrome response was interrupted"))?;
        if bytes.len() > 4 * 1024 * 1024 {
            bail!("Navidrome response too large");
        }
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).context("Invalid Navidrome response")?;
        let root = &value["subsonic-response"];
        if root["status"] != "ok" {
            bail!("Navidrome rejected the request; check server credentials");
        }
        let songs = root["searchResult3"]["song"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for song in &songs {
            let Some(id) = song["id"].as_str() else {
                continue;
            };
            tracks.push(Track {
                title: song["title"]
                    .as_str()
                    .unwrap_or("Untitled")
                    .chars()
                    .take(256)
                    .collect(),
                artist: song["artist"]
                    .as_str()
                    .unwrap_or("Unknown artist")
                    .chars()
                    .take(256)
                    .collect(),
                album: song["album"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(256)
                    .collect(),
                duration: song["duration"].as_f64().unwrap_or(0.0).max(0.0),
                source: Source::Navidrome {
                    server: server.url.clone(),
                    id: id.into(),
                },
            });
        }
        if songs.len() < 250 {
            break;
        }
    }
    Ok(tracks)
}

/// Each range is at most 256 KiB, bounding buffering and each read's deadline.
/// Seeking requires a standards-compliant range response; never misinterpret 200 as 206.
pub struct HttpSource {
    client: Client,
    url: Url,
    position: u64,
    length: u64,
    response: Option<Response>,
    remaining: u64,
}
impl HttpSource {
    pub fn new(url: Url) -> Result<Self> {
        let mut source = Self {
            client: client()?,
            url,
            position: 0,
            length: 0,
            response: None,
            remaining: 0,
        };
        source.open_range(true)?;
        Ok(source)
    }
    fn open_range(&mut self, initial: bool) -> Result<()> {
        let end = self.position.saturating_add(256 * 1024 - 1);
        let response = self
            .client
            .get(self.url.clone())
            .header(header::RANGE, format!("bytes={}-{end}", self.position))
            .header(header::ACCEPT_ENCODING, "identity")
            .send()
            .map_err(clean_error)?;
        if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            bail!("Server must support byte-range streaming (use original files, not transcoding)");
        }
        let range = response
            .headers()
            .get(header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .context("Missing Content-Range")?;
        let (start, finish, total) = parse_range(range)?;
        if start != self.position || finish > end || (!initial && total != self.length) {
            bail!("Invalid or changed stream range");
        }
        self.length = total;
        self.remaining = finish - start + 1;
        self.response = Some(response);
        Ok(())
    }
}
fn parse_range(value: &str) -> Result<(u64, u64, u64)> {
    let (span, total) = value
        .strip_prefix("bytes ")
        .and_then(|v| v.split_once('/'))
        .context("Invalid Content-Range")?;
    let (start, end) = span.split_once('-').context("Invalid Content-Range")?;
    let (start, end, total) = (
        start.parse::<u64>()?,
        end.parse::<u64>()?,
        total.parse::<u64>()?,
    );
    if start > end || end >= total {
        bail!("Invalid Content-Range bounds");
    }
    Ok((start, end, total))
}
impl Read for HttpSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.position == self.length {
            return Ok(0);
        }
        if self.remaining == 0 {
            self.open_range(false).map_err(io::Error::other)?;
        }
        let count = buf.len().min(self.remaining as usize);
        let n = self
            .response
            .as_mut()
            .unwrap()
            .read(&mut buf[..count])
            .map_err(|_| io::Error::other("Music stream was interrupted"))?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Truncated music stream",
            ));
        }
        self.position += n as u64;
        self.remaining -= n as u64;
        Ok(n)
    }
}
impl Seek for HttpSource {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let next = match from {
            SeekFrom::Start(n) => n as i128,
            SeekFrom::Current(n) => self.position as i128 + n as i128,
            SeekFrom::End(n) => self.length as i128 + n as i128,
        };
        if next < 0 || next > self.length as i128 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Seek outside file",
            ));
        }
        if next as u64 != self.position {
            self.position = next as u64;
            self.remaining = 0;
            self.response = None;
        }
        Ok(self.position)
    }
}
impl symphonia::core::io::MediaSource for HttpSource {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        Some(self.length)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn range_validation() {
        assert_eq!(parse_range("bytes 10-19/20").unwrap(), (10, 19, 20));
        for s in [
            "bytes 20-10/30",
            "bytes 0-20/20",
            "bytes */20",
            "bytes 0-1/*",
        ] {
            assert!(parse_range(s).is_err());
        }
    }
    #[test]
    fn authentication_retains_subpath_and_escapes() {
        let server = Server {
            url: "https://example.org/music/".into(),
            username: "a&b".into(),
            password: "secret".into(),
        };
        let u = endpoint(&server, "stream").unwrap();
        assert_eq!(u.path(), "/music/rest/stream.view");
        assert!(!u.as_str().contains("secret"));
        assert!(u.query_pairs().any(|(k, v)| k == "u" && v == "a&b"));
    }
}

#[cfg(test)]
mod http_tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        sync::mpsc,
    };
    #[test]
    fn streaming_seek_uses_validated_ranges() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            for expected in [0u64, 300_000] {
                let (mut socket, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut range = String::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if line.to_lowercase().starts_with("range:") {
                        range = line;
                    }
                }
                assert!(range.contains(&format!("bytes={expected}-")));
                let end = (expected + 256 * 1024 - 1).min(399_999);
                let len = end - expected + 1;
                write!(socket,"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {expected}-{end}/400000\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n").unwrap();
                // The client may close an old range early when seeking.
                let _ = socket.write_all(&vec![42u8; len as usize]);
            }
            tx.send(()).unwrap();
        });
        let mut source =
            HttpSource::new(Url::parse(&format!("http://{addr}/audio")).unwrap()).unwrap();
        let mut b = [0u8; 100];
        source.read_exact(&mut b).unwrap();
        assert_eq!(b, [42; 100]);
        source.seek(SeekFrom::Start(300_000)).unwrap();
        source.read_exact(&mut b).unwrap();
        assert_eq!(b, [42; 100]);
        assert!(source.seek(SeekFrom::End(1)).is_err());
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        server.join().unwrap();
    }
    #[test]
    fn server_ignoring_range_is_not_treated_as_seekable() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(s.try_clone().unwrap());
            loop {
                let mut l = String::new();
                reader.read_line(&mut l).unwrap();
                if l == "\r\n" {
                    break;
                }
            }
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnope")
                .unwrap();
        });
        assert!(HttpSource::new(Url::parse(&format!("http://{addr}/audio")).unwrap()).is_err());
        server.join().unwrap();
    }
}

#[cfg(test)]
mod tls_smoke {
    #[test]
    #[ignore = "external HTTPS smoke test; run explicitly"]
    fn public_https_validates_certificates() {
        let response = super::client()
            .unwrap()
            .get("https://example.org/")
            .send()
            .unwrap();
        assert!(response.status().is_success());
        assert!(super::client()
            .unwrap()
            .get("https://expired.badssl.com/")
            .send()
            .is_err());
    }
}
