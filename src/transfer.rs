use localsend_rs::{
    core::build_file_metadata,
    crypto::TlsCertificate,
    protocol::{DeviceInfo, FileId, FileMetadata},
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendError {
    Cancelled,
    PinRequired,
    Declined,
    RecipientBusy,
    TooManyAttempts,
    Failed(String),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("Transfer cancelled."),
            Self::PinRequired => f.write_str("The recipient requires a valid PIN."),
            Self::Declined => f.write_str("The recipient has rejected the request."),
            Self::RecipientBusy => f.write_str("The recipient is busy with another request."),
            Self::TooManyAttempts => f.write_str("Too many attempts. Try again later."),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for SendError {}

impl From<String> for SendError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

impl From<crate::http_client::PrepareError> for SendError {
    fn from(error: crate::http_client::PrepareError) -> Self {
        use crate::http_client::PrepareError;
        match error {
            PrepareError::Status(status) => match status.as_u16() {
                401 => Self::PinRequired,
                403 => Self::Declined,
                409 => Self::RecipientBusy,
                429 => Self::TooManyAttempts,
                code => Self::Failed(format!("The recipient returned HTTP {code}.")),
            },
            PrepareError::Failed(message) => Self::Failed(message),
        }
    }
}

/// A PIN is requested only after the chosen, pinned receiver returns HTTP 401.
/// Dropping the request or answering None cancels this outgoing operation.
pub struct PinRequest {
    pub invalid: bool,
    pub answer: tokio::sync::oneshot::Sender<Option<String>>,
    pub cancellation: CancellationToken,
}

pub struct PinRequests {
    pub initial_pin: Option<String>,
    pub requests: async_channel::Sender<PinRequest>,
}

enum Authentication {
    #[cfg(test)]
    Fixed(Option<String>),
    Interactive(PinRequests),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileOutcome {
    Queued,
    Skipped,
    Sending,
    Finished,
    Failed,
    Canceled,
}

#[derive(Clone, Debug)]
pub struct FileProgress {
    pub name: String,
    pub size: u64,
    pub bytes_sent: u64,
    pub outcome: FileOutcome,
}

#[derive(Clone, Debug, Default)]
pub struct TransferProgress {
    pub files: Vec<FileProgress>,
    pub current_file: Option<usize>,
    pub bytes_sent: u64,
    pub total_bytes: u64,
    pub finished_files: usize,
    pub accepted_files: usize,
    pub elapsed: Duration,
    pub bytes_per_second: f64,
    pub remaining: Option<Duration>,
    pub started: bool,
    pub complete: bool,
}

impl TransferProgress {
    pub fn queued(items: &[Selection]) -> Self {
        Self {
            files: items
                .iter()
                .map(|item| FileProgress {
                    name: item.name.clone(),
                    size: item.size,
                    bytes_sent: 0,
                    outcome: FileOutcome::Queued,
                })
                .collect(),
            total_bytes: items
                .iter()
                .fold(0_u64, |sum, item| sum.saturating_add(item.size)),
            accepted_files: items.len(),
            ..Self::default()
        }
    }
    pub fn fraction(&self) -> f64 {
        if self.total_bytes > 0 {
            self.bytes_sent as f64 / self.total_bytes as f64
        } else if self.accepted_files > 0 {
            self.finished_files as f64 / self.accepted_files as f64
        } else {
            0.0
        }
        .clamp(0.0, 1.0)
    }
    fn update_timing(&mut self, elapsed: Duration) {
        self.elapsed = elapsed;
        self.bytes_per_second = if self.started && elapsed.as_secs_f64() > 0.0 {
            self.bytes_sent as f64 / elapsed.as_secs_f64()
        } else {
            0.0
        };
        self.remaining = if self.complete {
            (self.finished_files == self.accepted_files).then_some(Duration::ZERO)
        } else if self.bytes_per_second > 0.0 && self.bytes_sent < self.total_bytes {
            Some(Duration::from_secs_f64(
                ((self.total_bytes - self.bytes_sent) as f64 / self.bytes_per_second)
                    .min(315_360_000.0),
            ))
        } else {
            None
        };
    }
    fn update_file(&mut self, index: usize, bytes: u64, outcome: FileOutcome) {
        let Some(file) = self.files.get_mut(index) else {
            return;
        };
        if matches!(
            file.outcome,
            FileOutcome::Skipped
                | FileOutcome::Finished
                | FileOutcome::Failed
                | FileOutcome::Canceled
        ) {
            return;
        }
        let bytes = bytes.min(file.size).max(file.bytes_sent);
        self.bytes_sent = self.bytes_sent.saturating_add(bytes - file.bytes_sent);
        file.bytes_sent = bytes;
        file.outcome = outcome;
        self.current_file = Some(index);
        if outcome == FileOutcome::Finished {
            self.finished_files += 1;
        }
    }
}

struct ProgressState {
    report: TransferProgress,
    started: Option<Instant>,
    last_report: Instant,
}

#[derive(Clone)]
struct ProgressReporter {
    state: Arc<Mutex<ProgressState>>,
    output: tokio::sync::watch::Sender<TransferProgress>,
}

impl ProgressReporter {
    fn new(items: &[Selection], output: tokio::sync::watch::Sender<TransferProgress>) -> Self {
        let report = TransferProgress::queued(items);
        output.send_replace(report.clone());
        Self {
            state: Arc::new(Mutex::new(ProgressState {
                report,
                started: None,
                last_report: Instant::now(),
            })),
            output,
        }
    }
    fn prepared(&self, prepared: &[(Selection, FileMetadata)]) {
        let mut state = self.state.lock().unwrap();
        for (file, (_, metadata)) in state.report.files.iter_mut().zip(prepared) {
            file.size = metadata.size;
        }
        state.report.total_bytes = state
            .report
            .files
            .iter()
            .fold(0_u64, |total, file| total.saturating_add(file.size));
        self.output.send_replace(state.report.clone());
    }
    fn accepted(&self, accepted: &[bool]) {
        let mut state = self.state.lock().unwrap();
        for (file, accepted) in state.report.files.iter_mut().zip(accepted) {
            if !accepted {
                file.outcome = FileOutcome::Skipped;
            }
        }
        state.report.accepted_files = accepted.iter().filter(|accepted| **accepted).count();
        state.report.total_bytes = state
            .report
            .files
            .iter()
            .filter(|file| file.outcome != FileOutcome::Skipped)
            .fold(0_u64, |total, file| total.saturating_add(file.size));
        state.report.started = true;
        state.started = Some(Instant::now());
        self.output.send_replace(state.report.clone());
    }
    fn file(&self, index: usize, bytes: u64, outcome: FileOutcome) {
        let mut state = self.state.lock().unwrap();
        let transition = state
            .report
            .files
            .get(index)
            .is_some_and(|file| file.outcome != outcome);
        state.report.update_file(index, bytes, outcome);
        let elapsed = state
            .started
            .map(|start| start.elapsed())
            .unwrap_or_default();
        state.report.update_timing(elapsed);
        if transition || state.last_report.elapsed() >= Duration::from_millis(100) {
            state.last_report = Instant::now();
            self.output.send_replace(state.report.clone());
        }
    }
    fn finish(&self, result: &Result<usize, SendError>) {
        let mut state = self.state.lock().unwrap();
        for file in &mut state.report.files {
            file.outcome = match file.outcome {
                FileOutcome::Queued => {
                    if matches!(result, Err(SendError::Cancelled)) {
                        FileOutcome::Canceled
                    } else {
                        FileOutcome::Skipped
                    }
                }
                FileOutcome::Sending => {
                    if matches!(result, Err(SendError::Cancelled)) {
                        FileOutcome::Canceled
                    } else {
                        FileOutcome::Failed
                    }
                }
                outcome => outcome,
            };
        }
        state.report.complete = true;
        let elapsed = state
            .started
            .map(|start| start.elapsed())
            .unwrap_or_default();
        state.report.update_timing(elapsed);
        self.output.send_replace(state.report.clone());
    }
    fn tick(&self) {
        let mut state = self.state.lock().unwrap();
        if let Some(started) = state.started {
            state.report.update_timing(started.elapsed());
            self.output.send_replace(state.report.clone());
        }
    }
}

#[derive(Clone, Debug)]
pub enum Source {
    File(PathBuf),
    Text(String),
}

#[derive(Clone, Debug)]
pub struct Selection {
    pub name: String,
    pub size: u64,
    pub source: Source,
}

impl Selection {
    pub fn text(text: String) -> Self {
        Self {
            name: "Message.txt".into(),
            size: text.len() as u64,
            source: Source::Text(text),
        }
    }
}

pub fn collect_paths(paths: &[PathBuf]) -> std::io::Result<Vec<Selection>> {
    fn visit(path: &Path, base: &Path, result: &mut Vec<Selection>) -> std::io::Result<()> {
        let meta = std::fs::symlink_metadata(path)?;
        // Do not follow symlinks out of the selected directory or recurse into cycles.
        if meta.file_type().is_symlink() {
            return Ok(());
        }
        if meta.is_dir() {
            let mut entries = std::fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                visit(&entry.path(), base, result)?;
            }
        } else if meta.is_file() {
            result.push(Selection {
                name: path
                    .strip_prefix(base)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .replace('\\', "/"),
                size: meta.len(),
                source: Source::File(path.to_owned()),
            });
        }
        Ok(())
    }
    let mut result = Vec::new();
    for path in paths {
        visit(path, path.parent().unwrap_or(Path::new("")), &mut result)?;
    }
    Ok(result)
}

pub async fn metadata(items: &[Selection]) -> Result<Vec<(Selection, FileMetadata)>, String> {
    let mut result = Vec::new();
    for item in items {
        let mut meta = match &item.source {
            Source::File(path) => build_file_metadata(path)
                .await
                .map_err(|e| format!("{}: {e}", item.name))?,
            Source::Text(text) => FileMetadata {
                id: FileId::new(),
                file_name: item.name.clone(),
                size: text.len() as u64,
                file_type: "text/plain".into(),
                sha256: None,
                preview: Some(text.clone()),
                metadata: None,
            },
        };
        meta.file_name = item.name.clone();
        result.push((item.clone(), meta));
    }
    Ok(result)
}

#[cfg(test)]
pub async fn send(
    local: DeviceInfo,
    certificate: TlsCertificate,
    peer: DeviceInfo,
    items: Vec<Selection>,
    pin: Option<String>,
    progress: tokio::sync::watch::Sender<TransferProgress>,
) -> Result<usize, String> {
    send_cancellable(
        local,
        certificate,
        peer,
        items,
        pin,
        progress,
        CancellationToken::new(),
    )
    .await
    .map_err(|e| e.to_string())
}

#[cfg(test)]
pub async fn send_cancellable(
    local: DeviceInfo,
    certificate: TlsCertificate,
    peer: DeviceInfo,
    items: Vec<Selection>,
    pin: Option<String>,
    progress: tokio::sync::watch::Sender<TransferProgress>,
    cancellation: CancellationToken,
) -> Result<usize, SendError> {
    send_with_auth(
        local,
        certificate,
        peer,
        items,
        Authentication::Fixed(pin),
        progress,
        cancellation,
    )
    .await
}

pub async fn send_interactive(
    local: DeviceInfo,
    certificate: TlsCertificate,
    peer: DeviceInfo,
    items: Vec<Selection>,
    pins: PinRequests,
    progress: tokio::sync::watch::Sender<TransferProgress>,
    cancellation: CancellationToken,
) -> Result<usize, SendError> {
    send_with_auth(
        local,
        certificate,
        peer,
        items,
        Authentication::Interactive(pins),
        progress,
        cancellation,
    )
    .await
}

async fn send_with_auth(
    local: DeviceInfo,
    certificate: TlsCertificate,
    peer: DeviceInfo,
    items: Vec<Selection>,
    authentication: Authentication,
    progress: tokio::sync::watch::Sender<TransferProgress>,
    cancellation: CancellationToken,
) -> Result<usize, SendError> {
    let reporter = ProgressReporter::new(&items, progress);
    let operation = send_with_report(
        local,
        certificate,
        peer,
        items,
        authentication,
        reporter.clone(),
        cancellation,
    );
    tokio::pin!(operation);
    let mut clock = tokio::time::interval(Duration::from_secs(1));
    clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let result = loop {
        tokio::select! {
            result = &mut operation => break result,
            _ = clock.tick() => reporter.tick(),
        }
    };
    reporter.finish(&result);
    result
}

async fn send_with_report(
    local: DeviceInfo,
    certificate: TlsCertificate,
    peer: DeviceInfo,
    items: Vec<Selection>,
    authentication: Authentication,
    progress: ProgressReporter,
    cancellation: CancellationToken,
) -> Result<usize, SendError> {
    if cancellation.is_cancelled() {
        return Err(SendError::Cancelled);
    }
    if items.is_empty() {
        return Err(SendError::Failed(
            "Select files or text before sending.".into(),
        ));
    }
    let client = crate::http_client::Client::new(local, &peer, certificate)?;
    // Build each ID once; the receiver's tokens refer to these exact IDs.
    let prepared = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return Err(SendError::Cancelled),
        prepared = metadata(&items) => prepared?,
    };
    progress.prepared(&prepared);
    let offer: HashMap<_, _> = prepared
        .iter()
        .map(|(_, m)| (m.id.clone(), m.clone()))
        .collect();
    if cancellation.is_cancelled() {
        return Err(SendError::Cancelled);
    }
    let (mut pin, prompts) = match authentication {
        #[cfg(test)]
        Authentication::Fixed(pin) => (pin, None),
        Authentication::Interactive(pins) => (pins.initial_pin, Some(pins.requests)),
    };
    let response = loop {
        let prepare = tokio::time::timeout(
            std::time::Duration::from_secs(90),
            client.prepare_upload(offer.clone(), pin.as_deref()),
        );
        let response = tokio::select! {
        // Take an already available session id so cancellation can name it.
        biased;
        response = prepare => match response {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(error)) => {
                if cancellation.is_cancelled() {
                    let _ = client.cancel_pending().await;
                    return Err(SendError::Cancelled);
                }
                Err(SendError::from(error))
            },
            Err(_) => {
                let _ = client.cancel_pending().await;
                if cancellation.is_cancelled() {
                    return Err(SendError::Cancelled);
                }
                return Err(SendError::Failed("The recipient did not respond in time.".into()));
            }
        },
        _ = cancellation.cancelled() => {
            // The receiver has not returned a session id while it waits for consent.
            let _ = client.cancel_pending().await;
            return Err(SendError::Cancelled);
        },
        };
        match response {
            Ok(response) => break response,
            Err(SendError::PinRequired) => {
                let Some(prompts) = prompts.as_ref() else {
                    return Err(SendError::PinRequired);
                };
                let (answer, receiver) = tokio::sync::oneshot::channel();
                let request = PinRequest {
                    invalid: pin.is_some(),
                    answer,
                    cancellation: cancellation.clone(),
                };
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => return Err(SendError::Cancelled),
                    sent = prompts.send(request) => if sent.is_err() { return Err(SendError::Cancelled); },
                }
                pin = tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => return Err(SendError::Cancelled),
                    answer = receiver => match answer {
                        Ok(Some(pin)) => Some(pin),
                        _ => return Err(SendError::Cancelled),
                    },
                };
                // Authentication rejected the offer before a session exists.
                // Retry only its prepare with the same metadata and pinned client.
            }
            Err(error) => return Err(error),
        }
    };
    if cancellation.is_cancelled() {
        if !response.session_id.is_empty() {
            let _ = client.cancel(&response.session_id).await;
        }
        return Err(SendError::Cancelled);
    }
    if response.session_id.is_empty() {
        if prepared.len() == 1 && matches!(prepared[0].0.source, Source::Text(_)) {
            progress.accepted(&[true]);
            progress.file(0, prepared[0].1.size, FileOutcome::Finished);
            return Ok(1);
        }
        return Err(SendError::Failed(
            "The recipient did not accept any files.".into(),
        ));
    }
    let accepted: Vec<_> = prepared
        .iter()
        .map(|(_, meta)| response.files.contains_key(&meta.id))
        .collect();
    progress.accepted(&accepted);
    let mut sent = 0;
    for (index, (item, meta)) in prepared.iter().enumerate() {
        let Some(token) = response.files.get(&meta.id) else {
            continue;
        };
        progress.file(index, 0, FileOutcome::Sending);
        let upload = async {
            match &item.source {
                Source::File(path) => {
                    let tx = progress.clone();
                    client
                        .upload_file(
                            &response.session_id,
                            &meta.id,
                            token,
                            path,
                            cancellation.clone(),
                            move |bytes, _| tx.file(index, bytes, FileOutcome::Sending),
                        )
                        .await
                }
                Source::Text(text) => {
                    client
                        .upload_bytes(
                            &response.session_id,
                            &meta.id,
                            token,
                            text.as_bytes().to_vec(),
                            cancellation.clone(),
                            {
                                let progress = progress.clone();
                                move |bytes, _| progress.file(index, bytes, FileOutcome::Sending)
                            },
                        )
                        .await
                }
            }
        };
        let result = tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                let _ = client.cancel(&response.session_id).await;
                return Err(SendError::Cancelled);
            },
            result = upload => result,
        };
        if let Err(e) = result {
            let _ = client.cancel(&response.session_id).await;
            if cancellation.is_cancelled() {
                return Err(SendError::Cancelled);
            }
            return Err(SendError::Failed(format!("{}: {e}", item.name)));
        }
        sent += 1;
        progress.file(index, meta.size, FileOutcome::Finished);
    }
    if sent == 0 {
        let _ = client.cancel(&response.session_id).await;
        return Err(SendError::Failed(
            "The recipient did not accept any files.".into(),
        ));
    }
    Ok(sent)
}

