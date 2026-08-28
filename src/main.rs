//! 云链盘 —— 内嵌 dufs 的文件共享桌面端。
//!
//! 全平台 Dioxus 桌面应用（Windows / macOS / Linux），主要交付目标是 Windows。
//! dufs 与中继隧道客户端都编在本进程里（见 `crates/dufs-core`、`crates/tunnel`），
//! 安装包里只有一个可执行文件，任务管理器里也只有一个进程。
//!
//! 结构：
//! - [`config`] —— 界面配置，存 `<config_dir>/yunpan/config.json`
//! - [`engine`] —— dufs 与隧道的生命周期，跑在专属 tokio 运行时上
//! - [`logbus`] —— 全局日志环形缓冲，「日志」页轮询它
//! - [`tray`] —— 程序化画的状态图标（灰 = 未共享，朱砂红 = 共享中）
//! - [`views`] —— 共享 / 中继 / 日志三个页面

// Windows 发布构建用 windows 子系统启动，双击不弹黑色控制台（debug 保留，看日志用）
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

use dioxus::prelude::*;

mod autostart;
mod config;
mod engine;
mod logbus;
mod tray;
mod views;

use config::AppConfig;
use engine::ShareStatus;
use views::{LogsView, RelayView, ShareView};
use yunpan_tunnel::RelayStatus;

#[cfg(feature = "desktop")]
const TRAY_SHOW_ID: &str = "yp-show";
#[cfg(feature = "desktop")]
const TRAY_TOGGLE_ID: &str = "yp-toggle";
#[cfg(feature = "desktop")]
const TRAY_QUIT_ID: &str = "yp-quit";

// ─────────────────────────────────────────────────────────────────────────────
// 单实例（仅发布版）：第二个实例不再起一份，而是通知已有实例把窗口显示出来。
// 写法承自 ProxyZms。端口刻意避开临时端口范围（49152-65535）——落在那个区间里
// 会被随机进程占走，我们就会误判「已有实例」而静默退出，应用从此打不开。
// 17654 = ProxyZms 的 17653 + 1，两个应用可以同机共存。

#[cfg(not(debug_assertions))]
static SHOW_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(not(debug_assertions))]
const SINGLE_INSTANCE_ADDR: &str = "127.0.0.1:17654";
/// 握手串：确认端口对面确实是本程序，而不是碰巧占了端口的别人。
#[cfg(not(debug_assertions))]
const HELLO: &[u8] = b"yunpan-show\n";
#[cfg(not(debug_assertions))]
const ACK: &[u8] = b"yunpan-ok\n";

/// true = 本进程是主实例，继续；false = 已有实例（已请它显示窗口），退出。
#[cfg(not(debug_assertions))]
fn acquire_single_instance() -> bool {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    // —— 探测：端口对面是不是「另一个我」——
    if let Ok(addr) = SINGLE_INSTANCE_ADDR.parse() {
        if let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) {
            // Windows：把「窃取前台焦点」的权限放给任意进程，主实例的 set_focus()
            // 里的 SetForegroundWindow 才不会被前台保护策略静默拒掉（否则只闪任务栏）
            #[cfg(windows)]
            allow_other_set_foreground();

            // 全程带超时：对面若只 accept 不回话，不能把启动流程挂死
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
            let mut buf = [0u8; ACK.len()];
            if stream.write_all(HELLO).is_ok()
                && stream.read_exact(&mut buf).is_ok()
                && buf == *ACK
            {
                return false;
            }
            // 端口被别人占了：不退出，继续以主实例身份启动。
            // 宁可多开一个，也不能出现「双击没反应」。
            return true;
        }
    }

    // —— 主实例：占住端口，起线程接收「显示」请求 ——
    if let Ok(listener) = TcpListener::bind(SINGLE_INSTANCE_ADDR) {
        std::thread::spawn(move || {
            for mut conn in listener.incoming().flatten() {
                let _ = conn.set_read_timeout(Some(Duration::from_millis(500)));
                let _ = conn.set_write_timeout(Some(Duration::from_millis(500)));
                let mut buf = [0u8; HELLO.len()];
                if conn.read_exact(&mut buf).is_ok() && buf == *HELLO {
                    let _ = conn.write_all(ACK);
                    SHOW_REQUESTED.store(true, Ordering::SeqCst);
                }
            }
        });
    }
    true
}

