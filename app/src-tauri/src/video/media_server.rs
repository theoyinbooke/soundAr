//! Loopback media origin for the desktop webview.
//!
//! WebKitGTK routes `<video>`/`<audio>` loading through GStreamer, and that backend cannot read
//! custom URI schemes. Tauri's `asset:` protocol therefore fails every media element on Linux with
//! `MEDIA_ERR_SRC_NOT_SUPPORTED` even though the same bytes decode fine over HTTP. Images are
//! unaffected because they use the regular resource loader, which is why posters rendered while
//! playback never started.
//!
//! This server hands the webview a real `http://127.0.0.1:<port>` origin with byte-range support so
//! seeking works, and confines reads to the artifact roots behind a per-launch token.

use std::cmp::min;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const MAX_CONNECTIONS: usize = 64;
const CONNECTION_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const HANDLER_STACK_BYTES: usize = 128 * 1024;
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const SOCKET_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REQUEST_LINE_BYTES: u64 = 8 * 1024;
const MAX_HEADER_COUNT: usize = 64;
const STREAM_CHUNK_BYTES: usize = 256 * 1024;

/// A running loopback media origin. Dropping this stops the accept loop and joins its workers.
pub struct LocalMediaServer {
    origin: String,
    token: String,
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    accept_thread: Option<JoinHandle<()>>,
}

impl LocalMediaServer {
    /// Bind an ephemeral loopback port that serves regular files beneath `roots`.
    ///
    /// Roots are canonicalised once at start-up; a request path must canonicalise to a regular file
    /// beneath one of them, so symlinks and `..` cannot escape the artifact directories.
    pub fn start(roots: Vec<PathBuf>) -> io::Result<Self> {
        let canonical_roots: Vec<PathBuf> = roots
            .iter()
            .filter_map(|root| std::fs::canonicalize(root).ok())
            .collect();
        if canonical_roots.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no readable media root",
            ));
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let token = uuid::Uuid::new_v4().simple().to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker_token = token.clone();
        let accept_thread = thread::Builder::new()
            .name("soundar-media-origin".to_string())
            .spawn(move || {
                run_media_server(
                    listener,
                    worker_token,
                    Arc::new(canonical_roots),
                    worker_stop,
                )
            })?;
        Ok(Self {
            origin: format!("http://127.0.0.1:{}", address.port()),
            token,
            address,
            stop,
            accept_thread: Some(accept_thread),
        })
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// Absolute media URL for `path`, in the shape the webview helper reconstructs.
    pub fn url_for(&self, path: &Path) -> String {
        format!(
            "{}/media/{}/{}",
            self.origin,
            self.token,
            percent_encode_path(&path.to_string_lossy())
        )
    }
}

impl Drop for LocalMediaServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Unblock the accept poll immediately instead of waiting out the interval.
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_millis(100));
        if let Some(worker) = self.accept_thread.take() {
            let _ = worker.join();
        }
    }
}

