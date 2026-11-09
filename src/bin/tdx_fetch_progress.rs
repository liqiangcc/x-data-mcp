//! `tdx-fetch-progress`: the streaming half of the pinned fetch helper (#625).
//!
//! `scripts/tdx_fetch.py` moves bytes with `urllib` and can only report a
//! download once it is complete, so `get_acquisition_task` showed
//! `bytes_done: 0` for the whole 550 MB day archive. This binary performs the
//! same fetch and prints one `{"bytes": N}` line per landed chunk on stdout,
//! then the same result object the helper prints (`status`, `etag`,
//! `last_modified`, `content_length`, `bytes`, `sha256`).
//!
//! It is deliberately a *helper of the helper*: the service still shells out
//! to `tdx_fetch.py`, which delegates here only when
//! `X_DATA_MCP_TDX_FETCH_NATIVE` names this binary, and then re-verifies the
//! staged bytes against the reported digest before printing the result. So the
//! byte-verified pipeline, the temp+rename+fsync write and the size check
//! against `Content-Length` are unchanged; only the progress reporting is new.
//!
//! The three declared TDX URLs are always `https://`, so this is a small
//! HTTP/1.1 client over `std::net` + rustls (no extra HTTP stack in the build
//! graph). Anything it cannot do — a redirect, a non-2xx answer, a
//! non-`https` URL — exits non-zero with the reason on stderr, exactly like the
//! helper's other failures; the service maps that to a task failure, never to
//! a retry of its own.
//!
//! Usage: `tdx-fetch-progress <url> <dest>`.
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::ExitCode;

use x_data_mcp::hash::{self, STREAM_BUFFER};

const HEAD_TIMEOUT_SECS: u64 = 30;
const FETCH_TIMEOUT_SECS: u64 = 900;
const USER_AGENT: &str = "x-data-mcp-tdx-fetch/1";

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let [url, dest] = args.as_slice() else {
        eprintln!("usage: tdx-fetch-progress <url> <dest>");
        return ExitCode::from(2);
    };
    match run(url, std::path::Path::new(dest)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// One `<scheme>://<host>[:port]/<path>` split. Only `https` is accepted.
struct Target {
    host: String,
    port: u16,
    path: String,
}

fn parse_url(url: &str) -> Result<Target, String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| format!("only https URLs are fetched, got {url}"))?;
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err(format!("{url} has no host"));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (
            host.to_string(),
            port.parse::<u16>()
                .map_err(|_| format!("{url} has an invalid port {port:?}"))?,
        ),
        None => (authority.to_string(), 443),
    };
    Ok(Target {
        host,
        port,
        path: path.to_string(),
    })
}

fn run(url: &str, dest: &std::path::Path) -> Result<(), String> {
    let target = parse_url(url)?;
    let head = head(&target, url)?;
    let dir = dest
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let tmp = dir.join(format!(
        ".{}.{}.part",
        dest.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "fetch".into()),
        std::process::id()
    ));
    if let Err(error) = stream(&target, url, &tmp, &head) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    std::fs::rename(&tmp, dest).map_err(|error| format!("rename to dest failed: {error}"))?;
    Ok(())
}

struct Head {
    etag: Option<String>,
    last_modified: Option<String>,
    content_length: Option<u64>,
}

fn head(target: &Target, url: &str) -> Result<Head, String> {
    let response = request(target, "HEAD", HEAD_TIMEOUT_SECS)?;
    if response.status != 200 {
        return Err(format!("head {url} answered {}", response.status));
    }
    Ok(Head {
        etag: response.etag,
        last_modified: response.last_modified,
        content_length: response.content_length,
    })
}

/// A response whose body has not been consumed yet.
struct Pending {
    status: u16,
    etag: Option<String>,
    last_modified: Option<String>,
    content_length: Option<u64>,
    reader: Box<dyn Read + Send>,
}

fn request(target: &Target, method: &str, timeout_secs: u64) -> Result<Pending, String> {
    let timeout = std::time::Duration::from_secs(timeout_secs);
    let tcp = TcpStream::connect((target.host.as_str(), target.port))
        .map_err(|error| format!("connect {}:{} failed: {error}", target.host, target.port))?;
    tcp.set_read_timeout(Some(timeout))
        .map_err(|error| format!("set read timeout failed: {error}"))?;
    tcp.set_write_timeout(Some(timeout))
        .map_err(|error| format!("set write timeout failed: {error}"))?;
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server_name = target
        .host
        .clone()
        .try_into()
        .map_err(|error| format!("{} is not a valid server name: {error}", target.host))?;
    let connection = rustls::ClientConnection::new(std::sync::Arc::new(config), server_name)
        .map_err(|error| format!("TLS setup failed: {error}"))?;
    let mut tls = rustls::StreamOwned::new(connection, tcp);
    let request = format!(
        "{method} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {USER_AGENT}\r\nAccept: */*\r\n\
         Accept-Encoding: identity\r\nConnection: close\r\n\r\n",
        target.path, target.host
    );
    tls.write_all(request.as_bytes())
        .map_err(|error| format!("write request failed: {error}"))?;
    tls.flush()
        .map_err(|error| format!("flush request failed: {error}"))?;
    let mut reader = BufReader::new(tls);
    let status = read_status(&mut reader)?;
    let mut etag = None;
    let mut last_modified = None;
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(|error| format!("read headers failed: {error}"))?;
        if read == 0 {
            return Err("connection closed inside the response headers".to_string());
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(format!("malformed header line {line:?}"));
        };
        let value = value.trim().to_string();
        match name.trim().to_ascii_lowercase().as_str() {
            "etag" => etag = Some(value),
            "last-modified" => last_modified = Some(value),
            "content-length" => {
                content_length = value
                    .parse::<u64>()
                    .map_err(|_| format!("content-length {value:?} is not a length"))?
                    .into()
            }
            "transfer-encoding" if !value.eq_ignore_ascii_case("identity") => {
                return Err(format!("unsupported transfer-encoding {value:?}"));
            }
            _ => {}
        }
    }
    if method == "HEAD" {
        return Ok(Pending {
            status,
            etag,
            last_modified,
            content_length,
            reader: Box::new(std::io::empty()),
        });
    }
    let body: Box<dyn Read + Send> = match content_length {
        Some(length) => Box::new(reader.take(length)),
        None => Box::new(Chunked::new(reader)),
    };
    Ok(Pending {
        status,
        etag,
        last_modified,
        content_length,
        reader: body,
    })
}

