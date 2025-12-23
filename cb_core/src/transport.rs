// cb_core/src/transport.rs

mod cert;

use std::fmt::{Debug, Formatter};
use std::net::{SocketAddr, Ipv4Addr}; // Ipv6Addr 在 v1 暂不强制
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use quinn::{Endpoint, ServerConfig, TransportConfig, VarInt};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
pub use quinn::{Connection, RecvStream, SendStream};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use rustls::crypto::CryptoProvider;
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature};
use rustls::{ClientConfig as TlsClientConfig, ServerConfig as TlsServerConfig};



/// 自定义 TLS 验证器：接受任何服务器证书 (Blind Trust)
#[derive(Debug)]
struct BlindVerifier(Arc<CryptoProvider>);

impl BlindVerifier {
    fn new(provider: Arc<CryptoProvider>) -> Self {
        Self(provider)
    }
}

impl ServerCertVerifier for BlindVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}


pub struct Transport {
    endpoint: Endpoint,
    pub local_cert_der: Vec<u8>,
}

impl Transport {
    pub fn new(listen_port: u16) -> Result<Self> {
        // 1. 生成证书
        let (cert_chain, priv_key) = cert::generate_self_signed_cert("clipbridge")
            .context("failed to generate self-signed cert")?;

        let local_cert_der = cert_chain[0].as_ref().to_vec();


        // 2. rustls provider
        let crypto = Arc::new(rustls::crypto::ring::default_provider());

        // 3. Server TLS
        let priv_key_server = priv_key.clone_key(); // PrivateKeyDer 不 Clone，要用 clone_key :contentReference[oaicite:3]{index=3}
        let mut server_tls = TlsServerConfig::builder_with_provider(crypto.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .context("server: unsupported TLS versions")?
            .with_no_client_auth()
            .with_single_cert(cert_chain.clone(), priv_key_server)
            .context("invalid server cert")?;

        server_tls.alpn_protocols = vec![b"clipbridge-v1".to_vec()];


        let quic_server = QuicServerConfig::try_from(server_tls)
            .context("failed to build QuicServerConfig from rustls ServerConfig")?;
        let mut server_config = ServerConfig::with_crypto(Arc::new(quic_server));

        let mut transport_config = TransportConfig::default();
        transport_config.max_idle_timeout(Some(VarInt::from_u32(10_000).into()));
        transport_config.keep_alive_interval(Some(Duration::from_secs(2)));
        server_config.transport_config(Arc::new(transport_config));

        // 3. Client TLS
        let mut client_tls = TlsClientConfig::builder_with_provider(crypto.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .context("client: unsupported TLS versions")?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(BlindVerifier::new(crypto.clone())))
            .with_client_auth_cert(cert_chain, priv_key)
            .context("invalid client cert")?;

        client_tls.alpn_protocols = vec![b"clipbridge-v1".to_vec()];

        let quic_client = QuicClientConfig::try_from(client_tls)
            .context("failed to build QuicClientConfig from rustls ClientConfig")?;
        let client_config = quinn::ClientConfig::new(Arc::new(quic_client));


        // 4. Bind
        let addr = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), listen_port);
        let mut endpoint = Endpoint::server(server_config, addr)?;
        endpoint.set_default_client_config(client_config);

        Ok(Self {
            endpoint,
            local_cert_der,
        })
    }

    pub fn local_port(&self) -> Result<u16> {
        Ok(self.endpoint.local_addr()?.port())
    }

    pub async fn connect(&self, addr_str: &str) -> Result<Connection> {
        let addr: SocketAddr = addr_str.parse().context("invalid socket addr")?;
        // "localhost" 只是占位，BlindVerifier 会忽略
        let connecting = self.endpoint.connect(addr, "localhost")?;
        let conn = connecting.await?;
        Ok(conn)
    }

    pub async fn accept(&self) -> Option<Connection> {
        let incoming = self.endpoint.accept().await?;
        match incoming.await {
            Ok(conn) => Some(conn),
            Err(e) => {
                eprintln!("[Transport] Handshake failed: {}", e);
                None
            }
        }
    }

    pub fn is_ipv4(&self) -> bool {
        // 如果获取失败，默认认为是 IPv4（为了兼容性）
        self.endpoint.local_addr().map(|a| a.is_ipv4()).unwrap_or(true)
    }

    pub fn shutdown(&self) {
        self.endpoint.close(0u32.into(), b"shutdown");
    }
}