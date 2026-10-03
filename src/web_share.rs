//! Explicit, temporary browser access to a selected set of files.
//! Paths are opened once without following symlinks; downloads use those held
//! descriptors, and refuse files whose size or modification time has changed.
use crate::transfer::{Selection, Source};
use axum::{
    body::Body,
    extract::{ConnectInfo, DefaultBodyLimit, Path, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    serve::ListenerExt,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::File,
    future::Future,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::Path as FsPath,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll},
    time::{Duration, Instant, SystemTime},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::oneshot,
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

const MAX_FILES: usize = 512;
const MAX_SESSIONS: usize = 32;
const CONSENT_TIMEOUT: Duration = Duration::from_secs(60);
const SESSION_TIMEOUT: Duration = Duration::from_secs(300);
const CHUNK_SIZE: usize = 64 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SharedFile {
    pub id: String,
    pub name: String,
    pub size: u64,
}

pub enum ShareEvent {
    DownloadRequest(PendingDownload),
    Started {
        id: String,
        file_name: String,
        total_bytes: u64,
    },
    Progress {
        id: String,
        bytes_sent: u64,
        total_bytes: u64,
    },
    Finished {
        id: String,
        file_name: String,
    },
    Failed {
        id: String,
        file_name: String,
        error: String,
    },
}

pub struct PendingDownload {
    ip: IpAddr,
    files: Vec<SharedFile>,
    decision: oneshot::Sender<bool>,
    cancellation: CancellationToken,
}
impl PendingDownload {
    pub fn ip(&self) -> IpAddr {
        self.ip
    }
    pub fn files(&self) -> &[SharedFile] {
        &self.files
    }
    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }
    pub fn accept(self) {
        let _ = self.decision.send(true);
    }
    pub fn decline(self) {
        let _ = self.decision.send(false);
    }
    #[cfg(test)]
    pub(crate) fn fixture(ip: IpAddr, files: Vec<SharedFile>) -> (Self, oneshot::Receiver<bool>) {
        let (decision, receiver) = oneshot::channel();
        (
            Self {
                ip,
                files,
                decision,
                cancellation: CancellationToken::new(),
            },
            receiver,
        )
    }
}

pub struct BrowserShare {
    host: Arc<Host>,
    task: Option<JoinHandle<()>>,
    pub urls: Vec<String>,
}

// Axum spawns independent connection tasks. Cancel their socket operations as
// well as the response bodies so a browser that stops reading cannot retain a
// selected file descriptor after the share has been stopped or dropped.
struct ShareListener {
    listener: tokio::net::TcpListener,
    stop: CancellationToken,
}
struct RevocableConnection {
    stream: tokio::net::TcpStream,
    cancelled: Pin<Box<dyn Future<Output = ()> + Send>>,
    revoked: bool,
}
impl axum::serve::Listener for ShareListener {
    type Io = RevocableConnection;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        let (stream, address) = axum::serve::Listener::accept(&mut self.listener).await;
        (
            RevocableConnection {
                stream,
                cancelled: Box::pin(self.stop.clone().cancelled_owned()),
                revoked: false,
            },
            address,
        )
    }
    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}
impl RevocableConnection {
    fn check_cancelled(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        self.revoked = self.revoked || self.cancelled.as_mut().poll(cx).is_ready();
        if self.revoked {
            Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "Browser sharing stopped",
            ))
        } else {
            Ok(())
        }
    }
}
impl AsyncRead for RevocableConnection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.check_cancelled(cx)?;
        Pin::new(&mut self.stream).poll_read(cx, buffer)
    }
}
impl AsyncWrite for RevocableConnection {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.check_cancelled(cx)?;
        Pin::new(&mut self.stream).poll_write(cx, buffer)
    }
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffers: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        self.check_cancelled(cx)?;
        Pin::new(&mut self.stream).poll_write_vectored(cx, buffers)
    }
    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.check_cancelled(cx)?;
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.check_cancelled(cx)?;
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

