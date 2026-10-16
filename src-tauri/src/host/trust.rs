//! Trust authority has its own service; provider account references cannot read it.
use crate::core::trust::{TrustBackend, TrustError};

pub const SERVICE: &str = "codewave.yangshifu.xyz.trust.v1";

pub struct SystemTrustBackend;

impl TrustBackend for SystemTrustBackend {
    fn read(&self, account: &str) -> Result<Option<String>, TrustError> {
        let entry = keyring::Entry::new(SERVICE, account).map_err(|_| TrustError::Unavailable)?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(TrustError::Unavailable),
        }
    }

    fn write(&self, account: &str, value: &str) -> Result<(), TrustError> {
        keyring::Entry::new(SERVICE, account)
            .and_then(|entry| entry.set_password(value))
            .map_err(|_| TrustError::Unavailable)
    }
}