fn run_media_server(
    listener: TcpListener,
    token: String,
    roots: Arc<Vec<PathBuf>>,
    stop: Arc<AtomicBool>,
) {
    let token: Arc<str> = Arc::from(token);
    let mut handlers: Vec<JoinHandle<()>> = Vec::new();
    while !stop.load(Ordering::Acquire) {
        reap_finished(&mut handlers);
        match listener.accept() {
            Ok((mut client, _)) => {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                if handlers.len() >= MAX_CONNECTIONS {
                    // A library grid paints many media elements at once, and each opens its own
                    // connection. Wait for slots rather than refusing, which the webview would
                    // surface as a media error the user reads as "the video is broken".
                    let deadline = Instant::now() + CONNECTION_WAIT_TIMEOUT;
                    while handlers.len() >= MAX_CONNECTIONS && Instant::now() < deadline {
                        thread::sleep(ACCEPT_POLL_INTERVAL);
                        reap_finished(&mut handlers);
                    }
                }
                if handlers.len() >= MAX_CONNECTIONS {
                    let _ = write_status(&mut client, 503, "Service Unavailable");
                    continue;
                }
                let handler_token = Arc::clone(&token);
                let handler_roots = Arc::clone(&roots);
                match thread::Builder::new()
                    .name("soundar-media-connection".to_string())
                    .stack_size(HANDLER_STACK_BYTES)
                    .spawn(move || {
                        let _ = handle_connection(client, &handler_token, &handler_roots);
                    }) {
                    Ok(handler) => handlers.push(handler),
                    Err(_) => {
                        // The listener stays healthy; the webview retries the request.
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
            Err(_) => break,
        }
    }
    stop.store(true, Ordering::Release);
    for handler in handlers {
        let _ = handler.join();
    }
}

/// Join every handler that has already finished, freeing its connection slot.
fn reap_finished(handlers: &mut Vec<JoinHandle<()>>) {
    let mut index = 0;
    while index < handlers.len() {
        if handlers[index].is_finished() {
            let handler = handlers.swap_remove(index);
            let _ = handler.join();
        } else {
            index += 1;
        }
    }
}

fn handle_connection(stream: TcpStream, token: &str, roots: &[PathBuf]) -> io::Result<()> {
    stream.set_read_timeout(Some(SOCKET_TIMEOUT))?;
    stream.set_write_timeout(Some(SOCKET_TIMEOUT))?;
    stream.set_nodelay(true).ok();
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);

    let request = match read_request(&mut reader)? {
        Some(request) => request,
        None => return write_status(&mut writer, 400, "Bad Request"),
    };

    if request.method == "OPTIONS" {
        return write_preflight(&mut writer);
    }
    if request.method != "GET" && request.method != "HEAD" {
        return write_status(&mut writer, 405, "Method Not Allowed");
    }

    let target = match resolve_target(&request.target, token, roots) {
        Ok(target) => target,
        Err(status) => return write_status(&mut writer, status.0, status.1),
    };

    serve_file(
        &mut writer,
        &target,
        request.range.as_deref(),
        request.method == "HEAD",
    )
}

struct Request {
    method: String,
    target: String,
    range: Option<String>,
}

fn read_request(reader: &mut BufReader<TcpStream>) -> io::Result<Option<Request>> {
    let mut line = String::new();
    if reader
        .by_ref()
        .take(MAX_REQUEST_LINE_BYTES)
        .read_line(&mut line)?
        == 0
    {
        return Ok(None);
    }
    let mut parts = line.trim_end().split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Ok(None);
    };
    let method = method.to_ascii_uppercase();
    let target = target.to_string();

    let mut range = None;
    for _ in 0..MAX_HEADER_COUNT {
        let mut header = String::new();
        if reader
            .by_ref()
            .take(MAX_REQUEST_LINE_BYTES)
            .read_line(&mut header)?
            == 0
        {
            break;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.trim().eq_ignore_ascii_case("range") {
                range = Some(value.trim().to_string());
            }
        }
    }
    Ok(Some(Request {
        method,
        target,
        range,
    }))
}

/// Map `/media/<token>/<encoded-path>` onto a regular file inside one of `roots`.
fn resolve_target(
    target: &str,
    token: &str,
    roots: &[PathBuf],
) -> Result<PathBuf, (u16, &'static str)> {
    let path = target.split('?').next().unwrap_or(target);
    let rest = path.strip_prefix("/media/").ok_or((404, "Not Found"))?;
    let (supplied_token, encoded_path) = rest.split_once('/').ok_or((404, "Not Found"))?;
    if !constant_time_eq(supplied_token.as_bytes(), token.as_bytes()) {
        return Err((403, "Forbidden"));
    }
    let decoded = percent_decode(encoded_path).ok_or((400, "Bad Request"))?;
    if decoded.is_empty() {
        return Err((400, "Bad Request"));
    }
    let requested = PathBuf::from(decoded);
    if !requested.is_absolute() {
        return Err((403, "Forbidden"));
    }
    // Canonicalise before the prefix check so symlinks and `..` cannot escape the roots.
    let canonical = std::fs::canonicalize(&requested).map_err(|_| (404, "Not Found"))?;
    if !canonical.is_file() {
        return Err((404, "Not Found"));
    }
    if !roots.iter().any(|root| canonical.starts_with(root)) {
        return Err((403, "Forbidden"));
    }
    Ok(canonical)
}

fn serve_file(
    writer: &mut TcpStream,
    path: &Path,
    range: Option<&str>,
    head_only: bool,
) -> io::Result<()> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return write_status(writer, 404, "Not Found"),
    };
    let total = file.metadata()?.len();
    let content_type = content_type_for(path);

    let (start, end, partial) = match range.map(|value| parse_range(value, total)) {
        Some(Some(bounds)) => (bounds.0, bounds.1, true),
        // A syntactically valid but unsatisfiable range must not be answered with the whole file.
        Some(None) => {
            let headers = format!(
                "HTTP/1.1 416 Range Not Satisfiable\r\n\
                 Content-Range: bytes */{total}\r\n\
                 Accept-Ranges: bytes\r\n\
                 Access-Control-Allow-Origin: *\r\n\
                 Content-Length: 0\r\n\
                 Connection: close\r\n\r\n"
            );
            writer.write_all(headers.as_bytes())?;
            return writer.flush();
        }
        None => (0, total.saturating_sub(1), false),
    };

    let length = if total == 0 { 0 } else { end - start + 1 };
    let status = if partial {
        "206 Partial Content"
    } else {
        "200 OK"
    };
    let mut headers = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {length}\r\n\
         Accept-Ranges: bytes\r\n\
         Cache-Control: no-store\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Connection: close\r\n"
    );
    if partial {
        headers.push_str(&format!("Content-Range: bytes {start}-{end}/{total}\r\n"));
    }
    headers.push_str("\r\n");
    writer.write_all(headers.as_bytes())?;

    if head_only || length == 0 {
        return writer.flush();
    }

    file.seek(SeekFrom::Start(start))?;
    let mut remaining = length;
    let mut buffer = vec![0_u8; STREAM_CHUNK_BYTES];
    while remaining > 0 {
        let wanted = min(remaining as usize, buffer.len());
        let read = file.read(&mut buffer[..wanted])?;
        if read == 0 {
            break;
        }
        // The webview aborts in-flight range reads while scrubbing; a broken pipe is expected.
        if writer.write_all(&buffer[..read]).is_err() {
            return Ok(());
        }
        remaining -= read as u64;
    }
    writer.flush()
}

