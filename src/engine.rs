//! 引擎：dufs 文件服务与中继隧道的生命周期，都跑在**专属 tokio 运行时**上。
//!
//! 不借 Dioxus 桌面端自带的运行时，是刻意的：dufs 内部到处 `tokio::spawn`，
//! 把它挂在 UI 的执行器上，一次大目录打包就能把界面卡成幻灯片。专属多线程
//! 运行时把文件服务与 UI 彻底隔开；`RT.spawn` 返回的 `JoinHandle` 本身是普通
//! Future，在 Dioxus 这边 await 没有任何问题。

use crate::config::AppConfig;
use std::sync::LazyLock;
use tokio::sync::Mutex;
use yunpan_tunnel::client::TunnelHandle;
use yunpan_tunnel::RelayStatus;

static RT: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .thread_name("yunpan-engine")
        .build()
        .expect("创建引擎运行时失败")
});

static SHARE: Mutex<Option<dufs_core::RunningServer>> = Mutex::const_new(None);
static TUNNEL: Mutex<Option<TunnelHandle>> = Mutex::const_new(None);

/// 共享状态，界面与托盘共读。
#[derive(Debug, Clone, PartialEq)]
pub enum ShareStatus {
    Stopped,
    Starting,
    Running { urls: Vec<String> },
    Failed { reason: String },
}

/// 启动 dufs。返回本机可访问的地址列表。
///
/// 幂等保护：已经在跑就先停旧的再起新的——「改完设置点重启」与「重复点启动」
/// 走的是同一条路，不会出现两个 dufs 抢一个端口。
pub async fn start_share(cfg: &AppConfig) -> Result<Vec<String>, String> {
    let yaml = cfg.to_dufs_yaml();
    RT.spawn(async move {
        let mut slot = SHARE.lock().await;
        if let Some(old) = slot.take() {
            old.shutdown().await;
        }
        let args = dufs_core::Args::from_yaml(&yaml).map_err(|e| format!("{e:#}"))?;
        let server = dufs_core::serve(args).map_err(|e| format!("{e:#}"))?;
        let urls = server.urls().to_vec();
        *slot = Some(server);
        log::info!("共享已启动：{}", urls.join("  "));
        Ok(urls)
    })
    .await
    .map_err(|e| format!("引擎任务崩溃：{e}"))?
}

/// 停掉 dufs（连同在途下载一起断开）。没在跑就什么也不做。
pub async fn stop_share() {
    let _ = RT
        .spawn(async {
            if let Some(server) = SHARE.lock().await.take() {
                server.shutdown().await;
                log::info!("共享已停止");
            }
        })
        .await;
}

/// 拉起中继隧道，返回状态接收端（界面上的中继状态、托盘图标都靠它驱动）。
/// 同样幂等：旧隧道先停。
pub async fn start_tunnel(
    cfg: yunpan_tunnel::ClientConfig,
) -> tokio::sync::watch::Receiver<RelayStatus> {
    RT.spawn(async move {
        let mut slot = TUNNEL.lock().await;
        if let Some(old) = slot.take() {
            old.stop().await;
        }
        let handle = yunpan_tunnel::client::spawn(cfg);
        let status = handle.status.clone();
        *slot = Some(handle);
        status
    })
    .await
    .expect("引擎任务崩溃")
}

pub async fn stop_tunnel() {
    let _ = RT
        .spawn(async {
            if let Some(handle) = TUNNEL.lock().await.take() {
                handle.stop().await;
            }
        })
        .await;
}
