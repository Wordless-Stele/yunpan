//! 中继侧（跑在 Linux 公网服务器上）。
//!
//! 职责就三件：验明客户端身份、按申请把公网端口指给它、把访客的连接与客户端
//! 回连的数据连接对接起来。不解析任何业务协议。
//!
//! 配了 `tls_cert`/`tls_key`（跟中继域名走的证书，certbot 签）后**全部端口都套 TLS**：
//! 访客那头是正经 HTTPS（浏览器认、无告警），客户端那头的控制/数据连接也是 TLS
//! （客户端用系统根证书验域名）。TLS 在中继终结——中继本来就是你自己的服务器，
//! 加密防的是路上的人，不是防它自己。

use crate::protocol::{
    read_msg, write_msg, ClientMsg, Role, ServerMsg, PAIR_TIMEOUT_SECS, PROTO_VERSION,
};
use crate::tls::{make_acceptor, BoxStream};
use anyhow::{bail, Context, Result};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch, Mutex};
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;

/// 握手阶段的耐心。连上来不说话的，10 秒后请走——否则慢速连接攻击能把中继的
/// 文件描述符占满。
const HANDSHAKE_TIMEOUT_SECS: u64 = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayConfig {
    /// 控制端口的绑定地址，默认 `0.0.0.0`。
    #[serde(default = "default_bind")]
    pub bind: IpAddr,
    /// 控制连接与数据连接共用的端口。防火墙上只需为它单独开一个口。
    #[serde(default = "default_control_port")]
    pub control_port: u16,
    /// 心跳间隔。连续三次没等到 Pong 就判定客户端掉线并释放它的端口。
    #[serde(default = "default_heartbeat")]
    pub heartbeat_secs: u64,
    /// 对外域名，仅用于日志里拼出人能点的地址，不参与任何判断。
    #[serde(default)]
    pub public_host: Option<String>,
    /// TLS 证书与私钥（PEM）。都给了就全端口加密；只给一半在启动时报错。
    #[serde(default)]
    pub tls_cert: Option<PathBuf>,
    #[serde(default)]
    pub tls_key: Option<PathBuf>,
    /// 访客端口的绑定地址，缺省与 `bind` 相同。访客流量经 nginx 反代时设成
    /// `127.0.0.1`——端口只对本机 nginx 可见，安全组一个访客端口都不用开。
    #[serde(default)]
    pub visitor_bind: Option<IpAddr>,
    pub clients: Vec<ClientAuth>,
}

