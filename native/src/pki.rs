use anyhow::{Context, Result, ensure};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use sha2::{Digest, Sha256};
use std::{path::Path, sync::Arc};

fn pinned_certificate(pem: &str) -> Result<rustls::pki_types::CertificateDer<'static>> {
    let mut bytes = pem.as_bytes();
    let mut certificates = rustls_pemfile::certs(&mut bytes);
    let cert = certificates
        .next()
        .transpose()?
        .context("missing CA certificate")?;
    ensure!(
        certificates.next().transpose()?.is_none(),
        "expected exactly one pinned CA certificate"
    );
    Ok(cert)
}
pub fn fingerprint(pem: &str) -> Result<String> {
    let cert = pinned_certificate(pem)?;
    Ok(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(cert.as_ref()))
    ))
}
pub fn normalize_fingerprint(input: &str) -> Result<String> {
    let input = input.trim();
    let input = if input
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("sha256:"))
    {
        &input[7..]
    } else {
        input
    };
    let hex = input.replace(':', "").to_ascii_lowercase();
    ensure!(
        hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "BAD_REQUEST: invalid CA fingerprint (expected SHA-256)"
    );
    Ok(format!("sha256:{hex}"))
}
pub fn generate(dir: &Path, cluster_id: &str, extra: &[String]) -> Result<String> {
    crate::state::private_dir(&dir.join("pki"))?;
    let mut ca_params = CertificateParams::new(Vec::<String>::new())?;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    ca_params.distinguished_name.push(
        DnType::CommonName,
        format!("Gridstator Cluster CA {cluster_id}"),
    );
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let key = KeyPair::generate()?;
    let ca = ca_params.self_signed(&key)?;
    let pem = ca.pem();
    let issuer = Issuer::new(ca_params, key);
    let mut names = vec!["localhost".to_string(), "127.0.0.1".into(), "::1".into()];
    names.extend(
        extra
            .iter()
            .filter(|s| s.as_str() != "0.0.0.0" && s.as_str() != "::")
            .cloned(),
    );
    if let Some(host) = sysinfo::System::host_name() {
        names.push(host);
    }
    names.sort();
    names.dedup();
    let mut leaf_params = CertificateParams::new(names)?;
    leaf_params
        .distinguished_name
        .push(DnType::CommonName, "gridstator-controller");
    leaf_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let leaf_key = KeyPair::generate()?;
    let leaf = leaf_params.signed_by(&leaf_key, &issuer)?;
    crate::state::write_atomic(&dir.join("pki/ca.pem"), pem.as_bytes())?;
    crate::state::write_atomic(
        &dir.join("pki/ca.key"),
        issuer.key().serialize_pem().as_bytes(),
    )?;
    crate::state::write_atomic(
        &dir.join("pki/server.pem"),
        format!("{}{}", leaf.pem(), pem).as_bytes(),
    )?;
    crate::state::write_atomic(
        &dir.join("pki/server.key"),
        leaf_key.serialize_pem().as_bytes(),
    )?;
    fingerprint(&pem)
}

pub fn http_client(ca: &str) -> Result<reqwest::Client> {
    // The private pinned CA, rather than changing LAN DNS/IP names, is the trust anchor.
    Ok(reqwest::Client::builder()
        .tls_built_in_root_certs(false)
        .add_root_certificate(reqwest::Certificate::from_der(
            pinned_certificate(ca)?.as_ref(),
        )?)
        .danger_accept_invalid_hostnames(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?)
}
pub async fn fetch_pinned_ca(url: &str, expected: &str) -> Result<String> {
    let expected = normalize_fingerprint(expected)?;
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let mut response = client
        .get(format!("{}/v1/pki/ca.pem", url.trim_end_matches('/')))
        .send()
        .await?;
    ensure!(
        response.status().is_success(),
        "UNAVAILABLE: controller did not serve a cluster CA (HTTP {})",
        response.status().as_u16()
    );
    ensure!(
        response.content_length().unwrap_or(0) <= 1024 * 1024,
        "CA response too large"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= 1024 * 1024,
            "CA response too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    let pem =
        String::from_utf8(bytes).context("TLS_PIN_MISMATCH: invalid CA certificate encoding")?;
    let actual = fingerprint(&pem)
        .context("TLS_PIN_MISMATCH: controller returned an invalid CA certificate")?;
    ensure!(
        actual == expected,
        "UNAUTHORIZED: controller CA fingerprint does not match"
    );
    Ok(pem)
}

#[derive(Debug)]
struct LanVerifier(Arc<rustls::client::WebPkiServerVerifier>);
impl rustls::client::danger::ServerCertVerifier for LanVerifier {
    fn verify_server_cert(
        &self,
        end: &rustls::pki_types::CertificateDer<'_>,
        chain: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        ocsp: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        // Native cluster leaf certificates always contain localhost. Validate their chain,
        // validity, purpose and signatures against only the pinned CA at this stable name.
        self.0.verify_server_cert(
            end,
            chain,
            &rustls::pki_types::ServerName::try_from("localhost").unwrap(),
            ocsp,
            now,
        )
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls12_signature(message, cert, dss)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls13_signature(message, cert, dss)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.supported_verify_schemes()
    }
}
pub fn websocket_tls(ca: &str) -> Result<Arc<rustls::ClientConfig>> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(pinned_certificate(ca)?)?;
    let roots = Arc::new(roots);
    let verifier = rustls::client::WebPkiServerVerifier::builder(roots.clone()).build()?;
    let mut config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config
        .dangerous()
        .set_certificate_verifier(Arc::new(LanVerifier(verifier)));
    Ok(Arc::new(config))
}
