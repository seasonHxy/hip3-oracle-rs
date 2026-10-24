use crate::{
    config::Config,
    market::{aggregate, check_jump, format_price, fresh},
    now_ms,
    protocol::{Action, Signer},
    state::{Pending, State, Store},
    transport::Http,
};
use anyhow::{Result, ensure};
use futures::{StreamExt, stream};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};
use tokio::time::{sleep, timeout};

pub struct Engine {
    pub config: Config,
    pub http: Http,
    pub store: Store,
    pub signer: Option<Signer>,
}
impl Engine {
    pub fn new(config: Config, live: bool) -> Result<Self> {
        if live {
            ensure!(!config.dry_run, "--live requires dryRun=false");
            ensure!(
                std::env::var("HIP3_ENABLE_LIVE").as_deref() == Ok("YES"),
                "HIP3_ENABLE_LIVE=YES required"
            );
            ensure!(
                config.network != "mainnet"
                    || std::env::var("HIP3_ENABLE_MAINNET").as_deref() == Ok("YES"),
                "HIP3_ENABLE_MAINNET=YES required"
            );
        }
        Ok(Self {
            http: Http::new(&config)?,
            store: Store::new(&config),
            config,
            signer: if live {
                Some(Signer::from_environment()?)
            } else {
                None
            },
        })
    }
    pub async fn build(&self, state: &State) -> Result<Action> {
        Ok(self.build_timed(state, false).await?.0)
    }
    async fn build_timed(
        &self,
        state: &State,
        audit: bool,
    ) -> Result<(Action, BTreeMap<String, u64>)> {
        ensure!(state.pause.is_none(), "operator pause active");
        ensure!(
            state.pending.is_none(),
            "pending submission requires reconcile"
        );
        let c = &self.config;
        let mut prices = BTreeMap::new();
        // Each provider is bounded; every feed must pass before building an action.
        let jobs = stream::iter(
            c.feeds
                .iter()
                .map(|f| async move { (f, self.http.collect(c, &f.coin, &f.sources).await) }),
        )
        .buffer_unordered(16);
        tokio::pin!(jobs);
        while let Some((f, quotes)) = jobs.next().await {
            let result = aggregate(f, quotes.clone(), now_ms());
            // Includes raw normalized quotes and result for incident analysis; no secrets.
            tracing::debug!(coin=%f.coin,quotes=quotes.len(),"aggregation completed");
            if audit {
                self.store.audit(json!({"time":now_ms(),"event":"aggregation","coin":f.coin,"quotes":quotes,"result":result.as_ref().ok(),"error":result.as_ref().err().map(|e|e.to_string())}))?;
            }
            let a = result?;
            let text = format_price(a.price, f.sz_decimals)?;
            if let Some(previous) = state.prices.get(&c.wire(&f.coin)) {
                check_jump(f, text.parse()?, previous.parse()?)?;
            } else {
                ensure!(c.dry_run, "live price reference missing: bootstrap first");
            }
            // Validate again after all sources and metadata reads below.
            prices.insert(c.wire(&f.coin), (text, a.timestamp));
        }
        for f in &c.feeds {
            ensure!(
                fresh(f, prices[&c.wire(&f.coin)].1, now_ms()),
                "batch quotes expired"
            );
        }
        let values = prices
            .iter()
            .map(|(k, (p, _))| (k.clone(), p.clone()))
            .collect();
        Ok((
            Action::new(&c.dex, &values, c.mark_price_sets == 1),
            prices.into_iter().map(|(k, (_, t))| (k, t)).collect(),
        ))
    }
    pub async fn preflight(&self) -> Result<Action> {
        let s = self.store.load()?;
        if !self.config.dry_run {
            let snap = self.http.info(&self.config).await?;
            snap.require_config(&self.config)?;
        }
        self.build(&s).await
    }
    pub async fn cycle(&self) -> Result<Value> {
        let _lock = self.store.lock()?;
        let mut state = self.store.load()?;
        ensure!(
            self.config.dry_run || self.signer.is_some(),
            "live config requires --live"
        );
        // Preflight before collection, then collect a fresh complete batch.
        if !self.config.dry_run {
            ensure!(
                state.pending.is_none() && state.pause.is_none(),
                "pending or paused"
            );
            self.http
                .info(&self.config)
                .await?
                .require_config(&self.config)?;
            ensure!(
                now_ms().saturating_sub(state.last_attempt_ms) >= 2500,
                "persistent publication rate limit"
            );
        }
        let (action, timestamps) = self.build_timed(&state, true).await?;
        let prices = action.set_oracle.oracle_pxs.iter().cloned().collect();
        if self.config.dry_run {
            self.store
                .audit(json!({"time":now_ms(),"event":"simulated","action":action}))?;
            state.prices = prices;
            self.store.save(&state)?;
            return Ok(json!({"status":"simulated","action":action}));
        }
        // Preserve the oldest upstream observation through the durable write and submission.
        for f in &self.config.feeds {
            ensure!(
                fresh(f, timestamps[&self.config.wire(&f.coin)], now_ms()),
                "submission quote expired"
            );
        }
        let nonce = now_ms().max(
            state
                .last_nonce
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("nonce exhausted"))?,
        );
        ensure!(
            nonce <= now_ms().saturating_add(1000),
            "clock rollback: nonce ahead of wall clock"
        );
        let signature =
            self.signer
                .as_ref()
                .unwrap()
                .sign(&action, nonce, self.config.network == "mainnet")?;
        state.last_nonce = nonce;
        state.last_attempt_ms = now_ms();
        state.pending = Some(Pending {
            nonce,
            action: action.clone(),
        });
        self.store.save(&state)?;
        self.store
            .audit(json!({"time":now_ms(),"event":"pending","nonce":nonce,"action":action}))?;
        let outcome=timeout(Duration::from_millis(self.config.publish_timeout_ms),async {
            for f in &self.config.feeds {ensure!(fresh(f,timestamps[&self.config.wire(&f.coin)],now_ms()),"quote expired during durable write; pending retained");}
            let response=self.http.exchange(&json!({"action":action,"nonce":nonce,"signature":signature,"vaultAddress":null})).await?;
            ensure!(response.get("status").and_then(Value::as_str)==Some("ok"),"exchange did not acknowledge success; pending retained");
            for attempt in 0..self.config.confirmation_attempts {
                if let Ok(snap)=self.http.info(&self.config).await
                    && snap.require_config(&self.config).is_ok() && snap.matches(&prices).is_ok() {return Ok(());}
                if attempt+1<self.config.confirmation_attempts {sleep(Duration::from_millis(200)).await;}
            }
            anyhow::bail!("readback not confirmed; pending retained")
        }).await;
        match outcome {
            Ok(Ok(())) => {
                state.prices = prices;
                state.last_attempt_ms = now_ms();
                state.pending = None;
                self.store.save(&state)?;
                self.store
                    .audit(json!({"time":now_ms(),"event":"confirmed","nonce":nonce}))?;
                Ok(json!({"status":"confirmed","nonce":nonce}))
            }
            Ok(Err(e)) => Err(e),
            Err(_) => anyhow::bail!("publication timeout; pending retained; do not resend"),
        }
    }
    pub async fn bootstrap(&self, reason: &str) -> Result<()> {
        ensure!(!reason.trim().is_empty(), "reason required");
        ensure!(!self.config.dry_run, "bootstrap is for live scope");
        let _lock = self.store.lock()?;
        let mut s = self.store.load()?;
        ensure!(
            s.prices.is_empty() && s.pending.is_none(),
            "bootstrap refuses existing reference/pending"
        );
        let snap = self.http.info(&self.config).await?;
        snap.require_config(&self.config)?;
        self.store.audit(
            json!({"time":now_ms(),"event":"bootstrap","reason":reason,"prices":snap.prices}),
        )?;
        s.prices = snap.prices;
        self.store.save(&s)
    }
    pub async fn reconcile(&self, reason: &str) -> Result<()> {
        ensure!(!reason.trim().is_empty(), "reason required");
        let _lock = self.store.lock()?;
        let mut s = self.store.load()?;
        let pending = s
            .pending
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no pending submission"))?;
        ensure!(
            pending.action.set_oracle.dex == self.config.dex,
            "pending dex mismatch"
        );
        let prices = pending
            .action
            .set_oracle
            .oracle_pxs
            .iter()
            .cloned()
            .collect();
        let snap = self.http.info(&self.config).await?;
        snap.require_config(&self.config)?;
        snap.matches(&prices)?;
        self.store.audit(
            json!({"time":now_ms(),"event":"reconciled","reason":reason,"nonce":pending.nonce}),
        )?;
        s.prices = prices;
        s.last_attempt_ms = now_ms();
        s.pending = None;
        self.store.save(&s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, extract::State as AxumState, routing::post};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    #[derive(Clone)]
    struct MockState {
        calls: Arc<AtomicUsize>,
        kind: &'static str,
    }
    async fn info(AxumState(s): AxumState<MockState>) -> axum::Json<Value> {
        let price = if s.kind == "mismatch" { "100" } else { "230.2" };
        axum::Json(json!([{"universe":[{"name":"demo:AAPL","szDecimals":2}]},[{"oraclePx":price}]]))
    }
    async fn exchange(AxumState(s): AxumState<MockState>) -> axum::Json<Value> {
        s.calls.fetch_add(1, Ordering::Relaxed);
        if s.kind == "timeout" {
            sleep(Duration::from_secs(2)).await;
        }
        axum::Json(if s.kind == "reject" {
            json!({"status":"err","response":"rejected"})
        } else {
            json!({"status":"ok","response":{"type":"default"}})
        })
    }
    async fn setup(
        kind: &'static str,
    ) -> (
        Engine,
        tempfile::TempDir,
        Arc<AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        let mut config = Config::load(std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/config/example.json"
        )))
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        config.state_file = dir.path().join("oracle.json");
        // Static fixtures inject deterministic quotes only in unit tests; CLI rejects these for live configs.
        let mut e = Engine::new(config, false).unwrap();
        e.config.dry_run = false;
        e.store.scope = e.config.scope();
        e.config.publish_timeout_ms = 300;
        e.config.confirmation_attempts = 1;
        e.signer = Some(Signer::from_bytes(&[1; 32]).unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let state = MockState {
            calls: calls.clone(),
            kind,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/info", post(info))
            .route("/exchange", post(exchange))
            .with_state(state);
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        e.http.test_endpoint(format!("http://{addr}"));
        let _guard = e.store.lock().unwrap();
        let mut state = e.store.load().unwrap();
        state.prices.insert("demo:AAPL".into(), "230.2".into());
        e.store.save(&state).unwrap();
        (e, dir, calls, server)
    }
    #[tokio::test]
    async fn acknowledged_and_read_back_commit() {
        let (e, _dir, calls, server) = setup("ok").await;
        assert_eq!(e.cycle().await.unwrap()["status"], "confirmed");
        let s = e.store.load().unwrap();
        assert!(s.pending.is_none());
        assert_eq!(s.prices["demo:AAPL"], "230.2");
        assert!(e.cycle().await.is_err());
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        server.abort();
    }
    #[tokio::test]
    async fn ambiguous_results_survive_restart_and_never_resend() {
        for kind in ["timeout", "mismatch", "reject"] {
            let (e, _dir, calls, server) = setup(kind).await;
            assert!(e.cycle().await.is_err());
            assert!(e.store.load().unwrap().pending.is_some());
            // New engine instance uses the same durable scope; no signer is needed to inspect/reconcile.
            let restarted = Engine {
                config: e.config.clone(),
                http: Http::new(&e.config).unwrap(),
                store: Store::new(&e.config),
                signer: Some(Signer::from_bytes(&[1; 32]).unwrap()),
            };
            assert!(restarted.cycle().await.is_err());
            assert_eq!(calls.load(Ordering::Relaxed), 1);
            server.abort();
        }
    }
    #[tokio::test]
    async fn reconcile_only_reads_and_preserves_pending_on_mismatch() {
        let (e, _dir, calls, server) = setup("mismatch").await;
        assert!(e.cycle().await.is_err());
        assert!(e.reconcile("investigated").await.is_err());
        assert!(e.store.load().unwrap().pending.is_some());
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        server.abort();
        let (e, _dir, calls, server) = setup("reject").await;
        assert!(e.cycle().await.is_err());
        e.reconcile("verified values; exclusive updater")
            .await
            .unwrap();
        assert!(e.store.load().unwrap().pending.is_none());
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        server.abort();
    }
    #[tokio::test]
    async fn pause_and_jump_block_before_exchange() {
        let (e, _dir, calls, server) = setup("ok").await;
        let mut s = e.store.load().unwrap();
        s.pause = Some("incident".into());
        e.store.save(&s).unwrap();
        assert!(e.cycle().await.is_err());
        s.pause = None;
        s.prices.insert("demo:AAPL".into(), "1".into());
        e.store.save(&s).unwrap();
        assert!(e.cycle().await.is_err());
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        server.abort();
    }
}
