//! Pinned peer TLS with our client certificate, required by official LocalSend 1.18+.
use futures_util::{StreamExt, TryStreamExt};
use localsend_rs::{
    crypto::{sha256_from_bytes, TlsCertificate},
    protocol::{
        DeviceInfo, FileId, FileMetadata, PrepareUploadRequest, PrepareUploadResponse, Protocol,
        SessionId, Token,
    },
};
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct PeerVerifier {
    fingerprint: Option<String>,
    observed: Arc<Mutex<Option<String>>>,
}
impl PeerVerifier {
    fn pinned(fingerprint: String) -> Self {
        Self {
            fingerprint: Some(fingerprint),
            observed: Arc::new(Mutex::new(None)),
        }
    }
}
impl ServerCertVerifier for PeerVerifier {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let actual = sha256_from_bytes(cert.as_ref());
        if self
            .fingerprint
            .as_ref()
            .is_some_and(|expected| !actual.eq_ignore_ascii_case(expected))
        {
            return Err(rustls::Error::General(
                "The peer certificate does not match its announced fingerprint".into(),
            ));
        }
        let mut observed = self
            .observed
            .lock()
            .map_err(|_| rustls::Error::General("TLS identity unavailable".into()))?;
        // Pin even discovery retries to the certificate observed on the first connection.
        if observed
            .as_ref()
            .is_some_and(|previous| previous != &actual)
        {
            return Err(rustls::Error::General(
                "The peer certificate changed during discovery".into(),
            ));
        }
        *observed = Some(actual);
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            signature,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            signature,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn normalize_fingerprint(fingerprint: &str) -> Result<String, String> {
    let fingerprint: String = fingerprint.chars().filter(|c| *c != ':').collect();
    if fingerprint.len() != 64 || !fingerprint.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("Invalid peer TLS fingerprint".into());
    }
    Ok(fingerprint.to_ascii_uppercase())
}

fn host_for_ip(ip: std::net::IpAddr) -> String {
    match ip {
        std::net::IpAddr::V4(ip) => ip.to_string(),
        std::net::IpAddr::V6(ip) => format!("[{ip}]"),
    }
}

