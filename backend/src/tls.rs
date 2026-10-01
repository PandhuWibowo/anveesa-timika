//! TLS for both listeners (API + cluster) and trust for outbound peer calls.
//!
//! Node-to-node traffic is *authenticated* by the cluster HMAC regardless of
//! TLS; TLS adds confidentiality for paths and metadata (values are already
//! ciphertext) and lets peers verify each other's identity.

use std::net::SocketAddr;

use axum::Router;

use crate::config::Config;

/// Shared outbound HTTP client (raft RPC, request forwarding, join).
pub fn http_client(cfg: &Config) -> anyhow::Result<reqwest::Client> {
    let mut b = reqwest::Client::builder().use_rustls_tls();
    if let Some(ca) = &cfg.tls_ca_file {
        let pem = std::fs::read(ca)?;
        for cert in reqwest::Certificate::from_pem_bundle(&pem)? {
            b = b.add_root_certificate(cert);
        }
    }
    Ok(b.build()?)
}

/// Serve `app` on `addr`, with TLS when a cert/key pair is configured.
pub async fn serve(cfg: &Config, addr: &str, app: Router) -> anyhow::Result<()> {
    let addr: SocketAddr = addr.parse()?;
    match (&cfg.tls_cert_file, &cfg.tls_key_file) {
        (Some(cert), Some(key)) => {
            let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key).await?;
            axum_server::bind_rustls(addr, tls)
                .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                .await?;
        }
        (None, None) => {
            let listener = tokio::net::TcpListener::bind(addr).await?;
            // Connect info = client address, for the audit log.
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
        }
        _ => anyhow::bail!("set both TLS_CERT_FILE and TLS_KEY_FILE, or neither"),
    }
    Ok(())
}
