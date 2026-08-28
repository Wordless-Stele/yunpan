//! 端到端：在本机环回地址上起一个真实中继、一个假 dufs（回显服务器）、一个隧道客户端，
//! 从「公网」端口进去的字节要原样从假 dufs 回来。协议实现的任何一环断了，这条都过不了。

use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use yunpan_tunnel::{ClientAuth, ClientConfig, Relay, RelayConfig, RelayStatus};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

/// 挑一个可用端口。不用「bind 0 读回端口再释放」——workspace 并发跑多个测试
/// 进程时，释放到复用的空当里内核会把同一个端口分给别人（实测撞过）。
/// 改成 PID 加盐的候选序列 + 探测循环：各进程各扫各的序列，互不相撞；
/// 候选压在 49152 以下，内核给出站连接随机分配的临时端口也永远撞不进来。
fn free_port() -> u16 {
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    loop {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let candidate = (17000 + (std::process::id().wrapping_mul(61) + n * 13) % 30000) as u16;
        if TcpListener::bind(("127.0.0.1", candidate)).is_ok() {
            return candidate;
        }
    }
}

/// 假 dufs：收什么回什么，连接断了就算完。
async fn spawn_echo(port: u16) {
    let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else { break };
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    match s.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if s.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn 从公网端口进的字节原样从本地服务回来() {
    let control_port = free_port();
    let remote_port = free_port();
    let local_port = free_port();

    spawn_echo(local_port).await;

    let relay = Arc::new(Relay::new(RelayConfig {
        bind: "127.0.0.1".parse().unwrap(),
        control_port,
        heartbeat_secs: 2,
        public_host: None,
        tls_cert: None,
        tls_key: None,
        clients: vec![ClientAuth {
            id: "test".into(),
            token: TOKEN.into(),
            ports: vec![remote_port],
        }],
    }).unwrap());
    tokio::spawn(relay.run());
    tokio::time::sleep(Duration::from_millis(100)).await;

    let handle = yunpan_tunnel::client::spawn(ClientConfig {
        relay_host: "127.0.0.1".into(),
        control_port,
        client_id: "test".into(),
        token: TOKEN.into(),
        name: "echo".into(),
        remote_port,
        local_port,
        tls: false,
        extra_trust_der: None,
    });

    // 等隧道上线
    let mut status = handle.status.clone();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(&*status.borrow(), RelayStatus::Online { .. }) {
                break;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .expect("5 秒内隧道没能上线");

    // 访客从「公网」端口进来发一段话
    let mut visitor = TcpStream::connect(("127.0.0.1", remote_port)).await.unwrap();
    let payload = "云链盘穿透测试 hello relay".as_bytes();
    visitor.write_all(payload).await.unwrap();

    let mut got = vec![0u8; payload.len()];
    tokio::time::timeout(Duration::from_secs(5), visitor.read_exact(&mut got))
        .await
        .expect("5 秒内没收到回显")
        .unwrap();
    assert_eq!(got, payload, "回显内容变了样");

    handle.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn 令牌错误的客户端会停在_fatal_而不是无限重试() {
    let control_port = free_port();
    let relay = Arc::new(Relay::new(RelayConfig {
        bind: "127.0.0.1".parse().unwrap(),
        control_port,
        heartbeat_secs: 2,
        public_host: None,
        tls_cert: None,
        tls_key: None,
        clients: vec![ClientAuth {
            id: "test".into(),
            token: TOKEN.into(),
            ports: vec![18080],
        }],
    }).unwrap());
    tokio::spawn(relay.run());
    tokio::time::sleep(Duration::from_millis(100)).await;

    let handle = yunpan_tunnel::client::spawn(ClientConfig {
        relay_host: "127.0.0.1".into(),
        control_port,
        client_id: "test".into(),
        token: "完全不对的令牌完全不对的令牌".into(),
        name: "echo".into(),
        remote_port: 18080,
        local_port: 18081,
        tls: false,
        extra_trust_der: None,
    });

    let mut status = handle.status.clone();
    let fatal = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let RelayStatus::Fatal { reason } = &*status.borrow() {
                break reason.clone();
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .expect("5 秒内没有进入 Fatal 状态");
    assert!(fatal.contains("令牌") || fatal.contains("拒绝"), "报错文案：{fatal}");

    handle.stop().await;
}

/// TLS 全链路：中继用自签证书（域名 localhost），客户端把它设为额外信任锚，
/// 访客也走 TLS 连公网端口。任何一段没真正套上加密，这条都过不了。
#[tokio::test(flavor = "multi_thread")]
async fn 开了_tls_后三段连接都加密且字节原样往返() {
    use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName};
    use tokio_rustls::rustls::{ClientConfig as RustlsClientConfig, RootCertStore};

    let control_port = free_port();
    let remote_port = free_port();
    let local_port = free_port();
    spawn_echo(local_port).await;

    // 自签证书写进临时文件，喂给中继
    let signed = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let dir = std::env::temp_dir().join(format!("yunpan-tls-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    std::fs::write(&cert_path, signed.cert.pem()).unwrap();
    std::fs::write(&key_path, signed.key_pair.serialize_pem()).unwrap();
    let cert_der = signed.cert.der().to_vec();

    let relay = Arc::new(Relay::new(RelayConfig {
        bind: "127.0.0.1".parse().unwrap(),
        control_port,
        heartbeat_secs: 2,
        public_host: None,
        tls_cert: Some(cert_path),
        tls_key: Some(key_path),
        clients: vec![ClientAuth {
            id: "test".into(),
            token: TOKEN.into(),
            ports: vec![remote_port],
        }],
    }).unwrap());
    tokio::spawn(relay.run());
    tokio::time::sleep(Duration::from_millis(100)).await;

    let handle = yunpan_tunnel::client::spawn(ClientConfig {
        relay_host: "localhost".into(),
        control_port,
        client_id: "test".into(),
        token: TOKEN.into(),
        name: "echo".into(),
        remote_port,
        local_port,
        tls: true,
        extra_trust_der: Some(cert_der.clone()),
    });

    let mut status = handle.status.clone();
    let url = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let RelayStatus::Online { public_url } = &*status.borrow() {
                break public_url.clone();
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .expect("5 秒内隧道没能上线");
    assert!(url.starts_with("https://"), "公网地址应当是 https，实际：{url}");

    // 访客端也走 TLS 连「公网」端口
    let mut roots = RootCertStore::empty();
    roots.add(CertificateDer::from(cert_der)).unwrap();
    let connector = tokio_rustls::TlsConnector::from(std::sync::Arc::new(
        RustlsClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ));
    let tcp = TcpStream::connect(("127.0.0.1", remote_port)).await.unwrap();
    let mut visitor = connector
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .expect("访客 TLS 握手失败");

    let payload = "TLS 全链路测试 hello".as_bytes();
    visitor.write_all(payload).await.unwrap();
    let mut got = vec![0u8; payload.len()];
    tokio::time::timeout(Duration::from_secs(5), visitor.read_exact(&mut got))
        .await
        .expect("5 秒内没收到回显")
        .unwrap();
    assert_eq!(got, payload);

    handle.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// 一台中继带两个客户端：各占各的公网端口、各回各的本地服务，互不串门。
/// 「中继只有一个、客户端有多个」就是靠 clients 表 + 端口分流撑起来的，这条钉死它。
#[tokio::test(flavor = "multi_thread")]
async fn 一台中继两个客户端各走各的端口互不串() {
    let control_port = free_port();
    let (port_a, port_b) = (free_port(), free_port());
    let (local_a, local_b) = (free_port(), free_port());

    // 两个本地服务回声内容不同，串没串一验便知
    async fn spawn_tagged_echo(port: u16, tag: &'static str) {
        let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = listener.accept().await else { break };
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    while let Ok(n) = s.read(&mut buf).await {
                        if n == 0 { break }
                        let reply = format!("{tag}:{}", String::from_utf8_lossy(&buf[..n]));
                        if s.write_all(reply.as_bytes()).await.is_err() { break }
                    }
                });
            }
        });
    }
    spawn_tagged_echo(local_a, "甲").await;
    spawn_tagged_echo(local_b, "乙").await;

    let relay = Arc::new(Relay::new(RelayConfig {
        bind: "127.0.0.1".parse().unwrap(),
        control_port,
        heartbeat_secs: 2,
        public_host: None,
        tls_cert: None,
        tls_key: None,
        clients: vec![
            ClientAuth { id: "jia".into(), token: TOKEN.into(), ports: vec![port_a] },
            ClientAuth { id: "yi".into(), token: "fedcba9876543210fedcba9876543210".into(), ports: vec![port_b] },
        ],
    }).unwrap());
    tokio::spawn(relay.run());
    tokio::time::sleep(Duration::from_millis(100)).await;

    let make = |id: &str, token: &str, remote_port: u16, local_port: u16| ClientConfig {
        relay_host: "127.0.0.1".into(),
        control_port,
        client_id: id.into(),
        token: token.into(),
        name: "dufs".into(),
        remote_port,
        local_port,
        tls: false,
        extra_trust_der: None,
    };
    let h_a = yunpan_tunnel::client::spawn(make("jia", TOKEN, port_a, local_a));
    let h_b = yunpan_tunnel::client::spawn(make("yi", "fedcba9876543210fedcba9876543210", port_b, local_b));

    for handle in [&h_a, &h_b] {
        let mut status = handle.status.clone();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if matches!(&*status.borrow(), RelayStatus::Online { .. }) { break }
                status.changed().await.unwrap();
            }
        }).await.expect("5 秒内没能上线");
    }

    // 各打各的公网端口，回声标签必须对得上
    async fn ask(port: u16, msg: &str) -> String {
        let mut v = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        v.write_all(msg.as_bytes()).await.unwrap();
        let mut buf = [0u8; 256];
        let n = tokio::time::timeout(Duration::from_secs(5), v.read(&mut buf))
            .await.expect("5 秒没回声").unwrap();
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
    assert_eq!(ask(port_a, "ping").await, "甲:ping", "A 端口串到别人家了");
    assert_eq!(ask(port_b, "ping").await, "乙:ping", "B 端口串到别人家了");

    // 乙的令牌去申请甲的端口：白名单必须拦下
    let h_rogue = yunpan_tunnel::client::spawn(make("yi", "fedcba9876543210fedcba9876543210", port_a, local_b));
    let mut status = h_rogue.status.clone();
    let fatal = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let RelayStatus::Fatal { reason } = &*status.borrow() { break reason.clone() }
            status.changed().await.unwrap();
        }
    }).await.expect("越权申请端口居然没被拦成 Fatal");
    assert!(fatal.contains("白名单"), "报错没说清原因：{fatal}");

    h_rogue.stop().await;
    h_a.stop().await;
    h_b.stop().await;
}
