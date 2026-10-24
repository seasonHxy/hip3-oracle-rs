use crate::{
    config::{Config, Source},
    market::{Quote, valid_price},
    now_ms,
};
use anyhow::{Result, ensure};
use futures::{StreamExt, stream};
use reqwest::{Client, Response};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::{
    sync::Mutex,
    time::{Instant, sleep, timeout},
};

pub struct Http {
    pub client: Client,
    endpoint: String,
    gates: BTreeMap<String, Arc<Mutex<Option<Instant>>>>,
}
pub async fn bounded_json(response: Response) -> Result<Value> {
    ensure!(
        response.status().is_success(),
        "HTTP non-success status {}",
        response.status().as_u16()
    );
    ensure!(
        response.content_length().unwrap_or(0) <= 2_000_000,
        "response too large"
    );
    let mut bytes = vec![];
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|_| anyhow::anyhow!("response body transport failure"))?;
        ensure!(bytes.len() + chunk.len() <= 2_000_000, "response too large");
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}
pub fn extract<'a>(v: &'a Value, path: &str) -> Result<&'a Value> {
    if path.starts_with('/') {
        return v
            .pointer(path)
            .ok_or_else(|| anyhow::anyhow!("missing JSON pointer"));
    }
    let mut current = v;
    for part in path.split('.') {
        current = if let Some(a) = current.as_array() {
            a.get(part.parse::<usize>()?)
        } else {
            current.get(part)
        }
        .ok_or_else(|| anyhow::anyhow!("missing JSON field"))?;
    }
    Ok(current)
}
pub fn decimal(v: &Value) -> Result<Decimal> {
    let s = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => anyhow::bail!("invalid decimal type"),
    };
    Ok(s.parse()?)
}
fn expand(value: &str) -> Result<String> {
    let mut output = String::new();
    let mut rest = value;
    while let Some(i) = rest.find("${") {
        output.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let end = after
            .find('}')
            .ok_or_else(|| anyhow::anyhow!("invalid environment placeholder"))?;
        output.push_str(
            &std::env::var(&after[..end])
                .map_err(|_| anyhow::anyhow!("missing provider environment variable"))?,
        );
        rest = &after[end + 1..];
    }
    output.push_str(rest);
    Ok(output)
}
impl Http {
    pub fn new(c: &Config) -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_millis(c.publish_timeout_ms))
                .user_agent("hip3-oracle-rs/0.1.0")
                .build()?,
            endpoint: c.endpoint().into(),
            gates: c
                .sources
                .keys()
                .map(|s| (s.clone(), Arc::new(Mutex::new(None))))
                .collect(),
        })
    }
    #[cfg(test)]
    pub(crate) fn test_endpoint(&mut self, endpoint: String) {
        self.endpoint = endpoint;
    }
    async fn quote(&self, name: &str, s: &Source, coin: &str) -> Result<Quote> {
        if s.kind == "static" {
            return Ok(Quote {
                source: name.into(),
                group: s.independence_group.clone(),
                price: s
                    .prices
                    .get(coin)
                    .ok_or_else(|| anyhow::anyhow!("missing static price"))?
                    .parse()?,
                weight: s.weight,
                timestamp: now_ms(),
                status: s.market_status.clone(),
                bid: None,
                ask: None,
            });
        }
        let symbol = s.symbols.get(coin).map(String::as_str).unwrap_or(coin);
        // Encode path/query substitutions; provider credentials belong only in headers.
        let encoded: String = reqwest::Url::parse("https://unused.invalid")?
            .query_pairs_mut()
            .append_pair("s", symbol)
            .finish()
            .query()
            .unwrap()[2..]
            .to_string();
        let url = s
            .url
            .replace("{symbol}", &encoded)
            .replace("{coin}", &encoded);
        for attempt in 0..=s.max_retries {
            let mut gate = self.gates[name].lock().await;
            if let Some(last) = *gate {
                sleep(
                    Duration::from_millis(s.min_request_interval_ms).saturating_sub(last.elapsed()),
                )
                .await;
            }
            *gate = Some(Instant::now());
            let mut request = self.client.get(&url);
            for (k, v) in &s.headers {
                request = request.header(k, expand(v)?);
            }
            let response = request.send().await;
            let retry = match &response {
                Err(e) => e.is_timeout() || e.is_connect() || e.is_body(),
                Ok(r) => r.status().as_u16() == 429 || r.status().is_server_error(),
            };
            // Gate covers the full request; other feeds share this provider's budget.
            drop(gate);
            if retry && attempt < s.max_retries {
                sleep(Duration::from_millis(100 * (1u64 << attempt))).await;
                continue;
            }
            let raw =
                bounded_json(response.map_err(|_| anyhow::anyhow!("provider request failed"))?)
                    .await?;
            let price = decimal(extract(&raw, &s.price_path)?)?;
            let raw_time = extract(&raw, &s.timestamp_path)?;
            let time = if let Some(s) = raw_time.as_str() {
                s.parse::<u64>()?
            } else {
                raw_time
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("timestamp must be unsigned integer"))?
            };
            let timestamp = if s.timestamp_unit == "s" {
                time.checked_mul(1000)
                    .ok_or_else(|| anyhow::anyhow!("timestamp overflow"))?
            } else {
                time
            };
            let status = if let Some(path) = &s.market_status_path {
                extract(&raw, path)?
                    .as_str()
                    .unwrap_or("unknown")
                    .to_lowercase()
            } else {
                s.market_status.clone()
            };
            return Ok(Quote {
                source: name.into(),
                group: s.independence_group.clone(),
                price,
                weight: s.weight,
                timestamp,
                status,
                bid: s
                    .bid_path
                    .as_ref()
                    .map(|p| decimal(extract(&raw, p)?))
                    .transpose()?,
                ask: s
                    .ask_path
                    .as_ref()
                    .map(|p| decimal(extract(&raw, p)?))
                    .transpose()?,
            });
        }
        anyhow::bail!("source retry budget exhausted")
    }
    pub async fn collect(&self, c: &Config, coin: &str, names: &[String]) -> Vec<Quote> {
        stream::iter(names.iter().map(|name| async move {
            match timeout(
                Duration::from_millis(c.source_timeout_ms),
                self.quote(name, &c.sources[name], coin),
            )
            .await
            {
                Ok(Ok(q)) => Some(q),
                _ => {
                    tracing::warn!(source=%name,"source failed or timed out");
                    None
                }
            }
        }))
        .buffer_unordered(16)
        .filter_map(|q| async move { q })
        .collect()
        .await
    }
    pub async fn info(&self, c: &Config) -> Result<Snapshot> {
        let response = self
            .client
            .post(format!("{}/info", self.endpoint))
            .json(&json!({"type":"metaAndAssetCtxs","dex":c.dex}))
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("info transport failure"))?;
        parse_snapshot(&bounded_json(response).await?, c)
    }
    pub async fn exchange(&self, body: &Value) -> Result<Value> {
        let response = self
            .client
            .post(format!("{}/exchange", self.endpoint))
            .json(body)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("exchange result unknown"))?;
        bounded_json(response).await
    }
}
#[derive(Debug)]
pub struct Snapshot {
    pub prices: BTreeMap<String, String>,
    pub decimals: BTreeMap<String, u32>,
}
impl Snapshot {
    pub fn require_config(&self, c: &Config) -> Result<()> {
        let expected: BTreeMap<_, _> = c
            .feeds
            .iter()
            .map(|f| (c.wire(&f.coin), f.sz_decimals))
            .collect();
        ensure!(
            self.decimals == expected,
            "DEX universe or szDecimals mismatch"
        );
        Ok(())
    }
    pub fn matches(&self, prices: &BTreeMap<String, String>) -> Result<()> {
        ensure!(
            self.prices.len() == prices.len(),
            "readback universe mismatch"
        );
        for (coin, p) in prices {
            ensure!(
                self.prices
                    .get(coin)
                    .ok_or_else(|| anyhow::anyhow!("missing readback coin"))?
                    .parse::<Decimal>()?
                    == p.parse::<Decimal>()?,
                "oraclePx readback mismatch"
            );
        }
        Ok(())
    }
}
pub fn parse_snapshot(raw: &Value, c: &Config) -> Result<Snapshot> {
    let a = raw
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("invalid snapshot"))?;
    ensure!(a.len() == 2, "invalid snapshot length");
    let assets = a[0]["universe"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("invalid universe"))?;
    let contexts = a[1]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("invalid contexts"))?;
    ensure!(
        !assets.is_empty() && assets.len() == contexts.len(),
        "snapshot length mismatch"
    );
    let mut prices = BTreeMap::new();
    let mut decimals = BTreeMap::new();
    for (asset, ctx) in assets.iter().zip(contexts) {
        let name = asset["name"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing asset name"))?;
        ensure!(
            !name.contains(':') || name.starts_with(&format!("{}:", c.dex)),
            "wrong dex namespace"
        );
        let coin = c.wire(name);
        let sz = asset["szDecimals"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("invalid decimals"))?;
        ensure!(
            sz <= 6 && !decimals.contains_key(&coin),
            "invalid/duplicate asset"
        );
        let p = decimal(&ctx["oraclePx"])?;
        ensure!(valid_price(p), "invalid oraclePx");
        decimals.insert(coin.clone(), sz as u32);
        prices.insert(coin, p.normalize().to_string());
    }
    Ok(Snapshot { prices, decimals })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, extract::State, http::StatusCode, response::Redirect, routing::get};
    use std::sync::atomic::{AtomicUsize, Ordering};
    async fn source_server() -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let router=Router::new().route("/quote",get(|| async {axum::Json(json!({"price":"100.25","time":1234567890123u64,"bid":"100.2","ask":"100.3"}))}))
            .route("/retry",get(|State(c):State<Arc<AtomicUsize>>|async move {
                if c.fetch_add(1,Ordering::Relaxed)==0 {(StatusCode::TOO_MANY_REQUESTS,axum::Json(json!({})))} else {(StatusCode::OK,axum::Json(json!({"price":"100","time":1234567890123u64})))}
            }))
            .route("/redirect",get(||async {Redirect::temporary("/quote")}))
            .route("/slow",get(||async {sleep(Duration::from_secs(1)).await;axum::Json(json!({}))}))
            .with_state(calls.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (format!("http://{address}"), calls, server)
    }
    #[tokio::test]
    async fn rest_parsing_retry_redirect_and_timeout() {
        let (url, calls, server) = source_server().await;
        let mut c = Config::load(std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/config/public-btc.dry-run.json"
        )))
        .unwrap();
        let mut source = c.sources["binance"].clone();
        source.url = format!("{url}/quote");
        source.price_path = "price".into();
        source.timestamp_path = "time".into();
        source.bid_path = Some("bid".into());
        source.ask_path = Some("ask".into());
        let http = Http::new(&c).unwrap();
        let q = http.quote("binance", &source, "BTC").await.unwrap();
        assert_eq!(q.price, "100.25".parse::<Decimal>().unwrap());
        assert_eq!(q.timestamp, 1234567890123);
        source.url = format!("{url}/retry");
        source.bid_path = None;
        source.ask_path = None;
        source.min_request_interval_ms = 0;
        assert!(http.quote("binance", &source, "BTC").await.is_ok());
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        source.url = format!("{url}/redirect");
        assert!(http.quote("binance", &source, "BTC").await.is_err());
        source.url = format!("{url}/slow");
        c.source_timeout_ms = 20;
        c.sources.insert("binance".into(), source);
        assert!(
            http.collect(&c, "BTC", &["binance".into()])
                .await
                .is_empty()
        );
        server.abort();
    }
    #[test]
    fn field_paths_and_decimals_preserve_exact_input() {
        let v: Value =
            serde_json::from_str(r#"{"data":[{"a/b":"0.1234567890123456789012345678"}]}"#).unwrap();
        assert_eq!(
            decimal(extract(&v, "/data/0/a~1b").unwrap())
                .unwrap()
                .to_string(),
            "0.1234567890123456789012345678"
        );
        assert!(extract(&v, "data.1.price").is_err());
        assert!(decimal(&json!(true)).is_err());
    }
}
