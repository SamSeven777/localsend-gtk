use crate::settings::{atomic_write, same_fingerprint, QuickSaveMode, Settings};
use localsend_rs::{
    core::{AtomicFileSink, PendingReceive, ReceiveSink, SinkError},
    crypto::{generate_tls_certificate, tls_certificate_from_pem, TlsCertificate},
    discovery::{Discovery, MulticastDiscovery},
    protocol::{DeviceInfo, FileId, FileMetadata, Protocol, SessionId},
    server::{LocalSendServer, PendingRequest, ServerEvent},
};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

pub enum Command {
    Refresh,
    QuickSave(QuickSaveMode),
    Favorites(Vec<crate::settings::FavoriteDevice>),
    Reconfigure(Settings),
    StartServer(Settings),
    StopServer,
    StartWebReceive,
    StopWebReceive,
    CancelIncoming,
    StartWebShare {
        items: Vec<crate::transfer::Selection>,
        auto_accept: bool,
    },
    SetWebShareAutoAccept(bool),
    StopWebShare,
    Shutdown,
}

/// Decide native receive consent from a transport-authenticated identity.
/// `authenticated_fingerprint` must come from the peer TLS certificate, never
/// from `PrepareUploadRequest.info` or discovery metadata.
fn should_quick_save_native(
    mode: QuickSaveMode,
    favorites: &[crate::settings::FavoriteDevice],
    sender: &DeviceInfo,
    authenticated_fingerprint: Option<&str>,
) -> bool {
    match mode {
        QuickSaveMode::On => true,
        QuickSaveMode::Off => false,
        QuickSaveMode::Paired => {
            let Some(authenticated) = authenticated_fingerprint else {
                return false;
            };
            sender.protocol == Protocol::Https
                && same_fingerprint(authenticated, &sender.fingerprint)
                && favorites.iter().any(|favorite| {
                    favorite.protocol == Protocol::Https
                        && same_fingerprint(&favorite.fingerprint, authenticated)
                })
        }
    }
}

fn inline_message(files: &HashMap<FileId, FileMetadata>) -> Option<&str> {
    if files.len() != 1 {
        return None;
    }
    let file = files.values().next()?;
    file.preview
        .as_deref()
        .filter(|text| !text.is_empty() && file.size < 1024 * 1024)
}
pub enum Event {
    Ready(DeviceInfo),
    ServerState {
        running: bool,
        settings: Settings,
    },
    Peer(DeviceInfo),
    TransferRequest(IncomingRequest),
    TextReceived {
        text: String,
        sender_alias: String,
        preview_handled: bool,
    },
    WebReceiveReady(Vec<String>),
    WebReceiveStopped,
    IncomingCanceled,
    IncomingCancelFailed(String),
    WebShareReady(Vec<String>),
    WebShareStopped,
    WebShare(crate::web_share::ShareEvent),
    Server(ServerEvent),
    Error(String),
    Offline(String),
}

/// Both native and browser offers use the same GTK consent and file-selection dialog.
pub enum IncomingRequest {
    Native(TrackedRequest),
    Browser(crate::web_receive::PendingRequest),
}
impl IncomingRequest {
    #[cfg(test)]
    pub(crate) fn native_fixture(request: PendingRequest) -> Self {
        Arc::new(ReceiveActivity::default()).track(request)
    }

    /// Stable identity available before the first upload. Native offers use
    /// their tracked lease until the protocol supplies its session id.
    pub fn progress_identity(&self) -> (String, bool) {
        match self {
            Self::Native(request) => (request.id.to_string(), true),
            Self::Browser(request) => (request.session_id().to_string(), false),
        }
    }
    pub fn sender(&self) -> &DeviceInfo {
        match self {
            Self::Native(r) => r.request.as_ref().unwrap().sender(),
            Self::Browser(r) => r.sender(),
        }
    }
    pub fn files(&self) -> &HashMap<FileId, FileMetadata> {
        match self {
            Self::Native(r) => r.request.as_ref().unwrap().files(),
            Self::Browser(r) => r.files(),
        }
    }
    pub fn cancellation(&self) -> Option<tokio_util::sync::CancellationToken> {
        match self {
            Self::Native(request) => Some(request.cancel.clone()),
            Self::Browser(request) => Some(request.cancellation()),
        }
    }
    /// Match the protocol's complete inline message, never a file thumbnail or
    /// a preview attached to a multi-file offer. Browser uploads stay files.
    pub fn inline_message(&self) -> Option<&str> {
        if !matches!(self, Self::Native(_)) {
            return None;
        }
        inline_message(self.files())
    }

    /// Acknowledge a complete message already acted on in its preview dialog.
    /// Return false for a withdrawn offer so its Open/Copy action is not run.
    pub fn accept_preview(self) -> bool {
        if self.inline_message().is_none() {
            self.decline();
            return false;
        }
        let Self::Native(mut request) = self else {
            unreachable!()
        };
        if request.mark_accepted(true) {
            request.request.take().unwrap().accept();
            true
        } else {
            request.request.take().unwrap().decline();
            false
        }
    }
    pub fn accept(self) {
        match self {
            Self::Native(mut r) => {
                if r.mark_accepted(false) {
                    r.request.take().unwrap().accept();
                } else {
                    r.request.take().unwrap().decline();
                }
            }
            Self::Browser(r) => r.accept(),
        }
    }
    pub fn accept_files(self, ids: Vec<FileId>) {
        if ids.is_empty() {
            self.decline();
            return;
        }
        match self {
            Self::Native(mut r) => {
                if r.mark_accepted(false) {
                    r.request.take().unwrap().accept_files(ids);
                } else {
                    r.request.take().unwrap().decline();
                }
            }
            Self::Browser(r) => r.accept_files(ids),
        }
    }
    pub fn decline(self) {
        match self {
            Self::Native(mut r) => {
                r.activity.clear(r.id);
                r.request.take().unwrap().decline();
            }
            Self::Browser(r) => r.decline(),
        }
    }
}

pub struct TrackedRequest {
    request: Option<PendingRequest>,
    activity: Arc<ReceiveActivity>,
    id: uuid::Uuid,
    cancel: CancellationToken,
}
impl TrackedRequest {
    fn mark_accepted(&mut self, preview_handled: bool) -> bool {
        let mut lease = self.activity.lease.lock().unwrap();
        if self.cancel.is_cancelled() {
            return false;
        }
        if let Some(lease) = lease.as_mut().filter(|lease| lease.id == self.id) {
            lease.accepted = true;
            lease.touched = Instant::now();
            self.activity
                .handled_preview
                .store(preview_handled, Ordering::Release);
            return true;
        }
        false
    }
}
impl Drop for TrackedRequest {
    fn drop(&mut self) {
        if self.request.is_some() {
            self.activity.clear(self.id);
        }
    }
}

struct Lease {
    id: uuid::Uuid,
    accepted: bool,
    touched: Instant,
    session_id: Option<SessionId>,
    cancel: CancellationToken,
}
impl Lease {
    fn expired(&self) -> bool {
        self.touched.elapsed() > Duration::from_secs(if self.accepted { 370 } else { 65 })
    }
    fn terminal_event(self) -> Event {
        self.cancel.cancel();
        Event::Server(ServerEvent::SessionDone {
            session_id: self
                .session_id
                .unwrap_or_else(|| SessionId::from_string(self.id.to_string())),
        })
    }
}
#[derive(Default)]
struct ReceiveActivity {
    lease: Mutex<Option<Lease>>,
    writers: AtomicUsize,
    writers_done: tokio::sync::Notify,
    // Keep this receipt outside the lease: a cancellation command can remove
    // that lease after the server has already queued TextReceived. The native
    // server sends that event before any next TransferRequest on its FIFO
    // channel. Consuming the receipt or tracking the next offer resets it.
    handled_preview: AtomicBool,
}
impl ReceiveActivity {
    fn cancel_request(&self) {
        if let Some(lease) = self.lease.lock().unwrap().as_ref() {
            lease.cancel.cancel();
        }
    }
    async fn wait_idle(&self) {
        loop {
            let notified = self.writers_done.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.writers.load(Ordering::Acquire) == 0 {
                break;
            }
            notified.await;
        }
    }
    fn track(self: &Arc<Self>, request: PendingRequest) -> IncomingRequest {
        let id = uuid::Uuid::new_v4();
        let cancel = CancellationToken::new();
        *self.lease.lock().unwrap() = Some(Lease {
            id,
            accepted: false,
            touched: Instant::now(),
            session_id: None,
            cancel: cancel.clone(),
        });
        self.handled_preview.store(false, Ordering::Release);
        IncomingRequest::Native(TrackedRequest {
            request: Some(request),
            activity: self.clone(),
            id,
            cancel,
        })
    }
    fn clear(&self, id: uuid::Uuid) {
        let mut lease = self.lease.lock().unwrap();
        if lease.as_ref().is_some_and(|lease| lease.id == id) {
            *lease = None;
        }
    }
    fn touch(&self, session_id: &SessionId) {
        if let Some(lease) = self.lease.lock().unwrap().as_mut() {
            lease.touched = Instant::now();
            lease.session_id = Some(session_id.clone());
        }
    }
    fn busy(&self) -> bool {
        // The backend expires consent after 60s and idle sessions after 300s,
        // sweeping every 60s. Allow slack, and never expire an open receive sink.
        if self.writers.load(Ordering::Acquire) > 0 {
            return true;
        }
        self.lease
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|lease| !lease.expired())
    }
    fn expire(&self) -> Option<Lease> {
        if self.writers.load(Ordering::Acquire) > 0 {
            return None;
        }
        let mut lease = self.lease.lock().unwrap();
        if lease.as_ref().is_some_and(Lease::expired) {
            lease.take()
        } else {
            None
        }
    }
}

