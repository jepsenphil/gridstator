use anyhow::Result;
use gridstator::pki;
#[test]
fn fingerprint_input_forms_match_the_existing_workflow() -> Result<()> {
    let hex = "af".repeat(32);
    let expected = format!("sha256:{hex}");
    let colon = (0..32).map(|_| "AF").collect::<Vec<_>>().join(":");
    for input in [hex.clone(), hex.to_uppercase(), format!(" SHA256:{colon} ")] {
        assert_eq!(pki::normalize_fingerprint(&input)?, expected);
    }
    assert!(pki::normalize_fingerprint("sha256:bad").is_err());
    Ok(())
}
#[test]
fn bootstrap_cannot_append_an_unpinned_certificate_authority() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let temp = tempfile::tempdir()?;
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    let expected = pki::generate(&first, "trusted", &[])?;
    pki::generate(&second, "attacker", &[])?;
    let trusted = std::fs::read_to_string(first.join("pki/ca.pem"))?;
    let attacker = std::fs::read_to_string(second.join("pki/ca.pem"))?;
    assert_eq!(pki::fingerprint(&trusted)?, expected);
    pki::http_client(&trusted)?;
    pki::websocket_tls(&trusted)?;
    for poisoned in [
        format!("{trusted}{attacker}"),
        format!("{trusted}{trusted}"),
    ] {
        assert!(pki::fingerprint(&poisoned).is_err());
        assert!(pki::http_client(&poisoned).is_err());
        assert!(pki::websocket_tls(&poisoned).is_err());
    }
    Ok(())
}