#[cfg(all(windows, not(debug_assertions)))]
fn allow_other_set_foreground() {
    // ASFW_ANY = 0xFFFFFFFF（任意 PID）。签名：BOOL AllowSetForegroundWindow(DWORD)
    #[link(name = "user32")]
    extern "system" {
        fn AllowSetForegroundWindow(dwProcessId: u32) -> i32;
    }
    const ASFW_ANY: u32 = 0xFFFF_FFFF;
    unsafe { AllowSetForegroundWindow(ASFW_ANY) };
}

/// 系统当前是否深色模式。只用来定窗口首帧底色，页面内配色由 CSS 自己跟系统，
/// 所以探测粗糙点无妨：macOS 问 defaults，其它平台返回 false（首帧白底）。
#[cfg(feature = "desktop")]
fn dark_scheme() -> bool {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("defaults")
            .args(["read", "-g", "AppleInterfaceStyle"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("Dark"))
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "macos"))]
    false
}

fn main() {
    #[cfg(not(debug_assertions))]
    if !acquire_single_instance() {
        return;
    }

    logbus::install();

    #[cfg(feature = "desktop")]
    {
        use dioxus::desktop::tao::dpi::LogicalSize;
        use dioxus::desktop::{Config, WindowBuilder, WindowCloseBehaviour};

        // 开机自启的条目带 --hidden：登录时静默进托盘，不弹主窗口
        let start_hidden = std::env::args().any(|a| a == autostart::HIDDEN_FLAG);

        let window = WindowBuilder::new()
            .with_title("云链盘")
            .with_window_icon(tray::window_icon())
            .with_visible(!start_hidden)
            .with_inner_size(LogicalSize::new(760.0, 640.0))
            .with_min_inner_size(LogicalSize::new(620.0, 480.0));

        // CSS 编译期内联进初始 HTML：不依赖 asset 路径解析，发布/开发一致，无闪白
        let custom_head = format!(
            "<meta name=\"color-scheme\" content=\"light dark\"><style>{}</style>",
            include_str!("../assets/main.css"),
        );

        #[cfg_attr(target_os = "macos", allow(unused_mut))]
        let mut cfg = Config::new()
            .with_window(window)
            // 初始背景跟随系统深浅色（值要与 main.css 的 --paper 一致），首帧不闪白/闪黑
            .with_background_color(if dark_scheme() { (15, 15, 15, 255) } else { (255, 255, 255, 255) })
            // 关窗只是收进托盘，共享不断——这正是托盘应用的意义
            .with_close_behaviour(WindowCloseBehaviour::WindowHides)
            .with_custom_head(custom_head);

        // 隐藏 Windows/Linux 上的默认菜单栏；macOS 保留（Edit 菜单提供 Cmd+C/V）
        #[cfg(not(target_os = "macos"))]
        {
            cfg = cfg.with_menu(None::<dioxus::desktop::muda::Menu>);
        }

        dioxus::LaunchBuilder::new().with_cfg(cfg).launch(App);
    }
    #[cfg(not(feature = "desktop"))]
    dioxus::launch(App);
}