/// Looks up an explicitly entered address without registering or uploading any user data.
/// Callers should default to HTTPS; HTTP must be an explicit user choice.
/// HTTPS learns the actual certificate on first use, checks the advertised identity, and
/// returns that identity for all later pinned requests. A saved fingerprint is checked
/// during the TLS handshake, before even the GET is transmitted.
/// The application's certificate is presented for official peers that require mutual TLS.
pub async fn lookup_peer(
    address: &str,
    port: u16,
    protocol: Protocol,
    expected_fingerprint: Option<&str>,
    certificate: TlsCertificate,
) -> Result<DeviceInfo, String> {
    const INFO_LIMIT: usize = 64 * 1024;
    let ip: std::net::IpAddr = address
        .trim()
        .parse()
        .map_err(|_| "Enter a valid IP address.")?;
    if port == 0 {
        return Err("Enter a port between 1 and 65535.".into());
    }
    localsend_rs::crypto::ensure_crypto_provider();
    let observed = Arc::new(Mutex::new(None));
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(8));
    if protocol == Protocol::Https {
        let expected = expected_fingerprint
            .map(normalize_fingerprint)
            .transpose()?;
        let verifier = PeerVerifier {
            fingerprint: expected,
            observed: observed.clone(),
        };
        let key = rustls_pemfile::private_key(&mut certificate.key_pem.as_bytes())
            .map_err(|e| e.to_string())?
            .ok_or("Missing TLS key")?;
        let tls = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_client_auth_cert(vec![CertificateDer::from(certificate.cert_der)], key)
            .map_err(|e| e.to_string())?;
        builder = builder.tls_backend_preconfigured(tls);
    }
    let client = builder.build().map_err(|e| e.to_string())?;
    let mut response = client
        .get(format!(
            "{protocol}://{}:{port}/api/localsend/v2/info",
            host_for_ip(ip)
        ))
        .send()
        .await
        .map_err(|e| {
            let mut description = e.to_string();
            let mut cause = std::error::Error::source(&e);
            while let Some(error) = cause {
                description.push_str(&format!(": {error}"));
                cause = error.source();
            }
            description
        })?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(format!(
            "The device returned HTTP {} during discovery.",
            response.status().as_u16()
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > INFO_LIMIT as u64)
    {
        return Err("The device information response is too large.".into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if body.len().saturating_add(chunk.len()) > INFO_LIMIT {
            return Err("The device information response is too large.".into());
        }
        body.extend_from_slice(&chunk);
    }
    let mut peer: DeviceInfo = serde_json::from_slice(&body)
        .map_err(|_| "The address did not return valid LocalSend device information.")?;
    if peer.alias.trim().is_empty() || peer.fingerprint.is_empty() {
        return Err("The device information is missing its name or fingerprint.".into());
    }
    if protocol == Protocol::Https {
        let actual = observed
            .lock()
            .map_err(|_| "TLS identity unavailable")?
            .clone()
            .ok_or("The peer did not provide a TLS certificate.")?;
        if normalize_fingerprint(&peer.fingerprint)? != actual.to_ascii_uppercase() {
            return Err(
                "The device's advertised fingerprint does not match its TLS certificate.".into(),
            );
        }
        peer.fingerprint = actual;
    } else if expected_fingerprint
        .is_some_and(|expected| !expected.eq_ignore_ascii_case(&peer.fingerprint))
    {
        return Err("The device identity no longer matches this favorite.".into());
    }
    // /info need not include an address, port, or protocol. Never let its JSON redirect
    // later transfers away from the endpoint the user explicitly asked to discover.
    peer.ip = Some(ip.to_string());
    peer.port = port;
    peer.protocol = protocol;
    Ok(peer)
}

pub struct Client {
    http: reqwest::Client,
    local: DeviceInfo,
    base: String,
}

#[derive(Debug)]
pub enum PrepareError {
    Status(reqwest::StatusCode),
    Failed(String),
}

impl Client {
    pub fn new(
        local: DeviceInfo,
        peer: &DeviceInfo,
        certificate: TlsCertificate,
    ) -> Result<Self, String> {
        localsend_rs::crypto::ensure_crypto_provider();
        let ip: std::net::IpAddr = peer
            .ip
            .as_deref()
            .ok_or("Missing peer address")?
            .parse()
            .map_err(|_| "Invalid peer address")?;
        let host = host_for_ip(ip);
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(90));
        if peer.protocol == Protocol::Https {
            let fingerprint = normalize_fingerprint(&peer.fingerprint)?;
            if !certificate
                .fingerprint
                .eq_ignore_ascii_case(&local.fingerprint)
            {
                return Err("Local device identity does not match its certificate".into());
            }
            let key = rustls_pemfile::private_key(&mut certificate.key_pem.as_bytes())
                .map_err(|e| e.to_string())?
                .ok_or("Missing TLS key")?;
            let tls = rustls::ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(PeerVerifier::pinned(fingerprint)))
                .with_client_auth_cert(vec![CertificateDer::from(certificate.cert_der)], key)
                .map_err(|e| e.to_string())?;
            builder = builder.tls_backend_preconfigured(tls);
        }
        Ok(Self {
            http: builder.build().map_err(|e| e.to_string())?,
            local,
            base: format!("{}://{host}:{}/api/localsend/v2", peer.protocol, peer.port),
        })
    }
    pub async fn prepare_upload(
        &self,
        files: HashMap<FileId, FileMetadata>,
        pin: Option<&str>,
    ) -> Result<PrepareUploadResponse, PrepareError> {
        let mut request = self
            .http
            .post(format!("{}/prepare-upload", self.base))
            .json(&PrepareUploadRequest {
                info: self.local.clone(),
                files,
            });
        if let Some(pin) = pin {
            request = request.query(&[("pin", pin)]);
        }
        let response = request
            .send()
            .await
            .map_err(|e| PrepareError::Failed(e.without_url().to_string()))?;
        if response.status() == reqwest::StatusCode::NO_CONTENT {
            return Ok(PrepareUploadResponse {
                session_id: SessionId::from_string(String::new()),
                files: HashMap::new(),
            });
        }
        if !response.status().is_success() {
            return Err(PrepareError::Status(response.status()));
        }
        response
            .json()
            .await
            .map_err(|e| PrepareError::Failed(e.without_url().to_string()))
    }
    fn upload(&self, session: &SessionId, file: &FileId, token: &Token) -> reqwest::RequestBuilder {
        self.http.post(format!("{}/upload", self.base)).query(&[
            ("sessionId", session.as_str()),
            ("fileId", file.as_str()),
            ("token", token.as_str()),
        ])
    }
    pub async fn upload_file(
        &self,
        session: &SessionId,
        file: &FileId,
        token: &Token,
        path: &std::path::Path,
        cancellation: CancellationToken,
        progress: impl Fn(u64, u64) + Send + Sync + 'static,
    ) -> Result<(), String> {
        let source = tokio::fs::File::open(path)
            .await
            .map_err(|e| e.to_string())?;
        let size = source.metadata().await.map_err(|e| e.to_string())?.len();
        let mut sent = 0;
        let stream = tokio_util::io::ReaderStream::new(source)
            .take_until(cancellation.cancelled_owned())
            .inspect_ok(move |bytes| {
                sent += bytes.len() as u64;
                progress(sent, size);
            });
        let response = self
            .upload(session, file, token)
            .header(reqwest::header::CONTENT_LENGTH, size)
            .body(reqwest::Body::wrap_stream(stream))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        Self::check(response).await.map(|_| ())
    }
    pub async fn upload_bytes(
        &self,
        session: &SessionId,
        file: &FileId,
        token: &Token,
        bytes: Vec<u8>,
        cancellation: CancellationToken,
        progress: impl Fn(u64, u64) + Send + Sync + 'static,
    ) -> Result<(), String> {
        let size = bytes.len() as u64;
        let mut sent = 0;
        let stream = tokio_util::io::ReaderStream::new(std::io::Cursor::new(bytes))
            .take_until(cancellation.cancelled_owned())
            .inspect_ok(move |bytes| {
                sent += bytes.len() as u64;
                progress(sent, size);
            });
        let response = self
            .upload(session, file, token)
            .header(reqwest::header::CONTENT_LENGTH, size)
            .body(reqwest::Body::wrap_stream(stream))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        Self::check(response).await.map(|_| ())
    }
    pub async fn cancel(&self, session: &SessionId) -> Result<(), String> {
        self.cancel_request(Some(session)).await
    }
    pub async fn cancel_pending(&self) -> Result<(), String> {
        self.cancel_request(None).await
    }
    async fn cancel_request(&self, session: Option<&SessionId>) -> Result<(), String> {
        let mut request = self.http.post(format!("{}/cancel", self.base));
        if let Some(session) = session {
            request = request.query(&[("sessionId", session.as_str())]);
        }
        let response = request
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        Self::check(response).await.map(|_| ())
    }
    async fn check(response: reqwest::Response) -> Result<reqwest::Response, String> {
        if response.status().is_success() {
            return Ok(response);
        }
        Err(match response.status().as_u16() {
            401 => "The recipient requires a valid PIN.".into(),
            403 => "The recipient declined this transfer.".into(),
            409 => "The recipient is busy with another transfer.".into(),
            429 => "Too many attempts. Try again later.".into(),
            code => format!("The recipient returned HTTP {code}."),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn tls_info_server(
        response: impl FnOnce(&TlsCertificate) -> Vec<u8>,
    ) -> (u16, String, tokio::task::JoinHandle<Option<String>>) {
        let certificate = localsend_rs::crypto::generate_tls_certificate().unwrap();
        let bytes = response(&certificate);
        let fingerprint = certificate.fingerprint.clone();
        let key = rustls_pemfile::private_key(&mut certificate.key_pem.as_bytes())
            .unwrap()
            .unwrap();
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![CertificateDer::from(certificate.cert_der)], key)
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let Ok(mut stream) = tokio_rustls::TlsAcceptor::from(Arc::new(config))
                .accept(stream)
                .await
            else {
                return None;
            };
            let mut request = Vec::new();
            loop {
                let mut chunk = [0; 4096];
                let read = stream.read(&mut chunk).await.unwrap_or(0);
                if read == 0 {
                    return None;
                }
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
                assert!(request.len() < 64 * 1024);
            }
            let _ = stream.write_all(&bytes).await;
            let _ = stream.shutdown().await;
            Some(String::from_utf8(request).unwrap())
        });
        (port, fingerprint, server)
    }

    fn json_response(body: serde_json::Value) -> Vec<u8> {
        let body = serde_json::to_vec(&body).unwrap();
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend(body);
        response
    }

    #[tokio::test]
    async fn manual_discovery_only_reads_info_and_pins_actual_certificate() {
        let (port, fingerprint, server) = tls_info_server(|certificate| {
            json_response(serde_json::json!({
                "alias": "Manual peer", "version": "2.0", "fingerprint": certificate.fingerprint,
                "ip": "192.0.2.99", "port": 1, "protocol": "http"
            }))
        })
        .await;
        let peer = lookup_peer(
            "127.0.0.1",
            port,
            Protocol::Https,
            None,
            localsend_rs::crypto::generate_tls_certificate().unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(peer.alias, "Manual peer");
        assert_eq!(peer.fingerprint, fingerprint);
        assert_eq!(peer.ip.as_deref(), Some("127.0.0.1"));
        assert_eq!(peer.port, port);
        assert_eq!(peer.protocol, Protocol::Https);
        let request = server.await.unwrap().unwrap();
        assert!(request.starts_with("GET /api/localsend/v2/info HTTP/"));
        assert_eq!(request.split_once("\r\n\r\n").unwrap().1, "");
        assert!(!request.to_ascii_lowercase().contains("content-type:"));
    }

    #[tokio::test]
    async fn discovery_rejects_a_claimed_identity_different_from_tls() {
        let (port, _, server) = tls_info_server(|_| {
            json_response(serde_json::json!({
                "alias": "Impostor", "version": "2.0", "fingerprint": "0".repeat(64)
            }))
        })
        .await;
        let error = lookup_peer(
            "127.0.0.1",
            port,
            Protocol::Https,
            None,
            localsend_rs::crypto::generate_tls_certificate().unwrap(),
        )
        .await
        .unwrap_err();
        assert!(error.contains("does not match its TLS certificate"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn favorite_reconnect_rejects_changed_certificate_before_http() {
        let (port, _, server) = tls_info_server(|certificate| {
            json_response(serde_json::json!({
                "alias": "Changed peer", "version": "2.0", "fingerprint": certificate.fingerprint
            }))
        })
        .await;
        let error = lookup_peer(
            "127.0.0.1",
            port,
            Protocol::Https,
            Some(&"0".repeat(64)),
            localsend_rs::crypto::generate_tls_certificate().unwrap(),
        )
        .await
        .unwrap_err();
        assert!(error.contains("fingerprint"), "{error}");
        assert!(
            server.await.unwrap().is_none(),
            "An unpinned peer received HTTP data"
        );
    }

    #[tokio::test]
    async fn favorite_reconnect_accepts_matching_pinned_certificate() {
        let (port, fingerprint, server) = tls_info_server(|certificate| {
            json_response(serde_json::json!({
                "alias": "Favorite", "version": "2.0", "fingerprint": certificate.fingerprint
            }))
        })
        .await;
        let peer = lookup_peer(
            "127.0.0.1",
            port,
            Protocol::Https,
            Some(&fingerprint.to_ascii_lowercase()),
            localsend_rs::crypto::generate_tls_certificate().unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(peer.fingerprint, fingerprint);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn discovery_does_not_follow_redirects() {
        let (port, _, server) = tls_info_server(|_| b"HTTP/1.1 302 Found\r\nLocation: https://127.0.0.1:1/private\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()).await;
        let error = lookup_peer(
            "127.0.0.1",
            port,
            Protocol::Https,
            None,
            localsend_rs::crypto::generate_tls_certificate().unwrap(),
        )
        .await
        .unwrap_err();
        assert!(error.contains("HTTP 302"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn discovery_bounds_declared_and_streamed_response_size() {
        for chunked in [false, true] {
            let (port, _, server) = tls_info_server(|_| {
                if chunked {
                    let mut response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n10001\r\n".to_vec();
                    response.extend(vec![b'x'; 65537]);
                    response.extend(b"\r\n0\r\n\r\n");
                    response
                } else {
                    b"HTTP/1.1 200 OK\r\nContent-Length: 65537\r\nConnection: close\r\n\r\n".to_vec()
                }
            }).await;
            let error = lookup_peer(
                "127.0.0.1",
                port,
                Protocol::Https,
                None,
                localsend_rs::crypto::generate_tls_certificate().unwrap(),
            )
            .await
            .unwrap_err();
            assert!(error.contains("too large"), "{error}");
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn sends_matching_client_identity_when_peer_requires_mutual_tls() {
        check_mutual_tls_identity(false).await;
    }

    #[tokio::test]
    async fn manual_discovery_sends_client_identity_when_peer_requires_mutual_tls() {
        check_mutual_tls_identity(true).await;
    }

    async fn check_mutual_tls_identity(manual_discovery: bool) {
        localsend_rs::crypto::ensure_crypto_provider();
        let local_cert = localsend_rs::crypto::generate_tls_certificate().unwrap();
        let peer_cert = localsend_rs::crypto::generate_tls_certificate().unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from(local_cert.cert_der.clone()))
            .unwrap();
        let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
            .build()
            .unwrap();
        let key = rustls_pemfile::private_key(&mut peer_cert.key_pem.as_bytes())
            .unwrap()
            .unwrap();
        let config = rustls::ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(vec![CertificateDer::from(peer_cert.cert_der)], key)
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let expected = local_cert.fingerprint.clone();
        let response_body = json_response(serde_json::json!({
            "alias": "Official-like peer", "version": "2.0", "fingerprint": peer_cert.fingerprint,
        }));
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = tokio_rustls::TlsAcceptor::from(Arc::new(config))
                .accept(stream)
                .await
                .unwrap();
            let certs = stream.get_ref().1.peer_certificates().unwrap();
            assert_eq!(sha256_from_bytes(certs[0].as_ref()), expected);
            let mut request = Vec::new();
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let mut chunk = [0; 4096];
                let read = stream.read(&mut chunk).await.unwrap();
                assert!(read > 0, "Expected an HTTP request after the TLS handshake");
                request.extend_from_slice(&chunk[..read]);
                assert!(request.len() < 64 * 1024);
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with("GET /api/localsend/v2/info HTTP/"));
            assert_eq!(request.split_once("\r\n\r\n").unwrap().1, "");
            stream.write_all(&response_body).await.unwrap();
        });
        let mut local = DeviceInfo::new("Native".into(), 53317, Protocol::Https);
        local.fingerprint = local_cert.fingerprint.clone();
        let mut peer = DeviceInfo::new("Official-like peer".into(), port, Protocol::Https);
        peer.ip = Some("127.0.0.1".into());
        peer.fingerprint = peer_cert.fingerprint;
        if manual_discovery {
            let discovered = lookup_peer("127.0.0.1", port, Protocol::Https, None, local_cert)
                .await
                .unwrap();
            assert_eq!(discovered.fingerprint, peer.fingerprint);
        } else {
            let client = Client::new(local, &peer, local_cert).unwrap();
            let response = client
                .http
                .get(format!("{}/info", client.base))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::OK);
        }
        server.await.unwrap();
    }
    #[test]
    fn rejects_a_different_certificate_before_sending_data() {
        let cert = localsend_rs::crypto::generate_tls_certificate().unwrap();
        let verifier = PeerVerifier::pinned("0".repeat(64));
        assert!(verifier
            .verify_server_cert(
                &CertificateDer::from(cert.cert_der.clone()),
                &[],
                &ServerName::try_from("localhost").unwrap(),
                &[],
                UnixTime::now()
            )
            .is_err());
        let verifier = PeerVerifier::pinned(cert.fingerprint);
        assert!(verifier
            .verify_server_cert(
                &CertificateDer::from(cert.cert_der),
                &[],
                &ServerName::try_from("localhost").unwrap(),
                &[],
                UnixTime::now()
            )
            .is_ok());
    }
}