struct WriterGuard(Arc<ReceiveActivity>);
impl Drop for WriterGuard {
    fn drop(&mut self) {
        self.0.writers.fetch_sub(1, Ordering::Release);
        self.0.writers_done.notify_waiters();
    }
}
struct TrackedSink(Arc<ReceiveActivity>);
struct TrackedFile {
    file: Option<Box<dyn PendingReceive>>,
    _guard: WriterGuard,
}
#[async_trait::async_trait]
impl ReceiveSink for TrackedSink {
    async fn create(
        &self,
        directory: &Path,
        name: &str,
    ) -> Result<Box<dyn PendingReceive>, SinkError> {
        self.0.writers.fetch_add(1, Ordering::AcqRel);
        let guard = WriterGuard(self.0.clone());
        let file = AtomicFileSink.create(directory, name).await?;
        Ok(Box::new(TrackedFile {
            file: Some(file),
            _guard: guard,
        }))
    }
}
#[async_trait::async_trait]
impl PendingReceive for TrackedFile {
    fn writer(&mut self) -> &mut (dyn tokio::io::AsyncWrite + Unpin + Send) {
        self.file.as_mut().unwrap().writer()
    }
    fn display_path(&self) -> &Path {
        self.file.as_ref().unwrap().display_path()
    }
    async fn commit(mut self: Box<Self>) -> Result<PathBuf, SinkError> {
        self.file.take().unwrap().commit().await
    }
    async fn abort(mut self: Box<Self>) -> Result<(), SinkError> {
        self.file.take().unwrap().abort().await
    }
}
pub fn certificate() -> Result<TlsCertificate, String> {
    let path = Settings::directory().join("identity.json");
    match std::fs::read(&path) {
        Ok(bytes) => {
            let (cert, key): (String, String) =
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            tls_certificate_from_pem(cert, key).map_err(|e| e.to_string())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let cert = generate_tls_certificate().map_err(|e| e.to_string())?;
            let bytes =
                serde_json::to_vec(&(&cert.cert_pem, &cert.key_pem)).map_err(|e| e.to_string())?;
            atomic_write(&path, &bytes).map_err(|e| e.to_string())?;
            Ok(cert)
        }
        Err(e) => Err(e.to_string()),
    }
}

async fn build_server(
    settings: &Settings,
    activity: Arc<ReceiveActivity>,
    certificate: &TlsCertificate,
) -> Result<(LocalSendServer, tokio::sync::mpsc::Receiver<ServerEvent>), String> {
    settings.validate().map_err(|e| e.to_string())?;
    tokio::fs::create_dir_all(&settings.save_dir)
        .await
        .map_err(|e| e.to_string())?;
    let mut builder = LocalSendServer::builder()
        .alias(&settings.alias)
        .port(settings.port)
        .save_dir(&settings.save_dir)
        .protocol(Protocol::Https)
        // Quick Save is decided below so every native session is visible to
        // activity tracking before the sender receives permission to upload.
        .auto_accept(false)
        .sink(Arc::new(TrackedSink(activity)))
        .tls_certificate(certificate.clone());
    if let Some(pin) = settings.receive_pin.as_ref() {
        builder = builder.pin(pin);
    }
    builder.build().await.map_err(|e| e.to_string())
}

async fn start_discovery(
    identity: DeviceInfo,
    certificate: &TlsCertificate,
    events: &async_channel::Sender<Event>,
    enabled: bool,
) -> (MulticastDiscovery, bool) {
    let mut discovery = MulticastDiscovery::new_with_device(identity);
    discovery.set_client_certificate(certificate.clone());
    let tx = events.clone();
    discovery.on_discovered(move |peer| {
        let _ = tx.try_send(Event::Peer(peer));
    });
    let started = if !enabled {
        false
    } else {
        match discovery.start().await {
            Ok(()) => true,
            Err(e) => {
                let _ = events
                    .send(Event::Error(format!("Device discovery: {e}")))
                    .await;
                false
            }
        }
    };
    (discovery, started)
}
fn announce(
    discovery: &MulticastDiscovery,
    events: &async_channel::Sender<Event>,
) -> tokio::task::JoinHandle<()> {
    let discovery = discovery.clone();
    let tx = events.clone();
    tokio::spawn(async move {
        if let Err(e) = discovery.announce_presence().await {
            let _ = tx
                .send(Event::Error(format!("Device discovery: {e}")))
                .await;
        }
    })
}

pub async fn run(
    settings: Settings,
    commands: async_channel::Receiver<Command>,
    events: async_channel::Sender<Event>,
) {
    let certificate = match certificate() {
        Ok(certificate) => certificate,
        Err(error) => {
            let _ = events.send(Event::Offline(error)).await;
            return;
        }
    };
    run_with_certificate(settings, commands, events, certificate, true).await;
}

async fn stop_native(
    server: &mut Option<LocalSendServer>,
    incoming: &mut Option<tokio::sync::mpsc::Receiver<ServerEvent>>,
    activity: &Arc<ReceiveActivity>,
    events: &async_channel::Sender<Event>,
) {
    activity.cancel_request();
    if let Some(mut server) = server.take() {
        if let Ok(Some(session_id)) = server.cancel_incoming().await {
            let _ = events
                .send(Event::Server(ServerEvent::SessionDone { session_id }))
                .await;
        }
        server.stop().await;
    }
    activity.wait_idle().await;
    // A publication can win its atomic commit immediately before Stop. Preserve
    // its receipt, while discarding queued progress and declining stale offers.
    if let Some(mut receiver) = incoming.take() {
        while let Ok(event) = receiver.try_recv() {
            match event {
                ServerEvent::FileReceived { .. } | ServerEvent::SessionDone { .. } => {
                    let _ = events.send(Event::Server(event)).await;
                }
                ServerEvent::TextReceived {
                    text, sender_alias, ..
                } => {
                    let preview_handled = activity.handled_preview.swap(false, Ordering::AcqRel);
                    let _ = events
                        .send(Event::TextReceived {
                            text,
                            sender_alias,
                            preview_handled,
                        })
                        .await;
                }
                ServerEvent::TransferRequest(request) => request.decline(),
                _ => {}
            }
        }
    }
    let previous = activity.lease.lock().unwrap().take();
    if let Some(previous) = previous {
        let _ = events.send(previous.terminal_event()).await;
    }
}