/// Parse a single-range `bytes=` header. `None` means the range is unsatisfiable.
fn parse_range(value: &str, total: u64) -> Option<(u64, u64)> {
    let spec = value.trim().strip_prefix("bytes=")?.trim();
    // Multi-range requests are answered with the first range, which browsers accept.
    let spec = spec.split(',').next()?.trim();
    let (start_text, end_text) = spec.split_once('-')?;
    let start_text = start_text.trim();
    let end_text = end_text.trim();
    if total == 0 {
        return None;
    }
    if start_text.is_empty() {
        // Suffix form: the last N bytes.
        let suffix: u64 = end_text.parse().ok()?;
        if suffix == 0 {
            return None;
        }
        let start = total.saturating_sub(suffix);
        return Some((start, total - 1));
    }
    let start: u64 = start_text.parse().ok()?;
    if start >= total {
        return None;
    }
    let end = if end_text.is_empty() {
        total - 1
    } else {
        min(end_text.parse::<u64>().ok()?, total - 1)
    };
    if end < start {
        return None;
    }
    Some((start, end))
}

fn content_type_for(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "ogg" | "opus" => "audio/ogg",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "vtt" => "text/vtt",
        "srt" => "application/x-subrip",
        "json" => "application/json",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

fn write_status(writer: &mut TcpStream, code: u16, reason: &str) -> io::Result<()> {
    let headers = format!(
        "HTTP/1.1 {code} {reason}\r\n\
         Content-Length: 0\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Connection: close\r\n\r\n"
    );
    writer.write_all(headers.as_bytes())?;
    writer.flush()
}

fn write_preflight(writer: &mut TcpStream) -> io::Result<()> {
    let headers = "HTTP/1.1 204 No Content\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Methods: GET, HEAD, OPTIONS\r\n\
         Access-Control-Allow-Headers: Range\r\n\
         Access-Control-Max-Age: 600\r\n\
         Content-Length: 0\r\n\
         Connection: close\r\n\r\n";
    writer.write_all(headers.as_bytes())?;
    writer.flush()
}

/// Percent-encode everything outside the unreserved set, keeping `/` readable in logs.
pub fn percent_encode_path(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                encoded.push(*byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let high = hex_value(*bytes.get(index + 1)?)?;
                let low = hex_value(*bytes.get(index + 2)?)?;
                decoded.push(high * 16 + low);
                index += 3;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |accumulator, (a, b)| accumulator | (a ^ b))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;
    use std::sync::atomic::AtomicU64;

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!(
                "soundar-media-server-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fixture(label: &str) -> (TestDirectory, PathBuf) {
        let directory = TestDirectory::new(label);
        let path = directory.path().join("clip.mp4");
        let mut file = File::create(&path).expect("create fixture");
        file.write_all(&(0..=255_u8).collect::<Vec<u8>>())
            .expect("write fixture");
        (directory, path)
    }

    fn serve(directory: &TestDirectory) -> LocalMediaServer {
        LocalMediaServer::start(vec![directory.path().to_path_buf()]).expect("start media server")
    }

    fn target_for(server: &LocalMediaServer, path: &Path) -> String {
        server
            .url_for(path)
            .strip_prefix(server.origin())
            .expect("origin prefix")
            .to_string()
    }

    fn request(server: &LocalMediaServer, raw: &str) -> (String, Vec<u8>) {
        let mut stream = TcpStream::connect(server.address).expect("connect to the media server");
        stream.write_all(raw.as_bytes()).expect("write request");
        stream.flush().expect("flush request");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).expect("read response");
        let split = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("headers terminate");
        (
            String::from_utf8_lossy(&response[..split]).to_string(),
            response[split + 4..].to_vec(),
        )
    }

    fn get(server: &LocalMediaServer, target: &str, extra: &str) -> (String, Vec<u8>) {
        request(
            server,
            &format!(
                "GET {target} HTTP/1.1\r\nHost: localhost\r\n{extra}Connection: close\r\n\r\n"
            ),
        )
    }

    #[test]
    fn serves_whole_files_and_advertises_range_support() {
        let (directory, path) = fixture("whole");
        let server = serve(&directory);
        let (headers, body) = get(&server, &target_for(&server, &path), "");
        assert!(headers.starts_with("HTTP/1.1 200 OK"), "{headers}");
        assert!(headers.contains("Content-Type: video/mp4"), "{headers}");
        assert!(headers.contains("Accept-Ranges: bytes"), "{headers}");
        assert_eq!(body.len(), 256);
    }

    #[test]
    fn answers_byte_ranges_so_the_webview_can_seek() {
        let (directory, path) = fixture("range");
        let server = serve(&directory);
        let (headers, body) = get(
            &server,
            &target_for(&server, &path),
            "Range: bytes=10-19\r\n",
        );
        assert!(
            headers.starts_with("HTTP/1.1 206 Partial Content"),
            "{headers}"
        );
        assert!(
            headers.contains("Content-Range: bytes 10-19/256"),
            "{headers}"
        );
        assert_eq!(body, (10..=19_u8).collect::<Vec<u8>>());
    }

    #[test]
    fn answers_open_ended_and_suffix_ranges() {
        let (directory, path) = fixture("suffix");
        let server = serve(&directory);
        let target = target_for(&server, &path);

        let (headers, body) = get(&server, &target, "Range: bytes=250-\r\n");
        assert!(
            headers.contains("Content-Range: bytes 250-255/256"),
            "{headers}"
        );
        assert_eq!(body.len(), 6);

        let (headers, body) = get(&server, &target, "Range: bytes=-4\r\n");
        assert!(
            headers.contains("Content-Range: bytes 252-255/256"),
            "{headers}"
        );
        assert_eq!(body, vec![252, 253, 254, 255]);
    }

    #[test]
    fn rejects_unsatisfiable_ranges_instead_of_sending_the_whole_file() {
        let (directory, path) = fixture("unsatisfiable");
        let server = serve(&directory);
        let (headers, body) = get(
            &server,
            &target_for(&server, &path),
            "Range: bytes=900-999\r\n",
        );
        assert!(headers.starts_with("HTTP/1.1 416"), "{headers}");
        assert!(headers.contains("Content-Range: bytes */256"), "{headers}");
        assert!(body.is_empty());
    }

    #[test]
    fn head_requests_report_length_without_a_body() {
        let (directory, path) = fixture("head");
        let server = serve(&directory);
        let (headers, body) = request(
            &server,
            &format!(
                "HEAD {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
                target_for(&server, &path)
            ),
        );
        assert!(headers.contains("Content-Length: 256"), "{headers}");
        assert!(body.is_empty());
    }

    #[test]
    fn rejects_a_wrong_token() {
        let (directory, path) = fixture("token");
        let server = serve(&directory);
        let target = format!(
            "/media/{}/{}",
            "0".repeat(server.token().len()),
            percent_encode_path(&path.to_string_lossy())
        );
        let (headers, _) = get(&server, &target, "");
        assert!(headers.starts_with("HTTP/1.1 403"), "{headers}");
    }

    #[test]
    fn refuses_paths_outside_the_configured_roots() {
        let (directory, _) = fixture("outside-root");
        let outside = TestDirectory::new("outside");
        let secret = outside.path().join("secret.mp4");
        File::create(&secret).expect("create outside file");
        let server = serve(&directory);
        let target = format!(
            "/media/{}/{}",
            server.token(),
            percent_encode_path(&secret.to_string_lossy())
        );
        let (headers, _) = get(&server, &target, "");
        assert!(headers.starts_with("HTTP/1.1 403"), "{headers}");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_traversal_through_a_symlink_out_of_the_roots() {
        let (directory, _) = fixture("symlink-root");
        let outside = TestDirectory::new("symlink-target");
        let secret = outside.path().join("secret.mp4");
        File::create(&secret).expect("create outside file");
        let link = directory.path().join("escape.mp4");
        std::os::unix::fs::symlink(&secret, &link).expect("symlink");
        let server = serve(&directory);
        let target = format!(
            "/media/{}/{}",
            server.token(),
            percent_encode_path(&link.to_string_lossy())
        );
        let (headers, _) = get(&server, &target, "");
        assert!(headers.starts_with("HTTP/1.1 403"), "{headers}");
    }

    #[test]
    fn refuses_relative_and_dot_dot_paths() {
        let (directory, _) = fixture("relative");
        let server = serve(&directory);
        let (headers, _) = get(
            &server,
            &format!("/media/{}/etc/passwd", server.token()),
            "",
        );
        assert!(headers.starts_with("HTTP/1.1 403"), "{headers}");
    }

    #[test]
    fn round_trips_paths_containing_spaces_and_unicode() {
        let directory = TestDirectory::new("unicode");
        let path = directory.path().join("Anejo clip #1 \u{e9}.mp4");
        File::create(&path).expect("create fixture");
        let server = serve(&directory);
        let target = target_for(&server, &path);
        assert!(!target.contains(' '), "{target}");
        assert!(!target.contains('#'), "{target}");
        let (headers, _) = get(&server, &target, "");
        assert!(headers.starts_with("HTTP/1.1 200 OK"), "{headers}");
    }

    #[test]
    fn refuses_to_start_without_a_readable_root() {
        // The shell creates the export root before starting; if that ever regresses, starting must
        // fail loudly rather than serving from an unexpected place.
        let missing = env::temp_dir().join(format!("soundar-media-absent-{}", std::process::id()));
        let _ = fs::remove_dir_all(&missing);
        assert!(LocalMediaServer::start(vec![missing]).is_err());
    }

    #[test]
    fn rejects_methods_other_than_get_and_head() {
        let (directory, path) = fixture("method");
        let server = serve(&directory);
        let (headers, _) = request(
            &server,
            &format!(
                "POST {} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                target_for(&server, &path)
            ),
        );
        assert!(headers.starts_with("HTTP/1.1 405"), "{headers}");
    }
}
