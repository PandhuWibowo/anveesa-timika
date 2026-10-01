mod audit;
mod automation;
mod auth;
mod barrier;
mod bastion;
mod cli;
mod config;
mod core;
mod error;
mod grpc;
mod kv;
mod logging;
mod raft;
mod seal;
mod routes;
mod state;
mod storage;
mod tls;
mod token;

use std::sync::Arc;

use axum::Router;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::config::{Config, RedisMode, StorageKind};
use crate::core::{Core, Identity};
use crate::raft::types::ClusterNode;
use crate::raft::{RaftBackend, RaftConfig};
use crate::state::AppState;
use crate::storage::{RedisStorage, Storage};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let _ = rustls::crypto::ring::default_provider().install_default();

    // `timika operator …` is the built-in CLI, not the server.
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("operator") {
        return cli::run(args.collect()).await;
    }

    let cfg = Arc::new(Config::from_env()?);
    // Held for the whole process: dropping it stops (and flushes) the file writer.
    let _log_guard = logging::init(&cfg.node_name)?;
    let http = tls::http_client(&cfg)?;
    grpc::client::init(cfg.tls_ca_file.as_ref().map(std::fs::read).transpose()?, false);

    let storage = match cfg.storage {
        StorageKind::Redis => {
            // A vault without storage is meaningless, so an unreachable Redis is fatal.
            let redis = Arc::new(RedisStorage::connect(&cfg.redis).await?);
            redis.ping().await?;
            warn_on_unsafe_redis(&redis).await;
            redis.spawn_sentinel_watch();
            let target = match redis.mode() {
                RedisMode::Standalone => cfg.redis.url.clone(),
                RedisMode::Sentinel => format!("sentinel `{}` via {:?}", cfg.redis.sentinel_master, cfg.redis.sentinels),
                RedisMode::Cluster => format!("cluster seeds {:?}", cfg.redis.cluster_nodes),
            };
            tracing::info!(
                "storage: redis {} — {target} (wait_replicas={})",
                redis.mode().as_str(),
                cfg.redis.wait_replicas
            );
            Storage::Redis(redis)
        }
        StorageKind::Raft => {
            let backend = RaftBackend::open(
                RaftConfig {
                    node: ClusterNode {
                        name: cfg.node_name.clone(),
                        api_addr: cfg.api_addr.clone(),
                        cluster_addr: cfg.cluster_addr.clone(),
                    },
                    path: cfg.raft_path.clone(),
                    retry_join: cfg.retry_join.clone(),
                    snapshot_threshold: cfg.snapshot_threshold,
                    trailing_logs: cfg.trailing_logs,
                },
                http.clone(),
            )?;
            tracing::info!(
                "storage: raft node `{}` at {} (api {}, cluster {}), member={}",
                cfg.node_name,
                cfg.raft_path.display(),
                cfg.api_addr,
                cfg.cluster_addr,
                backend.is_member()
            );
            Storage::Raft(Arc::new(backend))
        }
    };

    let identity = Identity { id: cfg.node_name.clone(), api_addr: cfg.api_addr.clone(), started_at: chrono::Utc::now() };
    let auto = seal::AutoSeal::from_env(&http)?;
    let core = Arc::new(Core::new(storage, identity, auto));
    let status = core.status().await?;
    tracing::info!("initialized={}, sealed={}", status.initialized, status.sealed);
    core.spawn_retry_join();
    core.spawn_heartbeat();
    core.spawn_auto_unseal();
    automation::schedule::spawn(core.clone());
    // Bastion: drop recordings past their retention (default 90 days), hourly.
    {
        let core = core.clone();
        let days: i64 = std::env::var("SESSION_RETENTION_DAYS").ok().and_then(|v| v.parse().ok()).unwrap_or(90);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                if core.is_sealed().await {
                    continue;
                }
                if let Some(r) = core.storage.raft() {
                    if !r.is_leader().await {
                        continue;
                    }
                }
                match bastion::purge_old_sessions(&core, days).await {
                    Ok(n) if n > 0 => tracing::info!("bastion: purged {n} sessions older than {days} days"),
                    Err(e) => tracing::warn!("bastion: retention sweep failed: {e}"),
                    _ => {}
                }
                let run_days: i64 = std::env::var("AUTOMATION_RETENTION_DAYS").ok().and_then(|v| v.parse().ok()).unwrap_or(days);
                match automation::purge_old_runs(&core, run_days).await {
                    Ok(n) if n > 0 => tracing::info!("automation: purged {n} runs older than {run_days} days"),
                    Err(e) => tracing::warn!("automation: retention sweep failed: {e}"),
                    _ => {}
                }
            }
        });
    }
    if let Some(raft) = core.storage.raft().cloned() {
        let core = core.clone();
        tokio::spawn(async move {
            loop {
                raft.seal_requested.notified().await;
                core.seal().await;
            }
        });
    }

    let audit = audit::Auditor::from_env(&cfg.node_name).await?;
    let auth = Arc::new(auth::AuthService::from_env(http.clone())?);
    let files = bastion::files::FilePool::new();
    let state = AppState { cfg: cfg.clone(), core: core.clone(), http, audit, auth, files };

    // The cluster listener (Raft RPCs between peers) runs alongside the API.
    if let Some(raft) = core.storage.raft().cloned() {
        let cfg = cfg.clone();
        tokio::spawn(async move {
            tracing::info!("raft cluster listener on {}", cfg.cluster_bind_addr);
            if let Err(e) = tls::serve(&cfg, &cfg.cluster_bind_addr, raft::server::router(raft)).await {
                tracing::error!("cluster listener failed: {e}");
                std::process::exit(1);
            }
        });
    }

    // Permissive CORS for local dev; tighten via a reverse proxy in production.
    // gRPC-Web reads its status from response headers, so expose them.
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
        .expose_headers(Any);

    // One port: gRPC (HTTP/2) and gRPC-Web (HTTP/1.1) under `/timika.v1.*`,
    // the health check and terminal WebSocket under `/v1`, and the built Svelte
    // SPA from ./static (in dev Vite serves the frontend and proxies here).
    let app = Router::new()
        .nest("/v1", routes::http_router())
        .with_state(state.clone())
        .merge(grpc::router(state.clone()))
        .fallback(grpc::fallback)
        .layer(axum::middleware::from_fn_with_state(state, audit::middleware))
        .layer(cors)
        .layer(TraceLayer::new_for_http());

    tracing::info!("anveesa-timika listening on {} (advertised {})", cfg.bind_addr, cfg.api_addr);
    tls::serve(&cfg, &cfg.bind_addr, app).await
}

/// Log loudly at boot if Redis is configured in a way that can lose secrets.
async fn warn_on_unsafe_redis(storage: &RedisStorage) {
    if let Some(policy) = storage.config_get("maxmemory-policy").await {
        if policy != "noeviction" {
            tracing::warn!("redis maxmemory-policy is `{policy}` — keys can be evicted. Use `noeviction` for a vault.");
        }
    }
    if let Some(aof) = storage.config_get("appendonly").await {
        if aof != "yes" {
            tracing::warn!("redis appendonly is off — recent writes can be lost on crash. Use `--appendonly yes`.");
        }
    }
}