/// 启动共享（以及配置了的话，中继隧道）。界面按钮与托盘菜单共用这一个入口。
pub async fn do_start(
    cfg_sig: Signal<AppConfig>,
    mut share: Signal<ShareStatus>,
    mut relay_state: Signal<RelayStatus>,
    mut started_with: Signal<Option<AppConfig>>,
) {
    let cfg = cfg_sig.peek().clone();
    if cfg.serve_path.is_empty() {
        share.set(ShareStatus::Failed {
            reason: "还没选要共享的文件夹".into(),
        });
        return;
    }
    share.set(ShareStatus::Starting);

    match engine::start_share(&cfg).await {
        Ok(urls) => {
            share.set(ShareStatus::Running { urls });
            started_with.set(Some(cfg.clone()));
        }
        Err(reason) => {
            share.set(ShareStatus::Failed { reason });
            return;
        }
    }

    if cfg.relay.enabled {
        match cfg.tunnel_config() {
            Some(tunnel_cfg) => {
                let mut rx = engine::start_tunnel(tunnel_cfg).await;
                relay_state.set(rx.borrow().clone());
                // 状态搬运循环：隧道停了（sender 关闭）循环自然结束
                spawn(async move {
                    while rx.changed().await.is_ok() {
                        let s = rx.borrow().clone();
                        relay_state.set(s);
                    }
                });
            }
            None => relay_state.set(RelayStatus::Fatal {
                reason: "中继参数没配全（地址 / 客户端 ID / 令牌）".into(),
            }),
        }
    } else {
        relay_state.set(RelayStatus::Idle);
    }
}

/// 停止共享与隧道。
pub async fn do_stop(
    mut share: Signal<ShareStatus>,
    mut relay_state: Signal<RelayStatus>,
    mut started_with: Signal<Option<AppConfig>>,
) {
    engine::stop_tunnel().await;
    engine::stop_share().await;
    relay_state.set(RelayStatus::Idle);
    share.set(ShareStatus::Stopped);
    started_with.set(None);
}

#[derive(Clone, Copy, PartialEq)]
pub enum Tab {
    Share,
    Relay,
    Logs,
}

