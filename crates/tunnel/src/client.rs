//! 客户端侧（嵌在 Windows 端的云链盘进程里跑，不是独立进程）。
//!
//! 只做两件事：维持一条到中继的控制连接，以及每收到一次 `NewConn` 就架起一条
//! 「中继 ←→ 本机 dufs」的管道。

use crate::protocol::{
    read_msg, sign, write_msg, ClientMsg, Role, ServerMsg, PROTO_VERSION,
};
use anyhow::{bail, Context, Result};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::copy_bidirectional;
use tokio::net::TcpStream;
use tokio::sync::watch;

/// 重连退避的上下限。频繁重试对中继是压力，退避太久又让人以为程序死了。
const BACKOFF_START_SECS: u64 = 1;
const BACKOFF_MAX_SECS: u64 = 30;
const CONNECT_TIMEOUT_SECS: u64 = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientConfig {
    /// 中继服务器地址（域名或 IP）。
    pub relay_host: String,
    pub control_port: u16,
    pub client_id: String,
    pub token: String,
    /// 服务名，只用于日志与中继侧展示。
    pub name: String,
    /// 想占用的中继公网端口，必须在中继配置的白名单里。
    pub remote_port: u16,
    /// 本机 dufs 监听的端口。
    pub local_port: u16,
}

/// 中继连接的当前状态。界面上的托盘图标与状态条都读它。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayStatus {
    /// 用户没开中继。
    Idle,
    Connecting,
    /// 已打通，`public_url` 是可以直接发给别人的地址。
    Online { public_url: String },
    /// 掉线重试中。`reason` 如实带上一次失败的原因，别让用户对着转圈猜。
    Retrying { reason: String, attempt: u32 },
    /// 不该重试的错误（令牌不对、端口不在白名单、协议版本不一致）。
    /// 这类错误重试一万次也是同样的结果，停下来让人去改配置。
    Fatal { reason: String },
}

#[derive(Default, Debug)]
pub struct ClientStats {
    pub bytes_up: AtomicU64,
    pub bytes_down: AtomicU64,
    pub conns_total: AtomicU64,
    pub conns_active: AtomicU64,
}

/// 隧道客户端的控制句柄。丢弃它不会停机，必须显式 [`TunnelHandle::stop`]。
pub struct TunnelHandle {
    shutdown_tx: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
    pub status: watch::Receiver<RelayStatus>,
    pub stats: Arc<ClientStats>,
}

impl TunnelHandle {
    pub async fn stop(self) {
        let _ = self.shutdown_tx.send(true);
        let _ = self.task.await;
    }
}

/// 起一条隧道。立即返回，连接过程在后台跑，进度通过 [`TunnelHandle::status`] 汇报。
pub fn spawn(cfg: ClientConfig) -> TunnelHandle {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (status_tx, status_rx) = watch::channel(RelayStatus::Connecting);
    let stats = Arc::new(ClientStats::default());

    let task = tokio::spawn(run(cfg, shutdown_rx, status_tx, stats.clone()));

    TunnelHandle {
        shutdown_tx,
        task,
        status: status_rx,
        stats,
    }
}

/// 不值得重试的错误。裹一层是为了让重连循环能区分「网络抖了」和「配置写错了」。
#[derive(Debug)]
struct Fatal(String);

impl std::fmt::Display for Fatal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for Fatal {}

async fn run(
    cfg: ClientConfig,
    mut shutdown_rx: watch::Receiver<bool>,
    status_tx: watch::Sender<RelayStatus>,
    stats: Arc<ClientStats>,
) {
    let mut backoff = BACKOFF_START_SECS;
    let mut attempt = 0u32;

    loop {
        if *shutdown_rx.borrow() {
            break;
        }
        let _ = status_tx.send(RelayStatus::Connecting);

        let session_shutdown = shutdown_rx.clone();
        let result = tokio::select! {
            _ = shutdown_rx.changed() => break,
            r = session(&cfg, &status_tx, &stats, session_shutdown) => r,
        };

        if *shutdown_rx.borrow() {
            break;
        }

        match result {
            Ok(()) => {
                // 中继主动说再见（多半是同一身份在别处上线了），立刻重连，不退避
                attempt = 0;
                backoff = BACKOFF_START_SECS;
                log::info!("中继连接已结束，正在重连");
            }
            Err(e) if e.is::<Fatal>() => {
                let reason = e.to_string();
                log::error!("中继连接失败且不再重试：{reason}");
                let _ = status_tx.send(RelayStatus::Fatal { reason });
                // 直接 return，不走收尾那句 Idle —— Fatal 必须留在界面上，
                // 被 Idle 盖掉的话用户只会看到「未启用」，不知道是令牌错了
                return;
            }
            Err(e) => {
                attempt += 1;
                let reason = format!("{e:#}");
                log::warn!("中继连接失败（第 {attempt} 次）：{reason}，{backoff} 秒后重试");
                let _ = status_tx.send(RelayStatus::Retrying { reason, attempt });
                tokio::select! {
                    _ = shutdown_rx.changed() => break,
                    _ = tokio::time::sleep(Duration::from_secs(backoff)) => {}
                }
                backoff = (backoff * 2).min(BACKOFF_MAX_SECS);
            }
        }
    }

    let _ = status_tx.send(RelayStatus::Idle);
    log::info!("隧道已停止");
}