pub fn size_label(bytes: u64) -> String {
    if bytes < 1000 {
        format!("{bytes} B")
    } else if bytes < 1_000_000 {
        format!("{:.1} KB", bytes as f64 / 1000.0)
    } else if bytes < 1_000_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else {
        format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use localsend_rs::server::{LocalSendServer, ServerEvent};
    use localsend_rs::{crypto::generate_tls_certificate, protocol::Protocol};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn progress_counts_only_accepted_files_and_retains_the_final_snapshot() {
        let items = vec![
            Selection::text("first".into()),
            Selection::text("excluded".into()),
            Selection::text("third".into()),
        ];
        let (output, updates) = tokio::sync::watch::channel(TransferProgress::default());
        let reporter = ProgressReporter::new(&items, output);
        reporter.accepted(&[true, false, true]);
        reporter.file(0, 4, FileOutcome::Sending);
        reporter.file(0, 2, FileOutcome::Sending); // Late/smaller samples never regress.
        reporter.file(0, 50, FileOutcome::Finished); // Bound at the offered length.
        reporter.file(0, 1, FileOutcome::Sending); // Completed files cannot restart.
        reporter.file(2, 5, FileOutcome::Finished);
        reporter.finish(&Ok(2));
        let report = updates.borrow(); // No polling while the transfer was active.
        assert_eq!((report.bytes_sent, report.total_bytes), (10, 10));
        assert_eq!((report.finished_files, report.accepted_files), (2, 2));
        assert_eq!(report.files[0].bytes_sent, 5);
        assert_eq!(report.files[1].outcome, FileOutcome::Skipped);
        assert_eq!(report.files[2].outcome, FileOutcome::Finished);
        assert!(report.complete);
        assert_eq!(report.fraction(), 1.0);
        assert_eq!(report.remaining, Some(Duration::ZERO));
    }

    #[test]
    fn progress_timing_handles_stalls_empty_files_and_partial_failure() {
        let mut report = TransferProgress::queued(&[Selection::text("0123456789".into())]);
        report.started = true;
        report.update_file(0, 4, FileOutcome::Sending);
        report.update_timing(Duration::from_secs(2));
        assert_eq!(report.bytes_per_second, 2.0);
        assert_eq!(report.remaining, Some(Duration::from_secs(3)));
        report.update_timing(Duration::from_secs(4));
        assert_eq!(report.bytes_per_second, 1.0);
        assert_eq!(report.remaining, Some(Duration::from_secs(6)));
        report.update_file(0, 4, FileOutcome::Failed);
        report.complete = true;
        report.update_timing(Duration::from_secs(4));
        assert_eq!(report.fraction(), 0.4);
        assert_eq!(report.remaining, None);

        let mut empty = TransferProgress::queued(&[
            Selection::text(String::new()),
            Selection::text(String::new()),
        ]);
        empty.started = true;
        empty.update_file(0, 0, FileOutcome::Finished);
        empty.update_timing(Duration::ZERO);
        assert_eq!(empty.fraction(), 0.5);
        assert_eq!(empty.bytes_per_second, 0.0);
        assert_eq!(empty.remaining, None);
        empty.update_file(1, 0, FileOutcome::Finished);
        empty.complete = true;
        empty.update_timing(Duration::from_secs(1));
        assert_eq!(empty.fraction(), 1.0);
        assert_eq!(empty.remaining, Some(Duration::ZERO));
    }

    #[test]
    fn cancellation_and_failure_preserve_finished_files_and_each_recipient() {
        for error in [
            SendError::Cancelled,
            SendError::Failed("Receiver disconnected".into()),
        ] {
            let items = vec![
                Selection::text("done".into()),
                Selection::text("partial".into()),
                Selection::text("pending".into()),
            ];
            let (output_a, updates_a) = tokio::sync::watch::channel(TransferProgress::default());
            let (output_b, updates_b) = tokio::sync::watch::channel(TransferProgress::default());
            let first = ProgressReporter::new(&items, output_a);
            let second = ProgressReporter::new(&items, output_b);
            first.accepted(&[true, true, true]);
            second.accepted(&[true, true, true]);
            first.file(0, 4, FileOutcome::Finished);
            first.file(1, 3, FileOutcome::Sending);
            first.finish(&Err(error.clone()));
            let report = updates_a.borrow();
            assert_eq!(report.bytes_sent, 7);
            assert_eq!(report.finished_files, 1);
            assert_eq!(report.files[0].outcome, FileOutcome::Finished);
            assert_eq!(
                report.files[1].outcome,
                if error == SendError::Cancelled {
                    FileOutcome::Canceled
                } else {
                    FileOutcome::Failed
                }
            );
            assert_eq!(
                report.files[2].outcome,
                if error == SendError::Cancelled {
                    FileOutcome::Canceled
                } else {
                    FileOutcome::Skipped
                }
            );
            assert_eq!(report.remaining, None);
            assert_eq!(updates_b.borrow().bytes_sent, 0);
            assert!(!updates_b.borrow().complete);
        }
    }

    async fn read_headers(stream: &mut tokio::net::TcpStream) -> (String, Vec<u8>) {
        let mut bytes = Vec::new();
        loop {
            let mut chunk = [0; 4096];
            let read = stream.read(&mut chunk).await.unwrap();
            assert!(read > 0, "Request ended before its headers");
            bytes.extend_from_slice(&chunk[..read]);
            if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                return (
                    String::from_utf8(bytes[..end].to_vec()).unwrap(),
                    bytes[end + 4..].to_vec(),
                );
            }
            assert!(bytes.len() < 64 * 1024);
        }
    }

    async fn respond(stream: &mut tokio::net::TcpStream, body: &[u8]) {
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        stream.write_all(body).await.unwrap();
        stream.shutdown().await.unwrap();
    }

    async fn respond_status(stream: &mut tokio::net::TcpStream, status: u16) {
        stream
            .write_all(
                format!(
                    "HTTP/1.1 {status} Response\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        stream.shutdown().await.unwrap();
    }

    async fn read_offer(stream: &mut tokio::net::TcpStream) -> (reqwest::Url, serde_json::Value) {
        let (headers, mut body) = read_headers(stream).await;
        let target = headers
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap();
        let target = reqwest::Url::parse(&format!("http://localhost{target}")).unwrap();
        assert_eq!(target.path(), "/api/localsend/v2/prepare-upload");
        let length: usize = headers
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().unwrap())
            })
            .unwrap();
        while body.len() < length {
            let mut chunk = [0; 4096];
            let read = stream.read(&mut chunk).await.unwrap();
            assert!(read > 0);
            body.extend_from_slice(&chunk[..read]);
        }
        (target, serde_json::from_slice(&body).unwrap())
    }

    fn http_peer(port: u16) -> DeviceInfo {
        let mut peer = DeviceInfo::new("Receiver".into(), port, Protocol::Http);
        peer.ip = Some("127.0.0.1".into());
        peer
    }

    #[tokio::test]
    async fn pin_challenge_retries_only_prepare_with_stable_ids_and_exact_pin() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer = http_peer(listener.local_addr().unwrap().port());
        let server = tokio::spawn(async move {
            let mut initial_offer = None;
            for (expected_pin, status) in
                [(None, 401), (Some("wrong"), 401), (Some("Ab3+/? x"), 204)]
            {
                let (mut stream, _) = listener.accept().await.unwrap();
                let (target, offer) = read_offer(&mut stream).await;
                let pin = target
                    .query_pairs()
                    .find_map(|(key, value)| (key == "pin").then(|| value.into_owned()));
                assert_eq!(pin.as_deref(), expected_pin);
                if let Some(initial) = initial_offer.as_ref() {
                    assert_eq!(
                        &offer, initial,
                        "PIN retries must keep the chosen metadata and identity"
                    );
                } else {
                    initial_offer = Some(offer);
                }
                respond_status(&mut stream, status).await;
            }
        });
        let (requests, prompts) = async_channel::bounded::<PinRequest>(1);
        let answers = tokio::spawn(async move {
            for (invalid, pin) in [(false, "wrong"), (true, "Ab3+/? x")] {
                let request = prompts.recv().await.unwrap();
                assert_eq!(request.invalid, invalid);
                request.answer.send(Some(pin.into())).unwrap();
            }
        });
        let (progress, _) = tokio::sync::watch::channel(TransferProgress::default());
        let sent = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            send_interactive(
                DeviceInfo::new("Sender".into(), 53317, Protocol::Http),
                generate_tls_certificate().unwrap(),
                peer,
                vec![Selection::text("A message requiring a PIN".into())],
                PinRequests {
                    initial_pin: None,
                    requests,
                },
                progress,
                CancellationToken::new(),
            ),
        )
        .await
        .unwrap();
        assert_eq!(sent, Ok(1));
        answers.await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn refusal_busy_and_rate_limit_never_request_pin_or_retry() {
        for (status, expected) in [
            (403, SendError::Declined),
            (409, SendError::RecipientBusy),
            (429, SendError::TooManyAttempts),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let peer = http_peer(listener.local_addr().unwrap().port());
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let _ = read_offer(&mut stream).await;
                respond_status(&mut stream, status).await;
                assert!(
                    tokio::time::timeout(std::time::Duration::from_millis(150), listener.accept())
                        .await
                        .is_err(),
                    "A refusal must not be retried or cancelled as a pending offer"
                );
            });
            let (requests, prompts) = async_channel::bounded::<PinRequest>(1);
            let (progress, _) = tokio::sync::watch::channel(TransferProgress::default());
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                send_interactive(
                    DeviceInfo::new("Sender".into(), 53317, Protocol::Http),
                    generate_tls_certificate().unwrap(),
                    peer,
                    vec![Selection::text("Hello".into())],
                    PinRequests {
                        initial_pin: None,
                        requests,
                    },
                    progress,
                    CancellationToken::new(),
                ),
            )
            .await
            .unwrap();
            assert_eq!(result, Err(expected));
            assert!(prompts.try_recv().is_err());
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn cancelling_the_pin_prompt_prevents_a_late_answer_from_retrying() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer = http_peer(listener.local_addr().unwrap().port());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = read_offer(&mut stream).await;
            respond_status(&mut stream, 401).await;
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(250), listener.accept())
                    .await
                    .is_err()
            );
        });
        let cancellation = CancellationToken::new();
        let trigger = cancellation.clone();
        let (requests, prompts) = async_channel::bounded::<PinRequest>(1);
        let (progress, _) = tokio::sync::watch::channel(TransferProgress::default());
        let task = tokio::spawn(send_interactive(
            DeviceInfo::new("Sender".into(), 53317, Protocol::Http),
            generate_tls_certificate().unwrap(),
            peer,
            vec![Selection::text("Hello".into())],
            PinRequests {
                initial_pin: None,
                requests,
            },
            progress,
            cancellation,
        ));
        let request = tokio::time::timeout(std::time::Duration::from_secs(3), prompts.recv())
            .await
            .unwrap()
            .unwrap();
        trigger.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result, Err(SendError::Cancelled));
        assert!(request.cancellation.is_cancelled());
        assert!(request.answer.send(Some("late pin".into())).is_err());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_recipients_keep_independent_sessions_and_cancellation() {
        let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let listener_b = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer_a = http_peer(listener_a.local_addr().unwrap().port());
        let peer_b = http_peer(listener_b.local_addr().unwrap().port());
        let (ready_a, waiting_a) = tokio::sync::oneshot::channel();
        let (ready_b, waiting_b) = tokio::sync::oneshot::channel();
        let (accept_b, decision_b) = tokio::sync::oneshot::channel();
        let receiver_a = tokio::spawn(async move {
            let (mut prepare, _) = listener_a.accept().await.unwrap();
            let _ = read_offer(&mut prepare).await;
            ready_a.send(()).unwrap();
            let (mut cancel, _) = listener_a.accept().await.unwrap();
            let (headers, _) = read_headers(&mut cancel).await;
            assert!(headers.starts_with("POST /api/localsend/v2/cancel HTTP/"));
            respond(&mut cancel, b"").await;
        });
        let receiver_b = tokio::spawn(async move {
            let (mut prepare, _) = listener_b.accept().await.unwrap();
            let _ = read_offer(&mut prepare).await;
            ready_b.send(()).unwrap();
            decision_b.await.unwrap();
            respond_status(&mut prepare, 204).await;
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(150), listener_b.accept())
                    .await
                    .is_err(),
                "Canceling A must not send /cancel to B"
            );
        });
        let parent = CancellationToken::new();
        let cancel_a = parent.child_token();
        let cancel_b = parent.child_token();
        let local = DeviceInfo::new("Sender".into(), 53317, Protocol::Http);
        let certificate = generate_tls_certificate().unwrap();
        let (progress, _) = tokio::sync::watch::channel(TransferProgress::default());
        let send_a = tokio::spawn(send_cancellable(
            local.clone(),
            certificate.clone(),
            peer_a,
            vec![Selection::text("For A".into())],
            None,
            progress.clone(),
            cancel_a.clone(),
        ));
        let send_b = tokio::spawn(send_cancellable(
            local,
            certificate,
            peer_b,
            vec![Selection::text("For B".into())],
            None,
            progress,
            cancel_b.clone(),
        ));
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            waiting_a.await.unwrap();
            waiting_b.await.unwrap();
        })
        .await
        .unwrap();
        cancel_a.cancel();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(3), send_a)
                .await
                .unwrap()
                .unwrap(),
            Err(SendError::Cancelled)
        );
        assert!(!parent.is_cancelled());
        assert!(!cancel_b.is_cancelled());
        assert!(!send_b.is_finished());
        accept_b.send(()).unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(3), send_b)
                .await
                .unwrap()
                .unwrap(),
            Ok(1)
        );
        receiver_a.await.unwrap();
        receiver_b.await.unwrap();
    }

    #[tokio::test]
    async fn cancelling_before_start_does_not_need_a_peer_or_read_files() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let (progress, _) = tokio::sync::watch::channel(TransferProgress::default());
        let local = DeviceInfo::new("Sender".into(), 53317, Protocol::Http);
        let result = send_cancellable(
            local.clone(),
            generate_tls_certificate().unwrap(),
            local,
            vec![],
            None,
            progress,
            cancellation,
        )
        .await;
        assert_eq!(result, Err(SendError::Cancelled));
    }

    #[tokio::test]
    async fn cancelling_while_waiting_withdraws_the_pending_offer() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let cancellation = CancellationToken::new();
        let trigger = cancellation.clone();
        let server = tokio::spawn(async move {
            let (mut prepare, _) = listener.accept().await.unwrap();
            let (headers, _) = read_headers(&mut prepare).await;
            assert!(headers.starts_with("POST /api/localsend/v2/prepare-upload "));
            trigger.cancel();
            let (mut cancel, _) = listener.accept().await.unwrap();
            let (headers, _) = read_headers(&mut cancel).await;
            assert!(
                headers.starts_with("POST /api/localsend/v2/cancel HTTP/"),
                "Pending offers have no session id: {headers}"
            );
            respond(&mut cancel, b"").await;
        });
        let local = DeviceInfo::new("Sender".into(), 53317, Protocol::Http);
        let mut peer = DeviceInfo::new("Receiver".into(), port, Protocol::Http);
        peer.ip = Some("127.0.0.1".into());
        let (progress, _) = tokio::sync::watch::channel(TransferProgress::default());
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            send_cancellable(
                local,
                generate_tls_certificate().unwrap(),
                peer,
                vec![Selection::text("waiting".into())],
                None,
                progress,
                cancellation,
            ),
        )
        .await
        .unwrap();
        assert_eq!(result, Err(SendError::Cancelled));
        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn cancelling_streaming_stops_body_and_cancels_the_known_session() {
        let source = tempfile::tempdir().unwrap();
        let path = source.path().join("large.bin");
        // Sparse file: enough content to stay in flight when the peer stops reading.
        std::fs::File::create(&path)
            .unwrap()
            .set_len(32 * 1024 * 1024)
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let cancellation = CancellationToken::new();
        let trigger = cancellation.clone();
        let server = tokio::spawn(async move {
            let (mut prepare, _) = listener.accept().await.unwrap();
            let (headers, mut body) = read_headers(&mut prepare).await;
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(str::to_owned)
                })
                .unwrap()
                .parse()
                .unwrap();
            while body.len() < length {
                let mut chunk = [0; 4096];
                let read = prepare.read(&mut chunk).await.unwrap();
                assert!(read > 0);
                body.extend_from_slice(&chunk[..read]);
            }
            let offer: localsend_rs::protocol::PrepareUploadRequest =
                serde_json::from_slice(&body).unwrap();
            let id = offer.files.keys().next().unwrap().as_str();
            let response =
                serde_json::json!({ "sessionId": "test-session", "files": { id: "test-token" } });
            respond(&mut prepare, &serde_json::to_vec(&response).unwrap()).await;
            let (mut upload, _) = listener.accept().await.unwrap();
            let (headers, body) = read_headers(&mut upload).await;
            assert!(headers.starts_with("POST /api/localsend/v2/upload?"));
            assert!(headers.contains("sessionId=test-session"));
            assert!(headers.contains("token=test-token"));
            let mut bytes_read = body.len();
            if bytes_read == 0 {
                let mut chunk = [0; 4096];
                bytes_read += upload.read(&mut chunk).await.unwrap();
            }
            assert!(bytes_read > 0);
            trigger.cancel();
            let (mut cancel, _) = listener.accept().await.unwrap();
            let (headers, _) = read_headers(&mut cancel).await;
            assert!(
                headers.starts_with("POST /api/localsend/v2/cancel?sessionId=test-session HTTP/")
            );
            respond(&mut cancel, b"").await;
            // A cancelled upload must close or reset, without sending the whole file.
            let mut remaining = Vec::new();
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                upload.read_to_end(&mut remaining),
            )
            .await
            .unwrap();
            assert!(bytes_read + remaining.len() < 32 * 1024 * 1024);
        });
        let local = DeviceInfo::new("Sender".into(), 53317, Protocol::Http);
        let mut peer = DeviceInfo::new("Receiver".into(), port, Protocol::Http);
        peer.ip = Some("127.0.0.1".into());
        let (progress, updates) = tokio::sync::watch::channel(TransferProgress::default());
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(8),
            send_cancellable(
                local,
                generate_tls_certificate().unwrap(),
                peer,
                collect_paths(&[path]).unwrap(),
                None,
                progress,
                cancellation,
            ),
        )
        .await
        .unwrap();
        assert_eq!(result, Err(SendError::Cancelled));
        let final_report = updates.borrow().clone();
        assert!(final_report.complete);
        assert_eq!(final_report.files[0].outcome, FileOutcome::Canceled);
        assert!(final_report.bytes_sent > 0);
        assert!(final_report.bytes_sent < final_report.total_bytes);
        assert_eq!(final_report.finished_files, 0);
        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn folder_names_are_relative_and_symlink_cycles_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("photos/holiday");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("pic.txt"), b"hello").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.path().join("photos"), folder.join("cycle")).unwrap();
        let items = collect_paths(&[dir.path().join("photos")]).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "photos/holiday/pic.txt");
        assert_eq!(items[0].size, 5);
    }

    #[tokio::test]
    async fn accepted_subset_reports_real_streamed_bytes_and_empty_file_completion() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("accepted.txt"), b"content").unwrap();
        std::fs::write(source.path().join("empty.txt"), b"").unwrap();
        std::fs::write(source.path().join("skipped.txt"), b"not sent").unwrap();
        let mut items = collect_paths(&[
            source.path().join("accepted.txt"),
            source.path().join("empty.txt"),
            source.path().join("skipped.txt"),
        ])
        .unwrap();
        // Multiple-item offers upload text normally instead of using the inline 204 shortcut.
        items[0].source = Source::Text("content".into());
        let (mut server, mut events) = LocalSendServer::builder()
            .port(0)
            .protocol(Protocol::Http)
            .save_dir(target.path())
            .build()
            .await
            .unwrap();
        let mut peer = server.device().clone();
        peer.ip = Some("127.0.0.1".into());
        let approval = tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if let ServerEvent::TransferRequest(request) = event {
                    let accepted = request
                        .files()
                        .iter()
                        .filter(|(_, file)| file.file_name != "skipped.txt")
                        .map(|(id, _)| id.clone())
                        .collect();
                    request.accept_files(accepted);
                }
            }
        });
        let (progress, updates) = tokio::sync::watch::channel(TransferProgress::default());
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            send_cancellable(
                DeviceInfo::new("Sender".into(), 53317, Protocol::Http),
                generate_tls_certificate().unwrap(),
                peer,
                items,
                None,
                progress,
                CancellationToken::new(),
            ),
        )
        .await
        .unwrap();
        assert_eq!(result, Ok(2));
        let report = updates.borrow().clone();
        assert!(report.complete);
        assert_eq!((report.bytes_sent, report.total_bytes), (7, 7));
        assert_eq!((report.finished_files, report.accepted_files), (2, 2));
        assert_eq!(
            report
                .files
                .iter()
                .map(|file| file.outcome)
                .collect::<Vec<_>>(),
            vec![
                FileOutcome::Finished,
                FileOutcome::Finished,
                FileOutcome::Skipped
            ]
        );
        assert_eq!(report.fraction(), 1.0);
        assert_eq!(
            std::fs::read(target.path().join("accepted.txt")).unwrap(),
            b"content"
        );
        assert!(target.path().join("empty.txt").exists());
        assert!(!target.path().join("skipped.txt").exists());
        server.stop().await;
        approval.abort();
    }

    #[tokio::test]
    async fn https_transfer_keeps_approved_ids_and_relative_paths() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        std::fs::create_dir(source.path().join("folder")).unwrap();
        std::fs::write(source.path().join("folder/hello.txt"), b"LocalSend GTK4").unwrap();
        let (mut server, mut events) = LocalSendServer::builder()
            .alias("Receiver")
            .port(0)
            .protocol(Protocol::Https)
            .save_dir(target.path())
            .build()
            .await
            .unwrap();
        let mut peer = server.device().clone();
        peer.ip = Some("127.0.0.1".into());
        let cert = generate_tls_certificate().unwrap();
        let mut local = DeviceInfo::new("Sender".into(), 53318, Protocol::Https);
        local.fingerprint = cert.fingerprint.clone();
        let approval = tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if let ServerEvent::TransferRequest(request) = event {
                    request.accept();
                }
            }
        });
        let (tx, updates) = tokio::sync::watch::channel(TransferProgress::default());
        let result = send(
            local,
            cert,
            peer,
            collect_paths(&[source.path().join("folder")]).unwrap(),
            None,
            tx,
        )
        .await;
        assert_eq!(result.unwrap(), 1);
        let report = updates.borrow().clone();
        assert!(report.complete);
        assert_eq!(report.bytes_sent, b"LocalSend GTK4".len() as u64);
        assert_eq!(report.files[0].name, "folder/hello.txt");
        assert_eq!(report.files[0].outcome, FileOutcome::Finished);
        assert_eq!(
            std::fs::read(target.path().join("folder/hello.txt")).unwrap(),
            b"LocalSend GTK4"
        );
        server.stop().await;
        approval.abort();
    }

    #[tokio::test]
    async fn decline_is_reported_and_text_uses_inline_preview() {
        let target = tempfile::tempdir().unwrap();
        let (mut server, mut events) = LocalSendServer::builder()
            .port(0)
            .save_dir(target.path())
            .build()
            .await
            .unwrap();
        let mut peer = server.device().clone();
        peer.ip = Some("127.0.0.1".into());
        let local = DeviceInfo::new("Sender".into(), 53318, Protocol::Http);
        let approval = tokio::spawn(async move {
            let mut count = 0;
            while let Some(event) = events.recv().await {
                match event {
                    ServerEvent::TransferRequest(request) => {
                        count += 1;
                        if count == 1 {
                            request.decline();
                        } else {
                            request.accept();
                        }
                    }
                    ServerEvent::TextReceived { text, .. } => {
                        assert_eq!(text, "Hello 🌍");
                        return;
                    }
                    _ => {}
                }
            }
        });
        let (tx, _rx) = tokio::sync::watch::channel(TransferProgress::default());
        let items = vec![Selection::text("Hello 🌍".into())];
        let cert = generate_tls_certificate().unwrap();
        assert!(send(
            local.clone(),
            cert.clone(),
            peer.clone(),
            items.clone(),
            None,
            tx.clone()
        )
        .await
        .is_err());
        assert_eq!(send(local, cert, peer, items, None, tx).await.unwrap(), 1);
        tokio::time::timeout(std::time::Duration::from_secs(5), approval)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(std::fs::read_dir(target.path()).unwrap().count(), 0);
        server.stop().await;
    }
}
