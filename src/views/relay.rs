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
            if cfg().relay.enabled { "未连接（启动共享时自动连）".into() } else { "未启用".into() },
        ),
        RelayStatus::Connecting => ("dot busy", "正在连接中继…".into()),
        RelayStatus::Online { public_url } => ("dot on", format!("已打通：{public_url}")),
        RelayStatus::Retrying { reason, attempt } => {
            ("dot busy", format!("掉线重连中（第 {attempt} 次）：{reason}"))
        }
        RelayStatus::Fatal { reason } => ("dot err", format!("连不上：{reason}")),
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
                    "改好配置后，回「共享」页点一次「启动共享」重新连。"
                }
            }
        }

        section { class: "card",
            h3 { "中继配置（Linux 服务器上跑 yunpan-relay，这里填它的地址）" }
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
                    "启动共享时自动打通公网中继"
                }
            }
            div { class: "field",
                label { "服务器地址" }
                input {
                    r#type: "text",
                    placeholder: "如 relay.example.com 或 8.8.8.8",
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
                    placeholder: "与中继配置里 clients 的 id 一致",
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
                    placeholder: "服务器上 yunpan-relay token 生成的那串",
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
                    "TLS 加密（中继已配证书；访客走 https）"
                }
            }
            p { class: "hint",
                "别人访问的就是 服务器地址:公网端口，端口必须在中继侧该客户端的白名单里。"
                "证书只配在中继上（certbot 跟着它的域名签），这台电脑不需要任何证书文件。"
                "勾着加密时若一直连不上，多半是中继还没配证书，或上面填的不是证书对应的域名。"
            }
        }
    }
}
