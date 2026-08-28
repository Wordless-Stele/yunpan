//! 隧道的 TLS 层。
//!
//! 中继配了证书（跟着它的域名，certbot 签）后，**三种连接全走 TLS**：
//! 访客→中继的 HTTPS、客户端→中继的控制连接、客户端→中继的数据连接。
//! 客户端用系统根证书验中继域名——所以客户端侧零证书文件，勾一个开关即可。
//!
//! 证书加载函数与 dufs-core 的 `utils.rs` 同一写法（rustls-pki-types 的 PEM 解析）。

use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use tokio_rustls::rustls::{ClientConfig as RustlsClientConfig, RootCertStore, ServerConfig};
use tokio_rustls::{TlsAcceptor, TlsConnector};

/// 隧道里流动的连接：可能是裸 TCP，也可能套了 TLS。装箱抹平类型差异，
/// 帧编解码与 `copy_bidirectional` 两边都只认这个。
pub trait Rw: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Rw for T {}
pub type BoxStream = Box<dyn Rw>;

/// 显式选定 ring 为进程级加密后端。
///
/// workspace 里 agent 的 reqwest 会把 rustls 的 aws-lc-rs 特性拉进统一特性图，
/// 与我们的 ring 并存——两个后端同时可用时 rustls 拒绝自动选择，谁先建
/// TLS 配置谁 panic（整仓 cargo test 必现，单测 crate 不现）。首次用到时装一次，
/// 已装过（无论谁装的）就沿用。
fn ensure_crypto_provider() {
    use tokio_rustls::rustls::crypto::{ring, CryptoProvider};
    if CryptoProvider::get_default().is_none() {
        // 并发时装晚了会 Err——说明别人已装好，同样是可用状态
        let _ = ring::default_provider().install_default();
    }
}

pub fn load_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let mut certs = vec![];
    for cert in CertificateDer::pem_file_iter(path)
        .with_context(|| format!("读取证书文件 `{}` 失败", path.display()))?
    {
        certs.push(cert.with_context(|| format!("证书文件 `{}` 内容非法", path.display()))?);
    }
    Ok(certs)
}

pub fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>> {
    PrivateKeyDer::from_pem_file(path)
        .with_context(|| format!("读取私钥文件 `{}` 失败", path.display()))
}

/// 中继侧：由证书+私钥构造 TLS 接受器。
pub fn make_acceptor(cert_path: &Path, key_path: &Path) -> Result<TlsAcceptor> {
    ensure_crypto_provider();
    let certs = load_certs(cert_path)?;
    let key = load_private_key(key_path)?;
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .with_context(|| "证书与私钥装配失败（两个文件是一对吗？）")?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// 客户端侧：系统根证书（webpki）+ 可选的额外信任锚（测试/自签中继用）。
pub fn make_connector(extra_trust_der: Option<&[u8]>) -> Result<TlsConnector> {
    ensure_crypto_provider();
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(der) = extra_trust_der {
        roots
            .add(CertificateDer::from(der.to_vec()))
            .with_context(|| "附加信任证书非法")?;
    }
    let config = RustlsClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

/// 域名/IP 转 rustls 的 ServerName（SNI 与证书校验都用它）。
pub fn server_name(host: &str) -> Result<ServerName<'static>> {
    ServerName::try_from(host.to_string())
        .with_context(|| format!("`{host}` 不是合法的域名或 IP"))
}
