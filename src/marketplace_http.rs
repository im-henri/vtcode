//! HTTPS transport shared by extension and theme downloads.
use std::time::Duration;
use ureq::tls::{RootCerts, TlsConfig};

pub(crate) fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(5)
        .redirect_auth_headers(ureq::config::RedirectAuthHeaders::SameHost)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(Duration::from_secs(120)))
        .user_agent(concat!("VTCode/", env!("CARGO_PKG_VERSION")))
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .into()
}

pub(crate) fn certificate_message(error: &ureq::Error) -> Option<String> {
    let tls_error = match error {
        ureq::Error::Io(error) => error.get_ref()?.downcast_ref::<rustls::Error>()?,
        ureq::Error::Rustls(error) => error,
        _ => return None,
    };
    match tls_error {
        rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer) => Some(
            "Untrusted TLS certificate issuer. Check the system trust store for your VPN or proxy CA."
                .into(),
        ),
        rustls::Error::InvalidCertificate(_) => {
            Some(format!("TLS certificate validation failed ({tls_error})."))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marketplace_downloads_use_system_trust_and_require_https() {
        let agent = agent();
        let config = agent.config();
        assert!(config.https_only());
        let tls = config.tls_config();
        assert!(matches!(tls.root_certs(), RootCerts::PlatformVerifier));
        assert!(!tls.disable_verification());
    }
}
