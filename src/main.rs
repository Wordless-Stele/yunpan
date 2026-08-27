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

fn main() {
    logbus::install();

    #[cfg(feature = "desktop")]
    {
        use dioxus::desktop::tao::dpi::LogicalSize;
        use dioxus::desktop::{Config, WindowBuilder, WindowCloseBehaviour};

        let window = WindowBuilder::new()
            .with_title("云链盘")
            .with_window_icon(tray::window_icon())
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
enum Tab {
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
    let mut tab = use_signal(|| Tab::Share);

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
    }

    rsx! {
        div { class: "app",
            header { class: "topbar",
                span { class: "brand", "云" b { "链" } "盘" }
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
