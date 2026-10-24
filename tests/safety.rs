use hip3_oracle_rs::{
    config::Config,
    market::{Quote, aggregate, format_price},
    protocol::{Action, Signer, action_hash},
    state::Store,
    transport::parse_snapshot,
};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};
fn config() -> Config {
    Config::load(Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/config/example.json"
    )))
    .unwrap()
}
fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}
fn quotes(prices: &[&str]) -> Vec<Quote> {
    ["venue-a", "venue-b", "venue-c", "bad-outlier"]
        .iter()
        .zip(prices)
        .map(|(name, p)| Quote {
            source: (*name).into(),
            group: (*name).into(),
            price: d(p),
            weight: Decimal::ONE,
            timestamp: 10000,
            status: "open".into(),
            bid: None,
            ask: None,
        })
        .collect()
}
#[test]
fn official_sdk_signature_vectors() {
    let fixtures: Vec<Value> = serde_json::from_str(include_str!("sdk_vectors.json")).unwrap();
    let signer = Signer::from_bytes(&[1; 32]).unwrap();
    for v in fixtures {
        let action: Action = serde_json::from_value(v["action"].clone()).unwrap();
        let nonce = v["nonce"].as_u64().unwrap();
        assert_eq!(
            hex::encode(action_hash(&action, nonce).unwrap()),
            v["hash"].as_str().unwrap()
        );
        let sig = signer
            .sign(&action, nonce, v["mainnet"].as_bool().unwrap())
            .unwrap();
        // Python emits integers with no leading zero padding.
        let actual = serde_json::to_value(sig).unwrap();
        for k in ["r", "s"] {
            assert_eq!(
                actual[k]
                    .as_str()
                    .unwrap()
                    .trim_start_matches("0x")
                    .trim_start_matches('0'),
                v["signature"][k]
                    .as_str()
                    .unwrap()
                    .trim_start_matches("0x")
                    .trim_start_matches('0')
            );
        }
        assert_eq!(actual["v"], v["signature"]["v"]);
    }
}
#[test]
fn formatting_obeys_wire_limits() {
    for (value, sz, expected) in [
        ("230.185", 2, "230.18"),
        ("0.000123456", 0, "0.000123"),
        ("123456", 6, "123456"),
        ("123456.7", 0, "123460"),
        ("1.23455", 2, "1.2346"),
    ] {
        assert_eq!(format_price(d(value), sz).unwrap(), expected);
    }
    assert!(format_price(d("0.00000001"), 6).is_err());
    assert!(format_price(Decimal::ZERO, 2).is_err());
}
#[test]
fn outlier_quorum_and_staleness() {
    let c = config();
    let f = &c.feeds[0];
    let a = aggregate(f, quotes(&["230.18", "230.20", "230.22", "500"]), 10000).unwrap();
    assert_eq!(a.price, d("230.20"));
    assert_eq!(a.sources, 3);
    assert_eq!(a.rejected, vec!["bad-outlier"]);
    assert!(aggregate(f, quotes(&["230", "230", "500"]), 10000).is_err());
    assert!(aggregate(f, quotes(&["230", "230", "230"]), 20000).is_err());
    let mut qs = quotes(&["230", "230", "230"]);
    qs[0].timestamp = 12000;
    assert!(aggregate(f, qs, 10000).is_err());
}
#[test]
fn groups_cannot_gain_votes_by_duplicate_endpoints() {
    let c = config();
    let f = &c.feeds[0];
    let mut q = quotes(&["230", "230", "230", "230"]);
    q[1].group = q[0].group.clone();
    q[2].group = q[0].group.clone();
    assert!(aggregate(f, q, 10000).is_err());
    let mut q = quotes(&["230", "230", "230", "230"]);
    q[0].weight = d("3");
    assert!(aggregate(f, q, 10000).is_err());
    let mut q = quotes(&["230", "230", "230"]);
    q[0].ask = Some(d("250"));
    q[0].bid = Some(d("200"));
    assert!(aggregate(f, q, 10000).is_err());
}
#[test]
fn snapshot_requires_exact_complete_universe() {
    let c = config();
    let s = parse_snapshot(
        &json!([{"universe":[{"name":"demo:AAPL","szDecimals":2}]},[{"oraclePx":"230.20"}]]),
        &c,
    )
    .unwrap();
    s.require_config(&c).unwrap();
    s.matches(&BTreeMap::from([("demo:AAPL".into(), "230.2".into())]))
        .unwrap();
    assert!(s.matches(&BTreeMap::new()).is_err());
    assert!(
        parse_snapshot(
            &json!([{"universe":[{"name":"AAPL","szDecimals":2}]},[]]),
            &c
        )
        .is_err()
    );
}
#[test]
fn durable_state_scope_lock_and_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = config();
    c.state_file = dir.path().join("state.json");
    let store = Store::new(&c);
    let guard = store.lock().unwrap();
    assert!(store.lock().is_err());
    let mut s = store.load().unwrap();
    s.pause = Some("test".into());
    store.save(&s).unwrap();
    assert_eq!(store.load().unwrap().pause.as_deref(), Some("test"));
    c.network = "mainnet".into();
    assert!(Store::new(&c).load().is_err());
    drop(guard);
    assert!(store.lock().is_ok());
    std::fs::write(&store.path, b"truncated").unwrap();
    assert!(store.load().is_err());
}
#[test]
fn config_rejects_unsafe_inputs() {
    let mut c = config();
    c.dry_run = false;
    assert!(c.validate().is_err());
    let mut c = config();
    c.feeds.push(c.feeds[0].clone());
    assert!(c.validate().is_err());
    let mut c = config();
    c.interval_ms = 2499;
    assert!(c.validate().is_err());
    let mut c = config();
    c.feeds[0].max_group_weight_share_bps = d("5000");
    assert!(c.validate().is_err());
}
