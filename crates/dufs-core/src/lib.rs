//! dufs 内嵌版 —— 把 [dufs](https://github.com/sigoden/dufs) v0.46.0 的文件服务端
//! 搬进本进程当库用。
//!
//! **为什么要 vendor 而不是当子进程拉起来**：上游 dufs 是纯二进制 crate（没有 `[lib]`
//! 目标），`cargo` 无法把它作为依赖引入。而云链盘要的是「一个 exe 就是全部」——不想在
//! 安装包里再塞一个 dufs.exe、不想管子进程的存活与孤儿清理、也不想让用户在任务管理器里
//! 看见两个进程。所以把源码搬进来，按库的形态重新组织。
//!
//! **相对上游的改动，只有四处**（升级 dufs 时照这张单子重做一遍即可）：
//!
//! 1. `main.rs` → 本文件：`serve()` 返回 [`RunningServer`] 句柄，支持停机；上游是
//!    `tokio::select!` 等 Ctrl-C，跑到进程结束为止。
//! 2. `args.rs`：剥掉 clap（`build_cli` / `print_completions` / `impl ValueEnum`），
//!    `Args::parse(ArgMatches)` 换成 [`Args::from_yaml`] + [`Args::finalize`]。
//!    界面上的配置序列化成 YAML 再进来，走的仍是上游 `-c config.yaml` 那条解析路径。
//! 3. `server.rs`：静态资源 URL 前缀里的 `env!("CARGO_PKG_VERSION")` 换成
//!    [`DUFS_VERSION`] 常量 —— 否则前缀会变成云链盘自己的版本号，跟着每次发版乱跳，
//!    浏览器缓存的 index.js 会因此天天失效。
//! 4. 不带 `logger.rs`：`log::set_boxed_logger` 全进程只能设一次，日志归宿主应用管
//!    （见 `yunpan::logbus`），这里只发 `log` 记录，不抢全局 logger。
//!
//! 其余文件（`auth.rs` / `server.rs` / `http_logger.rs` / `http_utils.rs` /
//! `noscript.rs` / `utils.rs` / `assets/`）与上游 v0.46.0 逐字节一致。
//! 许可证见同目录 `LICENSE-MIT` / `LICENSE-APACHE`。

#[macro_use]
extern crate log;

mod args;
mod auth;
mod http_logger;
mod http_utils;
mod noscript;
mod server;
mod utils;

pub use args::{Args, BindAddr, Compress};
pub use server::Server;

use anyhow::{Context, Result};
use hyper::{body::Incoming, service::service_fn, Request};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder,
};
use std::net::{IpAddr, SocketAddr, TcpListener as StdTcpListener};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::timeout;
use tokio::{net::TcpListener, task::JoinHandle};
#[cfg(feature = "tls")]
use tokio_rustls::{rustls::ServerConfig, TlsAcceptor};
#[cfg(feature = "tls")]
use utils::{load_certs, load_private_key};

/// 被 vendor 进来的 dufs 上游版本。静态资源 URL 前缀按它拼，**升级 dufs 时必须同步改**。
pub const DUFS_VERSION: &str = "0.46.0";

/// 一个正在跑的 dufs 实例。
///
/// 持有它就等于持有端口：[`RunningServer::shutdown`] 之前，监听不会释放。界面上的
/// 「停止共享」必须 await 到 shutdown 返回，否则紧接着的「启动」会撞上还没关掉的旧监听，
/// 报「端口被占用」——而占用者正是自己。
pub struct RunningServer {
    handles: Vec<JoinHandle<()>>,
    running: Arc<AtomicBool>,
    shutdown_tx: watch::Sender<bool>,
    urls: Vec<String>,
}

impl RunningServer {
    /// 本机可访问的地址列表，已展开 `0.0.0.0` 为各网卡实际 IP —— 界面直接拿去显示/生成二维码。
    pub fn urls(&self) -> &[String] {
        &self.urls
    }

    /// 停机：通知所有 accept 循环与在途连接退出，并等它们真的结束。
    ///
    /// `running` 置 false 是给 dufs 内部的长任务（目录搜索、打包 zip）看的中止标志；
    /// `shutdown_tx` 负责唤醒 accept 循环。两者缺一：前者漏了会让大目录搜索继续跑完，
    /// 后者漏了端口永远不释放。
    pub async fn shutdown(self) {
        self.running.store(false, Ordering::SeqCst);
        let _ = self.shutdown_tx.send(true);
        for handle in self.handles {
            let _ = handle.await;
        }
    }
}

