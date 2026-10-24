use anyhow::{Result, ensure};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

fn interval() -> u64 {
    3000
}
fn timeout() -> u64 {
    2000
}
fn publish_timeout() -> u64 {
    5000
}
fn one() -> Decimal {
    Decimal::ONE
}
fn three() -> usize {
    3
}
fn age() -> u64 {
    5000
}
fn future() -> u64 {
    1000
}
fn spread() -> Decimal {
    Decimal::from(100)
}
fn mad() -> Decimal {
    Decimal::from(6)
}
fn band() -> Decimal {
    Decimal::from(10)
}
fn jump() -> Decimal {
    Decimal::from(1000)
}
fn share() -> Decimal {
    Decimal::from(4000)
}
fn yes() -> bool {
    true
}
fn status_default() -> String {
    "unknown".into()
}
fn timestamp_unit() -> String {
    "ms".into()
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub dex: String,
    pub network: String,
    #[serde(default = "interval")]
    pub interval_ms: u64,
    #[serde(default = "yes")]
    pub dry_run: bool,
    pub state_file: PathBuf,
    #[serde(default = "timeout")]
    pub source_timeout_ms: u64,
    #[serde(default = "publish_timeout")]
    pub publish_timeout_ms: u64,
    #[serde(default = "three")]
    pub confirmation_attempts: usize,
    #[serde(default)]
    pub mark_price_sets: u8,
    pub sources: BTreeMap<String, Source>,
    pub feeds: Vec<Feed>,
    #[serde(default)]
    pub monitoring: Monitoring,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Source {
    pub kind: String,
    pub independence_group: String,
    #[serde(default = "one", with = "rust_decimal::serde::str")]
    pub weight: Decimal,
    #[serde(default)]
    pub prices: BTreeMap<String, String>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub symbols: BTreeMap<String, String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub price_path: String,
    #[serde(default)]
    pub timestamp_path: String,
    #[serde(default = "timestamp_unit")]
    pub timestamp_unit: String,
    #[serde(default)]
    pub bid_path: Option<String>,
    #[serde(default)]
    pub ask_path: Option<String>,
    #[serde(default)]
    pub market_status_path: Option<String>,
    #[serde(default = "status_default")]
    pub market_status: String,
    #[serde(default)]
    pub max_retries: u8,
    #[serde(default)]
    pub min_request_interval_ms: u64,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Feed {
    pub coin: String,
    pub sz_decimals: u32,
    pub sources: Vec<String>,
    #[serde(default = "three")]
    pub min_sources: usize,
    #[serde(default = "three")]
    pub min_independent_groups: usize,
    #[serde(default = "age")]
    pub max_source_age_ms: u64,
    #[serde(default = "future")]
    pub max_future_ms: u64,
    #[serde(default = "spread", with = "rust_decimal::serde::str")]
    pub max_spread_bps: Decimal,
    #[serde(default = "mad", with = "rust_decimal::serde::str")]
    pub outlier_mad_multiplier: Decimal,
    #[serde(default = "band", with = "rust_decimal::serde::str")]
    pub outlier_min_band_bps: Decimal,
    #[serde(default = "spread", with = "rust_decimal::serde::str")]
    pub max_confidence_bps: Decimal,
    #[serde(default = "jump", with = "rust_decimal::serde::str")]
    pub max_jump_bps: Decimal,
    #[serde(default = "share", with = "rust_decimal::serde::str")]
    pub max_group_weight_share_bps: Decimal,
    #[serde(default)]
    pub block_when_market_closed: bool,
    #[serde(default = "yes")]
    pub block_when_market_unknown: bool,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Monitoring {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "host")]
    pub host: String,
    #[serde(default = "port")]
    pub port: u16,
    #[serde(default = "ready_age")]
    pub readiness_max_age_ms: u64,
}
fn host() -> String {
    "127.0.0.1".into()
}
fn port() -> u16 {
    9109
}
fn ready_age() -> u64 {
    10000
}
impl Default for Monitoring {
    fn default() -> Self {
        Self {
            enabled: false,
            host: host(),
            port: port(),
            readiness_max_age_ms: ready_age(),
        }
    }
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let mut c: Self = serde_json::from_slice(&std::fs::read(path)?)?;
        c.validate()?;
        let mode = if c.dry_run { "dry-run" } else { "live" };
        let stem = c
            .state_file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("oracle");
        let name = format!("{stem}.{}.{}.{}.rust.json", c.network, mode, c.dex);
        c.state_file = path
            .parent()
            .unwrap_or(Path::new("."))
            .join(c.state_file.with_file_name(name));
        Ok(c)
    }
    pub fn scope(&self) -> String {
        format!(
            "rust-v1:{}:{}:{}",
            self.network,
            if self.dry_run { "dry-run" } else { "live" },
            self.dex
        )
    }
    pub fn endpoint(&self) -> &'static str {
        if self.network == "mainnet" {
            "https://api.hyperliquid.xyz"
        } else {
            "https://api.hyperliquid-testnet.xyz"
        }
    }
    pub fn wire(&self, coin: &str) -> String {
        if coin.contains(':') {
            coin.into()
        } else {
            format!("{}:{coin}", self.dex)
        }
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (2..=6).contains(&self.dex.len())
                && self
                    .dex
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                && self.dex.as_bytes()[0].is_ascii_lowercase(),
            "invalid dex"
        );
        ensure!(
            ["testnet", "mainnet"].contains(&self.network.as_str()),
            "invalid network"
        );
        ensure!(
            (2500..=10000).contains(&self.interval_ms),
            "interval must be 2500..10000 ms"
        );
        ensure!(
            (1..=30000).contains(&self.source_timeout_ms)
                && (1..=30000).contains(&self.publish_timeout_ms),
            "invalid timeouts"
        );
        ensure!(
            (1..=10).contains(&self.confirmation_attempts) && self.mark_price_sets <= 1,
            "invalid publication options"
        );
        ensure!(
            !self.state_file.as_os_str().is_empty(),
            "stateFile required"
        );
        ensure!(
            !self.feeds.is_empty() && self.feeds.len() <= 128 && self.sources.len() <= 128,
            "invalid feed/source count"
        );
        ensure!(
            ["127.0.0.1", "0.0.0.0"].contains(&self.monitoring.host.as_str())
                && self.monitoring.readiness_max_age_ms >= self.interval_ms,
            "invalid monitoring"
        );
        for s in self.sources.values() {
            ensure!(
                !s.independence_group.trim().is_empty()
                    && s.weight >= Decimal::new(1, 6)
                    && s.weight <= Decimal::from(1000000),
                "invalid group/weight"
            );
            ensure!(
                s.max_retries <= 2 && s.min_request_interval_ms <= 30000,
                "invalid source budgets"
            );
            ensure!(
                s.bid_path.is_some() == s.ask_path.is_some(),
                "bidPath and askPath must be paired"
            );
            ensure!(
                ["ms", "s"].contains(&s.timestamp_unit.as_str()),
                "timestampUnit must be ms or s"
            );
            match s.kind.as_str() {
                "static" => ensure!(self.dry_run, "static sources forbidden in live mode"),
                "json" => {
                    let u = reqwest::Url::parse(
                        &s.url.replace("{symbol}", "X").replace("{coin}", "X"),
                    )?;
                    ensure!(
                        u.scheme() == "https"
                            && u.username().is_empty()
                            && u.password().is_none()
                            && u.fragment().is_none(),
                        "source requires HTTPS without URL credentials/fragment"
                    );
                    ensure!(
                        !s.price_path.is_empty() && !s.timestamp_path.is_empty(),
                        "JSON sources require upstream pricePath and timestampPath"
                    );
                }
                _ => anyhow::bail!("unknown source kind"),
            }
        }
        let mut coins = BTreeSet::new();
        for f in &self.feeds {
            ensure!(
                !f.coin.is_empty()
                    && (!f.coin.contains(':')
                        || (f.coin.starts_with(&format!("{}:", self.dex))
                            && f.coin.matches(':').count() == 1
                            && !f.coin.ends_with(':'))),
                "invalid coin namespace"
            );
            ensure!(coins.insert(self.wire(&f.coin)), "duplicate wire coin");
            ensure!(
                f.sz_decimals <= 6
                    && f.max_source_age_ms > 0
                    && f.max_source_age_ms <= 60000
                    && f.max_future_ms <= 1000,
                "invalid precision/timestamp bounds"
            );
            let names: BTreeSet<_> = f.sources.iter().collect();
            ensure!(
                names.len() == f.sources.len()
                    && f.min_sources >= 1
                    && f.min_sources <= names.len(),
                "invalid source quorum"
            );
            let mut groups = BTreeMap::<&str, Decimal>::new();
            for n in names {
                let s = self
                    .sources
                    .get(n)
                    .ok_or_else(|| anyhow::anyhow!("unknown source {n}"))?;
                let w = groups.entry(&s.independence_group).or_default();
                *w = (*w).max(s.weight);
            }
            ensure!(
                f.min_independent_groups >= 3 && f.min_independent_groups <= groups.len(),
                "at least three independent groups required"
            );
            ensure!(
                f.max_group_weight_share_bps > Decimal::ZERO
                    && f.max_group_weight_share_bps < Decimal::from(5000),
                "invalid group weight cap"
            );
            let sum: Decimal = groups.values().copied().sum();
            ensure!(
                groups
                    .values()
                    .all(|w| *w * Decimal::from(10000) <= sum * f.max_group_weight_share_bps),
                "unbalanced groups"
            );
            for v in [
                f.max_spread_bps,
                f.outlier_mad_multiplier,
                f.outlier_min_band_bps,
                f.max_confidence_bps,
                f.max_jump_bps,
            ] {
                ensure!(
                    v >= Decimal::ZERO && v <= Decimal::from(10000),
                    "risk threshold outside 0..10000"
                );
            }
        }
        Ok(())
    }
}
