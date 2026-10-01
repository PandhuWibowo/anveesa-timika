//! gRPC calls from one timika instance to another (join, unseal fan-out,
//! instance probes) and from the `timika operator` CLI.

use std::sync::OnceLock;
use std::time::Duration;

use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};

use super::pb;
use crate::core::JoinChallenge;
use crate::raft::types::ClusterNode;

static TLS: OnceLock<(Option<Vec<u8>>, bool)> = OnceLock::new();

/// CA to trust for `https://` peers (TLS_CA_FILE), and whether to skip
/// verification (CLI `--tls-skip-verify` only).
pub fn init(ca_pem: Option<Vec<u8>>, skip_verify: bool) {
    let _ = TLS.set((ca_pem, skip_verify));
}

pub async fn channel(addr: &str) -> anyhow::Result<Channel> {
    let addr = addr.trim_end_matches('/');
    let (ca, skip) = TLS.get().cloned().unwrap_or((None, false));
    if skip && addr.starts_with("https://") {
        return insecure_channel(addr).await;
    }
    let mut ep = Endpoint::from_shared(addr.to_string())?
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(60));
    if addr.starts_with("https://") {
        let mut tls = ClientTlsConfig::new().with_native_roots();
        if let Some(ca) = ca {
            tls = tls.ca_certificate(Certificate::from_pem(ca));
        }
        ep = ep.tls_config(tls)?;
    }
    Ok(ep.connect().await?)
}

/// TLS without certificate verification — only for the operator CLI's
/// `--tls-skip-verify` (e.g. talking to 127.0.0.1 behind a cert for a DNS name).
async fn insecure_channel(addr: &str) -> anyhow::Result<Channel> {
    use std::sync::Arc;

    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{DigitallySignedStruct, SignatureScheme};

    #[derive(Debug)]
    struct NoVerify(Arc<rustls::crypto::CryptoProvider>);
    impl ServerCertVerifier for NoVerify {
        fn verify_server_cert(
            &self,
            _: &CertificateDer<'_>,
            _: &[CertificateDer<'_>],
            _: &ServerName<'_>,
            _: &[u8],
            _: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            m: &[u8],
            c: &CertificateDer<'_>,
            d: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(m, c, d, &self.0.signature_verification_algorithms)
        }
        fn verify_tls13_signature(
            &self,
            m: &[u8],
            c: &CertificateDer<'_>,
            d: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(m, c, d, &self.0.signature_verification_algorithms)
        }
        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut cfg = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
        .with_no_client_auth();
    cfg.alpn_protocols = vec![b"h2".to_vec()];
    let connector = tokio_rustls::TlsConnector::from(Arc::new(cfg));

    let uri: tonic::transport::Uri = addr.parse()?;
    let host = uri.host().unwrap_or("localhost").to_string();
    let port = uri.port_u16().unwrap_or(443);
    // tonic must not try TLS itself: give it an http:// URI and our connector.
    let plain = format!("http://{host}:{port}");
    let ep = Endpoint::from_shared(plain)?.connect_timeout(Duration::from_secs(3)).timeout(Duration::from_secs(60));
    Ok(ep
        .connect_with_connector(tower::service_fn(move |_: tonic::transport::Uri| {
            let connector = connector.clone();
            let host = host.clone();
            async move {
                let tcp = tokio::net::TcpStream::connect((host.as_str(), port)).await?;
                let name = ServerName::try_from(host.clone()).map_err(std::io::Error::other)?;
                let tls = connector.connect(name, tcp).await?;
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(tls))
            }
        }))
        .await?)
}

pub fn msg(s: tonic::Status) -> anyhow::Error {
    anyhow::anyhow!("{}", s.message())
}

fn node_pb(n: &ClusterNode) -> pb::ClusterNode {
    pb::ClusterNode { name: n.name.clone(), api_addr: n.api_addr.clone(), cluster_addr: n.cluster_addr.clone() }
}

/// Another instance's seal status (None = unreachable within 2s).
pub async fn seal_status(addr: &str) -> Option<pb::SealStatus> {
    let fut = async {
        let mut c = pb::sys_service_client::SysServiceClient::new(channel(addr).await.ok()?);
        c.get_seal_status(()).await.ok().map(|r| r.into_inner())
    };
    tokio::time::timeout(Duration::from_secs(2), fut).await.ok().flatten()
}

pub async fn unseal(addr: &str, key: &str) -> anyhow::Result<pb::SealStatus> {
    let mut c = pb::sys_service_client::SysServiceClient::new(channel(addr).await?);
    let r = c
        .unseal(pb::UnsealRequest { key: key.to_string(), reset: false, all: false })
        .await
        .map_err(msg)?
        .into_inner();
    r.status.ok_or_else(|| anyhow::anyhow!("empty response"))
}

pub async fn sys(addr: &str) -> anyhow::Result<pb::sys_service_client::SysServiceClient<Channel>> {
    Ok(pb::sys_service_client::SysServiceClient::new(channel(addr).await?))
}

pub async fn join_challenge(via: &str, node: &ClusterNode) -> anyhow::Result<JoinChallenge> {
    let mut c = pb::cluster_service_client::ClusterServiceClient::new(channel(via).await?);
    let r = c
        .join_challenge(pb::JoinChallengeRequest { node: Some(node_pb(node)) })
        .await
        .map_err(msg)?
        .into_inner();
    Ok(JoinChallenge {
        seal_config: serde_json::from_str(&r.seal_config)?,
        keyring: r.keyring,
        challenge: r.challenge,
        wrapped_root: match r.wrapped_root {
            Some(w) => Some(serde_json::from_str(&w)?),
            None => None,
        },
    })
}

pub async fn join_answer(via: &str, node: &ClusterNode, answer: &str) -> anyhow::Result<()> {
    let mut c = pb::cluster_service_client::ClusterServiceClient::new(channel(via).await?);
    c.join_answer(pb::JoinAnswerRequest { node: Some(node_pb(node)), answer: answer.to_string() })
        .await
        .map_err(msg)?;
    Ok(())
}
