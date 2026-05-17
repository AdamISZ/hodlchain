//! Config types shared by the wallet and the daemons.

use bitcoin::Network;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NetworkName {
    Bitcoin,
    Testnet,
    Signet,
    Regtest,
}

impl NetworkName {
    pub fn into_bitcoin(self) -> Network {
        match self {
            NetworkName::Bitcoin => Network::Bitcoin,
            NetworkName::Testnet => Network::Testnet,
            NetworkName::Signet => Network::Signet,
            NetworkName::Regtest => Network::Regtest,
        }
    }

    pub fn from_str_ci(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "bitcoin" | "mainnet" | "main" => NetworkName::Bitcoin,
            "testnet" | "test" => NetworkName::Testnet,
            "signet" => NetworkName::Signet,
            "regtest" => NetworkName::Regtest,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BitcoindConfig {
    pub url: String,
    pub auth: BitcoindAuth,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BitcoindAuth {
    Cookie { path: PathBuf },
    UserPass { user: String, password: String },
}
