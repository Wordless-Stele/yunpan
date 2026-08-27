//! 端到端：在本机环回地址上起一个真实中继、一个假 dufs（回显服务器）、一个隧道客户端，
//! 从「公网」端口进去的字节要原样从假 dufs 回来。协议实现的任何一环断了，这条都过不了。

use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use yunpan_tunnel::{ClientAuth, ClientConfig, Relay, RelayConfig, RelayStatus};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

/// 找一个空闲端口。bind 到 0 再读回来，比写死端口稳。
async fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    l.local_addr().unwrap().port()
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
    let control_port = free_port().await;
    let remote_port = free_port().await;
    let local_port = free_port().await;

    spawn_echo(local_port).await;

    let relay = Arc::new(Relay::new(RelayConfig {
        bind: "127.0.0.1".parse().unwrap(),
        control_port,
        heartbeat_secs: 2,
        public_host: None,
        clients: vec![ClientAuth {
            id: "test".into(),
            token: TOKEN.into(),
            ports: vec![remote_port],
        }],
    }));
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
    let control_port = free_port().await;
    let relay = Arc::new(Relay::new(RelayConfig {
        bind: "127.0.0.1".parse().unwrap(),
        control_port,
        heartbeat_secs: 2,
        public_host: None,
        clients: vec![ClientAuth {
            id: "test".into(),
            token: TOKEN.into(),
            ports: vec![18080],
        }],
    }));
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
