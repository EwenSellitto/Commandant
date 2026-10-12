//! TLS to an orchestrator whose certificate a link pins. There is no
//! certificate authority and no name to check: the server must show the
//! very certificate the link names, so nobody in between can pose as it.

use std::sync::Arc;

use commandant_common::link::Pin;
use hyper_util::rt::TokioIo;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms, ring};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tonic::transport::Uri;

/// What a server showing another certificate than its link's is told.
const NOT_PINNED: &str = "the server's certificate isn't the one its link names: something \
     else answered at its address, or it was reset and has a new link";

/// The name a connection asks for. The server has one certificate, and its
/// pin is what is checked, so the name doesn't matter.
const SERVER_NAME: &str = "commandant";

/// Connects to the URI's host over TLS, to a server showing the certificate
/// `pin` names.
pub(crate) fn connector(
    pin: Pin,
) -> impl tower::Service<
    Uri,
    Response = TokioIo<tokio_rustls::client::TlsStream<TcpStream>>,
    Error = std::io::Error,
    Future = impl Send,
> + Send
+ 'static {
    let tls = TlsConnector::from(Arc::new(config(pin)));
    tower::service_fn(move |uri: Uri| {
        let tls = tls.clone();
        async move {
            let host = uri.authority().map(|a| a.as_str().to_string());
            let host = host.ok_or_else(|| std::io::Error::other("no host to connect to"))?;
            let tcp = TcpStream::connect(host).await?;
            tcp.set_nodelay(true)?;
            let name = ServerName::try_from(SERVER_NAME).expect("a valid name");
            Ok(TokioIo::new(tls.connect(name, tcp).await?))
        }
    })
}

/// TLS 1.3, speaking HTTP/2, trusting only the pinned certificate.
fn config(pin: Pin) -> ClientConfig {
    let provider = Arc::new(ring::default_provider());
    let algorithms = provider.signature_verification_algorithms;
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("ring supports TLS 1.3")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned { pin, algorithms }))
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec()];
    config
}

/// Accepts the one certificate a link pins, and checks the server holds its
/// key.
#[derive(Debug)]
struct Pinned {
    pin: Pin,
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        certificate: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match Pin::of(certificate) == self.pin {
            true => Ok(ServerCertVerified::assertion()),
            false => Err(rustls::Error::General(NOT_PINNED.into())),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signed: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, certificate, signed, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signed: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, certificate, signed, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// The ring provider, for a server's TLS: installed once as the process's
/// default, which is what tonic's server uses.
pub fn install_provider() {
    let _ = CryptoProvider::install_default(ring::default_provider());
}