struct Host {
    capability: String,
    items: Vec<Arc<Item>>,
    events: async_channel::Sender<ShareEvent>,
    auto_accept: AtomicBool,
    stop: CancellationToken,
    sessions: Mutex<HashMap<String, Session>>,
    withdrawn: Mutex<HashMap<(IpAddr, String), Instant>>,
}
struct Session {
    peer: IpAddr,
    request_id: String,
    accepted: bool,
    active: usize,
    touched: Instant,
    cancellation: CancellationToken,
}
struct Item {
    info: SharedFile,
    source: Data,
}
enum Data {
    File {
        file: Arc<File>,
        modified: Option<SystemTime>,
    },
    Text(Arc<[u8]>),
}

impl Item {
    fn unchanged(&self) -> io::Result<()> {
        if let Data::File { file, modified } = &self.source {
            let metadata = file.metadata()?;
            if !metadata.is_file()
                || metadata.len() != self.info.size
                || metadata.modified().ok() != *modified
            {
                return Err(io::Error::other(
                    "The selected file changed. Stop sharing and select it again.",
                ));
            }
        }
        Ok(())
    }
    fn read_chunk(&self, offset: u64) -> io::Result<Vec<u8>> {
        self.unchanged()?;
        let length = (self.info.size.saturating_sub(offset)).min(CHUNK_SIZE as u64) as usize;
        let mut buffer = vec![0; length];
        let read = match &self.source {
            Data::Text(text) => {
                buffer.copy_from_slice(&text[offset as usize..offset as usize + length]);
                length
            }
            Data::File { file, .. } => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::FileExt;
                    file.read_at(&mut buffer, offset)?
                }
                #[cfg(not(unix))]
                {
                    use std::os::windows::fs::FileExt;
                    file.seek_read(&mut buffer, offset)?
                }
            }
        };
        if read == 0 && length != 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "The selected file ended early",
            ));
        }
        buffer.truncate(read);
        self.unchanged()?;
        Ok(buffer)
    }
}

fn snapshot(items: Vec<Selection>) -> Result<Vec<Arc<Item>>, String> {
    if items.is_empty() || items.len() > MAX_FILES {
        return Err(format!(
            "Select between 1 and {MAX_FILES} files or messages to share."
        ));
    }
    items
        .into_iter()
        .map(|item| {
            let (source, size) = match item.source {
                Source::Text(text) => {
                    let bytes: Arc<[u8]> = text.into_bytes().into();
                    let size = bytes.len() as u64;
                    (Data::Text(bytes), size)
                }
                Source::File(path) => {
                    let file = open_verified(&path).map_err(|e| format!("{}: {e}", item.name))?;
                    let metadata = file.metadata().map_err(|e| e.to_string())?;
                    if !metadata.is_file() {
                        return Err(format!("{} is not a regular file.", item.name));
                    }
                    (
                        Data::File {
                            file: Arc::new(file),
                            modified: metadata.modified().ok(),
                        },
                        metadata.len(),
                    )
                }
            };
            Ok(Arc::new(Item {
                info: SharedFile {
                    id: random_id(),
                    name: item.name,
                    size,
                },
                source,
            }))
        })
        .collect()
}

#[cfg(unix)]
fn open_verified(path: &FsPath) -> io::Result<File> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    };
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut names = Vec::new();
    for part in absolute.components() {
        match part {
            std::path::Component::RootDir | std::path::Component::CurDir => {}
            std::path::Component::Normal(name) => names.push(name),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Parent path components are not allowed",
                ))
            }
        }
    }
    if names.is_empty() {
        return Err(io::Error::other("Select a regular file"));
    }
    let mut directory = File::open("/")?;
    for (index, name) in names.iter().enumerate() {
        let name = std::ffi::CString::new(name.as_bytes())
            .map_err(|_| io::Error::other("Invalid file path"))?;
        let final_component = index + 1 == names.len();
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | if final_component {
                libc::O_NONBLOCK
            } else {
                libc::O_DIRECTORY
            };
        // Each held parent descriptor prevents a path component from being swapped
        // for a symlink between checking the selection and opening its contents.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let opened = unsafe { File::from_raw_fd(fd) };
        if final_component {
            if !opened.metadata()?.is_file() {
                return Err(io::Error::other("Select a regular file"));
            }
            return Ok(opened);
        }
        directory = opened;
    }
    unreachable!()
}
#[cfg(not(unix))]
fn open_verified(_path: &FsPath) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Verified browser file sharing currently requires Unix",
    ))
}
fn random_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}
fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

