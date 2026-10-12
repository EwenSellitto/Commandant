//! The server's TLS identity: a self-signed certificate, made once and kept
//! in the database, so the pin in every link stays good across restarts.

use anyhow::{Context, Result};
use commandant_common::link::Pin;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use tonic::transport::{Identity, ServerTlsConfig};

use crate::store::Store;

/// What the certificate is issued to. Clients check its pin, not its name.
const NAME: &str = "commandant";

pub struct TlsIdentity {
    certificate: String,
    key: String,
    pin: Pin,
}

impl TlsIdentity {
    /// The one the database keeps, else a new one, kept from now on.
    pub async fn load_or_create(store: &Store) -> Result<Self> {
        let (certificate, key) = match store.tls_identity().await? {
            Some(kept) => kept,
            None => {
                let key = rcgen::KeyPair::generate()?;
                let params = rcgen::CertificateParams::new(vec![NAME.to_string()])?;
                let certificate = params.self_signed(&key)?.pem();
                let key = key.serialize_pem();
                store.keep_tls_identity(&certificate, &key).await?;
                (certificate, key)
            }
        };
        let der = CertificateDer::from_pem_slice(certificate.as_bytes())
            .context("the database's TLS certificate is unreadable")?;
        Ok(Self {
            pin: Pin::of(&der),
            certificate,
            key,
        })
    }

    /// What links carry, for clients and workers to check.
    pub fn pin(&self) -> Pin {
        self.pin
    }

    pub fn config(&self) -> ServerTlsConfig {
        ServerTlsConfig::new().identity(Identity::from_pem(&self.certificate, &self.key))
    }
}
