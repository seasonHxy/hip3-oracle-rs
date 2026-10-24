use anyhow::{Result, ensure};
use axum::{Router, http::StatusCode, routing::get};
use clap::{Parser, Subcommand};
use hip3_oracle_rs::{config::Config, engine::Engine, now_ms};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

#[derive(Parser)]
#[command(version, about = "Native fail-closed HIP-3 oracle")]
struct Cli {
    #[arg(long, default_value = "config/example.json")]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Validate,
    Status,
    Preflight,
    Once {
        #[arg(long)]
        live: bool,
    },
    Run {
        #[arg(long)]
        live: bool,
    },
    Bootstrap {
        #[arg(long)]
        reason: String,
    },
    Reconcile {
        #[arg(long)]
        reason: String,
    },
    Pause {
        #[arg(long)]
        reason: String,
    },
    Resume {
        #[arg(long)]
        pause_id: String,
        #[arg(long)]
        reason: String,
    },
}
#[derive(Default)]
struct Health {
    last: AtomicU64,
    ready: AtomicBool,
    cycles: AtomicU64,
    failed: AtomicU64,
}
async fn shutdown() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
async fn run(e: Engine) -> Result<()> {
    let h = Arc::new(Health::default());
    let mut server = None;
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    if e.config.monitoring.enabled {
        let ready = h.clone();
        let metrics = h.clone();
        let age = e.config.monitoring.readiness_max_age_ms;
        let live = !e.config.dry_run;
        let ready_route = get(move || {
            let h = ready.clone();
            async move {
                if h.ready.load(Ordering::Relaxed)
                    && now_ms().saturating_sub(h.last.load(Ordering::Relaxed)) <= age
                {
                    (StatusCode::OK, "ready")
                } else {
                    (StatusCode::SERVICE_UNAVAILABLE, "not ready")
                }
            }
        });
        let router=Router::new().route("/livez",get(||async {"alive"})).route("/readyz",ready_route.clone()).route("/healthz",ready_route).route("/metrics",get(move || {let h=metrics.clone();async move {
            format!("# TYPE hip3_oracle_live_mode gauge\nhip3_oracle_live_mode {}\n# TYPE hip3_oracle_cycles_total counter\nhip3_oracle_cycles_total {}\n# TYPE hip3_oracle_failures_total counter\nhip3_oracle_failures_total {}\n# TYPE hip3_oracle_last_success_timestamp_seconds gauge\nhip3_oracle_last_success_timestamp_seconds {}\n",u8::from(live),h.cycles.load(Ordering::Relaxed),h.failed.load(Ordering::Relaxed),h.last.load(Ordering::Relaxed)/1000)
        }}));
        let listener = tokio::net::TcpListener::bind(format!(
            "{}:{}",
            e.config.monitoring.host, e.config.monitoring.port
        ))
        .await?;
        server = Some(tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = stop_rx.await;
                })
                .await
        }));
    }
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    let signal_task = tokio::spawn(async move {
        shutdown().await;
        let _ = shutdown_tx.send(true);
    });
    loop {
        if *shutdown_rx.borrow() {
            break;
        }
        let start = tokio::time::Instant::now();
        h.cycles.fetch_add(1, Ordering::Relaxed);
        h.ready.store(false, Ordering::Relaxed);
        match e.cycle().await {
            Ok(result) => {
                h.last.store(now_ms(), Ordering::Relaxed);
                h.ready.store(true, Ordering::Relaxed);
                tracing::info!(result=%result,"cycle succeeded");
            }
            Err(err) => {
                h.failed.fetch_add(1, Ordering::Relaxed);
                tracing::error!(error=%err,"cycle blocked");
            }
        }
        if server.as_ref().is_some_and(|s| s.is_finished()) {
            anyhow::bail!("monitoring server stopped unexpectedly");
        }
        let delay = Duration::from_millis(e.config.interval_ms).saturating_sub(start.elapsed());
        tokio::select! {_=tokio::time::sleep(delay)=>{},_=shutdown_rx.changed()=>{break;}}
    }
    // Signals stop scheduling new cycles; the current publication finishes or times out.
    signal_task.abort();
    let _ = stop_tx.send(());
    if let Some(server) = server {
        server.await??;
    }
    Ok(())
}
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let config = Config::load(&cli.config)?;
    if matches!(cli.command, Command::Validate) {
        println!("{}", json!({"valid":true,"scope":config.scope()}));
        return Ok(());
    }
    let live = matches!(
        cli.command,
        Command::Once { live: true } | Command::Run { live: true }
    );
    let e = Engine::new(config, live)?;
    match cli.command {
        Command::Validate => unreachable!(),
        Command::Status => println!("{}", serde_json::to_string_pretty(&e.store.load()?)?),
        Command::Preflight => println!("{}", json!({"ok":true,"action":e.preflight().await?})),
        Command::Once { .. } => println!("{}", e.cycle().await?),
        Command::Run { .. } => run(e).await?,
        Command::Bootstrap { reason } => e.bootstrap(&reason).await?,
        Command::Reconcile { reason } => e.reconcile(&reason).await?,
        Command::Pause { reason } => {
            ensure!(!reason.trim().is_empty(), "reason required");
            let _lock = e.store.lock()?;
            let mut s = e.store.load()?;
            ensure!(s.pause.is_none(), "already paused");
            let id = format!("{}-{}", now_ms(), std::process::id());
            e.store
                .audit(json!({"time":now_ms(),"event":"pause","id":id,"reason":reason}))?;
            s.pause = Some(id.clone());
            e.store.save(&s)?;
            println!("{}", json!({"pauseId":id}));
        }
        Command::Resume { pause_id, reason } => {
            ensure!(!reason.trim().is_empty(), "reason required");
            let _lock = e.store.lock()?;
            let mut s = e.store.load()?;
            ensure!(
                s.pause.as_deref() == Some(pause_id.as_str()),
                "pause ID mismatch"
            );
            e.store
                .audit(json!({"time":now_ms(),"event":"resume","id":pause_id,"reason":reason}))?;
            s.pause = None;
            e.store.save(&s)?;
        }
    }
    Ok(())
}