fn default_bind() -> IpAddr {
    "0.0.0.0".parse().unwrap()
}
fn default_control_port() -> u16 {
    7100
}
fn default_heartbeat() -> u64 {
    15
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientAuth {
    pub id: String,
    pub token: String,
    /// 允许这个客户端申请的公网端口白名单。
    ///
    /// 不是摆设：没有白名单的话，任何一个拿到令牌的客户端都能把中继上的 22 或 443
    /// 抢过去（进程若以 root 跑更是直接劫持），一个客户端被攻破就等于整台服务器失守。
    pub ports: Vec<u16>,
    /// 访客实际访问的地址（如 `https://yp.yizuw.cn`）。经 nginx 反代时必填，
    /// 客户端界面与二维码都显示它；不填则客户端按端口自己拼。
    #[serde(default)]
    pub public_url: Option<String>,
}

/// 等待客户端回连接管的访客连接（可能已套 TLS，装箱抹平）。
struct Pending {
    stream: BoxStream,
    peer: SocketAddr,
    client_id: String,
    since: Instant,
}

/// 一个客户端当前的控制会话。
struct Session {
    shutdown_tx: watch::Sender<bool>,
    done_rx: watch::Receiver<bool>,
}

#[derive(Default)]
pub struct Stats {
    pub conns_total: AtomicU64,
    pub bytes_up: AtomicU64,
    pub bytes_down: AtomicU64,
}

pub struct Relay {
    cfg: RelayConfig,
    /// Some = 全端口 TLS；None = 裸 TCP（内网联调用）。
    acceptor: Option<TlsAcceptor>,
    pending: Mutex<HashMap<u64, Pending>>,
    sessions: Mutex<HashMap<String, Session>>,
    next_conn_id: AtomicU64,
    pub stats: Stats,
}

impl Relay {
    pub fn new(cfg: RelayConfig) -> Result<Self> {
        let acceptor = match (&cfg.tls_cert, &cfg.tls_key) {
            (Some(cert), Some(key)) => Some(make_acceptor(cert, key)?),
            (None, None) => None,
            _ => bail!("tls_cert 和 tls_key 要么都给要么都不给"),
        };
        Ok(Self {
            cfg,
            acceptor,
            pending: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            next_conn_id: AtomicU64::new(1),
            stats: Stats::default(),
        })
    }

    /// 按配置给刚 accept 的连接套（或不套）TLS。带超时：TLS 握手挂着不动的连接
    /// 不能占着文件描述符。
    async fn maybe_tls(&self, tcp: TcpStream) -> Result<BoxStream> {
        match &self.acceptor {
            Some(acceptor) => {
                let stream = timeout(
                    Duration::from_secs(HANDSHAKE_TIMEOUT_SECS),
                    acceptor.accept(tcp),
                )
                .await
                .with_context(|| "TLS 握手超时")?
                .with_context(|| "TLS 握手失败")?;
                Ok(Box::new(stream))
            }
            None => Ok(Box::new(tcp)),
        }
    }

    fn client(&self, id: &str) -> Option<&ClientAuth> {
        self.cfg.clients.iter().find(|c| c.id == id)
    }

    /// 跑起来，直到进程被杀。
    pub async fn run(self: Arc<Self>) -> Result<()> {
        let addr = SocketAddr::new(self.cfg.bind, self.cfg.control_port);
        let listener = TcpListener::bind(addr)
            .await
            .with_context(|| format!("绑定控制端口 {addr} 失败"))?;
        log::info!(
            "中继已启动，控制端口 {addr}（{}），已登记 {} 个客户端",
            if self.acceptor.is_some() { "TLS" } else { "明文" },
            self.cfg.clients.len()
        );

        {
            let relay = self.clone();
            tokio::spawn(async move { relay.sweep_pending().await });
        }

        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    log::warn!("接受控制连接失败：{e}");
                    continue;
                }
            };
            let relay = self.clone();
            tokio::spawn(async move {
                let result = async {
                    let stream = relay.maybe_tls(stream).await?;
                    relay.clone().handle_incoming(stream, peer).await
                }
                .await;
                if let Err(e) = result {
                    log::info!("来自 {peer} 的连接结束：{e}");
                }
            });
        }
    }

    /// 定期清掉没人来接的访客连接。没有这个循环，扫端口的机器人会让 `pending` 只涨不落。
    async fn sweep_pending(&self) {
        let mut ticker = tokio::time::interval(Duration::from_secs(2));
        loop {
            ticker.tick().await;
            let mut pending = self.pending.lock().await;
            let before = pending.len();
            pending.retain(|_, p| p.since.elapsed().as_secs() < PAIR_TIMEOUT_SECS);
            let dropped = before - pending.len();
            if dropped > 0 {
                log::warn!("{dropped} 个访客连接超时未被接管，已丢弃");
            }
        }
    }

    async fn handle_incoming(self: Arc<Self>, mut stream: BoxStream, peer: SocketAddr) -> Result<()> {
        let hello: ClientMsg = timeout(
            Duration::from_secs(HANDSHAKE_TIMEOUT_SECS),
            read_msg(&mut stream),
        )
        .await
        .with_context(|| "握手超时")??;

        let (proto, role, agent) = match hello {
            ClientMsg::Hello { proto, role, agent } => (proto, role, agent),
            other => bail!("第一帧应当是 Hello，收到的是 {other:?}"),
        };

        if proto != PROTO_VERSION {
            let _ = write_msg(
                &mut stream,
                &ServerMsg::AuthErr {
                    reason: format!("协议版本不一致：中继 {PROTO_VERSION}，客户端 {proto}"),
                },
            )
            .await;
            bail!("协议版本不一致");
        }

        // 身份不存在时也要走完整的挑战-应答，不能提前返回：否则「令牌错」与「客户端 ID
        // 不存在」的耗时差别会把有效 ID 探出来。
        let token = self.client(role.client_id()).map(|c| c.token.clone());

        let mut nonce = [0u8; 32];
        rand::rng().fill_bytes(&mut nonce);
        let nonce_hex = crate::protocol::hex_encode(&nonce);
        write_msg(
            &mut stream,
            &ServerMsg::Challenge {
                nonce: nonce_hex.clone(),
            },
        )
        .await?;

        let auth: ClientMsg = timeout(
            Duration::from_secs(HANDSHAKE_TIMEOUT_SECS),
            read_msg(&mut stream),
        )
        .await
        .with_context(|| "等待鉴权应答超时")??;
        let mac = match auth {
            ClientMsg::Auth { mac } => mac,
            other => bail!("这一帧应当是 Auth，收到的是 {other:?}"),
        };

        let ok = token
            .as_deref()
            .map(|t| crate::protocol::verify(t, proto, &role, &nonce_hex, &mac))
            .unwrap_or(false);
        if !ok {
            let _ = write_msg(
                &mut stream,
                &ServerMsg::AuthErr {
                    reason: "客户端 ID 或令牌不正确".into(),
                },
            )
            .await;
            log::warn!("{peer} 鉴权失败（声称身份 {}）", role.client_id());
            bail!("鉴权失败");
        }

        write_msg(
            &mut stream,
            &ServerMsg::AuthOk {
                server: format!("yunpan-relay/{}", env!("CARGO_PKG_VERSION")),
                heartbeat_secs: self.cfg.heartbeat_secs,
            },
        )
        .await?;

        match role {
            Role::Control { client_id } => {
                log::info!("客户端 {client_id} 已上线（来自 {peer}，{agent}）");
                self.run_control(stream, client_id).await
            }
            Role::Data { client_id, conn_id } => self.splice(stream, client_id, conn_id).await,
        }
    }

    /// 控制会话：处理端口申请、心跳，并把新访客的通知推给客户端。
    async fn run_control(self: Arc<Self>, mut stream: BoxStream, client_id: String) -> Result<()> {
        // 顶掉这个客户端的旧会话。断线重连时旧会话往往还没被 TCP 判死，它手里攥着端口，
        // 新会话的 Bind 就会撞上「地址已被占用」——而占用者是它自己的上一条命。
        self.kick_previous(&client_id).await;

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (done_tx, done_rx) = watch::channel(false);
        self.sessions.lock().await.insert(
            client_id.clone(),
            Session {
                shutdown_tx: shutdown_tx.clone(),
                done_rx,
            },
        );

        let (event_tx, mut event_rx) = mpsc::channel::<ServerMsg>(64);
        let mut listeners: Vec<tokio::task::JoinHandle<()>> = vec![];
        let mut ticker = tokio::time::interval(Duration::from_secs(self.cfg.heartbeat_secs));
        let mut missed_pongs = 0u32;
        let mut shutdown_rx_loop = shutdown_rx.clone();

        let result = loop {
            tokio::select! {
                _ = shutdown_rx_loop.changed() => {
                    let _ = write_msg(&mut stream, &ServerMsg::Bye {
                        reason: "同一客户端建立了新的控制连接".into(),
                    }).await;
                    break Ok(());
                }
                Some(event) = event_rx.recv() => {
                    if write_msg(&mut stream, &event).await.is_err() {
                        break Ok(());
                    }
                }
                _ = ticker.tick() => {
                    if missed_pongs >= 3 {
                        break Err(anyhow::anyhow!("连续 3 次心跳无应答"));
                    }
                    missed_pongs += 1;
                    let ts = missed_pongs as u64;
                    if write_msg(&mut stream, &ServerMsg::Ping { ts }).await.is_err() {
                        break Ok(());
                    }
                }
                msg = read_msg::<_, ClientMsg>(&mut stream) => {
                    match msg {
                        Ok(ClientMsg::Pong { .. }) => missed_pongs = 0,
                        Ok(ClientMsg::Bind { name, remote_port }) => {
                            match self.clone().bind_public(
                                &client_id, &name, remote_port,
                                event_tx.clone(), shutdown_rx.clone(),
                            ).await {
                                Ok(handle) => {
                                    listeners.push(handle);
                                    let public_url = self
                                        .client(&client_id)
                                        .and_then(|c| c.public_url.clone());
                                    write_msg(&mut stream, &ServerMsg::BindOk {
                                        name: name.clone(), remote_port, public_url,
                                    }).await?;
                                    log::info!(
                                        "已把 {}:{remote_port} 指给客户端 {client_id} 的「{name}」",
                                        self.cfg.public_host.as_deref().unwrap_or("0.0.0.0"),
                                    );
                                }
                                Err(e) => {
                                    write_msg(&mut stream, &ServerMsg::BindErr {
                                        name, reason: e.to_string(),
                                    }).await?;
                                }
                            }
                        }
                        Ok(ClientMsg::Bye { reason }) => {
                            log::info!("客户端 {client_id} 主动下线：{reason}");
                            break Ok(());
                        }
                        Ok(other) => log::debug!("控制连接收到意料之外的帧：{other:?}"),
                        Err(e) => break Err(e),
                    }
                }
            }
        };

        // 会话结束：先掐掉端口监听，再宣告结束——顺序反了的话，顶替它的新会话会在
        // 端口还没释放时就去 bind。
        let _ = shutdown_tx.send(true);
        for handle in listeners {
            let _ = handle.await;
        }
        self.sessions.lock().await.remove(&client_id);
        let _ = done_tx.send(true);
        log::info!("客户端 {client_id} 已下线");
        result
    }

    async fn kick_previous(&self, client_id: &str) {
        let old = self.sessions.lock().await.remove(client_id);
        let Some(old) = old else { return };
        let _ = old.shutdown_tx.send(true);
        let mut done = old.done_rx.clone();
        // 等旧会话真的把端口还回来。等不到也得往下走，总比死等强。
        let _ = timeout(Duration::from_secs(5), done.wait_for(|done| *done)).await;
    }

    /// 按申请监听一个公网端口，每来一个访客就生成 `conn_id` 并通知客户端来接管。
    async fn bind_public(
        self: Arc<Self>,
        client_id: &str,
        name: &str,
        remote_port: u16,
        event_tx: mpsc::Sender<ServerMsg>,
        mut shutdown_rx: watch::Receiver<bool>,
    ) -> Result<tokio::task::JoinHandle<()>> {
        let allowed = self
            .client(client_id)
            .map(|c| c.ports.contains(&remote_port))
            .unwrap_or(false);
        if !allowed {
            bail!("端口 {remote_port} 不在该客户端的白名单里");
        }

        let addr = SocketAddr::new(self.cfg.visitor_bind.unwrap_or(self.cfg.bind), remote_port);
        let listener = TcpListener::bind(addr)
            .await
            .with_context(|| format!("监听 {addr} 失败"))?;

        let relay = self.clone();
        let client_id = client_id.to_string();
        let name = name.to_string();
        Ok(tokio::spawn(async move {
            loop {
                let (stream, peer) = tokio::select! {
                    _ = shutdown_rx.changed() => break,
                    accepted = listener.accept() => match accepted {
                        Ok(v) => v,
                        Err(e) => {
                            log::warn!("{addr} 接受访客连接失败：{e}");
                            continue;
                        }
                    },
                };

                // 访客那头也按同一开关走 TLS——配了证书就是正经 HTTPS 端口
                let stream = match relay.maybe_tls(stream).await {
                    Ok(s) => s,
                    Err(e) => {
                        // 扫端口的机器人经常裸连 TLS 端口，握手失败是常态，debug 级即可
                        log::debug!("{peer} 的 TLS 握手没过：{e:#}");
                        continue;
                    }
                };
                let conn_id = relay.next_conn_id.fetch_add(1, Ordering::Relaxed);
                relay.pending.lock().await.insert(
                    conn_id,
                    Pending {
                        stream,
                        peer,
                        client_id: client_id.clone(),
                        since: Instant::now(),
                    },
                );
                let notice = ServerMsg::NewConn {
                    conn_id,
                    name: name.clone(),
                    peer: peer.to_string(),
                };
                if event_tx.send(notice).await.is_err() {
                    // 控制连接没了，这个端口也没有存在的意义
                    relay.pending.lock().await.remove(&conn_id);
                    break;
                }
            }
        }))
    }

    /// 把客户端回连的数据连接与在等的访客连接对接起来。
    async fn splice(&self, mut client_stream: BoxStream, client_id: String, conn_id: u64) -> Result<()> {
        let pending = self.pending.lock().await.remove(&conn_id);
        let Some(pending) = pending else {
            bail!("conn_id {conn_id} 不存在或已超时");
        };
        // 谁的连接谁接管。少了这一句，拿到任一有效令牌的客户端就能把别人的访客接走。
        if pending.client_id != client_id {
            bail!("conn_id {conn_id} 不属于客户端 {client_id}");
        }

        self.stats.conns_total.fetch_add(1, Ordering::Relaxed);
        let mut visitor = pending.stream;
        let peer = pending.peer;
        match copy_bidirectional(&mut visitor, &mut client_stream).await {
            Ok((up, down)) => {
                self.stats.bytes_up.fetch_add(up, Ordering::Relaxed);
                self.stats.bytes_down.fetch_add(down, Ordering::Relaxed);
                log::debug!("{peer} 的连接结束，上行 {up} 字节，下行 {down} 字节");
            }
            Err(e) => log::debug!("{peer} 的连接中断：{e}"),
        }
        Ok(())
    }
}
