use crate::{config::Config, market::valid_price, protocol::Action};
use anyhow::{Result, ensure};
use fs2::FileExt;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pending {
    pub nonce: u64,
    pub action: Action,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub version: u32,
    pub scope: String,
    pub prices: BTreeMap<String, String>,
    pub last_nonce: u64,
    pub pending: Option<Pending>,
    pub pause: Option<String>,
    pub last_attempt_ms: u64,
}
pub struct Store {
    pub path: PathBuf,
    pub scope: String,
}
impl Store {
    pub fn new(c: &Config) -> Self {
        Self {
            path: c.state_file.clone(),
            scope: c.scope(),
        }
    }
    pub fn lock(&self) -> Result<File> {
        std::fs::create_dir_all(self.path.parent().unwrap_or(Path::new(".")))?;
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.path.with_extension("lock"))?;
        f.try_lock_exclusive()
            .map_err(|_| anyhow::anyhow!("another writer owns this state lock"))?;
        Ok(f)
    }
    pub fn load(&self) -> Result<State> {
        if !self.path.exists() {
            return Ok(State {
                version: 1,
                scope: self.scope.clone(),
                prices: BTreeMap::new(),
                last_nonce: 0,
                pending: None,
                pause: None,
                last_attempt_ms: 0,
            });
        }
        let bytes = std::fs::read(&self.path)?;
        ensure!(bytes.len() <= 2_000_000, "state too large");
        let s: State = serde_json::from_slice(&bytes)?;
        ensure!(
            s.version == 1 && s.scope == self.scope,
            "state schema or scope mismatch"
        );
        for p in s.prices.values() {
            ensure!(valid_price(p.parse::<Decimal>()?), "invalid saved price");
        }
        if let Some(p) = &s.pending {
            ensure!(
                p.nonce == s.last_nonce
                    && p.action.kind == "perpDeploy"
                    && !p.action.set_oracle.oracle_pxs.is_empty(),
                "invalid pending state"
            );
        }
        Ok(s)
    }
    pub fn save(&self, s: &State) -> Result<()> {
        let parent = self.path.parent().unwrap_or(Path::new("."));
        let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
        tmp.write_all(&serde_json::to_vec_pretty(s)?)?;
        tmp.as_file().sync_all()?;
        tmp.persist(&self.path).map_err(|e| e.error)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    }
    pub fn audit(&self, event: serde_json::Value) -> Result<()> {
        let path = self.path.with_extension("audit.jsonl");
        if std::fs::metadata(&path)
            .map(|m| m.len() > 10_000_000)
            .unwrap_or(false)
        {
            for i in (1..5).rev() {
                let old = path.with_extension(format!("jsonl.{i}"));
                let new = path.with_extension(format!("jsonl.{}", i + 1));
                if old.exists() {
                    std::fs::rename(old, new)?;
                }
            }
            std::fs::rename(&path, path.with_extension("jsonl.1"))?;
        }
        let mut f = OpenOptions::new().create(true).append(true).open(path)?;
        serde_json::to_writer(&mut f, &event)?;
        f.write_all(b"\n")?;
        f.sync_data()?;
        Ok(())
    }
}
