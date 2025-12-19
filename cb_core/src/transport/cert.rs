use anyhow::Result;
use rcgen::{generate_simple_self_signed, CertifiedKey};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

pub fn generate_self_signed_cert(
    common_name: &str,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let subject_alt_names = vec![common_name.to_string()];

    let CertifiedKey { cert, signing_key } =
        generate_simple_self_signed(subject_alt_names)?;

    let cert_chain = vec![CertificateDer::from(cert.der().to_vec())];

    // rcgen 的 signing_key 可以 serialize_der() 得到 PKCS#8 DER :contentReference[oaicite:1]{index=1}
    let key_der = PrivatePkcs8KeyDer::from(signing_key.serialize_der());
    let priv_key = PrivateKeyDer::from(key_der);

    Ok((cert_chain, priv_key))
}
