//! An opt-in browser upload listener. Its capability expires when the dialog closes.
//! This is deliberately separate from the LocalSend HTTPS listener and discovery.
use crate::network::{Event, IncomingRequest};
use axum::{
    body::Body,
    extract::{ConnectInfo, DefaultBodyLimit, Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::StreamExt;
use localsend_rs::{
    protocol::{DeviceInfo, DeviceType, FileId, FileMetadata, Protocol, SessionId},
    server::ServerEvent,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path as FsPath, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{io::AsyncWriteExt, sync::oneshot, task::JoinHandle};
use tokio_util::sync::CancellationToken;

const MAX_FILES: usize = 512;
const MAX_OFFER_BYTES: u64 = 100 * 1024 * 1024 * 1024;
const CONSENT_TIMEOUT: Duration = Duration::from_secs(60);
const SESSION_TIMEOUT: Duration = Duration::from_secs(300);

pub struct PendingRequest {
    session_id: SessionId,
    sender: DeviceInfo,
    files: HashMap<FileId, FileMetadata>,
    decision: oneshot::Sender<Vec<FileId>>,
    cancellation: CancellationToken,
}

impl PendingRequest {
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }
    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }
    #[cfg(test)]
    pub(crate) fn fixture(
        sender: DeviceInfo,
        files: HashMap<FileId, FileMetadata>,
    ) -> (Self, oneshot::Receiver<Vec<FileId>>) {
        let (decision, answer) = oneshot::channel();
        (
            Self {
                session_id: SessionId::new(),
                sender,
                files,
                decision,
                cancellation: CancellationToken::new(),
            },
            answer,
        )
    }
    pub fn sender(&self) -> &DeviceInfo {
        &self.sender
    }
    pub fn files(&self) -> &HashMap<FileId, FileMetadata> {
        &self.files
    }
    pub fn accept(self) {
        let ids = self.files.keys().cloned().collect();
        self.accept_files(ids);
    }
    pub fn accept_files(self, ids: Vec<FileId>) {
        let _ = self.decision.send(ids);
    }
    pub fn decline(self) {
        let _ = self.decision.send(Vec::new());
    }
}

pub struct BrowserServer {
    state: Arc<Host>,
    task: JoinHandle<()>,
    pub urls: Vec<String>,
}

struct Host {
    capability: String,
    root: Arc<UploadRoot>,
    events: async_channel::Sender<Event>,
    stop: CancellationToken,
    session: Mutex<Option<Session>>,
    withdrawn: Mutex<HashMap<(IpAddr, String), Instant>>,
    idle: tokio::sync::Notify,
}

struct Session {
    id: SessionId,
    token: String,
    request_id: String,
    peer: IpAddr,
    sender: String,
    files: HashMap<FileId, FileMetadata>,
    file_count: usize,
    received_bytes: u64,
    total_bytes: u64,
    accepted: bool,
    uploading: bool,
    touched: Instant,
    cancel: CancellationToken,
}

impl BrowserServer {
    pub async fn start(
        save_dir: &FsPath,
        events: async_channel::Sender<Event>,
    ) -> Result<Self, String> {
        let root = UploadRoot::open(save_dir).map_err(|e| e.to_string())?;
        let capability = uuid::Uuid::new_v4().simple().to_string();
        let state = Arc::new(Host {
            capability: capability.clone(),
            root: Arc::new(root),
            events,
            stop: CancellationToken::new(),
            session: Mutex::new(None),
            withdrawn: Mutex::new(HashMap::new()),
            idle: tokio::sync::Notify::new(),
        });
        let app = Router::new()
            .route("/{cap}/", get(page))
            .route(
                "/{cap}/offer",
                post(offer).layer(DefaultBodyLimit::max(64 * 1024)),
            )
            .route(
                "/{cap}/upload/{token}/{id}",
                post(upload).layer(DefaultBodyLimit::disable()),
            )
            .route("/{cap}/cancel/{token}", post(cancel))
            .route("/{cap}/withdraw/{request_id}", post(withdraw))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0))
            .await
            .map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let mut ips = localsend_rs::discovery::local_ipv4_addresses().unwrap_or_default();
        ips.push(Ipv4Addr::LOCALHOST);
        let urls = ips
            .into_iter()
            .map(|ip| format!("http://{ip}:{port}/{capability}/"))
            .collect();
        let shutdown = state.stop.clone();
        let task_state = state.clone();
        let task = tokio::spawn(async move {
            let server = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown.cancelled_owned());
            let sweep =
                async {
                    let mut tick = tokio::time::interval(Duration::from_secs(10));
                    loop {
                        tick.tick().await;
                        let expired = {
                            let mut slot = task_state.session.lock().unwrap();
                            if slot.as_ref().is_some_and(|s| {
                                !s.uploading && s.touched.elapsed() > SESSION_TIMEOUT
                            }) {
                                slot.take()
                            } else {
                                None
                            }
                        };
                        if let Some(session) = expired {
                            session.cancel.cancel();
                            let _ = task_state.events.try_send(Event::Server(
                                ServerEvent::SessionDone {
                                    session_id: session.id,
                                },
                            ));
                        }
                    }
                };
            tokio::select! {
                result = server => if let Err(error) = result { let _ = task_state.events.try_send(Event::Error(format!("Browser receiving: {error}"))); },
                _ = sweep => {},
            }
        });
        Ok(Self { state, task, urls })
    }

    pub fn busy(&self) -> bool {
        self.state.session.lock().unwrap().is_some()
    }

    /// Cancel this receive while keeping its temporary browser link available.
    pub async fn cancel_incoming(&self) {
        let target = {
            let mut slot = self.state.session.lock().unwrap();
            let target = slot.as_ref().map(|session| session.id.clone());
            cancel_locked(&self.state, &mut slot);
            target
        };
        let Some(target) = target else {
            return;
        };
        loop {
            let notified = self.state.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self
                .state
                .session
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|session| session.id == target)
            {
                return;
            }
            notified.await;
        }
    }

    pub async fn stop(self) {
        self.state.stop.cancel();
        cancel_locked(&self.state, &mut self.state.session.lock().unwrap());
        let mut task = self.task;
        if tokio::time::timeout(Duration::from_secs(3), &mut task)
            .await
            .is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }
}