/// 在当前 tokio 运行时上启动 dufs。
///
/// 绑定失败（端口被占、地址非法）在这里就返回 `Err`，不会变成后台任务里悄悄死掉的错误
/// —— 界面要靠这个返回值把「启动失败」如实显示出来。
pub fn serve(mut args: Args) -> Result<RunningServer> {
    let (new_addrs, print_addrs) = check_addrs(&args)?;
    args.addrs = new_addrs;
    let urls = listening_urls(&args, &print_addrs);

    let addrs = args.addrs.clone();
    let port = args.port;
    let tls_config = (args.tls_cert.clone(), args.tls_key.clone());
    let running = Arc::new(AtomicBool::new(true));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let server_handle = Arc::new(Server::init(args, running.clone())?);
    let mut handles = vec![];

    for bind_addr in addrs.iter() {
        let server_handle = server_handle.clone();
        let shutdown_rx = shutdown_rx.clone();
        match bind_addr {
            BindAddr::IpAddr(ip) => {
                let listener = create_listener(SocketAddr::new(*ip, port))
                    .with_context(|| format!("绑定 `{ip}:{port}` 失败"))?;

                match &tls_config {
                    #[cfg(feature = "tls")]
                    (Some(cert_file), Some(key_file)) => {
                        let certs = load_certs(cert_file)?;
                        let key = load_private_key(key_file)?;
                        let mut config = ServerConfig::builder()
                            .with_no_client_auth()
                            .with_single_cert(certs, key)?;
                        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
                        let tls_accepter = TlsAcceptor::from(Arc::new(config));
                        let handshake_timeout = Duration::from_secs(10);
                        let mut shutdown_rx = shutdown_rx;

                        handles.push(tokio::spawn(async move {
                            loop {
                                let (stream, addr) = tokio::select! {
                                    _ = shutdown_rx.changed() => break,
                                    accepted = listener.accept() => match accepted {
                                        Ok(v) => v,
                                        Err(_) => continue,
                                    },
                                };
                                let Some(stream) =
                                    timeout(handshake_timeout, tls_accepter.accept(stream))
                                        .await
                                        .ok()
                                        .and_then(|v| v.ok())
                                else {
                                    continue;
                                };
                                tokio::spawn(handle_stream(
                                    server_handle.clone(),
                                    TokioIo::new(stream),
                                    Some(addr),
                                    shutdown_rx.clone(),
                                ));
                            }
                        }));
                    }
                    (None, None) => {
                        let mut shutdown_rx = shutdown_rx;
                        handles.push(tokio::spawn(async move {
                            loop {
                                let (stream, addr) = tokio::select! {
                                    _ = shutdown_rx.changed() => break,
                                    accepted = listener.accept() => match accepted {
                                        Ok(v) => v,
                                        Err(_) => continue,
                                    },
                                };
                                tokio::spawn(handle_stream(
                                    server_handle.clone(),
                                    TokioIo::new(stream),
                                    Some(addr),
                                    shutdown_rx.clone(),
                                ));
                            }
                        }));
                    }
                    _ => unreachable!(),
                }
            }
            #[cfg(unix)]
            BindAddr::SocketPath(path) => {
                let socket_path = if path.starts_with('@')
                    && cfg!(any(target_os = "linux", target_os = "android"))
                {
                    let mut path_buf = path.as_bytes().to_vec();
                    path_buf[0] = b'\0';
                    unsafe { std::ffi::OsStr::from_encoded_bytes_unchecked(&path_buf) }.to_os_string()
                } else {
                    let _ = std::fs::remove_file(path);
                    path.into()
                };
                let listener = tokio::net::UnixListener::bind(socket_path)
                    .with_context(|| format!("绑定 `{path}` 失败"))?;
                let mut shutdown_rx = shutdown_rx;
                handles.push(tokio::spawn(async move {
                    loop {
                        let (stream, _addr) = tokio::select! {
                            _ = shutdown_rx.changed() => break,
                            accepted = listener.accept() => match accepted {
                                Ok(v) => v,
                                Err(_) => continue,
                            },
                        };
                        tokio::spawn(handle_stream(
                            server_handle.clone(),
                            TokioIo::new(stream),
                            None,
                            shutdown_rx.clone(),
                        ));
                    }
                }));
            }
        }
    }

    Ok(RunningServer {
        handles,
        running,
        shutdown_tx,
        urls,
    })
}