#[component]
fn App() -> Element {
    let cfg = use_context_provider(|| Signal::new(AppConfig::load()));
    let share = use_context_provider(|| Signal::new(ShareStatus::Stopped));
    let relay_state = use_context_provider(|| Signal::new(RelayStatus::Idle));
    let started_with = use_context_provider(|| Signal::new(None::<AppConfig>));
    // 进 context：共享页的中继状态徽章要能跳到中继页
    let mut tab = use_context_provider(|| Signal::new(Tab::Share));

    // ── 系统托盘 ──
    #[cfg(feature = "desktop")]
    {
        use dioxus::desktop::muda::{Menu, MenuItem, PredefinedMenuItem};
        use dioxus::desktop::trayicon::{
            init_tray_icon, use_tray_icon, MouseButton, MouseButtonState, TrayIconEvent,
        };
        use dioxus::desktop::{
            use_muda_event_handler, use_tray_icon_event_handler, use_tray_menu_event_handler,
            use_window,
        };

        let (status_item, toggle_item) = use_hook(|| {
            let status = MenuItem::with_id("yp-status", "未共享", false, None);
            let show = MenuItem::with_id(TRAY_SHOW_ID, "显示主界面", true, None);
            let toggle = MenuItem::with_id(TRAY_TOGGLE_ID, "启动共享", true, None);
            let quit = MenuItem::with_id(TRAY_QUIT_ID, "退出", true, None);
            let menu = Menu::new();
            let _ = menu.append_items(&[
                &status,
                &PredefinedMenuItem::separator(),
                &show,
                &toggle,
                &PredefinedMenuItem::separator(),
                &quit,
            ]);
            init_tray_icon(menu, tray::tray_icon(false));
            (status, toggle)
        });
        let tray_handle = use_tray_icon();

        // 状态一变就刷新托盘图标与菜单文字
        let status_for_effect = status_item.clone();
        let toggle_for_effect = toggle_item.clone();
        use_effect(move || {
            let s = share();
            let running = matches!(s, ShareStatus::Running { .. });
            if let Some(t) = tray_handle.as_ref() {
                let _ = t.set_icon(tray::tray_icon(running));
                let _ = t.set_tooltip(Some(match &s {
                    ShareStatus::Running { .. } => "云链盘 — 共享中",
                    ShareStatus::Starting => "云链盘 — 启动中…",
                    _ => "云链盘 — 未共享",
                }));
            }
            status_for_effect.set_text(match (&s, &relay_state()) {
                (ShareStatus::Running { .. }, RelayStatus::Online { public_url }) => {
                    format!("共享中 · 公网 {public_url}")
                }
                (ShareStatus::Running { .. }, _) => "共享中".to_string(),
                (ShareStatus::Starting, _) => "启动中…".to_string(),
                (ShareStatus::Failed { .. }, _) => "启动失败".to_string(),
                _ => "未共享".to_string(),
            });
            toggle_for_effect.set_text(if running || matches!(s, ShareStatus::Starting) {
                "停止共享"
            } else {
                "启动共享"
            });
            toggle_for_effect.set_enabled(!matches!(s, ShareStatus::Starting));
        });

        // 左击托盘图标 → 显示主窗口
        let win_click = use_window();
        use_tray_icon_event_handler(move |event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Down | MouseButtonState::Up,
                ..
            } = event
            {
                win_click.set_visible(true);
                win_click.set_focus();
            }
        });

        // 菜单点击：muda 与 tray 是同一类型同一全局 handler（后注册覆盖前者），
        // 不确定哪个生效，两个都挂，共用一个处理函数（ProxyZms 验证过的写法）
        let handle_menu = {
            let win = use_window();
            move |id: &dioxus::desktop::muda::MenuId| {
                if id == TRAY_SHOW_ID {
                    win.set_visible(true);
                    win.set_focus();
                } else if id == TRAY_TOGGLE_ID {
                    let running = !matches!(*share.peek(), ShareStatus::Stopped | ShareStatus::Failed { .. });
                    spawn(async move {
                        if running {
                            do_stop(share, relay_state, started_with).await;
                        } else {
                            do_start(cfg, share, relay_state, started_with).await;
                        }
                    });
                } else if id == TRAY_QUIT_ID {
                    // 共享服务在本进程里，进程一退全部资源随之释放，无孤儿可清
                    std::process::exit(0);
                }
            }
        };
        {
            let h = handle_menu.clone();
            use_tray_menu_event_handler(move |e| h(&e.id));
        }
        use_muda_event_handler(move |e| handle_menu(&e.id));

        // 单实例：另一个实例请求显示时，把本窗口拉到前台（仅发布版有这条通道）
        #[cfg(not(debug_assertions))]
        {
            let win_show = use_window();
            use_future(move || {
                let win = win_show.clone();
                async move {
                    use std::sync::atomic::Ordering;
                    loop {
                        if SHOW_REQUESTED.swap(false, Ordering::SeqCst) {
                            win.set_visible(true);
                            win.set_focus();
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    }
                }
            });
        }
    }

    rsx! {
        div { class: "app",
            header { class: "topbar",
                div { class: "brand",
                    div { class: "brand-title", "云" b { "链" } "盘" }
                    div { class: "brand-sub", "Yunpan · File Station" }
                }
                button {
                    class: if tab() == Tab::Share { "tab active" } else { "tab" },
                    onclick: move |_| tab.set(Tab::Share),
                    "共享"
                }
                button {
                    class: if tab() == Tab::Relay { "tab active" } else { "tab" },
                    onclick: move |_| tab.set(Tab::Relay),
                    "中继"
                }
                button {
                    class: if tab() == Tab::Logs { "tab active" } else { "tab" },
                    onclick: move |_| tab.set(Tab::Logs),
                    "日志"
                }
            }
            main { class: "content",
                match tab() {
                    Tab::Share => rsx! { ShareView {} },
                    Tab::Relay => rsx! { RelayView {} },
                    Tab::Logs => rsx! { LogsView {} },
                }
            }
        }
    }
}