impl BrowserShare {
    pub async fn start(
        items: Vec<Selection>,
        auto_accept: bool,
        events: async_channel::Sender<ShareEvent>,
    ) -> Result<Self, String> {
        let items = tokio::task::spawn_blocking(move || snapshot(items))
            .await
            .map_err(|e| e.to_string())??;
        let host = Arc::new(Host {
            capability: random_id(),
            items,
            events,
            auto_accept: AtomicBool::new(auto_accept),
            stop: CancellationToken::new(),
            sessions: Mutex::new(HashMap::new()),
            withdrawn: Mutex::new(HashMap::new()),
        });
        let app = Router::new()
            .route("/{cap}/", get(page))
            .route(
                "/{cap}/prepare",
                post(prepare).layer(DefaultBodyLimit::max(1024)),
            )
            .route("/{cap}/withdraw/{request}", post(withdraw))
            .route("/{cap}/file/{session}/{id}", get(download))
            .with_state(host.clone());
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0))
            .await
            .map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let mut addresses = localsend_rs::discovery::local_ipv4_addresses().unwrap_or_default();
        addresses.push(Ipv4Addr::LOCALHOST);
        let urls = addresses
            .into_iter()
            .map(|ip| format!("http://{ip}:{port}/{}/", host.capability))
            .collect();
        let state = host.clone();
        let task = tokio::spawn(async move {
            let shutdown = state.stop.clone();
            // TapIo preserves Axum's SocketAddr connection metadata for a
            // custom listener; the transport itself handles cancellation.
            let listener = ShareListener {
                listener,
                stop: shutdown.clone(),
            }
            .tap_io(|_| {});
            let server = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown.cancelled_owned());
            let sweep =
                async {
                    let mut interval = tokio::time::interval(Duration::from_secs(10));
                    loop {
                        interval.tick().await;
                        state.sessions.lock().unwrap().retain(|_, session| {
                            if session.active == 0 && session.touched.elapsed() > SESSION_TIMEOUT {
                                session.cancellation.cancel();
                                false
                            } else {
                                true
                            }
                        });
                        state.withdrawn.lock().unwrap().retain(|_, at| {
                            at.elapsed() < CONSENT_TIMEOUT + Duration::from_secs(10)
                        });
                    }
                };
            tokio::select! { _ = server => {}, _ = sweep => {} }
        });
        Ok(Self {
            host,
            task: Some(task),
            urls,
        })
    }
    pub fn set_auto_accept(&self, enabled: bool) {
        self.host.auto_accept.store(enabled, Ordering::Release);
    }
    pub fn busy(&self) -> bool {
        self.host
            .sessions
            .lock()
            .unwrap()
            .values()
            .any(|s| !s.accepted || s.active > 0)
    }
    fn revoke(&self) {
        let mut sessions = self.host.sessions.lock().unwrap();
        self.host.stop.cancel();
        for session in sessions.values() {
            session.cancellation.cancel();
        }
        sessions.retain(|_, session| session.active > 0);
    }
    pub async fn stop(mut self) {
        self.revoke();
        if let Some(mut task) = self.task.take() {
            if tokio::time::timeout(Duration::from_secs(3), &mut task)
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
            }
        }
    }
}
impl Drop for BrowserShare {
    fn drop(&mut self) {
        self.revoke();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

fn authorized(host: &Host, cap: &str, headers: &HeaderMap) -> bool {
    if host.stop.is_cancelled() || cap != host.capability {
        return false;
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        let Some(authority) = headers
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
        else {
            return false;
        };
        if origin.to_str().ok() != Some(format!("http://{authority}").as_str()) {
            return false;
        }
    }
    !headers
        .get("sec-fetch-site")
        .is_some_and(|value| value == "cross-site")
}
async fn page(State(host): State<Arc<Host>>, Path(cap): Path<String>) -> Response {
    if host.stop.is_cancelled() || cap != host.capability {
        return StatusCode::NOT_FOUND.into_response();
    }
    ([("cache-control", "no-store"), ("referrer-policy", "no-referrer"), ("x-content-type-options", "nosniff"),
       ("x-frame-options", "DENY"), ("content-security-policy", "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'; form-action 'none'")],
     Html(include_str!("../assets/web-share.html"))).into_response()
}
#[derive(Deserialize)]
struct Prepare {
    #[serde(rename = "requestId")]
    request_id: String,
}
#[derive(Serialize, Deserialize)]
struct Manifest {
    session: String,
    files: Vec<SharedFile>,
}
struct PendingGuard {
    host: Arc<Host>,
    token: String,
    armed: bool,
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        if self.armed {
            if let Some(session) = self.host.sessions.lock().unwrap().remove(&self.token) {
                session.cancellation.cancel();
            }
        }
    }
}
async fn prepare(
    State(host): State<Arc<Host>>,
    Path(cap): Path<String>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<Prepare>,
) -> Response {
    if !authorized(&host, &cap, &headers) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !valid_id(&request.request_id) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let token = random_id();
    let cancellation = CancellationToken::new();
    {
        let mut withdrawn = host.withdrawn.lock().unwrap();
        withdrawn.retain(|_, at| at.elapsed() < CONSENT_TIMEOUT + Duration::from_secs(10));
        if withdrawn.contains_key(&(peer.ip(), request.request_id.clone())) {
            return StatusCode::GONE.into_response();
        }
        let mut sessions = host.sessions.lock().unwrap();
        if sessions.len() >= MAX_SESSIONS
            || sessions
                .values()
                .any(|s| s.peer == peer.ip() && s.request_id == request.request_id)
        {
            return StatusCode::TOO_MANY_REQUESTS.into_response();
        }
        sessions.insert(
            token.clone(),
            Session {
                peer: peer.ip(),
                request_id: request.request_id,
                accepted: false,
                active: 0,
                touched: Instant::now(),
                cancellation: cancellation.clone(),
            },
        );
    }
    let mut guard = PendingGuard {
        host: host.clone(),
        token: token.clone(),
        armed: true,
    };
    let files = host
        .items
        .iter()
        .map(|item| item.info.clone())
        .collect::<Vec<_>>();
    if !host.auto_accept.load(Ordering::Acquire) {
        let (decision, answer) = oneshot::channel();
        let pending = PendingDownload {
            ip: peer.ip(),
            files: files.clone(),
            decision,
            cancellation: cancellation.clone(),
        };
        if host
            .events
            .send(ShareEvent::DownloadRequest(pending))
            .await
            .is_err()
        {
            return StatusCode::FORBIDDEN.into_response();
        }
        let approved = tokio::select! {
            biased;
            _ = host.stop.cancelled() => false,
            _ = cancellation.cancelled() => false,
            answer = tokio::time::timeout(CONSENT_TIMEOUT, answer) => matches!(answer, Ok(Ok(true))),
        };
        if !approved {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    {
        let mut sessions = host.sessions.lock().unwrap();
        let Some(session) = sessions
            .get_mut(&token)
            .filter(|s| !s.cancellation.is_cancelled())
        else {
            return StatusCode::GONE.into_response();
        };
        if host.stop.is_cancelled() {
            return StatusCode::GONE.into_response();
        }
        session.accepted = true;
        session.touched = Instant::now();
    }
    guard.armed = false;
    (
        [("cache-control", "no-store")],
        Json(Manifest {
            session: token,
            files,
        }),
    )
        .into_response()
}
async fn withdraw(
    State(host): State<Arc<Host>>,
    Path((cap, request)): Path<(String, String)>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if !authorized(&host, &cap, &headers) || !valid_id(&request) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mut withdrawn = host.withdrawn.lock().unwrap();
    withdrawn.retain(|_, at| at.elapsed() < CONSENT_TIMEOUT + Duration::from_secs(10));
    let key = (peer.ip(), request.clone());
    let mut sessions = host.sessions.lock().unwrap();
    let matching = sessions
        .iter()
        .find(|(_, session)| session.peer == peer.ip() && session.request_id == request)
        .map(|(token, _)| token.clone());
    if withdrawn.len() < 128 || withdrawn.contains_key(&key) {
        withdrawn.insert(key, Instant::now());
    } else if matching.is_none() {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    if let Some(token) = matching {
        let session = sessions.get(&token).unwrap();
        session.cancellation.cancel();
        if session.active == 0 {
            sessions.remove(&token);
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

struct DownloadGuard {
    host: Arc<Host>,
    token: String,
    cancellation: CancellationToken,
    id: String,
    file_name: String,
    total: u64,
    terminal: bool,
    last_progress: Instant,
}
impl DownloadGuard {
    fn progress(&mut self, bytes: u64) -> io::Result<()> {
        let mut sessions = self.host.sessions.lock().unwrap();
        let Some(session) = sessions
            .get_mut(&self.token)
            .filter(|s| !s.cancellation.is_cancelled())
        else {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Download cancelled",
            ));
        };
        if self.host.stop.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Sharing stopped",
            ));
        }
        session.touched = Instant::now();
        if self.last_progress.elapsed() >= Duration::from_millis(100) || bytes == self.total {
            self.last_progress = Instant::now();
            let _ = self.host.events.try_send(ShareEvent::Progress {
                id: self.id.clone(),
                bytes_sent: bytes,
                total_bytes: self.total,
            });
        }
        if bytes == self.total {
            self.terminal = true;
            let _ = self.host.events.try_send(ShareEvent::Finished {
                id: self.id.clone(),
                file_name: self.file_name.clone(),
            });
        }
        Ok(())
    }
    fn fail(&mut self, error: &str) {
        if !self.terminal {
            self.terminal = true;
            let _ = self.host.events.try_send(ShareEvent::Failed {
                id: self.id.clone(),
                file_name: self.file_name.clone(),
                error: error.into(),
            });
        }
    }
}
impl Drop for DownloadGuard {
    fn drop(&mut self) {
        self.fail("The browser disconnected or the download was cancelled.");
        let mut sessions = self.host.sessions.lock().unwrap();
        if let Some(session) = sessions.get_mut(&self.token) {
            session.active = session.active.saturating_sub(1);
            if session.active == 0 && session.cancellation.is_cancelled() {
                sessions.remove(&self.token);
            }
        }
    }
}
struct StreamState {
    item: Arc<Item>,
    offset: u64,
    guard: DownloadGuard,
    failed: bool,
}

async fn download(
    State(host): State<Arc<Host>>,
    Path((cap, token, id)): Path<(String, String, String)>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    method: Method,
) -> Response {
    if !authorized(&host, &cap, &headers) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(item) = host.items.iter().find(|item| item.info.id == id).cloned() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut sessions = host.sessions.lock().unwrap();
    let Some(session) = sessions
        .get_mut(&token)
        .filter(|s| s.accepted && s.peer == peer.ip() && !s.cancellation.is_cancelled())
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if host.stop.is_cancelled() {
        return StatusCode::GONE.into_response();
    }
    if item.unchanged().is_err() {
        return (
            StatusCode::CONFLICT,
            "The shared file changed. Ask the sender to share it again.",
        )
            .into_response();
    }
    session.touched = Instant::now();
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    response_headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&item.info.size.to_string()).unwrap(),
    );
    response_headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&content_disposition(&item.info.name)).unwrap(),
    );
    response_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response_headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("none"));
    response_headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    if method == Method::HEAD {
        return (response_headers, Body::empty()).into_response();
    }
    if session.active >= 8 {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    session.active += 1;
    let mut guard = DownloadGuard {
        host: host.clone(),
        token,
        cancellation: session.cancellation.clone(),
        id: random_id(),
        file_name: item.info.name.clone(),
        total: item.info.size,
        terminal: false,
        last_progress: Instant::now() - Duration::from_secs(1),
    };
    let _ = host.events.try_send(ShareEvent::Started {
        id: guard.id.clone(),
        file_name: guard.file_name.clone(),
        total_bytes: guard.total,
    });
    drop(sessions);
    if item.info.size == 0 {
        let _ = guard.progress(0);
        return (response_headers, Body::empty()).into_response();
    }
    let stream = futures_util::stream::unfold(
        StreamState {
            item,
            offset: 0,
            guard,
            failed: false,
        },
        |mut state| async move {
            if state.failed || state.offset == state.item.info.size {
                return None;
            }
            let item = state.item.clone();
            let offset = state.offset;
            let result = tokio::select! {
                biased;
                _ = state.guard.host.stop.cancelled() => Err(io::Error::new(io::ErrorKind::Interrupted, "Sharing stopped")),
                _ = state.guard.cancellation.cancelled() => Err(io::Error::new(io::ErrorKind::Interrupted, "Download cancelled")),
                result = tokio::task::spawn_blocking(move || item.read_chunk(offset)) => result.map_err(io::Error::other).and_then(|result| result),
            };
            let result = result.and_then(|bytes| {
                state.offset += bytes.len() as u64;
                state.guard.progress(state.offset)?;
                Ok(bytes)
            });
            if let Err(error) = &result {
                state.failed = true;
                state.guard.fail(&error.to_string());
            }
            Some((result, state))
        },
    );
    (response_headers, Body::from_stream(stream)).into_response()
}