/// 一次完整的控制会话。正常返回意味着「该重连了」，返回 [`Fatal`] 意味着「别再连了」。
async fn session(
    cfg: &ClientConfig,
    status_tx: &watch::Sender<RelayStatus>,
    stats: &Arc<ClientStats>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    let addr = format!("{}:{}", cfg.relay_host, cfg.control_port);
    let mut stream = timeout(&addr).await?;
    let role = Role::Control {
        client_id: cfg.client_id.clone(),
    };
    handshake(&mut stream, cfg, &role).await?;

    write_msg(
        &mut stream,
        &ClientMsg::Bind {
            name: cfg.name.clone(),
            remote_port: cfg.remote_port,
        },
    )
    .await?;

    loop {
        let msg: ServerMsg = tokio::select! {
            _ = shutdown_rx.changed() => {
                let _ = write_msg(&mut stream, &ClientMsg::Bye { reason: "用户停用了中继".into() }).await;
                return Ok(());
            }
            msg = read_msg(&mut stream) => msg?,
        };

        match msg {
            ServerMsg::Ping { ts } => write_msg(&mut stream, &ClientMsg::Pong { ts }).await?,
            ServerMsg::BindOk { remote_port, .. } => {
                let public_url = format!("http://{}:{}", cfg.relay_host, remote_port);
                log::info!("中继已打通：{public_url}");
                let _ = status_tx.send(RelayStatus::Online { public_url });
            }
            ServerMsg::BindErr { name, reason } => {
                bail!(Fatal(format!("中继拒绝了「{name}」的端口申请：{reason}")))
            }
            ServerMsg::NewConn { conn_id, peer, .. } => {
                let cfg = cfg.clone();
                let stats = stats.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve_conn(&cfg, conn_id, &stats).await {
                        log::warn!("接管来自 {peer} 的连接失败：{e:#}");
                    }
                });
            }
            ServerMsg::Bye { reason } => {
                log::info!("中继要求断开：{reason}");
                return Ok(());
            }
            ServerMsg::AuthErr { reason } => bail!(Fatal(reason)),
            other => log::debug!("控制连接收到意料之外的帧：{other:?}"),
        }
    }
}

async fn timeout(addr: &str) -> Result<TcpStream> {
    tokio::time::timeout(
        Duration::from_secs(CONNECT_TIMEOUT_SECS),
        TcpStream::connect(addr),
    )
    .await
    .with_context(|| format!("连接中继 {addr} 超时"))?
    .with_context(|| format!("连接中继 {addr} 失败"))
}

/// 挑战-应答握手。控制连接和数据连接走的是同一套，区别只在 [`Role`]。
async fn handshake(stream: &mut TcpStream, cfg: &ClientConfig, role: &Role) -> Result<()> {
    write_msg(
        stream,
        &ClientMsg::Hello {
            proto: PROTO_VERSION,
            role: role.clone(),
            agent: format!("yunpan/{}", env!("CARGO_PKG_VERSION")),
        },
    )
    .await?;

    let nonce = match read_msg::<_, ServerMsg>(stream).await? {
        ServerMsg::Challenge { nonce } => nonce,
        ServerMsg::AuthErr { reason } => bail!(Fatal(reason)),
        other => bail!("等待 Challenge，却收到 {other:?}"),
    };

    write_msg(
        stream,
        &ClientMsg::Auth {
            mac: sign(&cfg.token, PROTO_VERSION, role, &nonce),
        },
    )
    .await?;

    match read_msg::<_, ServerMsg>(stream).await? {
        ServerMsg::AuthOk { server, .. } => {
            log::debug!("已通过中继鉴权（{server}）");
            Ok(())
        }
        ServerMsg::AuthErr { reason } => bail!(Fatal(format!("中继拒绝了本机：{reason}"))),
        other => bail!("等待 AuthOk，却收到 {other:?}"),
    }
}

/// 一条访客连接：向中继回连一条数据连接，另一头接本机 dufs，然后对着倒字节。
async fn serve_conn(cfg: &ClientConfig, conn_id: u64, stats: &Arc<ClientStats>) -> Result<()> {
    let addr = format!("{}:{}", cfg.relay_host, cfg.control_port);
    let mut relay = timeout(&addr).await?;
    let role = Role::Data {
        client_id: cfg.client_id.clone(),
        conn_id,
    };
    handshake(&mut relay, cfg, &role).await?;

    // 连本机 dufs 用 127.0.0.1：即便 dufs 绑的是 0.0.0.0 也走得通，而且不受
    // 「只绑本机」这种设置的影响。
    let mut local = TcpStream::connect(("127.0.0.1", cfg.local_port))
        .await
        .with_context(|| format!("连接本机 dufs 127.0.0.1:{} 失败", cfg.local_port))?;

    stats.conns_total.fetch_add(1, Ordering::Relaxed);
    stats.conns_active.fetch_add(1, Ordering::Relaxed);
    let result = copy_bidirectional(&mut relay, &mut local).await;
    stats.conns_active.fetch_sub(1, Ordering::Relaxed);

    match result {
        Ok((down, up)) => {
            // 站在本机的角度：从中继读进来的是下行请求，写回中继的是上行响应
            stats.bytes_down.fetch_add(down, Ordering::Relaxed);
            stats.bytes_up.fetch_add(up, Ordering::Relaxed);
        }
        Err(e) => log::debug!("连接 {conn_id} 中断：{e}"),
    }
    Ok(())
}