fn read_status(reader: &mut impl BufRead) -> Result<u16, String> {
    let mut line = String::new();
    let read = reader
        .read_line(&mut line)
        .map_err(|error| format!("read status line failed: {error}"))?;
    if read == 0 {
        return Err("connection closed before the status line".to_string());
    }
    let mut parts = line.trim_end_matches(['\r', '\n']).split(' ');
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/1.") {
        return Err(format!("unexpected status line {line:?}"));
    }
    parts
        .next()
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| format!("status line {line:?} has no code"))
}

/// `Transfer-Encoding: chunked` decoder. The declared TDX hosts answer with a
/// `Content-Length`, but a proxy in front of them does not have to.
struct Chunked<R: BufRead> {
    inner: R,
    remaining: u64,
    done: bool,
}

impl<R: BufRead> Chunked<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            remaining: 0,
            done: false,
        }
    }
}

impl<R: BufRead> Read for Chunked<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.done || buffer.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            let mut line = String::new();
            if self.inner.read_line(&mut line)? == 0 {
                return Ok(0);
            }
            let size = line.trim_end_matches(['\r', '\n']);
            let size = size.split(';').next().unwrap_or_default();
            let size = u64::from_str_radix(size.trim(), 16)
                .map_err(|_| std::io::Error::other(format!("bad chunk size {size:?}")))?;
            if size == 0 {
                // Trailer section: read until the blank line.
                let mut trailer = String::new();
                while self.inner.read_line(&mut trailer)? > 0 {
                    if trailer == "\r\n" || trailer == "\n" {
                        break;
                    }
                    trailer.clear();
                }
                self.done = true;
                return Ok(0);
            }
            self.remaining = size;
        }
        let take = buffer
            .len()
            .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        let read = self.inner.read(&mut buffer[..take])?;
        if read == 0 {
            return Err(std::io::Error::other("chunk truncated"));
        }
        self.remaining -= read as u64;
        if self.remaining == 0 {
            // Drain the CRLF that follows the chunk data.
            let mut tail = String::new();
            self.inner.read_line(&mut tail)?;
        }
        Ok(read)
    }
}

fn stream(target: &Target, url: &str, tmp: &std::path::Path, head: &Head) -> Result<(), String> {
    let mut response = request(target, "GET", FETCH_TIMEOUT_SECS)?;
    if response.status != 200 {
        return Err(format!("fetch {url} answered {}", response.status));
    }
    let mut file = std::fs::File::create(tmp)
        .map_err(|error| format!("create {} failed: {error}", tmp.display()))?;
    let mut digest = Sha256State::new();
    let mut size = 0u64;
    let mut buffer = vec![0u8; STREAM_BUFFER];
    loop {
        let read = response
            .reader
            .read(&mut buffer)
            .map_err(|error| format!("read body failed: {error}"))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .map_err(|error| format!("write failed: {error}"))?;
        digest.update(&buffer[..read]);
        size += read as u64;
        // One line per landed chunk: the service throttles the durable write,
        // so the helper never has to guess a reporting interval.
        println!("{{\"bytes\":{size}}}");
        std::io::stdout()
            .flush()
            .map_err(|error| format!("stdout flush failed: {error}"))?;
    }
    file.flush()
        .map_err(|error| format!("flush failed: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("fsync failed: {error}"))?;
    if let Some(expected) = head.content_length
        && expected != size
    {
        return Err(format!(
            "size mismatch: {size} != Content-Length {expected}"
        ));
    }
    println!(
        "{}",
        serde_json::json!({
            "status": response.status,
            "etag": head.etag,
            "last_modified": head.last_modified,
            "content_length": head.content_length,
            "bytes": size,
            "sha256": digest.finish(),
        })
    );
    std::io::stdout()
        .flush()
        .map_err(|error| format!("stdout flush failed: {error}"))?;
    Ok(())
}

/// sha256 accumulator over the crate's `sha2` dependency.
struct Sha256State {
    inner: sha2::Sha256,
}

impl Sha256State {
    fn new() -> Self {
        Self {
            inner: <sha2::Sha256 as sha2::Digest>::new(),
        }
    }
    fn update(&mut self, bytes: &[u8]) {
        sha2::Digest::update(&mut self.inner, bytes);
    }
    fn finish(self) -> String {
        let digest = sha2::Digest::finalize(self.inner);
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

/// Keeps the `hash` import meaningful for readers looking for the streaming
/// bound this binary reuses.
const _: usize = hash::STREAM_BUFFER;