fn content_disposition(name: &str) -> String {
    let leaf = name
        .rsplit(['/', '\\'])
        .next()
        .filter(|leaf| !leaf.is_empty() && *leaf != "." && *leaf != "..")
        .unwrap_or("download");
    let fallback: String = leaf
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded: String = leaf
        .as_bytes()
        .iter()
        .map(|&byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect();
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }
    async fn next_event(events: &async_channel::Receiver<ShareEvent>) -> ShareEvent {
        tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("share event timed out")
            .unwrap()
    }
    async fn setup(
        items: Vec<Selection>,
        auto: bool,
    ) -> (
        BrowserShare,
        async_channel::Receiver<ShareEvent>,
        reqwest::Client,
        String,
    ) {
        let (tx, rx) = async_channel::unbounded();
        let share = BrowserShare::start(items, auto, tx).await.unwrap();
        let url = share
            .urls
            .iter()
            .find(|url| url.contains("127.0.0.1"))
            .unwrap()
            .clone();
        (share, rx, client(), url)
    }
    async fn prepare_auto(client: &reqwest::Client, url: &str) -> (String, Manifest) {
        let request = random_id();
        let response = client
            .post(format!("{url}prepare"))
            .json(&serde_json::json!({"requestId":request}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        (request, response.json().await.unwrap())
    }
    fn file_selection(path: &FsPath, name: &str) -> Selection {
        Selection {
            name: name.into(),
            size: std::fs::metadata(path).unwrap().len(),
            source: Source::File(path.into()),
        }
    }
    fn download_url(url: &str, manifest: &Manifest, index: usize) -> String {
        format!(
            "{url}file/{}/{}",
            manifest.session, manifest.files[index].id
        )
    }

    #[tokio::test]
    async fn sharing_requires_capability_and_native_consent_by_default() {
        let (share, events, client, url) =
            setup(vec![Selection::text("private".into())], false).await;
        let base = reqwest::Url::parse(&url).unwrap().join("/wrong/").unwrap();
        assert_eq!(
            client.get(base).send().await.unwrap().status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            client
                .post(format!("{url}prepare"))
                .header("origin", "http://elsewhere.invalid")
                .json(&serde_json::json!({"requestId":random_id()}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        let request = tokio::spawn({
            let client = client.clone();
            let url = url.clone();
            async move {
                client
                    .post(format!("{url}prepare"))
                    .json(&serde_json::json!({"requestId":random_id()}))
                    .send()
                    .await
                    .unwrap()
            }
        });
        match next_event(&events).await {
            ShareEvent::DownloadRequest(pending) => {
                assert_eq!(pending.files().len(), 1);
                drop(pending);
            }
            _ => panic!("Expected native consent"),
        }
        assert_eq!(request.await.unwrap().status(), StatusCode::FORBIDDEN);
        assert!(!share.busy());
        share.stop().await;
    }

    #[tokio::test]
    async fn accepted_file_and_text_downloads_use_only_the_pinned_selection() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("chosen.txt");
        std::fs::write(&source, b"original").unwrap();
        std::fs::write(dir.path().join("unselected.txt"), b"not shared").unwrap();
        let items = vec![
            file_selection(&source, "folder/青い report.txt"),
            Selection::text("Hello, 世界!".into()),
        ];
        let (share, events, client, url) = setup(items, false).await;
        let request = tokio::spawn({
            let client = client.clone();
            let url = url.clone();
            async move {
                client
                    .post(format!("{url}prepare"))
                    .json(&serde_json::json!({"requestId":random_id()}))
                    .send()
                    .await
                    .unwrap()
            }
        });
        match next_event(&events).await {
            ShareEvent::DownloadRequest(pending) => pending.accept(),
            _ => panic!("Expected consent"),
        }
        let manifest: Manifest = request.await.unwrap().json().await.unwrap();
        std::fs::rename(&source, dir.path().join("old.txt")).unwrap();
        std::fs::write(&source, b"replacement").unwrap();
        let response = client
            .get(download_url(&url, &manifest, 0))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.content_length(), Some(8));
        let disposition = response.headers()[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap();
        assert!(disposition.starts_with("attachment;"));
        assert!(disposition.contains("filename*=UTF-8''%E9%9D%92%E3%81%84%20report.txt"));
        assert_eq!(response.bytes().await.unwrap().as_ref(), b"original");
        assert_eq!(
            client
                .get(download_url(&url, &manifest, 1))
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "Hello, 世界!"
        );
        assert_eq!(
            client
                .get(format!("{url}file/{}/unselected.txt", manifest.session))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        share.stop().await;
    }

    #[tokio::test]
    async fn file_changes_are_refused_and_concurrent_reads_have_independent_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        let bytes: Vec<_> = (0..400_000).map(|index| (index % 251) as u8).collect();
        std::fs::write(&path, &bytes).unwrap();
        let (share, _events, client, url) =
            setup(vec![file_selection(&path, "data.bin")], true).await;
        let (_, manifest) = prepare_auto(&client, &url).await;
        let target = download_url(&url, &manifest, 0);
        let first = client.get(&target).send();
        let second = client.get(&target).send();
        let (first, second) = tokio::join!(first, second);
        let (first, second) = tokio::join!(first.unwrap().bytes(), second.unwrap().bytes());
        assert_eq!(first.unwrap().as_ref(), bytes);
        assert_eq!(second.unwrap().as_ref(), bytes);
        std::fs::write(&path, b"changed").unwrap();
        assert_eq!(
            client.get(&target).send().await.unwrap().status(),
            StatusCode::CONFLICT
        );
        share.stop().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinks_in_the_selected_file_or_parent_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), b"secret").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), dir.path().join("file-link"))
            .unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("directory-link")).unwrap();
        for path in [
            dir.path().join("file-link"),
            dir.path().join("directory-link/secret"),
        ] {
            let (events, _) = async_channel::unbounded();
            let result =
                BrowserShare::start(vec![file_selection(&path, "selected")], true, events).await;
            assert!(result.is_err());
        }
    }

    #[tokio::test]
    async fn pending_withdrawal_cancels_the_dialog_and_rejects_late_approval() {
        let (share, events, client, url) =
            setup(vec![Selection::text("private".into())], false).await;
        let id = random_id();
        let request = tokio::spawn({
            let client = client.clone();
            let url = url.clone();
            let id = id.clone();
            async move {
                client
                    .post(format!("{url}prepare"))
                    .json(&serde_json::json!({"requestId":id}))
                    .send()
                    .await
                    .unwrap()
            }
        });
        let pending = match next_event(&events).await {
            ShareEvent::DownloadRequest(pending) => pending,
            _ => panic!("Expected consent"),
        };
        let cancellation = pending.cancellation();
        assert_eq!(
            client
                .post(format!("{url}withdraw/{id}"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        assert!(cancellation.is_cancelled());
        pending.accept();
        assert_eq!(request.await.unwrap().status(), StatusCode::FORBIDDEN);
        assert!(!share.busy());
        let early = random_id();
        client
            .post(format!("{url}withdraw/{early}"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            client
                .post(format!("{url}prepare"))
                .json(&serde_json::json!({"requestId":early}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::GONE
        );
        share.set_auto_accept(true);
        let (_, manifest) = prepare_auto(&client, &url).await;
        assert_eq!(manifest.files.len(), 1);
        share.stop().await;
    }

    #[tokio::test]
    async fn head_and_empty_files_are_valid_and_stopping_revokes_the_link() {
        let (share, events, client, url) = setup(vec![Selection::text(String::new())], true).await;
        let (_, manifest) = prepare_auto(&client, &url).await;
        let target = download_url(&url, &manifest, 0);
        assert_eq!(
            client.head(&target).send().await.unwrap().status(),
            StatusCode::OK
        );
        assert!(events.try_recv().is_err());
        assert_eq!(
            client
                .get(&target)
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
                .len(),
            0
        );
        assert!(matches!(
            next_event(&events).await,
            ShareEvent::Started { .. }
        ));
        assert!(matches!(
            next_event(&events).await,
            ShareEvent::Progress { bytes_sent: 0, .. }
        ));
        assert!(matches!(
            next_event(&events).await,
            ShareEvent::Finished { .. }
        ));
        share.stop().await;
        assert!(client.get(&target).send().await.is_err());
        assert!(client.get(&url).send().await.is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stopping_interrupts_a_real_stream_and_drop_revokes_the_listener() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.bin");
        File::create(&path)
            .unwrap()
            .set_len(64 * 1024 * 1024)
            .unwrap();
        let (share, _events, client, url) =
            setup(vec![file_selection(&path, "large.bin")], true).await;
        let (request_id, manifest) = prepare_auto(&client, &url).await;
        let response = client
            .get(download_url(&url, &manifest, 0))
            .send()
            .await
            .unwrap();
        let mut stream = response.bytes_stream();
        let first = stream.next().await.unwrap().unwrap().len();
        assert!(first > 0);
        assert_eq!(
            client
                .post(format!("{url}withdraw/{request_id}"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        let mut read = first as u64;
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(chunk) => read += chunk.len() as u64,
                Err(_) => break,
            }
        }
        assert!(
            read < 64 * 1024 * 1024,
            "The revoked stream must not finish"
        );
        // Disconnect revokes this browser's session while the published link stays available.
        let (_, manifest) = prepare_auto(&client, &url).await;
        let response = client
            .get(download_url(&url, &manifest, 0))
            .send()
            .await
            .unwrap();
        let mut stream = response.bytes_stream();
        let first = stream.next().await.unwrap().unwrap().len();
        let host = Arc::downgrade(&share.host);
        tokio::time::timeout(Duration::from_secs(2), share.stop())
            .await
            .expect("Stopping must close an unread response without waiting for its reader");
        assert!(
            host.upgrade().is_none(),
            "Stopped connections must release shared files"
        );
        let mut read = first as u64;
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(chunk) => read += chunk.len() as u64,
                Err(_) => break,
            }
        }
        assert!(
            read < 64 * 1024 * 1024,
            "Stopping the share must interrupt response streaming"
        );
        let (share, _, client, url) = setup(vec![Selection::text("drop".into())], true).await;
        drop(share);
        tokio::task::yield_now().await;
        match client.get(&url).send().await {
            Err(_) => {}
            Ok(response) => assert!(!response.status().is_success()),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stopping_closes_stalled_and_idle_connections_and_releases_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stalled.bin");
        File::create(&path)
            .unwrap()
            .set_len(64 * 1024 * 1024)
            .unwrap();
        let (share, events, client, url) =
            setup(vec![file_selection(&path, "stalled.bin")], true).await;
        let (_, manifest) = prepare_auto(&client, &url).await;
        let target = reqwest::Url::parse(&download_url(&url, &manifest, 0)).unwrap();
        let address = ("127.0.0.1", target.port().unwrap());
        let mut idle = tokio::net::TcpStream::connect(address).await.unwrap();
        let mut stalled = tokio::net::TcpStream::connect(address).await.unwrap();
        stalled
            .write_all(
                format!(
                    "GET {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
                    target.path(),
                    target.port().unwrap()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        // Never read the response. Once streaming starts, allow the sender to
        // fill its socket buffers, where body cancellation alone cannot wake it.
        loop {
            if matches!(next_event(&events).await, ShareEvent::Progress { bytes_sent, .. } if bytes_sent > 0)
            {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let host = Arc::downgrade(&share.host);
        let file = match &share.host.items[0].source {
            Data::File { file, .. } => Arc::downgrade(file),
            _ => panic!("expected a selected file"),
        };
        tokio::time::timeout(Duration::from_secs(2), share.stop())
            .await
            .expect("A stalled client must not delay stop");
        assert!(host.upgrade().is_none());
        assert!(
            file.upgrade().is_none(),
            "Stop must release selected file descriptors"
        );
        let mut byte = [0];
        let closed = tokio::time::timeout(Duration::from_secs(1), idle.read(&mut byte))
            .await
            .expect("Stop must close idle sockets");
        assert!(matches!(closed, Ok(0) | Err(_)));
        drop(stalled);
    }
}
