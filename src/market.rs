use crate::config::Feed;
use anyhow::{Result, ensure};
use rust_decimal::{Decimal, RoundingStrategy};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, serde::Serialize)]
pub struct Quote {
    pub source: String,
    pub group: String,
    pub price: Decimal,
    pub weight: Decimal,
    pub timestamp: u64,
    pub status: String,
    pub bid: Option<Decimal>,
    pub ask: Option<Decimal>,
}
#[derive(Debug, serde::Serialize)]
pub struct Aggregate {
    pub price: Decimal,
    pub timestamp: u64,
    pub confidence_bps: Decimal,
    pub sources: usize,
    pub groups: usize,
    pub rejected: Vec<String>,
}
pub fn valid_price(p: Decimal) -> bool {
    p >= Decimal::new(1, 12) && p <= Decimal::from(1_000_000_000_000u64)
}
fn quantile(items: &[(Decimal, Decimal)], q: Decimal) -> Decimal {
    let mut sorted = items.to_vec();
    sorted.sort_by_key(|x| x.0);
    let target = sorted.iter().map(|x| x.1).sum::<Decimal>() * q;
    let mut sum = Decimal::ZERO;
    for (p, w) in &sorted {
        sum += w;
        if sum >= target {
            return *p;
        }
    }
    sorted.last().expect("nonempty quantile").0
}
fn median(items: &[(Decimal, Decimal)]) -> Decimal {
    quantile(items, Decimal::new(5, 1))
}
fn quorum(f: &Feed, quotes: &[Quote]) -> Result<()> {
    ensure!(quotes.len() >= f.min_sources, "insufficient source quorum");
    ensure!(
        quotes
            .iter()
            .map(|q| &q.group)
            .collect::<BTreeSet<_>>()
            .len()
            >= f.min_independent_groups,
        "insufficient independent group quorum"
    );
    Ok(())
}
fn balance(f: &Feed, groups: &BTreeMap<String, (Decimal, Decimal)>) -> Result<()> {
    let sum: Decimal = groups.values().map(|v| v.1).sum();
    ensure!(
        groups
            .values()
            .all(|v| v.1 * Decimal::from(10000) <= sum * f.max_group_weight_share_bps),
        "group concentration exceeds cap"
    );
    Ok(())
}
pub fn fresh(f: &Feed, timestamp: u64, now: u64) -> bool {
    timestamp <= now.saturating_add(f.max_future_ms)
        && now.saturating_sub(timestamp) <= f.max_source_age_ms
}
pub fn aggregate(f: &Feed, input: Vec<Quote>, now: u64) -> Result<Aggregate> {
    let mut seen = BTreeSet::new();
    let mut rejected = vec![];
    let mut quotes: Vec<_> = input
        .into_iter()
        .filter(|q| {
            let valid = f.sources.contains(&q.source)
                && seen.insert(q.source.clone())
                && valid_price(q.price)
                && q.weight > Decimal::ZERO
                && q.weight <= Decimal::from(1000000)
                && fresh(f, q.timestamp, now)
                && match (q.bid, q.ask) {
                    (None, None) => true,
                    (Some(b), Some(a)) => {
                        valid_price(b)
                            && valid_price(a)
                            && b <= a
                            && (a - b) * Decimal::from(10000) / ((a + b) / Decimal::from(2))
                                <= f.max_spread_bps
                    }
                    _ => false,
                };
            if !valid {
                rejected.push(q.source.clone());
            }
            valid
        })
        .collect();
    quorum(f, &quotes)?;
    let mut members = BTreeMap::<String, Vec<&Quote>>::new();
    for q in &quotes {
        members.entry(q.group.clone()).or_default().push(q);
    }
    let mut groups: BTreeMap<_, _> = members
        .into_iter()
        .map(|(g, qs)| {
            let pairs: Vec<_> = qs.iter().map(|q| (q.price, q.weight)).collect();
            (
                g,
                (median(&pairs), qs.iter().map(|q| q.weight).max().unwrap()),
            )
        })
        .collect();
    balance(f, &groups)?;
    let pairs: Vec<_> = groups.values().copied().collect();
    let center = median(&pairs);
    let deviations: Vec<_> = pairs
        .iter()
        .map(|(p, w)| ((p - center).abs(), *w))
        .collect();
    let band = (median(&deviations) * f.outlier_mad_multiplier)
        .max(center * f.outlier_min_band_bps / Decimal::from(10000));
    groups.retain(|_, (p, _)| (*p - center).abs() <= band);
    quotes.retain(|q| {
        let keep = groups.contains_key(&q.group);
        if !keep {
            rejected.push(q.source.clone());
        }
        keep
    });
    quorum(f, &quotes)?;
    balance(f, &groups)?;
    let pairs: Vec<_> = groups.values().copied().collect();
    let price = median(&pairs);
    let confidence = (price - quantile(&pairs, Decimal::new(25, 2)))
        .abs()
        .max((quantile(&pairs, Decimal::new(75, 2)) - price).abs())
        * Decimal::from(10000)
        / price;
    ensure!(
        confidence <= f.max_confidence_bps,
        "confidence exceeds limit"
    );
    let statuses: BTreeSet<_> = quotes.iter().map(|q| q.status.as_str()).collect();
    let status = if statuses.len() == 1 {
        *statuses.first().unwrap()
    } else {
        "unknown"
    };
    ensure!(
        !(f.block_when_market_closed && status == "closed"),
        "market closed"
    );
    ensure!(
        !f.block_when_market_unknown || ["open", "closed"].contains(&status),
        "market unknown"
    );
    Ok(Aggregate {
        price,
        timestamp: quotes.iter().map(|q| q.timestamp).min().unwrap(),
        confidence_bps: confidence,
        sources: quotes.len(),
        groups: groups.len(),
        rejected,
    })
}
pub fn format_price(value: Decimal, sz: u32) -> Result<String> {
    ensure!(valid_price(value) && sz <= 6, "invalid price/precision");
    if value.fract().is_zero() {
        return Ok(value.normalize().to_string());
    }
    let mut adjusted = 0i32;
    let mut scaled = value;
    while scaled >= Decimal::TEN {
        scaled /= Decimal::TEN;
        adjusted += 1;
    }
    while scaled < Decimal::ONE {
        scaled *= Decimal::TEN;
        adjusted -= 1;
    }
    let exponent = (adjusted - 4).max(sz as i32 - 6);
    let rounded = if exponent > 0 {
        let unit = Decimal::from(10u64.pow(exponent as u32));
        (value / unit).round_dp_with_strategy(0, RoundingStrategy::MidpointNearestEven) * unit
    } else {
        value.round_dp_with_strategy((-exponent) as u32, RoundingStrategy::MidpointNearestEven)
    };
    ensure!(rounded > Decimal::ZERO, "price rounds to zero");
    Ok(rounded.normalize().to_string())
}
pub fn check_jump(f: &Feed, price: Decimal, previous: Decimal) -> Result<()> {
    ensure!(valid_price(previous), "invalid reference price");
    ensure!(
        (price - previous).abs() * Decimal::from(10000) <= previous * f.max_jump_bps,
        "jump circuit breaker"
    );
    Ok(())
}
