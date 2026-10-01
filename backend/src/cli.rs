//! `timika operator …` — a small client built into the server binary, so every
//! container/pod already has it: `docker exec <task> /app/timika operator unseal`.
//!
//! It talks gRPC to an instance (`--addr`, `$TIMIKA_ADDR`, or this container's
//! own listener) — the same API the UI uses.

use std::io::{BufRead, IsTerminal, Write};
use std::time::Duration;

use crate::grpc::client::{self, msg};
use crate::grpc::pb;

const USAGE: &str = "\
timika operator <command> [flags]

commands:
  status                     seal status of the instance
  instances                  every instance / peer and whether it's sealed
  init [--shares N] [--threshold T]
  unseal [KEY] [--all]       submit one unseal key (prompted if omitted);
                             --all sends it to every sealed instance
  seal [--all]               seal this instance, or every instance (needs $TIMIKA_TOKEN)
  health                     exit 0 if the process answers (for container HEALTHCHECK)
  audit-verify FILE…         check the audit log's hash chain (offline). Give the
                             current file to include its rotated siblings, or list
                             files oldest first. Exit 1 if tampering is found.
  audit-hash VALUE           the audit-log form of a value (e.g. a token), to grep for it

flags:
  --addr URL                 instance to talk to (default $TIMIKA_ADDR or the local listener)
  --tls-skip-verify          don't verify the server certificate (e.g. when using 127.0.0.1)
";

/// Attach `$TIMIKA_TOKEN` (if set) as call metadata.
fn authed<T>(msg: T) -> anyhow::Result<tonic::Request<T>> {
    let mut req = tonic::Request::new(msg);
    if let Ok(t) = std::env::var("TIMIKA_TOKEN") {
        req.metadata_mut().insert("x-timika-token", t.trim().parse()?);
    }
    Ok(req)
}

/// Where the local server listens, from the same env the server reads.
fn default_addr() -> String {
    if let Ok(a) = std::env::var("TIMIKA_ADDR") {
        return a;
    }
    let scheme = if std::env::var("TLS_CERT_FILE").is_ok() { "https" } else { "http" };
    let bind = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8200".into());
    let port = bind.rsplit(':').next().unwrap_or("8200");
    format!("{scheme}://127.0.0.1:{port}")
}