fn authorized(host: &Host, cap: &str, headers: &HeaderMap) -> bool {
    if host.stop.is_cancelled() || cap != host.capability {
        return false;
    }
    // Browsers may use this API only from the served page. There is no CORS layer.
    if let Some(origin) = headers.get("origin") {
        let Some(authority) = headers.get("host").and_then(|h| h.to_str().ok()) else {
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
    // Opening a shared link from another website is a valid top-level navigation.
    // Only the state-changing API calls below require a same-origin browser.
    if host.stop.is_cancelled() || cap != host.capability {
        return StatusCode::NOT_FOUND.into_response();
    }
    (
        [("cache-control", "no-store"), ("referrer-policy", "no-referrer"),
         ("x-content-type-options", "nosniff"), ("x-frame-options", "DENY"),
         ("content-security-policy", "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; form-action 'none'; frame-ancestors 'none'")],
        Html(include_str!("../assets/web-receive.html")),
    ).into_response()
}

#[derive(Deserialize)]
struct Offer {
    files: Vec<OfferedFile>,
    #[serde(default, rename = "requestId")]
    request_id: Option<String>,
}
#[derive(Deserialize)]
struct OfferedFile {
    name: String,
    size: u64,
}
#[derive(Serialize, Deserialize)]
struct Approval {
    token: String,
    files: Vec<FileId>,
}

fn validate_offer(offer: Offer) -> Result<HashMap<FileId, FileMetadata>, StatusCode> {
    if offer.files.is_empty() || offer.files.len() > MAX_FILES {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut total = 0u64;
    let mut files = HashMap::new();
    for (index, file) in offer.files.into_iter().enumerate() {
        if !safe_name(&file.name) {
            return Err(StatusCode::BAD_REQUEST);
        }
        total = total
            .checked_add(file.size)
            .ok_or(StatusCode::PAYLOAD_TOO_LARGE)?;
        if total > MAX_OFFER_BYTES {
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }
        let id = FileId::from_string(index.to_string());
        files.insert(
            id.clone(),
            FileMetadata {
                id,
                file_name: file.name,
                size: file.size,
                file_type: "application/octet-stream".into(),
                sha256: None,
                preview: None,
                metadata: None,
            },
        );
    }
    Ok(files)
}

async fn offer(
    State(host): State<Arc<Host>>,
    Path(cap): Path<String>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(offer): Json<Offer>,
) -> Response {
    if !authorized(&host, &cap, &headers) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let request_id = offer
        .request_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
    if !valid_request_id(&request_id) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let files = match validate_offer(offer) {
        Ok(files) => files,
        Err(code) => return code.into_response(),
    };
    let id = SessionId::new();
    let token = uuid::Uuid::new_v4().simple().to_string();
    let cancel = CancellationToken::new();
    let alias = format!("Web browser ({})", peer.ip());
    {
        // Lock in the same order as withdrawal. A cancel that reaches us before
        // its offer must not leave that later offer waiting for consent.
        let mut withdrawn = host.withdrawn.lock().unwrap();
        withdrawn.retain(|_, at| at.elapsed() < CONSENT_TIMEOUT + Duration::from_secs(10));
        if withdrawn.contains_key(&(peer.ip(), request_id.clone())) {
            return StatusCode::GONE.into_response();
        }
        let mut slot = host.session.lock().unwrap();
        if slot.is_some() {
            return StatusCode::CONFLICT.into_response();
        }
        *slot = Some(Session {
            id: id.clone(),
            token: token.clone(),
            request_id,
            peer: peer.ip(),
            sender: alias.clone(),
            file_count: files.len(),
            received_bytes: 0,
            total_bytes: files
                .values()
                .fold(0_u64, |total, file| total.saturating_add(file.size)),
            files: files.clone(),
            accepted: false,
            uploading: false,
            touched: Instant::now(),
            cancel: cancel.clone(),
        });
    }
    let mut guard = SessionGuard {
        host: host.clone(),
        id: id.clone(),
        armed: true,
    };
    let (decision, answer) = oneshot::channel();
    let mut sender = DeviceInfo::new(alias, peer.port(), Protocol::Http);
    sender.device_type = Some(DeviceType::Web);
    sender.ip = Some(peer.ip().to_string());
    let pending = PendingRequest {
        session_id: id.clone(),
        sender,
        files,
        decision,
        cancellation: cancel.clone(),
    };
    if host
        .events
        .send(Event::TransferRequest(IncomingRequest::Browser(pending)))
        .await
        .is_err()
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let answer = tokio::select! {
        _ = host.stop.cancelled() => None,
        _ = cancel.cancelled() => None,
        result = tokio::time::timeout(CONSENT_TIMEOUT, answer) => result.ok().and_then(Result::ok),
    };
    let Some(accepted) = answer.filter(|ids| !ids.is_empty()) else {
        return StatusCode::FORBIDDEN.into_response();
    };
    let accepted = {
        let mut slot = host.session.lock().unwrap();
        let Some(session) = slot.as_mut().filter(|s| s.id == id) else {
            return StatusCode::GONE.into_response();
        };
        session.files.retain(|id, _| accepted.contains(id));
        if session.files.is_empty() {
            return StatusCode::FORBIDDEN.into_response();
        }
        session.accepted = true;
        session.touched = Instant::now();
        session.file_count = session.files.len();
        session.total_bytes = session
            .files
            .values()
            .fold(0_u64, |total, file| total.saturating_add(file.size));
        session.files.keys().cloned().collect()
    };
    guard.armed = false;
    Json(Approval {
        token,
        files: accepted,
    })
    .into_response()
}

struct SessionGuard {
    host: Arc<Host>,
    id: SessionId,
    armed: bool,
}

/// Keep an active upload installed until its owner has dropped the partial file.
/// This prevents a terminal event racing ahead of late progress or publication.
fn cancel_locked(host: &Host, slot: &mut Option<Session>) {
    if let Some(session) = slot.as_ref() {
        session.cancel.cancel();
        if !session.uploading {
            let session = slot.take().unwrap();
            let _ = host
                .events
                .try_send(Event::Server(ServerEvent::SessionDone {
                    session_id: session.id,
                }));
            host.idle.notify_waiters();
        }
    }
}
impl Drop for SessionGuard {
    fn drop(&mut self) {
        if self.armed {
            let mut slot = self.host.session.lock().unwrap();
            if slot.as_ref().is_some_and(|s| s.id == self.id) {
                if let Some(session) = slot.take() {
                    session.cancel.cancel();
                }
                let _ = self
                    .host
                    .events
                    .try_send(Event::Server(ServerEvent::SessionDone {
                        session_id: self.id.clone(),
                    }));
                self.host.idle.notify_waiters();
            }
        }
    }
}

fn upload_rejection(code: StatusCode) -> Response {
    let mut response = code.into_response();
    // Rejected HTTP/1 uploads may have unread body bytes. Hyper then closes the
    // socket; advertise that before a client can pool it for the next offer.
    response
        .headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("close"));
    response
}

async fn upload(
    State(host): State<Arc<Host>>,
    Path((cap, token, file_id)): Path<(String, String, String)>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if !authorized(&host, &cap, &headers) {
        return upload_rejection(StatusCode::NOT_FOUND);
    }
    let file_id = FileId::from_string(file_id);
    let (id, file, sender, file_count, received_before, total_bytes, cancellation) = {
        let mut slot = host.session.lock().unwrap();
        let Some(session) = slot.as_mut().filter(|s| {
            s.accepted && !s.cancel.is_cancelled() && s.token == token && s.peer == peer.ip()
        }) else {
            return upload_rejection(StatusCode::NOT_FOUND);
        };
        if session.uploading {
            return upload_rejection(StatusCode::CONFLICT);
        }
        let Some(file) = session.files.remove(&file_id) else {
            return upload_rejection(StatusCode::NOT_FOUND);
        };
        session.uploading = true;
        (
            session.id.clone(),
            file,
            session.sender.clone(),
            session.file_count,
            session.received_bytes,
            session.total_bytes,
            session.cancel.clone(),
        )
    };
    // Any aborted or malformed upload terminalizes this session and deletes its partial file.
    let mut guard = SessionGuard {
        host: host.clone(),
        id: id.clone(),
        armed: true,
    };
    if let Some(length) = headers.get("content-length") {
        if length.to_str().ok().and_then(|n| n.parse::<u64>().ok()) != Some(file.size) {
            return upload_rejection(StatusCode::BAD_REQUEST);
        }
    }
    let operation = async {
        let mut destination = AtomicUpload::create(host.root.clone())
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let mut stream = body.into_data_stream();
        let mut received = 0u64;
        let mut progress = Instant::now() - Duration::from_secs(1);
        while let Some(chunk) = tokio::time::timeout(Duration::from_secs(30), stream.next())
            .await
            .map_err(|_| StatusCode::REQUEST_TIMEOUT)?
        {
            let chunk = chunk.map_err(|_| StatusCode::BAD_REQUEST)?;
            received = received
                .checked_add(chunk.len() as u64)
                .filter(|size| *size <= file.size)
                .ok_or(StatusCode::PAYLOAD_TOO_LARGE)?;
            destination
                .writer()
                .write_all(&chunk)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            if progress.elapsed() >= Duration::from_millis(100) || received == file.size {
                progress = Instant::now();
                let slot = host.session.lock().unwrap();
                if host.stop.is_cancelled()
                    || !slot
                        .as_ref()
                        .is_some_and(|s| s.id == id && !s.cancel.is_cancelled())
                {
                    return Err(StatusCode::GONE);
                }
                let _ = host
                    .events
                    .try_send(Event::Server(ServerEvent::FileReceiveProgress {
                        session_id: id.clone(),
                        file_id: file_id.clone(),
                        file_name: file.file_name.clone(),
                        sender_alias: sender.clone(),
                        bytes_received: received_before.saturating_add(received),
                        total_bytes,
                        file_bytes_received: received,
                        file_size: file.size,
                        file_count,
                    }));
            }
        }
        if received != file.size {
            return Err(StatusCode::BAD_REQUEST);
        }
        destination
            .finish_writing()
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        // Publication and cancellation share this lock. Once withdrawal returns,
        // this operation can neither publish a file nor emit later progress.
        let mut slot = host.session.lock().unwrap();
        let Some(session) = slot
            .as_mut()
            .filter(|s| s.id == id && !s.cancel.is_cancelled())
        else {
            return Err(StatusCode::GONE);
        };
        if host.stop.is_cancelled() {
            return Err(StatusCode::GONE);
        }
        let path = destination
            .publish(&file.file_name)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let actual_name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let _ = host
            .events
            .try_send(Event::Server(ServerEvent::FileReceived {
                session_id: id.clone(),
                file_id: file_id.clone(),
                file_name: actual_name,
                path,
                size: file.size,
                sender_alias: sender.clone(),
                message_text: None,
            }));
        session.uploading = false;
        session.received_bytes = session.received_bytes.saturating_add(file.size);
        session.touched = Instant::now();
        Ok(!session.files.is_empty())
    };
    let result = tokio::select! {
        biased;
        _ = host.stop.cancelled() => Err(StatusCode::GONE),
        _ = cancellation.cancelled() => Err(StatusCode::GONE),
        result = operation => result,
    };
    let more_files = match result {
        Ok(more_files) => more_files,
        Err(code) => {
            let _ = host.events.try_send(Event::Error(format!(
                "Browser upload of {} did not finish ({code}).",
                file.file_name
            )));
            return upload_rejection(code);
        }
    };
    guard.armed = !more_files;
    StatusCode::NO_CONTENT.into_response()
}

async fn cancel(
    State(host): State<Arc<Host>>,
    Path((cap, token)): Path<(String, String)>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if !authorized(&host, &cap, &headers) {
        return StatusCode::NOT_FOUND.into_response();
    }
    {
        let mut slot = host.session.lock().unwrap();
        let Some(_) = slot
            .as_ref()
            .filter(|s| s.token == token && s.peer == peer.ip())
        else {
            return StatusCode::NOT_FOUND.into_response();
        };
        cancel_locked(&host, &mut slot);
    }
    StatusCode::NO_CONTENT.into_response()
}

fn valid_request_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

async fn withdraw(
    State(host): State<Arc<Host>>,
    Path((cap, request_id)): Path<(String, String)>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if !authorized(&host, &cap, &headers) || !valid_request_id(&request_id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    {
        let mut withdrawn = host.withdrawn.lock().unwrap();
        withdrawn.retain(|_, at| at.elapsed() < CONSENT_TIMEOUT + Duration::from_secs(10));
        let key = (peer.ip(), request_id.clone());
        if !withdrawn.contains_key(&key) && withdrawn.len() >= 128 {
            return StatusCode::TOO_MANY_REQUESTS.into_response();
        }
        withdrawn.insert(key, Instant::now());
        let mut slot = host.session.lock().unwrap();
        if slot
            .as_ref()
            .is_some_and(|s| s.peer == peer.ip() && s.request_id == request_id)
        {
            cancel_locked(&host, &mut slot);
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

fn safe_name(name: &str) -> bool {
    if name.is_empty()
        || name.len() > 200
        || name == "."
        || name == ".."
        || name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
    {
        return false;
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    !(matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit()))
}

struct UploadRoot {
    path: PathBuf,
    #[cfg(unix)]
    directory: Arc<std::fs::File>,
}
impl UploadRoot {
    fn open(path: &FsPath) -> std::io::Result<Self> {
        std::fs::create_dir_all(path)?;
        let path = path.canonicalize()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let directory = Arc::new(
                std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
                    .open(&path)?,
            );
            Ok(Self { path, directory })
        }
        #[cfg(not(unix))]
        {
            Ok(Self { path })
        }
    }
}

struct AtomicUpload {
    root: Arc<UploadRoot>,
    temp: String,
    file: tokio::fs::File,
}
impl AtomicUpload {
    fn create(root: Arc<UploadRoot>) -> std::io::Result<Self> {
        let temp = format!(".localsend-{}.part", uuid::Uuid::new_v4());
        #[cfg(unix)]
        let file = {
            use std::os::fd::{AsRawFd, FromRawFd};
            let name = std::ffi::CString::new(temp.as_str()).unwrap();
            // The held directory descriptor prevents rename/symlink races escaping the receive root.
            let fd = unsafe {
                libc::openat(
                    root.directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CLOEXEC
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW,
                    0o600,
                )
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            unsafe { std::fs::File::from_raw_fd(fd) }
        };
        #[cfg(not(unix))]
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(root.path.join(&temp))?;
        Ok(Self {
            root,
            temp,
            file: tokio::fs::File::from_std(file),
        })
    }
    fn writer(&mut self) -> &mut tokio::fs::File {
        &mut self.file
    }
    async fn finish_writing(&mut self) -> std::io::Result<()> {
        self.file.flush().await?;
        self.file.sync_all().await?;
        Ok(())
    }
    #[cfg(test)]
    async fn commit(mut self, name: &str) -> std::io::Result<PathBuf> {
        self.finish_writing().await?;
        self.publish(name)
    }
    fn publish(self, name: &str) -> std::io::Result<PathBuf> {
        for attempt in 0..1024 {
            let leaf = if attempt == 0 {
                name.to_string()
            } else {
                match name.rsplit_once('.').filter(|(stem, _)| !stem.is_empty()) {
                    Some((stem, extension)) => format!("{stem} ({attempt}).{extension}"),
                    None => format!("{name} ({attempt})"),
                }
            };
            #[cfg(unix)]
            let result = {
                use std::os::fd::AsRawFd;
                let from = std::ffi::CString::new(self.temp.as_str()).unwrap();
                let to = std::ffi::CString::new(leaf.as_str()).unwrap();
                let fd = self.root.directory.as_raw_fd();
                // linkat publishes the complete inode atomically and never replaces an existing file or symlink.
                if unsafe { libc::linkat(fd, from.as_ptr(), fd, to.as_ptr(), 0) } == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            };
            #[cfg(not(unix))]
            let result =
                std::fs::hard_link(self.root.path.join(&self.temp), self.root.path.join(&leaf));
            match result {
                Ok(()) => {
                    #[cfg(unix)]
                    {
                        let _ = self.root.directory.sync_all();
                    }
                    return Ok(self.root.path.join(leaf));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(std::io::Error::other(
            "No unused destination name is available",
        ))
    }
}
impl Drop for AtomicUpload {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let name = std::ffi::CString::new(self.temp.as_str()).unwrap();
            unsafe {
                libc::unlinkat(self.root.directory.as_raw_fd(), name.as_ptr(), 0);
            }
        }
        #[cfg(not(unix))]
        {
            let _ = std::fs::remove_file(self.root.path.join(&self.temp));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn next_event(rx: &async_channel::Receiver<Event>) -> Event {
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("browser event timed out")
            .expect("browser event channel closed")
    }
    #[test]
    fn rejects_unsafe_names_and_oversized_offers() {
        for name in [
            "",
            ".",
            "..",
            "../escape",
            "a/b",
            "a\\b",
            "/tmp/a",
            "C:foo",
            "bad\nname",
            "NUL",
            "COM1.txt",
            "a.",
            "a ",
        ] {
            assert!(!safe_name(name), "{name:?}");
        }
        for name in ["photo.jpg", "你好.txt", ".hidden"] {
            assert!(safe_name(name));
        }
        assert!(validate_offer(Offer {
            request_id: None,
            files: vec![OfferedFile {
                name: "file".into(),
                size: MAX_OFFER_BYTES + 1
            }]
        })
        .is_err());
    }
    async fn setup() -> (
        tempfile::TempDir,
        BrowserServer,
        async_channel::Receiver<Event>,
        reqwest::Client,
        String,
    ) {
        localsend_rs::crypto::ensure_crypto_provider();
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = async_channel::unbounded();
        let server = BrowserServer::start(dir.path(), tx).await.unwrap();
        let url = server
            .urls
            .iter()
            .find(|url| url.contains("127.0.0.1"))
            .unwrap()
            .clone();
        (
            dir,
            server,
            rx,
            reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            url,
        )
    }
    #[tokio::test]
    async fn capability_and_consent_are_required() {
        let (dir, server, rx, client, url) = setup().await;
        assert_eq!(
            client
                .get(&url)
                .header("sec-fetch-site", "cross-site")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let root = url.rsplit_once('/').unwrap().0.rsplit_once('/').unwrap().0;
        assert_eq!(
            client
                .get(format!("{root}/wrong/"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            client
                .post(format!("{url}offer"))
                .header("origin", "http://evil.invalid")
                .json(&serde_json::json!({"files":[{"name":"file","size":3}]}))
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
                    .post(format!("{url}offer"))
                    .json(&serde_json::json!({"files":[{"name":"file","size":3}]}))
                    .send()
                    .await
                    .unwrap()
            }
        });
        match next_event(&rx).await {
            Event::TransferRequest(request) => drop(request),
            _ => panic!("missing consent"),
        }
        assert_eq!(request.await.unwrap().status(), StatusCode::FORBIDDEN);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        server.stop().await;
        assert!(client.get(&url).send().await.is_err());
    }
    async fn accept_offer(
        client: &reqwest::Client,
        url: &str,
        rx: &async_channel::Receiver<Event>,
        size: u64,
    ) -> Approval {
        let request = tokio::spawn({
            let client = client.clone();
            let url = url.to_string();
            async move {
                client
                    .post(format!("{url}offer"))
                    .json(&serde_json::json!({"files":[{"name":"file.txt","size":size}]}))
                    .send()
                    .await
                    .unwrap()
            }
        });
        loop {
            if let Event::TransferRequest(request) = next_event(rx).await {
                request.accept();
                break;
            }
        }
        request.await.unwrap().json().await.unwrap()
    }
    #[tokio::test]
    async fn exact_upload_is_atomic_and_never_overwrites() {
        let (dir, server, rx, client, url) = setup().await;
        std::fs::write(dir.path().join("file.txt"), b"old").unwrap();
        let approval = accept_offer(&client, &url, &rx, 3).await;
        assert_eq!(
            client
                .post(format!("{url}upload/{}/0", approval.token))
                .body("new")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(std::fs::read(dir.path().join("file.txt")).unwrap(), b"old");
        assert_eq!(
            std::fs::read(dir.path().join("file (1).txt")).unwrap(),
            b"new"
        );
        assert_eq!(
            client
                .post(format!("{url}upload/{}/0", approval.token))
                .body("new")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
        server.stop().await;
    }
    #[tokio::test]
    async fn wrong_size_and_cancel_do_not_leave_partial_files() {
        let (dir, server, rx, client, url) = setup().await;
        let approval = accept_offer(&client, &url, &rx, 4).await;
        let rejected = client
            .post(format!("{url}upload/{}/0", approval.token))
            .body("short")
            .send()
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
        assert_eq!(rejected.headers()["connection"], "close");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        let approval = accept_offer(&client, &url, &rx, 3).await;
        assert_eq!(
            client
                .post(format!("{url}cancel/{}", approval.token))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            client
                .post(format!("{url}upload/{}/0", approval.token))
                .body("new")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        server.stop().await;
    }
    #[tokio::test]
    async fn unselected_files_and_chunked_oversize_are_rejected() {
        let (dir, server, rx, client, url) = setup().await;
        let request = tokio::spawn({
            let client = client.clone();
            let url = url.clone();
            async move {
                client
                    .post(format!("{url}offer"))
                    .json(
                        &serde_json::json!({"files":[{"name":"a","size":3},{"name":"b","size":3}]}),
                    )
                    .send()
                    .await
                    .unwrap()
            }
        });
        match next_event(&rx).await {
            Event::TransferRequest(request) => {
                request.accept_files(vec![FileId::from_string("1".into())])
            }
            _ => panic!("missing consent"),
        }
        let approval: Approval = request.await.unwrap().json().await.unwrap();
        assert_eq!(approval.files, vec![FileId::from_string("1".into())]);
        assert_eq!(
            client
                .post(format!("{url}upload/{}/0", approval.token))
                .body("bad")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        let chunks = futures_util::stream::iter([Ok::<_, std::io::Error>("too"), Ok("many")]);
        let response = client
            .post(format!("{url}upload/{}/1", approval.token))
            .body(reqwest::Body::wrap_stream(chunks))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        server.stop().await;
    }
    #[tokio::test]
    async fn stopping_revokes_accepted_tokens() {
        let (_dir, server, rx, client, url) = setup().await;
        let approval = accept_offer(&client, &url, &rx, 3).await;
        server.stop().await;
        assert!(client
            .post(format!("{url}upload/{}/0", approval.token))
            .body("new")
            .send()
            .await
            .is_err());
    }

    #[tokio::test]
    async fn local_receive_cancel_waits_for_partial_cleanup_and_keeps_link_usable() {
        let (dir, server, rx, client, url) = setup().await;
        let approval = accept_offer(&client, &url, &rx, 3).await;
        let (chunks, stream) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::io::Error>>(2);
        let stream = futures_util::stream::unfold(stream, |mut receiver| async {
            receiver.recv().await.map(|chunk| (chunk, receiver))
        });
        let upload = tokio::spawn({
            let client = client.clone();
            let url = url.clone();
            async move {
                client
                    .post(format!("{url}upload/{}/0", approval.token))
                    .body(reqwest::Body::wrap_stream(stream))
                    .send()
                    .await
            }
        });
        chunks.send(Ok(b"n".to_vec())).await.unwrap();
        loop {
            if matches!(
                next_event(&rx).await,
                Event::Server(ServerEvent::FileReceiveProgress { .. })
            ) {
                break;
            }
        }
        tokio::time::timeout(Duration::from_secs(5), server.cancel_incoming())
            .await
            .unwrap();
        assert!(!server.busy());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        // The sender still has not delivered EOF. The error must explicitly
        // prevent reuse of this connection before another offer is submitted.
        let response = upload.await.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::GONE);
        assert_eq!(response.headers()["connection"], "close");
        drop(chunks);
        assert_eq!(
            client.get(&url).send().await.unwrap().status(),
            StatusCode::OK
        );
        let next = accept_offer(&client, &url, &rx, 3).await;
        assert_eq!(
            client
                .post(format!("{url}upload/{}/0", next.token))
                .body("new")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(std::fs::read(dir.path().join("file.txt")).unwrap(), b"new");
        server.stop().await;
    }
    #[tokio::test]
    async fn withdrawing_pending_consent_releases_it_and_handles_cancel_before_offer() {
        let (_dir, server, rx, client, url) = setup().await;
        let request_id = "a".repeat(32);
        let request = tokio::spawn({
            let client = client.clone();
            let url = url.clone();
            let request_id = request_id.clone();
            async move {
                client.post(format!("{url}offer")).json(&serde_json::json!({"requestId": request_id, "files":[{"name":"file","size":3}]})).send().await.unwrap()
            }
        });
        let pending = match next_event(&rx).await {
            Event::TransferRequest(request) => request,
            _ => panic!("Expected consent request"),
        };
        let cancellation = pending.cancellation().unwrap();
        assert_eq!(
            client
                .post(format!("{url}withdraw/{request_id}"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        assert!(cancellation.is_cancelled());
        assert!(!server.busy());
        pending.accept(); // A response from a stale dialog cannot revive an offer.
        assert!(matches!(
            request.await.unwrap().status(),
            StatusCode::FORBIDDEN | StatusCode::GONE
        ));
        let early = "b".repeat(32);
        assert_eq!(
            client
                .post(format!("{url}withdraw/{early}"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            client
                .post(format!("{url}offer"))
                .json(&serde_json::json!({"requestId": early, "files":[{"name":"file","size":3}]}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::GONE
        );
        assert!(!server.busy());
        server.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn withdrawal_serializes_with_final_bytes_and_terminal_events() {
        let (dir, server, rx, client, url) = setup().await;
        for round in 0..12 {
            let request_id = uuid::Uuid::new_v4().simple().to_string();
            let offer = tokio::spawn({
                let client = client.clone();
                let url = url.clone();
                let request_id = request_id.clone();
                async move {
                    client.post(format!("{url}offer")).json(&serde_json::json!({"requestId": request_id, "files":[{"name":"file.txt","size":6}]})).send().await.unwrap()
                }
            });
            match next_event(&rx).await {
                Event::TransferRequest(request) => request.accept(),
                _ => panic!("Expected consent"),
            }
            let approval: Approval = offer.await.unwrap().json().await.unwrap();
            let (chunks, stream) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::io::Error>>(2);
            let stream = futures_util::stream::unfold(stream, |mut receiver| async {
                receiver.recv().await.map(|chunk| (chunk, receiver))
            });
            let upload = tokio::spawn({
                let client = client.clone();
                let url = url.clone();
                async move {
                    client
                        .post(format!("{url}upload/{}/0", approval.token))
                        .body(reqwest::Body::wrap_stream(stream))
                        .send()
                        .await
                }
            });
            chunks.send(Ok(b"fir".to_vec())).await.unwrap();
            match next_event(&rx).await {
                Event::Server(ServerEvent::FileReceiveProgress { .. }) => {}
                _ => panic!("Expected initial progress"),
            }
            // Alternate a guaranteed mid-stream cancel with final-byte/cancel races.
            let chunks = if round % 2 == 1 {
                chunks.send(Ok(b"st!".to_vec())).await.unwrap();
                drop(chunks); // Deliver EOF so publication can race with withdrawal.
                None
            } else {
                Some(chunks)
            };
            let cancelled = client
                .post(format!("{url}withdraw/{request_id}"))
                .send()
                .await
                .unwrap();
            assert_eq!(cancelled.status(), StatusCode::NO_CONTENT);
            let published_at_ack = std::fs::read_dir(dir.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| !entry.file_name().to_string_lossy().ends_with(".part"))
                .count();
            if let Some(chunks) = chunks {
                let _ = chunks.send(Ok(b"st!".to_vec())).await;
                drop(chunks);
            }
            if let Ok(response) = upload.await.unwrap() {
                assert!(matches!(
                    response.status(),
                    StatusCode::GONE | StatusCode::NO_CONTENT
                ));
            }
            loop {
                if let Event::Server(ServerEvent::SessionDone { .. }) = next_event(&rx).await {
                    break;
                }
            }
            while let Ok(event) = rx.try_recv() {
                assert!(
                    !matches!(
                        event,
                        Event::Server(
                            ServerEvent::FileReceiveProgress { .. }
                                | ServerEvent::FileReceived { .. }
                        )
                    ),
                    "Progress/completion arrived after terminal event"
                );
            }
            assert!(!server.busy());
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), published_at_ack, "No file may publish after cancellation acknowledgment; partial files must be cleaned up");
        }
        server.stop().await;
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn symlinks_cannot_redirect_publication() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(other.path().join("escaped"), dir.path().join("file.txt"))
            .unwrap();
        let root = Arc::new(UploadRoot::open(dir.path()).unwrap());
        let mut upload = AtomicUpload::create(root).unwrap();
        upload.writer().write_all(b"data").await.unwrap();
        let path = upload.commit("file.txt").await.unwrap();
        assert_eq!(path.file_name().unwrap(), "file (1).txt");
        assert!(!other.path().join("escaped").exists());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn directory_descriptor_prevents_root_swap_escape() {
        let parent = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let original = parent.path().join("receive");
        let moved = parent.path().join("moved");
        let root = Arc::new(UploadRoot::open(&original).unwrap());
        std::fs::rename(&original, &moved).unwrap();
        std::os::unix::fs::symlink(other.path(), &original).unwrap();
        let mut upload = AtomicUpload::create(root).unwrap();
        upload.writer().write_all(b"data").await.unwrap();
        upload.commit("file.txt").await.unwrap();
        assert_eq!(std::fs::read(moved.join("file.txt")).unwrap(), b"data");
        assert!(!other.path().join("file.txt").exists());
    }
}
