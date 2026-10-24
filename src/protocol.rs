//! Native L1 signing. Struct field order is part of the MessagePack signing contract.
use anyhow::Result;
use k256::ecdsa::SigningKey;
use serde::{Deserialize, Serialize};
use sha3::{Digest, Keccak256};
use std::collections::BTreeMap;
use zeroize::Zeroizing;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetOracle {
    pub dex: String,
    pub oracle_pxs: Vec<(String, String)>,
    pub mark_pxs: Vec<Vec<(String, String)>>,
    pub external_perp_pxs: Vec<(String, String)>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Action {
    #[serde(rename = "type")]
    pub kind: String,
    pub set_oracle: SetOracle,
}
impl Action {
    pub fn new(dex: &str, prices: &BTreeMap<String, String>, marks: bool) -> Self {
        let pairs: Vec<_> = prices.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        Self {
            kind: "perpDeploy".into(),
            set_oracle: SetOracle {
                dex: dex.into(),
                oracle_pxs: pairs.clone(),
                mark_pxs: if marks { vec![pairs.clone()] } else { vec![] },
                external_perp_pxs: pairs,
            },
        }
    }
}
pub fn keccak(bytes: impl AsRef<[u8]>) -> [u8; 32] {
    Keccak256::digest(bytes).into()
}
pub fn action_hash(action: &Action, nonce: u64) -> Result<[u8; 32]> {
    let mut data = rmp_serde::to_vec_named(action)?;
    data.extend_from_slice(&nonce.to_be_bytes());
    data.push(0);
    Ok(keccak(data))
}
pub fn signing_digest(action: &Action, nonce: u64, mainnet: bool) -> Result<[u8; 32]> {
    let mut domain = vec![];
    domain.extend_from_slice(&keccak(
        b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
    ));
    domain.extend_from_slice(&keccak(b"Exchange"));
    domain.extend_from_slice(&keccak(b"1"));
    let mut chain = [0u8; 32];
    chain[24..].copy_from_slice(&1337u64.to_be_bytes());
    domain.extend_from_slice(&chain);
    domain.extend_from_slice(&[0u8; 32]);
    let mut agent = vec![];
    agent.extend_from_slice(&keccak(b"Agent(string source,bytes32 connectionId)"));
    agent.extend_from_slice(&keccak(if mainnet { b"a" } else { b"b" }));
    agent.extend_from_slice(&action_hash(action, nonce)?);
    let mut preimage = vec![0x19, 0x01];
    preimage.extend_from_slice(&keccak(domain));
    preimage.extend_from_slice(&keccak(agent));
    Ok(keccak(preimage))
}
#[derive(Debug, Serialize)]
pub struct Signature {
    pub r: String,
    pub s: String,
    pub v: u8,
}
pub struct Signer(SigningKey);
impl Signer {
    pub fn from_environment() -> Result<Self> {
        let key = Zeroizing::new(
            std::env::var("HIP3_PRIVATE_KEY")
                .map_err(|_| anyhow::anyhow!("HIP3_PRIVATE_KEY required"))?,
        );
        let bytes = Zeroizing::new(
            hex::decode(key.trim().trim_start_matches("0x"))
                .map_err(|_| anyhow::anyhow!("invalid private key encoding"))?,
        );
        Ok(Self(
            SigningKey::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid signing key"))?,
        ))
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        Ok(Self(SigningKey::from_slice(bytes)?))
    }
    pub fn sign(&self, action: &Action, nonce: u64, mainnet: bool) -> Result<Signature> {
        let (sig, id) = self
            .0
            .sign_prehash_recoverable(&signing_digest(action, nonce, mainnet)?)?;
        Ok(Signature {
            r: format!("0x{}", hex::encode(sig.r().to_bytes())),
            s: format!("0x{}", hex::encode(sig.s().to_bytes())),
            v: id.to_byte() + 27,
        })
    }
}