fn read_key(arg: Option<&String>) -> anyhow::Result<String> {
    if let Some(k) = arg {
        return Ok(k.clone());
    }
    if std::io::stdin().is_terminal() {
        return Ok(rpassword::prompt_password("Unseal key (hidden): ")?);
    }
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

pub async fn run(args: Vec<String>) -> anyhow::Result<()> {
    let mut addr = default_addr();
    let mut skip_verify = std::env::var("TIMIKA_SKIP_VERIFY").is_ok();
    let mut all = false;
    let mut shares = 5u8;
    let mut threshold = 3u8;
    let mut positional = Vec::new();

    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--addr" => addr = it.next().ok_or_else(|| anyhow::anyhow!("--addr needs a value"))?,
            "--tls-skip-verify" => skip_verify = true,
            "--all" => all = true,
            "--shares" => shares = it.next().unwrap_or_default().parse()?,
            "--threshold" => threshold = it.next().unwrap_or_default().parse()?,
            "-h" | "--help" | "help" => {
                print!("{USAGE}");
                return Ok(());
            }
            _ => positional.push(a),
        }
    }
    let Some(cmd) = positional.first().cloned() else {
        print!("{USAGE}");
        return Ok(());
    };

    let ca = std::env::var("TLS_CA_FILE").ok().map(std::fs::read).transpose()?;
    client::init(ca, skip_verify);

    match cmd.as_str() {
        "audit-verify" => {
            let args = &positional[1..];
            if args.is_empty() {
                anyhow::bail!("usage: timika operator audit-verify /var/log/timika/audit.log");
            }
            let files: Vec<std::path::PathBuf> = if args.len() == 1 {
                crate::audit::with_rotated(std::path::Path::new(&args[0]))
            } else {
                args.iter().map(std::path::PathBuf::from).collect()
            };
            let v = crate::audit::verify(&files)?;
            println!("checked {} entries in {} file(s)", v.entries, files.len());
            for s in &v.chain_starts {
                println!("  chain starts at {s}");
            }
            if v.problems.is_empty() {
                println!("OK — hash chain intact");
            } else {
                for p in &v.problems {
                    println!("  TAMPERED  {p}");
                }
                println!("FAILED — {} problem(s)", v.problems.len());
                std::process::exit(1);
            }
        }
        "audit-hash" => {
            let input = positional.get(1).cloned().map(Ok).unwrap_or_else(|| read_key(None))?;
            let r = client::sys(&addr).await?.audit_hash(authed(pb::AuditHashRequest { input })?).await.map_err(msg)?;
            println!("{}", r.into_inner().hash);
        }
        "health" => {
            // Plain HTTP on purpose: the container HEALTHCHECK must work even
            // when nothing else does. Liveness only — any status code is fine.
            let mut b = reqwest::Client::builder().use_rustls_tls().timeout(Duration::from_secs(3));
            if skip_verify || addr.contains("127.0.0.1") || addr.contains("localhost") {
                b = b.danger_accept_invalid_certs(true);
            }
            let url = format!("{}/v1/sys/health?standbyok=true&sealedok=true&uninitok=true", addr.trim_end_matches('/'));
            let res = b.build()?.get(url).send().await;
            std::process::exit(if matches!(res, Ok(r) if r.status().is_success()) { 0 } else { 1 });
        }
        "status" => {
            let s = client::sys(&addr).await?.get_seal_status(()).await.map_err(msg)?.into_inner();
            print_status(&s);
        }
        "instances" => {
            let r = client::sys(&addr).await?.instances(()).await.map_err(msg)?.into_inner();
            print_instances(&r);
        }
        "init" => {
            let r = client::sys(&addr)
                .await?
                .init(pb::InitRequest { secret_shares: shares as u32, secret_threshold: threshold as u32 })
                .await
                .map_err(msg)?
                .into_inner();
            println!("seal type:   {}", r.seal_type);
            for (i, k) in r.keys.iter().enumerate() {
                println!("unseal key {}: {k}", i + 1);
            }
            println!("root token:  {}", r.root_token);
            eprintln!("\nStore these keys and the root token now — they are never shown again.");
        }
        "unseal" => {
            let key = read_key(positional.get(1))?;
            let targets: Vec<(String, String)> = if all {
                let r = client::sys(&addr).await?.instances(()).await.map_err(msg)?.into_inner();
                r.instances
                    .into_iter()
                    // Don't trust the reported seal state (it can lag a heartbeat):
                    // unsealing an unsealed instance is a harmless no-op.
                    .filter(|i| !i.stale)
                    // The local one may only be reachable via --addr (e.g. TLS SANs).
                    .map(|i| (i.id, if i.self_ { addr.clone() } else { i.api_addr }))
                    .collect()
            } else {
                vec![("this instance".into(), addr.clone())]
            };
            if targets.is_empty() {
                println!("nothing to do — no sealed instances");
            }
            let mut failed = 0;
            for (id, base) in targets {
                match client::unseal(&base, &key).await {
                    Ok(s) => println!("{id:<28} sealed={} progress={}/{}", s.sealed, s.progress, s.t),
                    Err(e) => {
                        failed += 1;
                        println!("{id:<28} error: {e}");
                    }
                }
            }
            // Scripts must notice: any instance that refused the key is a failure.
            if failed > 0 {
                std::io::stdout().flush()?;
                std::process::exit(1);
            }
        }
        "seal" => {
            let r = client::sys(&addr).await?.seal(authed(pb::SealRequest { all })?).await.map_err(msg)?.into_inner();
            let report = r.report.map(crate::grpc::convert::from_struct).unwrap_or_default();
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        other => {
            eprint!("unknown command `{other}`\n\n{USAGE}");
            std::process::exit(2);
        }
    }
    std::io::stdout().flush()?;
    Ok(())
}

fn print_status(s: &pb::SealStatus) {
    println!("initialized: {}", s.initialized);
    println!("sealed:      {}", s.sealed);
    println!("seal type:   {}", s.seal_type);
    if s.seal_type == "shamir" {
        println!("threshold:   {}/{} (progress {})", s.t, s.n, s.progress);
    }
    println!("storage:     {}", s.storage);
    if let Some(n) = &s.node {
        println!("node:        {n}");
    }
    if let Some(j) = &s.joining {
        println!("joining via: {j}");
    }
    if let Some(a) = &s.auto_unseal {
        println!("auto-unseal: {a}{}", if s.auto_unseal_paused { " (paused — sealed on purpose)" } else { "" });
    }
    if let Some(e) = &s.auto_unseal_error {
        println!("auto-unseal error: {e}");
    }
}

fn print_instances(r: &pb::InstancesResponse) {
    println!("storage: {}   (you are talking to {})", r.storage, r.self_);
    println!("{:<28} {:<11} {:<8} ADDRESS", "INSTANCE", "STATE", "ROLE");
    for i in &r.instances {
        let state = match (i.sealed, i.stale) {
            (_, true) => "unreachable",
            (Some(true), _) => "sealed",
            (Some(false), _) => "unsealed",
            _ => "unknown",
        };
        let role = if i.leader {
            "leader"
        } else if i.voter {
            "voter"
        } else if i.joining {
            "joining"
        } else {
            "-"
        };
        let me = if i.self_ { " *" } else { "" };
        println!("{:<28} {:<11} {:<8} {}{me}", i.id, state, role, i.api_addr);
    }
}
