//! 「中继」页：公网穿透的配置与状态。

use crate::config::AppConfig;
use crate::engine::ShareStatus;
use dioxus::prelude::*;
use yunpan_tunnel::RelayStatus;

#[component]
pub fn RelayView() -> Element {
    let mut cfg = use_context::<Signal<AppConfig>>();
    let relay_state = use_context::<Signal<RelayStatus>>();
    let share = use_context::<Signal<ShareStatus>>();

    let (dot_class, state_text): (&str, String) = match relay_state() {
        RelayStatus::Idle => (
            "dot",
            if cfg().relay.enabled { "未连接（启动共享时自动连接）".into() } else { "未启用".into() },
        ),
        RelayStatus::Connecting => ("dot busy", "正在连接中继服务器…".into()),
        RelayStatus::Online { public_url } => ("dot on", format!("连接正常：{public_url}")),
        RelayStatus::Retrying { reason, attempt } => {
            ("dot busy", format!("连接中断，正在重试（第 {attempt} 次）：{reason}"))
        }
        RelayStatus::Fatal { reason } => ("dot err", format!("无法连接：{reason}")),
    };

    rsx! {
        section { class: "card",
            div { class: "hero",
                div { class: "{dot_class}" }
                div { class: "state", "{state_text}" }
                if let RelayStatus::Online { public_url } = relay_state() {
                    button {
                        class: "btn small",
                        onclick: move |_| super::copy_text(&public_url),
                        "复制公网地址"
                    }
                }
            }
            if matches!(relay_state(), RelayStatus::Fatal { .. })
                && matches!(share(), ShareStatus::Running { .. }) {
                p { class: "hint err", style: "margin-top:8px",
                    "修改配置后，请在「共享」页重新启动共享。"
                }
            }
        }

        section { class: "card",
            h3 { "中继配置（填写运行 yunpan-relay 的中继服务器信息）" }
            div { class: "field",
                label { "" }
                label { class: "check",
                    input {
                        r#type: "checkbox",
                        checked: cfg().relay.enabled,
                        onchange: move |e| {
                            let mut c = cfg();
                            c.relay.enabled = e.value().parse().unwrap_or(false);
                            c.save();
                            cfg.set(c);
                        },
                    }
                    "启动共享时自动连接公网中继"
                }
            }
            div { class: "field",
                label { "服务器地址" }
                input {
                    r#type: "text",
                    placeholder: "中继服务器的域名或 IP",
                    value: "{cfg().relay.host}",
                    oninput: move |e| {
                        let mut c = cfg();
                        c.relay.host = e.value();
                        c.save();
                        cfg.set(c);
                    },
                }
            }
            div { class: "field",
                label { "控制端口" }
                input {
                    r#type: "number",
                    min: "1",
                    max: "65535",
                    value: "{cfg().relay.control_port}",
                    oninput: move |e| {
                        if let Ok(p) = e.value().parse::<u16>() {
                            if p > 0 {
                                let mut c = cfg();
                                c.relay.control_port = p;
                                c.save();
                                cfg.set(c);
                            }
                        }
                    },
                }
            }
            div { class: "field",
                label { "客户端 ID" }
                input {
                    r#type: "text",
                    placeholder: "与中继端 clients 配置中的 id 一致",
                    value: "{cfg().relay.client_id}",
                    oninput: move |e| {
                        let mut c = cfg();
                        c.relay.client_id = e.value();
                        c.save();
                        cfg.set(c);
                    },
                }
            }
            div { class: "field",
                label { "令牌" }
                input {
                    r#type: "password",
                    placeholder: "由中继端 yunpan-relay token 命令生成",
                    value: "{cfg().relay.token}",
                    oninput: move |e| {
                        let mut c = cfg();
                        c.relay.token = e.value();
                        c.save();
                        cfg.set(c);
                    },
                }
            }
            div { class: "field",
                label { "公网端口" }
                input {
                    r#type: "number",
                    min: "1",
                    max: "65535",
                    value: "{cfg().relay.remote_port}",
                    oninput: move |e| {
                        if let Ok(p) = e.value().parse::<u16>() {
                            if p > 0 {
                                let mut c = cfg();
                                c.relay.remote_port = p;
                                c.save();
                                cfg.set(c);
                            }
                        }
                    },
                }
            }
            div { class: "field",
                label { "" }
                label { class: "check",
                    input {
                        r#type: "checkbox",
                        checked: cfg().relay.tls,
                        onchange: move |e| {
                            let mut c = cfg();
                            c.relay.tls = e.value().parse().unwrap_or(true);
                            c.save();
                            cfg.set(c);
                        },
                    }
                    "TLS 加密（中继服务器已配置证书时启用）"
                }
            }
            p { class: "hint",
                "公网端口为中继服务器上分配给本机的访客端口，须在中继端该客户端的端口白名单内；"
                "经 nginx 反向代理部署时，访客地址以中继下发为准。"
                "证书仅部署于中继服务器，本机无需任何证书文件。"
                "若启用加密后始终无法连接，通常为中继服务器未配置证书，或服务器地址与证书域名不符。"
            }
        }
    }
}