/// 单条连接的服务循环。停机信号一到就直接丢弃连接（在途下载被切断），
/// 这是「停止共享」应有的语义：点了停止，对面就该断，而不是等他下完 4G 的 ISO。
async fn handle_stream<T>(
    handle: Arc<Server>,
    stream: TokioIo<T>,
    addr: Option<SocketAddr>,
    mut shutdown_rx: watch::Receiver<bool>,
) where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let hyper_service =
        service_fn(move |request: Request<Incoming>| handle.clone().call(request, addr));

    let builder = Builder::new(TokioExecutor::new());
    let conn = builder.serve_connection_with_upgrades(stream, hyper_service);
    tokio::pin!(conn);

    tokio::select! {
        // 上游在这里吞掉错误：客户端连上却不发请求就断开时必然报错，不是问题。
        _ = &mut conn => {}
        _ = shutdown_rx.changed() => {}
    }
}

fn create_listener(addr: SocketAddr) -> Result<TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
    if addr.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    socket.set_reuse_address(true)?;
    socket.bind(&addr.into())?;
    socket.listen(1024)?;
    let std_listener = StdTcpListener::from(socket);
    std_listener.set_nonblocking(true)?;
    Ok(TcpListener::from_std(std_listener)?)
}

fn check_addrs(args: &Args) -> Result<(Vec<BindAddr>, Vec<BindAddr>)> {
    let mut new_addrs = vec![];
    let mut print_addrs = vec![];
    let has_unspecified = args
        .addrs
        .iter()
        .any(|a| matches!(a, BindAddr::IpAddr(ip) if ip.is_unspecified()));
    let (ipv4_addrs, ipv6_addrs) = if has_unspecified {
        interface_addrs()?
    } else {
        (vec![], vec![])
    };
    for bind_addr in args.addrs.iter() {
        new_addrs.push(bind_addr.clone());
        match bind_addr {
            BindAddr::IpAddr(ip) => match &ip {
                IpAddr::V4(_) => {
                    if ip.is_unspecified() {
                        print_addrs.extend(ipv4_addrs.clone());
                    } else {
                        print_addrs.push(bind_addr.clone());
                    }
                }
                IpAddr::V6(_) => {
                    if ip.is_unspecified() {
                        print_addrs.extend(ipv6_addrs.clone());
                    } else {
                        print_addrs.push(bind_addr.clone());
                    }
                }
            },
            #[cfg(unix)]
            _ => {
                new_addrs.push(bind_addr.clone());
                print_addrs.push(bind_addr.clone())
            }
        }
    }
    print_addrs.sort_unstable();
    Ok((new_addrs, print_addrs))
}

fn interface_addrs() -> Result<(Vec<BindAddr>, Vec<BindAddr>)> {
    let (mut ipv4_addrs, mut ipv6_addrs) = (vec![], vec![]);
    let ifaces = if_addrs::get_if_addrs().with_context(|| "读取本机网卡地址失败")?;
    for iface in ifaces.into_iter() {
        let ip = iface.ip();
        if ip.is_ipv4() {
            ipv4_addrs.push(BindAddr::IpAddr(ip))
        }
        if ip.is_ipv6() {
            ipv6_addrs.push(BindAddr::IpAddr(ip))
        }
    }
    Ok((ipv4_addrs, ipv6_addrs))
}

fn listening_urls(args: &Args, print_addrs: &[BindAddr]) -> Vec<String> {
    print_addrs
        .iter()
        .map(|bind_addr| match bind_addr {
            BindAddr::IpAddr(addr) => {
                let host = match addr {
                    IpAddr::V4(_) => format!("{}:{}", addr, args.port),
                    IpAddr::V6(_) => format!("[{}]:{}", addr, args.port),
                };
                let protocol = if args.tls_cert.is_some() { "https" } else { "http" };
                format!("{protocol}://{host}{}", args.uri_prefix)
            }
            #[cfg(unix)]
            BindAddr::SocketPath(path) => path.to_string(),
        })
        .collect()
}