async fn run_with_certificate(
    mut settings: Settings,
    commands: async_channel::Receiver<Command>,
    events: async_channel::Sender<Event>,
    certificate: TlsCertificate,
    discover: bool,
) {
    let mut activity = Arc::new(ReceiveActivity::default());
    let mut server = None;
    let mut incoming = None;
    let mut discovery = None;
    let mut discovery_started = false;
    let mut announcement = None;
    match build_server(&settings, activity.clone(), &certificate).await {
        Ok((started, receiver)) => {
            let (found, enabled) =
                start_discovery(started.device().clone(), &certificate, &events, discover).await;
            if enabled {
                announcement = Some(announce(&found, &events));
            }
            discovery = Some(found);
            discovery_started = enabled;
            let _ = events.send(Event::Ready(started.device().clone())).await;
            server = Some(started);
            incoming = Some(receiver);
        }
        Err(error) => {
            let _ = events.send(Event::Offline(error)).await;
        }
    }
    let _ = events
        .send(Event::ServerState {
            running: server.is_some(),
            settings: settings.clone(),
        })
        .await;
    let mut browser: Option<crate::web_receive::BrowserServer> = None;
    let mut web_share: Option<crate::web_share::BrowserShare> = None;
    let mut share_receiver: Option<async_channel::Receiver<crate::web_share::ShareEvent>> = None;
    let mut expiry = tokio::time::interval(Duration::from_secs(5));
    let mut cancelled_sessions = VecDeque::<SessionId>::new();
    loop {
        tokio::select! {
            event = async { incoming.as_mut().unwrap().recv().await }, if incoming.is_some() => match event {
                Some(ServerEvent::PeerRegistered(peer)) => { let _ = events.send(Event::Peer(peer)).await; }
                Some(ServerEvent::TransferRequest(request)) => {
                    // Backend idle expiry does not emit SessionDone. A new offer
                    // proves the previous session was released, even if its UI is stale.
                    let previous = activity.lease.lock().unwrap().take();
                    if let Some(previous) = previous {
                        if let Some(id) = previous.session_id.as_ref() {
                            if !cancelled_sessions.contains(id) {
                                cancelled_sessions.push_back(id.clone());
                                if cancelled_sessions.len() > 128 { cancelled_sessions.pop_front(); }
                            }
                        }
                        let _ = events.send(previous.terminal_event()).await;
                    }
                    // Capture the identity established by the TLS handshake
                    // before wrapping the one-shot consent handle.
                    let authenticated_fingerprint =
                        request.authenticated_fingerprint().map(str::to_owned);
                    let request = activity.track(request);
                    if should_quick_save_native(
                        settings.quick_save,
                        &settings.favorites,
                        request.sender(),
                        authenticated_fingerprint.as_deref(),
                    ) && inline_message(request.files()).is_none()
                    {
                        request.accept();
                    } else if events.send(Event::TransferRequest(request)).await.is_err() {
                        break;
                    }
                }
                Some(ServerEvent::TextReceived { text, sender_alias, .. }) => {
                    let preview_handled = activity.handled_preview.swap(false, Ordering::AcqRel);
                    if events.send(Event::TextReceived { text, sender_alias, preview_handled }).await.is_err() { break; }
                }
                Some(mut event) => {
                    let session_id = match &event {
                        ServerEvent::SessionDone { session_id }
                        | ServerEvent::FileReceiveProgress { session_id, .. }
                        | ServerEvent::FileReceived { session_id, .. } => Some(session_id),
                        _ => None,
                    };
                    if session_id.is_some_and(|id| cancelled_sessions.contains(id)) {
                        // A publication that won before cancellation is still history.
                        // Buffered progress/terminal events must not revive the old UI
                        // or clear the lease of a newly offered transfer.
                        if matches!(&event, ServerEvent::FileReceived { .. }) { let _ = events.send(Event::Server(event)).await; }
                        continue;
                    }
                    match &mut event {
                        ServerEvent::SessionDone { session_id } => {
                            if let Some(lease) = activity.lease.lock().unwrap().take() {
                                // A sender can cancel after consent but before
                                // uploading. In that case the UI only knows the
                                // tracked offer id; never guess its pending card.
                                if lease.session_id.is_none() {
                                    *session_id = SessionId::from_string(lease.id.to_string());
                                }
                                lease.cancel.cancel();
                            }
                        }
                        ServerEvent::FileReceiveProgress { session_id, .. } | ServerEvent::FileReceived { session_id, .. } => activity.touch(session_id),
                        _ => {}
                    }
                    if events.send(Event::Server(event)).await.is_err() { break; }
                }
                None => break,
            },
            command = commands.recv() => match command {
                Ok(Command::Refresh) if discovery_started => {
                    if announcement.as_ref().is_none_or(|task| task.is_finished()) { announcement = Some(announce(discovery.as_ref().unwrap(), &events)); }
                }
                Ok(Command::QuickSave(mode)) => settings.quick_save = mode,
                Ok(Command::Favorites(favorites)) => settings.favorites = favorites,
                Ok(Command::StartWebReceive) if server.is_none() => {
                    let _ = events.send(Event::Error("Start the server before receiving via link.".into())).await;
                    let _ = events.send(Event::WebReceiveStopped).await;
                }
                Ok(Command::StartWebReceive) => {
                    if let Some(browser) = &browser { let _ = events.send(Event::WebReceiveReady(browser.urls.clone())).await; }
                    else {
                        match crate::web_receive::BrowserServer::start(&settings.save_dir, events.clone()).await {
                            Ok(started) => { let _ = events.send(Event::WebReceiveReady(started.urls.clone())).await; browser = Some(started); }
                            Err(e) => { let _ = events.send(Event::Error(format!("Could not start browser receiving: {e}"))).await; let _ = events.send(Event::WebReceiveStopped).await; }
                        }
                    }
                }
                Ok(Command::StopWebReceive) => {
                    if let Some(browser) = browser.take() { browser.stop().await; }
                    let _ = events.send(Event::WebReceiveStopped).await;
                }
                Ok(Command::CancelIncoming) => {
                    activity.cancel_request();
                    if let Some(browser) = &browser { browser.cancel_incoming().await; }
                    let result = if let Some(server) = server.as_ref() { server.cancel_incoming().await } else { Ok(None) };
                    let cancelled = match result {
                        Ok(cancelled) => cancelled,
                        Err(error) => {
                            let _ = events.send(Event::IncomingCancelFailed(format!("Could not cancel the incoming transfer: {error}"))).await;
                            continue;
                        }
                    };
                    activity.wait_idle().await;
                    if let Some(id) = cancelled {
                        cancelled_sessions.push_back(id.clone());
                        if cancelled_sessions.len() > 128 { cancelled_sessions.pop_front(); }
                        let _ = events.send(Event::Server(ServerEvent::SessionDone { session_id: id })).await;
                    }
                    let previous = activity.lease.lock().unwrap().take();
                    if let Some(previous) = previous {
                        if let Some(id) = previous.session_id.as_ref() {
                            if !cancelled_sessions.contains(id) {
                                cancelled_sessions.push_back(id.clone());
                                if cancelled_sessions.len() > 128 { cancelled_sessions.pop_front(); }
                            }
                        }
                        let _ = events.send(previous.terminal_event()).await;
                    }
                    let _ = events.send(Event::IncomingCanceled).await;
                }
                Ok(Command::StartWebShare { .. }) if server.is_none() => {
                    let _ = events.send(Event::Error("Start the server before sharing via link.".into())).await;
                    let _ = events.send(Event::WebShareStopped).await;
                }
                Ok(Command::StartWebShare { items, auto_accept }) => {
                    let (share_events, receiver) = async_channel::unbounded();
                    match crate::web_share::BrowserShare::start(items, auto_accept, share_events).await {
                        Ok(started) => {
                            drop(share_receiver.take());
                            if let Some(previous) = web_share.take() { previous.stop().await; }
                            let _ = events.send(Event::WebShareReady(started.urls.clone())).await;
                            web_share = Some(started);
                            share_receiver = Some(receiver);
                        }
                        Err(error) => {
                            let _ = events.send(Event::Error(format!("Could not start browser sharing: {error}"))).await;
                            if web_share.is_none() { let _ = events.send(Event::WebShareStopped).await; }
                        }
                    }
                }
                Ok(Command::SetWebShareAutoAccept(enabled)) => {
                    if let Some(share) = &web_share { share.set_auto_accept(enabled); }
                }
                Ok(Command::StopWebShare) => {
                    share_receiver = None;
                    if let Some(share) = web_share.take() { share.stop().await; }
                    let _ = events.send(Event::WebShareStopped).await;
                }
                Ok(Command::StopServer) => {
                    if let Some(task) = announcement.take() { task.abort(); }
                    if let Some(mut discovery) = discovery.take() { discovery.stop(); }
                    discovery_started = false;
                    if let Some(browser) = browser.take() { browser.stop().await; let _ = events.send(Event::WebReceiveStopped).await; }
                    drop(share_receiver.take());
                    if let Some(share) = web_share.take() { share.stop().await; let _ = events.send(Event::WebShareStopped).await; }
                    stop_native(&mut server, &mut incoming, &activity, &events).await;
                    activity = Arc::new(ReceiveActivity::default());
                    cancelled_sessions.clear();
                    let _ = events.send(Event::ServerState { running: false, settings: settings.clone() }).await;
                }
                Ok(Command::StartServer(_)) if server.is_some() => {
                    let _ = events.send(Event::ServerState { running: true, settings: settings.clone() }).await;
                }
                Ok(Command::Reconfigure(next) | Command::StartServer(next)) => {
                    if let Err(e) = next.validate() {
                        let _ = events.send(Event::Error(e.to_string())).await;
                        let _ = events.send(Event::ServerState { running: server.is_some(), settings: settings.clone() }).await;
                        continue;
                    }
                    if activity.busy() || browser.as_ref().is_some_and(|browser| browser.busy()) || web_share.as_ref().is_some_and(|share| share.busy()) {
                        let _ = events.send(Event::Error("Finish or decline the incoming transfer or browser download before applying network changes.".into())).await;
                        let _ = events.send(Event::ServerState { running: server.is_some(), settings: settings.clone() }).await;
                        continue;
                    }
                    // Keep the persisted certificate: aliases, ports and PINs must not create a new identity.
                    let was_running = server.is_some();
                    if let Some(task) = announcement.take() { task.abort(); }
                    if let Some(mut discovery) = discovery.take() { discovery.stop(); }
                    discovery_started = false;
                    if let Some(browser) = browser.take() { browser.stop().await; let _ = events.send(Event::WebReceiveStopped).await; }
                    drop(share_receiver.take());
                    if let Some(share) = web_share.take() { share.stop().await; let _ = events.send(Event::WebShareStopped).await; }
                    stop_native(&mut server, &mut incoming, &activity, &events).await;
                    activity = Arc::new(ReceiveActivity::default());
                    cancelled_sessions.clear();
                    match build_server(&next, activity.clone(), &certificate).await {
                        Ok((replacement, receiver)) => { server = Some(replacement); incoming = Some(receiver); settings = next; }
                        Err(error) => {
                            if was_running {
                                let _ = events.send(Event::Error(format!("Could not apply network settings: {error}. Restoring the previous listener."))).await;
                                match build_server(&settings, activity.clone(), &certificate).await {
                                    Ok((replacement, receiver)) => { server = Some(replacement); incoming = Some(receiver); }
                                    Err(error) => { let _ = events.send(Event::Offline(format!("Could not restore receiving: {error}"))).await; }
                                }
                            } else {
                                let _ = events.send(Event::Offline(format!("Could not start receiving: {error}"))).await;
                            }
                        }
                    }
                    if let Some(server) = server.as_ref() {
                        let (found, enabled) = start_discovery(
                            server.device().clone(),
                            &certificate,
                            &events,
                            discover,
                        )
                        .await;
                        discovery_started = enabled;
                        if enabled { announcement = Some(announce(&found, &events)); }
                        discovery = Some(found);
                        let _ = events.send(Event::Ready(server.device().clone())).await;
                    }
                    let _ = events.send(Event::ServerState { running: server.is_some(), settings: settings.clone() }).await;
                }
                Ok(Command::Shutdown) | Err(_) => break,
                _ => {}
            },
            event = async { share_receiver.as_ref().unwrap().recv().await }, if share_receiver.is_some() => {
                match event {
                    Ok(event) => { let _ = events.send(Event::WebShare(event)).await; }
                    Err(_) => share_receiver = None,
                }
            },
            _ = expiry.tick() => {
                if let Some(expired) = activity.expire() {
                    if expired.accepted { let _ = events.send(Event::Error("Incoming transfer timed out before all files arrived.".into())).await; }
                    let _ = events.send(expired.terminal_event()).await;
                }
            },
        }
    }
    if let Some(task) = announcement {
        task.abort();
    }
    if let Some(browser) = browser {
        browser.stop().await;
    }
    if let Some(share) = web_share {
        share.stop().await;
    }
    if let Some(mut discovery) = discovery {
        discovery.stop();
    }
    stop_native(&mut server, &mut incoming, &activity, &events).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use localsend_rs::protocol::{PrepareUploadRequest, PrepareUploadResponse};
    use localsend_rs::{LocalSendClient, TlsTrustPolicy};

    fn free_port() -> u16 {
        #[cfg(target_os = "linux")]
        {
            use std::sync::{atomic::AtomicU32, OnceLock};

            static NEXT: AtomicU32 = AtomicU32::new(1024);
            static EPHEMERAL: OnceLock<(u16, u16)> = OnceLock::new();
            let &(first, last) = EPHEMERAL.get_or_init(|| {
                let range = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
                    .expect("read Linux automatic TCP port range");
                let ports: Vec<u16> = range
                    .split_whitespace()
                    .map(|port| port.parse().expect("valid automatic TCP port"))
                    .collect();
                assert_eq!(ports.len(), 2);
                assert!(ports[0] <= ports[1]);
                (ports[0], ports[1])
            });
            loop {
                let candidate = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let port = u16::try_from(candidate).expect("no available fixture TCP ports");
                if (first..=last).contains(&port) {
                    continue;
                }
                // Reconfiguration closes and rebinds this port. Never reuse a
                // fixture port or choose one that parallel port-zero listeners
                // or client connections can claim while it is briefly closed.
                // Probe the same wildcard address used by LocalSendServer.
                // An external process explicitly binding it can still race us.
                if std::net::TcpListener::bind(("0.0.0.0", port)).is_ok() {
                    return port;
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            std::net::TcpListener::bind("0.0.0.0:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port()
        }
    }
    fn test_client() -> reqwest::Client {
        // HTTP-only server fixtures may construct a client before any TLS
        // certificate is generated. Do not depend on another test installing it.
        localsend_rs::crypto::ensure_crypto_provider();
        reqwest::Client::builder()
            .no_proxy()
            .danger_accept_invalid_certs(true)
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }

    fn favorite_sender() -> (DeviceInfo, crate::settings::FavoriteDevice) {
        let mut sender = DeviceInfo::new("Kitchen tablet".into(), 53317, Protocol::Https);
        sender.fingerprint = "a1".repeat(32);
        sender.ip = Some("192.168.1.20".into());
        let favorite = crate::settings::FavoriteDevice::from_peer(&sender, "").unwrap();
        (sender, favorite)
    }

    #[test]
    fn favorite_quick_save_requires_the_authenticated_pinned_identity() {
        let (mut sender, favorite) = favorite_sender();
        let authenticated = sender.fingerprint.clone();
        assert!(should_quick_save_native(
            QuickSaveMode::Paired,
            std::slice::from_ref(&favorite),
            &sender,
            Some(&authenticated),
        ));

        // Aliases are presentation only and never participate in trust.
        sender.alias = "A renamed tablet".into();
        assert!(should_quick_save_native(
            QuickSaveMode::Paired,
            std::slice::from_ref(&favorite),
            &sender,
            Some(&authenticated),
        ));

        assert!(!should_quick_save_native(
            QuickSaveMode::Paired,
            std::slice::from_ref(&favorite),
            &sender,
            None,
        ));
        assert!(!should_quick_save_native(
            QuickSaveMode::Paired,
            std::slice::from_ref(&favorite),
            &sender,
            Some(&"b2".repeat(32)),
        ));
    }

    #[test]
    fn favorite_quick_save_rejects_transport_mismatch_and_global_mode_wins() {
        let (mut sender, favorite) = favorite_sender();
        let authenticated = sender.fingerprint.clone();
        sender.protocol = Protocol::Http;
        assert!(!should_quick_save_native(
            QuickSaveMode::Paired,
            std::slice::from_ref(&favorite),
            &sender,
            Some(&authenticated),
        ));
        assert!(should_quick_save_native(
            QuickSaveMode::On,
            &[],
            &sender,
            None,
        ));
        assert!(!should_quick_save_native(
            QuickSaveMode::Off,
            std::slice::from_ref(&favorite),
            &sender,
            Some(&authenticated),
        ));
    }

    #[tokio::test]
    async fn favorite_quick_save_tracks_runtime_favorite_additions_and_removals() {
        let root = tempfile::tempdir().unwrap();
        let server_certificate = generate_tls_certificate().unwrap();
        let settings = Settings {
            port: free_port(),
            save_dir: root.path().into(),
            quick_save: QuickSaveMode::Paired,
            ..Settings::default()
        };
        let (commands, command_rx) = async_channel::unbounded();
        let (event_tx, events) = async_channel::unbounded();
        let task = tokio::spawn(run_with_certificate(
            settings,
            command_rx,
            event_tx,
            server_certificate,
            false,
        ));
        let mut target = ready(&events).await;
        target.ip = Some("127.0.0.1".into());

        let sender_certificate = generate_tls_certificate().unwrap();
        let mut sender = DeviceInfo::new("Runtime favorite".into(), 53317, Protocol::Https);
        sender.fingerprint = sender_certificate.fingerprint.clone();
        sender.ip = Some("127.0.0.1".into());
        let favorite = crate::settings::FavoriteDevice::from_peer(&sender, "").unwrap();
        let client = LocalSendClient::with_trust_policy_and_client_certificate(
            sender,
            TlsTrustPolicy::new([target.fingerprint.clone()]),
            &sender_certificate,
        )
        .unwrap();

        let first = tokio::spawn({
            let client = client.clone();
            let target = target.clone();
            async move { client.prepare_upload(&target, offer().files, None).await }
        });
        match next_event(&events).await {
            Event::TransferRequest(request) => request.decline(),
            _ => panic!("A device that is not a favorite must require consent"),
        }
        assert!(first.await.unwrap().is_err());

        commands
            .send(Command::Favorites(vec![favorite]))
            .await
            .unwrap();
        // Commands are FIFO. This acknowledgement proves the favorite update
        // was applied before the next HTTPS offer reaches the listener.
        commands
            .send(Command::StartServer(Settings::default()))
            .await
            .unwrap();
        server_state(&events, true).await;
        let accepted = client
            .prepare_upload(&target, offer().files, None)
            .await
            .expect("a newly added authenticated favorite should be accepted immediately");
        assert!(!accepted.session_id.as_str().is_empty());
        assert!(
            tokio::time::timeout(Duration::from_millis(100), events.recv())
                .await
                .is_err(),
            "automatic consent must not emit a TransferRequest"
        );
        commands.send(Command::CancelIncoming).await.unwrap();
        cancelled(&events).await;

        commands.send(Command::Favorites(Vec::new())).await.unwrap();
        commands
            .send(Command::StartServer(Settings::default()))
            .await
            .unwrap();
        server_state(&events, true).await;
        let after_removal = tokio::spawn({
            let client = client.clone();
            let target = target.clone();
            async move { client.prepare_upload(&target, offer().files, None).await }
        });
        match next_event(&events).await {
            Event::TransferRequest(request) => request.decline(),
            _ => panic!("A removed favorite must require consent immediately"),
        }
        assert!(after_removal.await.unwrap().is_err());
        shutdown(commands, task).await;
    }

    async fn next_event(events: &async_channel::Receiver<Event>) -> Event {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let event = events.recv().await.expect("network event channel closed");
                if !matches!(event, Event::ServerState { .. }) {
                    return event;
                }
            }
        })
        .await
        .expect("network event timed out")
    }
    async fn ready(events: &async_channel::Receiver<Event>) -> DeviceInfo {
        match next_event(events).await {
            Event::Ready(identity) => identity,
            Event::Offline(error) | Event::Error(error) => {
                panic!("Network fixture failed: {error}")
            }
            _ => panic!("Expected ready event"),
        }
    }
    async fn server_state(
        events: &async_channel::Receiver<Event>,
        expected_running: bool,
    ) -> Settings {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match events.recv().await.expect("network event channel closed") {
                    Event::ServerState { running, settings } => {
                        assert_eq!(running, expected_running);
                        return settings;
                    }
                    Event::Server(ServerEvent::SessionDone { .. }) => {}
                    _ => panic!("Expected server state acknowledgement"),
                }
            }
        })
        .await
        .expect("server state timed out")
    }
    fn spawn_fixture(
        settings: Settings,
    ) -> (
        async_channel::Sender<Command>,
        async_channel::Receiver<Event>,
        tokio::task::JoinHandle<()>,
    ) {
        let (commands, receiver) = async_channel::unbounded();
        let (sender, events) = async_channel::unbounded();
        // A fresh injected certificate and disabled discovery make this an entirely
        // local service test: no persisted identity, settings, or LAN announcements.
        let certificate = generate_tls_certificate().unwrap();
        let task = tokio::spawn(run_with_certificate(
            settings,
            receiver,
            sender,
            certificate,
            false,
        ));
        (commands, events, task)
    }
    async fn shutdown(commands: async_channel::Sender<Command>, task: tokio::task::JoinHandle<()>) {
        commands.send(Command::Shutdown).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
    }
    fn offer() -> PrepareUploadRequest {
        let id = FileId::from_string("test".into());
        let file = FileMetadata {
            id: id.clone(),
            file_name: "received.txt".into(),
            size: 3,
            file_type: "application/octet-stream".into(),
            sha256: None,
            preview: None,
            metadata: None,
        };
        PrepareUploadRequest {
            info: DeviceInfo::new("Test sender".into(), 53317, Protocol::Http),
            files: HashMap::from([(id, file)]),
        }
    }

    fn message_offer() -> PrepareUploadRequest {
        let mut request = offer();
        let file = request.files.values_mut().next().unwrap();
        file.file_name = "message.txt".into();
        file.file_type = "text/plain".into();
        file.preview = Some("A message, not an uploaded file".into());
        file.size = file.preview.as_ref().unwrap().len() as u64;
        request
    }

    fn post_message(
        client: &reqwest::Client,
        port: u16,
    ) -> tokio::task::JoinHandle<reqwest::Response> {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .post(format!(
                    "https://127.0.0.1:{port}/api/localsend/v2/prepare-upload"
                ))
                .json(&message_offer())
                .send()
                .await
                .unwrap()
        })
    }

    async fn message_receipt(events: &async_channel::Receiver<Event>) -> bool {
        loop {
            match next_event(events).await {
                Event::TextReceived {
                    text,
                    sender_alias,
                    preview_handled,
                } => {
                    assert_eq!(text, "A message, not an uploaded file");
                    assert_eq!(sender_alias, "Test sender");
                    return preview_handled;
                }
                Event::Server(ServerEvent::SessionDone { .. }) => {}
                _ => panic!("Expected a completed message receipt"),
            }
        }
    }

    async fn pending_message(events: &async_channel::Receiver<Event>) -> IncomingRequest {
        loop {
            match next_event(events).await {
                Event::TransferRequest(request) => return request,
                Event::Server(ServerEvent::SessionDone { .. }) => {}
                _ => panic!("Expected the message preview before acceptance"),
            }
        }
    }

    #[tokio::test]
    async fn inline_preview_waits_for_action_and_does_not_duplicate_or_hide_the_next_message() {
        let root = tempfile::tempdir().unwrap();
        let (commands, events, task) = spawn_fixture(Settings {
            port: free_port(),
            save_dir: root.path().into(),
            ..Settings::default()
        });
        let identity = ready(&events).await;
        let client = test_client();
        let first = post_message(&client, identity.port);
        let request = pending_message(&events).await;
        assert_eq!(
            request.inline_message(),
            Some("A message, not an uploaded file")
        );
        assert!(
            !first.is_finished(),
            "Previewing must not acknowledge the offer"
        );
        assert!(request.accept_preview());
        assert_eq!(
            first.await.unwrap().status(),
            reqwest::StatusCode::NO_CONTENT
        );
        assert!(
            message_receipt(&events).await,
            "Do not display an acted-on preview twice"
        );

        // The same sender and same text is a distinct offer, not an alias/text
        // deduplication key. Regular/quick acceptance still needs its dialog.
        let second = post_message(&client, identity.port);
        pending_message(&events).await.accept();
        assert_eq!(
            second.await.unwrap().status(),
            reqwest::StatusCode::NO_CONTENT
        );
        assert!(!message_receipt(&events).await);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        shutdown(commands, task).await;
    }

    #[tokio::test]
    async fn quick_save_does_not_bypass_inline_message_consent() {
        let root = tempfile::tempdir().unwrap();
        let (commands, events, task) = spawn_fixture(Settings {
            port: free_port(),
            save_dir: root.path().into(),
            quick_save: QuickSaveMode::On,
            ..Settings::default()
        });
        let identity = ready(&events).await;
        let response = post_message(&test_client(), identity.port);
        let request = pending_message(&events).await;
        assert_eq!(
            request.inline_message(),
            Some("A message, not an uploaded file")
        );
        assert!(!response.is_finished());
        assert!(request.accept_preview());
        let response = response.await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
        assert!(message_receipt(&events).await);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        shutdown(commands, task).await;
    }

    #[tokio::test]
    async fn withdrawn_inline_preview_cannot_acknowledge_or_suppress_a_later_message() {
        let root = tempfile::tempdir().unwrap();
        let (commands, events, task) = spawn_fixture(Settings {
            port: free_port(),
            save_dir: root.path().into(),
            ..Settings::default()
        });
        let identity = ready(&events).await;
        let client = test_client();
        let first = post_message(&client, identity.port);
        let request = pending_message(&events).await;
        commands.send(Command::CancelIncoming).await.unwrap();
        cancelled(&events).await;
        assert!(
            !request.accept_preview(),
            "A withdrawn preview must not launch Open/Copy"
        );
        assert_eq!(
            first.await.unwrap().status(),
            reqwest::StatusCode::FORBIDDEN
        );
        let second = post_message(&client, identity.port);
        pending_message(&events).await.accept();
        assert_eq!(
            second.await.unwrap().status(),
            reqwest::StatusCode::NO_CONTENT
        );
        assert!(!message_receipt(&events).await);
        shutdown(commands, task).await;
    }

    async fn cancelled(events: &async_channel::Receiver<Event>) {
        loop {
            match next_event(events).await {
                Event::IncomingCanceled => return,
                Event::Offline(error) | Event::Error(error) => {
                    panic!("Cancellation failed: {error}")
                }
                _ => {}
            }
        }
    }

    async fn prepare(client: &reqwest::Client, base: &str) -> PrepareUploadResponse {
        let response = client
            .post(format!("{base}/prepare-upload"))
            .json(&offer())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        response.json().await.unwrap()
    }

    async fn upload_three(
        client: &reqwest::Client,
        base: &str,
        approval: &PrepareUploadResponse,
    ) -> reqwest::Response {
        let id = FileId::from_string("test".into());
        client
            .post(format!("{base}/upload"))
            .query(&[
                ("sessionId", approval.session_id.as_str()),
                ("fileId", id.as_str()),
                ("token", approval.files[&id].as_str()),
            ])
            .body("new")
            .send()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn cancel_waiting_consent_closes_dialog_and_stale_accept_cannot_revive_it() {
        let root = tempfile::tempdir().unwrap();
        let settings = Settings {
            port: free_port(),
            save_dir: root.path().into(),
            ..Settings::default()
        };
        let (commands, events, task) = spawn_fixture(settings);
        let identity = ready(&events).await;
        let client = test_client();
        let base = format!("https://127.0.0.1:{}/api/localsend/v2", identity.port);
        let first = tokio::spawn({
            let client = client.clone();
            let base = base.clone();
            async move {
                client
                    .post(format!("{base}/prepare-upload"))
                    .json(&offer())
                    .send()
                    .await
                    .unwrap()
            }
        });
        let pending = match next_event(&events).await {
            Event::TransferRequest(request) => request,
            _ => panic!("Expected consent"),
        };
        let token = pending.cancellation().unwrap();
        commands.send(Command::CancelIncoming).await.unwrap();
        cancelled(&events).await;
        assert!(token.is_cancelled());
        pending.accept();
        assert_eq!(
            first.await.unwrap().status(),
            reqwest::StatusCode::FORBIDDEN
        );
        let next = tokio::spawn({
            let client = client.clone();
            let base = base.clone();
            async move { prepare(&client, &base).await }
        });
        loop {
            if let Event::TransferRequest(request) = next_event(&events).await {
                request.accept();
                break;
            }
        }
        let approval = next.await.unwrap();
        assert!(upload_three(&client, &base, &approval)
            .await
            .status()
            .is_success());
        assert_eq!(
            std::fs::read(root.path().join("received.txt")).unwrap(),
            b"new"
        );
        shutdown(commands, task).await;
    }

    #[tokio::test]
    async fn sender_cancel_before_first_upload_preserves_the_accepted_offer_identity() {
        let root = tempfile::tempdir().unwrap();
        let (commands, events, task) = spawn_fixture(Settings {
            port: free_port(),
            save_dir: root.path().into(),
            ..Settings::default()
        });
        let identity = ready(&events).await;
        let client = test_client();
        let base = format!("https://127.0.0.1:{}/api/localsend/v2", identity.port);
        let preparing = tokio::spawn({
            let client = client.clone();
            let base = base.clone();
            async move { prepare(&client, &base).await }
        });
        let request = match next_event(&events).await {
            Event::TransferRequest(request) => request,
            _ => panic!("Expected consent"),
        };
        let (offer_id, needs_binding) = request.progress_identity();
        assert!(needs_binding);
        request.accept();
        let approval = preparing.await.unwrap();
        let response = client
            .post(format!("{base}/cancel"))
            .query(&[("sessionId", approval.session_id.as_str())])
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        match next_event(&events).await {
            Event::Server(ServerEvent::SessionDone { session_id }) => {
                assert_eq!(session_id.as_str(), offer_id);
            }
            _ => panic!("Expected the accepted offer's terminal event"),
        }
        shutdown(commands, task).await;
    }

    #[tokio::test]
    async fn accepted_session_cancel_preserves_listener_identity_and_reused_http_client() {
        let root = tempfile::tempdir().unwrap();
        let settings = Settings {
            port: free_port(),
            save_dir: root.path().into(),
            quick_save: QuickSaveMode::On,
            ..Settings::default()
        };
        let (commands, events, task) = spawn_fixture(settings);
        let identity = ready(&events).await;
        let client = test_client();
        let base = format!("https://127.0.0.1:{}/api/localsend/v2", identity.port);
        let first = prepare(&client, &base).await;
        commands.send(Command::CancelIncoming).await.unwrap();
        cancelled(&events).await;
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        assert!(!upload_three(&client, &base, &first)
            .await
            .status()
            .is_success());
        // This pooled client keeps its TLS connection: cancellation must not
        // strand it on a retired listener with a dead event receiver.
        let next = prepare(&client, &base).await;
        assert!(upload_three(&client, &base, &next)
            .await
            .status()
            .is_success());
        let current: DeviceInfo = client
            .get(format!("{base}/info"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(current.port, identity.port);
        assert_eq!(current.fingerprint, identity.fingerprint);
        assert_eq!(
            std::fs::read(root.path().join("received.txt")).unwrap(),
            b"new"
        );
        shutdown(commands, task).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn native_midstream_cancellation_cleans_partial_before_ack_and_allows_next_transfer() {
        let root = tempfile::tempdir().unwrap();
        let settings = Settings {
            port: free_port(),
            save_dir: root.path().into(),
            quick_save: QuickSaveMode::On,
            ..Settings::default()
        };
        let (commands, events, task) = spawn_fixture(settings);
        let identity = ready(&events).await;
        let client = test_client();
        let base = format!("https://127.0.0.1:{}/api/localsend/v2", identity.port);
        let approval = prepare(&client, &base).await;
        let (chunks, stream) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::io::Error>>(2);
        let stream = futures_util::stream::unfold(stream, |mut rx| async {
            rx.recv().await.map(|chunk| (chunk, rx))
        });
        let upload = tokio::spawn({
            let client = client.clone();
            let base = base.clone();
            async move {
                let id = FileId::from_string("test".into());
                client
                    .post(format!("{base}/upload"))
                    .query(&[
                        ("sessionId", approval.session_id.as_str()),
                        ("fileId", id.as_str()),
                        ("token", approval.files[&id].as_str()),
                    ])
                    .body(reqwest::Body::wrap_stream(stream))
                    .send()
                    .await
            }
        });
        chunks.send(Ok(b"n".to_vec())).await.unwrap();
        loop {
            if matches!(
                next_event(&events).await,
                Event::Server(ServerEvent::FileReceiveProgress { .. })
            ) {
                break;
            }
        }
        commands.send(Command::CancelIncoming).await.unwrap();
        cancelled(&events).await;
        assert_eq!(
            std::fs::read_dir(root.path()).unwrap().count(),
            0,
            "Cancellation must clean temp and reserved destination before acknowledgment"
        );
        let _ = chunks.send(Ok(b"ew".to_vec())).await;
        drop(chunks);
        if let Ok(response) = upload.await.unwrap() {
            assert!(!response.status().is_success());
        }
        let next = prepare(&client, &base).await;
        assert!(upload_three(&client, &base, &next)
            .await
            .status()
            .is_success());
        assert_eq!(
            std::fs::read(root.path().join("received.txt")).unwrap(),
            b"new"
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
        shutdown(commands, task).await;
    }

    struct CommitGate {
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }
    struct GatedSink(Arc<CommitGate>, bool);
    struct GatedFile {
        inner: Box<dyn PendingReceive>,
        gate: Option<Arc<CommitGate>>,
    }
    #[async_trait::async_trait]
    impl ReceiveSink for GatedSink {
        async fn create(
            &self,
            directory: &Path,
            name: &str,
        ) -> Result<Box<dyn PendingReceive>, SinkError> {
            if self.1 {
                self.0.entered.notify_one();
                self.0.release.notified().await;
            }
            Ok(Box::new(GatedFile {
                inner: AtomicFileSink.create(directory, name).await?,
                gate: (!self.1).then(|| self.0.clone()),
            }))
        }
    }
    #[async_trait::async_trait]
    impl PendingReceive for GatedFile {
        fn writer(&mut self) -> &mut (dyn tokio::io::AsyncWrite + Unpin + Send) {
            self.inner.writer()
        }
        fn display_path(&self) -> &Path {
            self.inner.display_path()
        }
        async fn commit(self: Box<Self>) -> Result<PathBuf, SinkError> {
            if let Some(gate) = &self.gate {
                gate.entered.notify_one();
                gate.release.notified().await;
            }
            self.inner.commit().await
        }
        async fn abort(self: Box<Self>) -> Result<(), SinkError> {
            self.inner.abort().await
        }
    }

    #[tokio::test]
    async fn receiver_cancellation_waits_for_publication_that_already_started() {
        let root = tempfile::tempdir().unwrap();
        let gate = Arc::new(CommitGate {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let (server, _events) = LocalSendServer::builder()
            .port(0)
            .save_dir(root.path())
            .auto_accept(true)
            .sink(Arc::new(GatedSink(gate.clone(), false)))
            .build()
            .await
            .unwrap();
        let server = Arc::new(server);
        let client = test_client();
        let base = format!("http://127.0.0.1:{}/api/localsend/v2", server.port());
        let approval = prepare(&client, &base).await;
        let upload = tokio::spawn({
            let client = client.clone();
            let base = base.clone();
            async move { upload_three(&client, &base, &approval).await }
        });
        tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
            .await
            .unwrap();
        let mut cancellation = tokio::spawn({
            let server = server.clone();
            async move { server.cancel_incoming().await }
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut cancellation)
                .await
                .is_err(),
            "Cancellation cannot acknowledge while publication is still pending"
        );
        gate.release.notify_one();
        assert!(upload.await.unwrap().status().is_success());
        assert!(cancellation.await.unwrap().unwrap().is_some());
        assert_eq!(
            std::fs::read(root.path().join("received.txt")).unwrap(),
            b"new"
        );
        let mut server = Arc::try_unwrap(server).ok().unwrap();
        server.stop().await;
    }

    #[tokio::test]
    async fn cancellation_waits_for_an_admitted_upload_that_has_not_created_its_file() {
        let root = tempfile::tempdir().unwrap();
        let gate = Arc::new(CommitGate {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let (server, _events) = LocalSendServer::builder()
            .port(0)
            .save_dir(root.path())
            .auto_accept(true)
            .sink(Arc::new(GatedSink(gate.clone(), true)))
            .build()
            .await
            .unwrap();
        let server = Arc::new(server);
        let client = test_client();
        let base = format!("http://127.0.0.1:{}/api/localsend/v2", server.port());
        let approval = prepare(&client, &base).await;
        let upload = tokio::spawn({
            let client = client.clone();
            let base = base.clone();
            async move { upload_three(&client, &base, &approval).await }
        });
        tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
            .await
            .unwrap();
        let mut cancellation = tokio::spawn({
            let server = server.clone();
            async move { server.cancel_incoming().await }
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut cancellation)
                .await
                .is_err(),
            "An admitted receive must settle before cancellation is acknowledged"
        );
        gate.release.notify_one();
        assert!(!upload.await.unwrap().status().is_success());
        assert!(cancellation.await.unwrap().unwrap().is_some());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        let mut server = Arc::try_unwrap(server).ok().unwrap();
        server.stop().await;
    }

    #[tokio::test]
    async fn applying_network_settings_updates_alias_port_pin_and_destination_without_rotating_identity(
    ) {
        let root = tempfile::tempdir().unwrap();
        let mut settings = Settings {
            alias: "Before".into(),
            port: free_port(),
            save_dir: root.path().join("before"),
            quick_save: QuickSaveMode::On,
            ..Settings::default()
        };
        let (commands, events, task) = spawn_fixture(settings.clone());
        let first = ready(&events).await;
        let client = test_client();
        settings.alias = "After".into();
        settings.port = free_port();
        settings.receive_pin = Some("1234".into());
        settings.save_dir = root.path().join("after");
        commands
            .send(Command::Reconfigure(settings.clone()))
            .await
            .unwrap();
        let changed = ready(&events).await;
        assert_eq!(changed.alias, "After");
        assert_eq!(changed.port, settings.port);
        assert_eq!(changed.fingerprint, first.fingerprint);
        assert_eq!(changed.protocol, Protocol::Https);
        assert!(client
            .get(format!(
                "https://127.0.0.1:{}/api/localsend/v2/info",
                first.port
            ))
            .send()
            .await
            .is_err());
        let base = format!("https://127.0.0.1:{}/api/localsend/v2", changed.port);
        let advertised: DeviceInfo = client
            .get(format!("{base}/info"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(advertised.alias, "After");
        assert_eq!(advertised.fingerprint, first.fingerprint);
        let offer = offer();
        assert_eq!(
            client
                .post(format!("{base}/prepare-upload"))
                .json(&offer)
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::UNAUTHORIZED
        );
        let prepared = client
            .post(format!("{base}/prepare-upload"))
            .query(&[("pin", "1234")])
            .json(&offer)
            .send()
            .await
            .unwrap();
        assert_eq!(prepared.status(), reqwest::StatusCode::OK);
        let prepared: PrepareUploadResponse = prepared.json().await.unwrap();
        let id = FileId::from_string("test".into());
        let uploaded = client
            .post(format!("{base}/upload"))
            .query(&[
                ("sessionId", prepared.session_id.as_str()),
                ("fileId", id.as_str()),
                ("token", prepared.files[&id].as_str()),
            ])
            .body("new")
            .send()
            .await
            .unwrap();
        assert!(uploaded.status().is_success());
        assert_eq!(
            std::fs::read(settings.save_dir.join("received.txt")).unwrap(),
            b"new"
        );
        assert!(!root.path().join("before/received.txt").exists());
        shutdown(commands, task).await;
    }

    #[tokio::test]
    async fn browser_share_consent_and_active_download_block_reconfiguration_until_idle() {
        use crate::{
            transfer::{Selection, Source},
            web_share::ShareEvent,
        };
        use futures_util::StreamExt;

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("selected.bin");
        let size = 64 * 1024 * 1024;
        std::fs::File::create(&path).unwrap().set_len(size).unwrap();
        let settings = Settings {
            alias: "Before sharing".into(),
            port: free_port(),
            save_dir: root.path().join("received"),
            ..Settings::default()
        };
        let next = Settings {
            alias: "After sharing".into(),
            ..settings.clone()
        };
        let (commands, events, task) = spawn_fixture(settings);
        let original = ready(&events).await;
        let client = test_client();
        commands
            .send(Command::StartWebShare {
                items: vec![Selection {
                    name: "selected.bin".into(),
                    size,
                    source: Source::File(path),
                }],
                auto_accept: false,
            })
            .await
            .unwrap();
        let url = match next_event(&events).await {
            Event::WebShareReady(urls) => urls
                .into_iter()
                .find(|url| url.contains("127.0.0.1"))
                .expect("Expected a loopback browser URL"),
            _ => panic!("Expected browser sharing to start"),
        };

        // First decline a real browser request, then accept another one and
        // keep its response unread so socket backpressure holds an active stream.
        for accept in [false, true] {
            let request_id = uuid::Uuid::new_v4().simple().to_string();
            let request = tokio::spawn({
                let client = client.clone();
                let url = url.clone();
                let request_id = request_id.clone();
                async move {
                    client
                        .post(format!("{url}prepare"))
                        .json(&serde_json::json!({ "requestId": request_id }))
                        .send()
                        .await
                        .unwrap()
                }
            });
            let pending = match next_event(&events).await {
                Event::WebShare(ShareEvent::DownloadRequest(pending)) => pending,
                _ => panic!("Expected forwarded browser consent"),
            };
            assert!(pending.ip().is_loopback());
            assert_eq!(pending.files().len(), 1);
            assert_eq!(pending.files()[0].name, "selected.bin");
            assert_eq!(pending.files()[0].size, size);
            commands
                .send(Command::Reconfigure(next.clone()))
                .await
                .unwrap();
            match next_event(&events).await {
                Event::Error(error) => assert!(error.contains("browser download")),
                _ => panic!("Pending browser consent must prevent reconfiguration"),
            }
            if !accept {
                let cancellation = pending.cancellation();
                pending.decline();
                assert_eq!(
                    request.await.unwrap().status(),
                    reqwest::StatusCode::FORBIDDEN
                );
                assert!(cancellation.is_cancelled());
                continue;
            }

            pending.accept();
            let approval = request.await.unwrap();
            assert_eq!(approval.status(), reqwest::StatusCode::OK);
            let manifest: serde_json::Value = approval.json().await.unwrap();
            let target = format!(
                "{url}file/{}/{}",
                manifest["session"].as_str().unwrap(),
                manifest["files"][0]["id"].as_str().unwrap()
            );
            let response = client.get(&target).send().await.unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::OK);
            let mut stream = response.bytes_stream();
            let mut read = stream.next().await.unwrap().unwrap().len() as u64;
            assert!(read > 0);
            commands
                .send(Command::Reconfigure(next.clone()))
                .await
                .unwrap();
            loop {
                match next_event(&events).await {
                    Event::Error(error) => {
                        assert!(error.contains("browser download"));
                        break;
                    }
                    Event::WebShare(ShareEvent::Started { .. } | ShareEvent::Progress { .. }) => {}
                    _ => panic!("An active browser download must prevent reconfiguration"),
                }
            }
            assert_eq!(
                client
                    .post(format!("{url}withdraw/{request_id}"))
                    .send()
                    .await
                    .unwrap()
                    .status(),
                reqwest::StatusCode::NO_CONTENT
            );
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(chunk) => read += chunk.len() as u64,
                    Err(_) => break,
                }
            }
            assert!(read < size, "Withdrawal must interrupt the download");
        }

        commands.send(Command::Reconfigure(next)).await.unwrap();
        let mut stopped = false;
        let changed = loop {
            match next_event(&events).await {
                Event::WebShareStopped => {
                    assert!(!stopped, "Expected exactly one stop notification");
                    stopped = true;
                }
                Event::Ready(identity) => break identity,
                Event::WebShare(_) if !stopped => {}
                _ => panic!("Expected idle browser sharing to close during reconfiguration"),
            }
        };
        assert!(
            stopped,
            "Sharing must stop before the new listener is ready"
        );
        assert_eq!(changed.alias, "After sharing");
        assert_eq!(changed.port, original.port);
        assert_eq!(changed.fingerprint, original.fingerprint);
        assert!(client.get(&url).send().await.is_err());
        let advertised: DeviceInfo = client
            .get(format!(
                "https://127.0.0.1:{}/api/localsend/v2/info",
                changed.port
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(advertised.alias, changed.alias);
        assert_eq!(advertised.fingerprint, original.fingerprint);
        shutdown(commands, task).await;
    }

    #[tokio::test]
    async fn stopping_withdraws_pending_consent_and_start_reuses_identity_and_port() {
        let root = tempfile::tempdir().unwrap();
        let settings = Settings {
            port: free_port(),
            save_dir: root.path().into(),
            ..Settings::default()
        };
        let (commands, events, task) = spawn_fixture(settings.clone());
        let original = ready(&events).await;
        server_state(&events, true).await;
        let first = post_message(&test_client(), original.port);
        let pending = pending_message(&events).await;
        let withdrawn = pending.cancellation().unwrap();
        commands.send(Command::StopServer).await.unwrap();
        server_state(&events, false).await;
        assert!(withdrawn.is_cancelled());
        assert!(
            !pending.accept_preview(),
            "A stopped server must revoke preview actions"
        );
        assert_eq!(
            first.await.unwrap().status(),
            reqwest::StatusCode::FORBIDDEN
        );
        // A stop acknowledgement means the listening socket really is released.
        let released = std::net::TcpListener::bind(("0.0.0.0", original.port)).unwrap();
        drop(released);
        commands
            .send(Command::StartServer(Settings {
                quick_save: QuickSaveMode::On,
                ..settings
            }))
            .await
            .unwrap();
        let restarted = ready(&events).await;
        server_state(&events, true).await;
        assert_eq!(restarted.fingerprint, original.fingerprint);
        assert_eq!(restarted.port, original.port);
        let client = test_client();
        let base = format!("https://127.0.0.1:{}/api/localsend/v2", restarted.port);
        let approval = prepare(&client, &base).await;
        assert!(upload_three(&client, &base, &approval)
            .await
            .status()
            .is_success());
        assert_eq!(
            std::fs::read(root.path().join("received.txt")).unwrap(),
            b"new"
        );
        shutdown(commands, task).await;
    }

    #[tokio::test]
    async fn a_failed_initial_bind_can_be_started_without_relaunching_the_actor() {
        let root = tempfile::tempdir().unwrap();
        let occupied = std::net::TcpListener::bind(("0.0.0.0", free_port())).unwrap();
        let settings = Settings {
            port: occupied.local_addr().unwrap().port(),
            save_dir: root.path().into(),
            ..Settings::default()
        };
        let (commands, events, task) = spawn_fixture(settings.clone());
        assert!(matches!(next_event(&events).await, Event::Offline(_)));
        assert_eq!(server_state(&events, false).await.port, settings.port);
        drop(occupied);
        commands
            .send(Command::StartServer(settings.clone()))
            .await
            .unwrap();
        let started = ready(&events).await;
        assert_eq!(server_state(&events, true).await.port, settings.port);
        assert_eq!(started.port, settings.port);
        assert!(test_client()
            .get(format!(
                "https://127.0.0.1:{}/api/localsend/v2/info",
                started.port
            ))
            .send()
            .await
            .unwrap()
            .status()
            .is_success());
        shutdown(commands, task).await;
    }

    #[tokio::test]
    async fn stopping_revokes_both_browser_links_and_rejects_new_links_until_started() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("selected.txt");
        std::fs::write(&path, b"old").unwrap();
        let settings = Settings {
            port: free_port(),
            save_dir: root.path().join("received"),
            ..Settings::default()
        };
        let (commands, events, task) = spawn_fixture(settings.clone());
        ready(&events).await;
        server_state(&events, true).await;
        commands.send(Command::StartWebReceive).await.unwrap();
        let receive_url = match next_event(&events).await {
            Event::WebReceiveReady(urls) => urls
                .into_iter()
                .find(|url| url.contains("127.0.0.1"))
                .unwrap(),
            _ => panic!("Expected receive link"),
        };
        commands
            .send(Command::StartWebShare {
                items: vec![crate::transfer::Selection {
                    name: "selected.txt".into(),
                    size: 3,
                    source: crate::transfer::Source::File(path),
                }],
                auto_accept: true,
            })
            .await
            .unwrap();
        let share_url = match next_event(&events).await {
            Event::WebShareReady(urls) => urls
                .into_iter()
                .find(|url| url.contains("127.0.0.1"))
                .unwrap(),
            _ => panic!("Expected share link"),
        };
        let client = test_client();
        for url in [&receive_url, &share_url] {
            assert!(client
                .get(url.as_str())
                .send()
                .await
                .unwrap()
                .status()
                .is_success());
        }
        commands.send(Command::StopServer).await.unwrap();
        assert!(matches!(
            next_event(&events).await,
            Event::WebReceiveStopped
        ));
        assert!(matches!(next_event(&events).await, Event::WebShareStopped));
        server_state(&events, false).await;
        for url in [&receive_url, &share_url] {
            if let Ok(response) = client.get(url.as_str()).send().await {
                assert!(
                    !response.status().is_success(),
                    "Stop must revoke existing browser links on pooled connections"
                );
            }
        }
        commands.send(Command::StartWebReceive).await.unwrap();
        assert!(matches!(next_event(&events).await, Event::Error(_)));
        assert!(matches!(
            next_event(&events).await,
            Event::WebReceiveStopped
        ));
        commands.send(Command::StartServer(settings)).await.unwrap();
        ready(&events).await;
        server_state(&events, true).await;
        commands.send(Command::StartWebReceive).await.unwrap();
        let next_url = match next_event(&events).await {
            Event::WebReceiveReady(urls) => urls
                .into_iter()
                .find(|url| url.contains("127.0.0.1"))
                .unwrap(),
            _ => panic!("Expected fresh receive link"),
        };
        assert_ne!(
            next_url, receive_url,
            "An earlier capability must never be reused"
        );
        shutdown(commands, task).await;
    }

    #[tokio::test]
    async fn a_failed_bind_restores_the_previous_listener_and_identity() {
        let root = tempfile::tempdir().unwrap();
        let settings = Settings {
            alias: "Original".into(),
            port: free_port(),
            save_dir: root.path().into(),
            ..Settings::default()
        };
        let (commands, events, task) = spawn_fixture(settings.clone());
        let original = ready(&events).await;
        let occupied = std::net::TcpListener::bind(("0.0.0.0", free_port())).unwrap();
        let next = Settings {
            alias: "Cannot bind".into(),
            port: occupied.local_addr().unwrap().port(),
            ..settings
        };
        commands.send(Command::Reconfigure(next)).await.unwrap();
        match next_event(&events).await {
            Event::Error(error) => assert!(error.contains("Restoring the previous listener")),
            _ => panic!("Expected rollback notification"),
        }
        let restored = ready(&events).await;
        let applied = server_state(&events, true).await;
        assert_eq!(applied.alias, original.alias);
        assert_eq!(applied.port, original.port);
        assert_eq!(restored.alias, original.alias);
        assert_eq!(restored.port, original.port);
        assert_eq!(restored.fingerprint, original.fingerprint);
        let advertised: DeviceInfo = test_client()
            .get(format!(
                "https://127.0.0.1:{}/api/localsend/v2/info",
                restored.port
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(advertised.alias, "Original");
        shutdown(commands, task).await;
    }

    #[tokio::test]
    async fn reconfiguration_is_refused_while_a_native_offer_waits_for_consent() {
        let root = tempfile::tempdir().unwrap();
        let settings = Settings {
            alias: "Original".into(),
            port: free_port(),
            save_dir: root.path().into(),
            ..Settings::default()
        };
        let (commands, events, task) = spawn_fixture(settings.clone());
        let original = ready(&events).await;
        let request = tokio::spawn(async move {
            test_client()
                .post(format!(
                    "https://127.0.0.1:{}/api/localsend/v2/prepare-upload",
                    original.port
                ))
                .json(&offer())
                .send()
                .await
                .unwrap()
        });
        let pending = match next_event(&events).await {
            Event::TransferRequest(request) => request,
            _ => panic!("Expected consent request"),
        };
        commands
            .send(Command::Reconfigure(Settings {
                alias: "Later".into(),
                ..settings.clone()
            }))
            .await
            .unwrap();
        match next_event(&events).await {
            Event::Error(error) => assert!(error.contains("incoming transfer")),
            _ => panic!("Expected busy refusal"),
        }
        pending.decline();
        assert_eq!(
            request.await.unwrap().status(),
            reqwest::StatusCode::FORBIDDEN
        );
        let advertised: DeviceInfo = test_client()
            .get(format!(
                "https://127.0.0.1:{}/api/localsend/v2/info",
                settings.port
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(advertised.alias, "Original");
        shutdown(commands, task).await;
    }
    #[tokio::test]
    async fn an_open_receive_sink_blocks_reconfiguration_until_committed_or_dropped() {
        let activity = Arc::new(ReceiveActivity::default());
        let root = tempfile::tempdir().unwrap();
        let sink = TrackedSink(activity.clone());
        let pending = sink.create(root.path(), "test.txt").await.unwrap();
        assert!(activity.busy());
        pending.abort().await.unwrap();
        assert!(!activity.busy());
        let pending = sink.create(root.path(), "test.txt").await.unwrap();
        assert!(activity.busy());
        drop(pending);
        assert!(!activity.busy());
    }

    #[tokio::test]
    async fn expired_native_session_emits_its_terminal_id_only_after_writer_closes() {
        let activity = Arc::new(ReceiveActivity::default());
        let root = tempfile::tempdir().unwrap();
        let id = SessionId::new();
        *activity.lease.lock().unwrap() = Some(Lease {
            id: uuid::Uuid::new_v4(),
            accepted: true,
            touched: Instant::now() - Duration::from_secs(400),
            session_id: Some(id.clone()),
            cancel: CancellationToken::new(),
        });
        let pending = TrackedSink(activity.clone())
            .create(root.path(), "partial.txt")
            .await
            .unwrap();
        assert!(activity.busy());
        assert!(activity.expire().is_none());
        pending.abort().await.unwrap();
        let expired = activity.expire().unwrap();
        match expired.terminal_event() {
            Event::Server(ServerEvent::SessionDone { session_id }) => assert_eq!(session_id, id),
            _ => panic!("Expected terminal event"),
        }
        assert!(!activity.busy());
        assert!(activity.expire().is_none());
    }
}
