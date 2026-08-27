//! 「共享」页：状态、启停、地址与二维码、共享设置。

use crate::config::AppConfig;
use crate::engine::ShareStatus;
use dioxus::prelude::*;
use yunpan_tunnel::RelayStatus;

#[component]
pub fn ShareView() -> Element {
    let mut cfg = use_context::<Signal<AppConfig>>();
    let share = use_context::<Signal<ShareStatus>>();
    let relay_state = use_context::<Signal<RelayStatus>>();
    let started_with = use_context::<Signal<Option<AppConfig>>>();
    // 点了哪条地址就显示哪条的二维码；None = 收起
    let mut qr_url = use_signal(|| None::<String>);
    // 开机自启：OS（plist/注册表/.desktop 是否存在）是唯一真相，进来时读一次
    let mut autostart_on = use_signal(crate::autostart::is_enabled);
    let mut autostart_err = use_signal(String::new);

    let status = share();
    let running = matches!(status, ShareStatus::Running { .. });
    let starting = matches!(status, ShareStatus::Starting);

    // 跑着的时候配置被改过 → 提示重启生效
    let dirty = running && started_with().is_some_and(|s| s != cfg());

    let (dot_class, state_text) = match &status {
        ShareStatus::Stopped => ("dot", "未共享".to_string()),
        ShareStatus::Starting => ("dot busy", "正在启动…".to_string()),
        ShareStatus::Running { .. } => ("dot on", "共享中".to_string()),
        ShareStatus::Failed { reason } => ("dot err", format!("启动失败：{reason}")),
    };

    rsx! {
        section { class: "card",
            div { class: "hero",
                div { class: "{dot_class}" }
                div { class: "state",
                    "{state_text}"
                    if let ShareStatus::Running { .. } = &status {
                        div { class: "sub", "文件夹：{cfg().serve_path}" }
                    }
                }
                if running || starting {
                    button {
                        class: "btn",
                        disabled: starting,
                        onclick: move |_| {
                            spawn(async move { crate::do_stop(share, relay_state, started_with).await });
                        },
                        "停止共享"
                    }
                } else {
                    button {
                        class: "btn primary",
                        disabled: cfg().serve_path.is_empty(),
                        onclick: move |_| {
                            spawn(async move { crate::do_start(cfg, share, relay_state, started_with).await });
                        },
                        "启动共享"
                    }
                }
            }
            if cfg().serve_path.is_empty() && !running {
                p { class: "hint", style: "margin-top:8px", "先在下面选一个要共享的文件夹。" }
            }
            if dirty {
                p { class: "hint warn", style: "margin-top:8px",
                    "设置已改动，点「启动共享」重启后生效。"
                }
            }
        }

        if let ShareStatus::Running { urls } = &status {
            section { class: "card",
                h3 { "访问地址（点地址显示二维码，手机扫码直连）" }
                for url in urls.iter().map(|u| u.to_string()) {
                    {
                        let url_for_qr = url.clone();
                        let url_for_copy = url.clone();
                        rsx! {
                            div { class: "url-row",
                                code {
                                    onclick: move |_| {
                                        let cur = qr_url();
                                        qr_url.set(if cur.as_deref() == Some(url_for_qr.as_str()) {
                                            None
                                        } else {
                                            Some(url_for_qr.clone())
                                        });
                                    },
                                    "{url}"
                                }
                                button {
                                    class: "btn small",
                                    onclick: move |_| super::copy_text(&url_for_copy),
                                    "复制"
                                }
                            }
                        }
                    }
                }
                if let Some(url) = qr_url() {
                    if let Some(svg) = super::qr_svg(&url) {
                        div { class: "qr-box", dangerous_inner_html: "{svg}" }
                    }
                }
                // 中继在线时把公网地址也列进来
                if let RelayStatus::Online { public_url } = relay_state() {
                    {
                        let url_for_qr = public_url.clone();
                        let url_for_copy = public_url.clone();
                        rsx! {
                            div { class: "url-row",
                                code {
                                    onclick: move |_| {
                                        let cur = qr_url();
                                        qr_url.set(if cur.as_deref() == Some(url_for_qr.as_str()) {
                                            None
                                        } else {
                                            Some(url_for_qr.clone())
                                        });
                                    },
                                    "{public_url}（公网）"
                                }
                                button {
                                    class: "btn small",
                                    onclick: move |_| super::copy_text(&url_for_copy),
                                    "复制"
                                }
                            }
                        }
                    }
                }
            }
        }

        section { class: "card",
            h3 { "共享设置" }
            div { class: "field",
                label { "共享文件夹" }
                input {
                    r#type: "text",
                    readonly: true,
                    placeholder: "还没选",
                    value: "{cfg().serve_path}",
                }
                button {
                    class: "btn small",
                    onclick: move |_| {
                        spawn(async move {
                            if let Some(folder) = rfd::AsyncFileDialog::new().pick_folder().await {
                                let mut c = cfg();
                                c.serve_path = folder.path().to_string_lossy().to_string();
                                c.save();
                                cfg.set(c);
                            }
                        });
                    },
                    "选择…"
                }
            }
            div { class: "field",
                label { "端口" }
                input {
                    r#type: "number",
                    min: "1",
                    max: "65535",
                    value: "{cfg().port}",
                    oninput: move |e| {
                        if let Ok(p) = e.value().parse::<u16>() {
                            if p > 0 {
                                let mut c = cfg();
                                c.port = p;
                                c.save();
                                cfg.set(c);
                            }
                        }
                    },
                }
            }
            div { class: "field",
                label { "访客权限" }
                div { class: "checks",
                    label { class: "check",
                        input {
                            r#type: "checkbox",
                            checked: cfg().lan_visible,
                            onchange: move |e| {
                                let mut c = cfg();
                                c.lan_visible = e.value().parse().unwrap_or(true);
                                c.save();
                                cfg.set(c);
                            },
                        }
                        "局域网可见"
                    }
                    label { class: "check",
                        input {
                            r#type: "checkbox",
                            checked: cfg().allow_upload,
                            onchange: move |e| {
                                let mut c = cfg();
                                c.allow_upload = e.value().parse().unwrap_or(false);
                                c.save();
                                cfg.set(c);
                            },
                        }
                        "允许上传"
                    }
                    label { class: "check",
                        input {
                            r#type: "checkbox",
                            checked: cfg().allow_delete,
                            onchange: move |e| {
                                let mut c = cfg();
                                c.allow_delete = e.value().parse().unwrap_or(false);
                                c.save();
                                cfg.set(c);
                            },
                        }
                        "允许删除"
                    }
                    label { class: "check",
                        input {
                            r#type: "checkbox",
                            checked: cfg().allow_search,
                            onchange: move |e| {
                                let mut c = cfg();
                                c.allow_search = e.value().parse().unwrap_or(true);
                                c.save();
                                cfg.set(c);
                            },
                        }
                        "允许搜索"
                    }
                    label { class: "check",
                        input {
                            r#type: "checkbox",
                            checked: cfg().allow_archive,
                            onchange: move |e| {
                                let mut c = cfg();
                                c.allow_archive = e.value().parse().unwrap_or(true);
                                c.save();
                                cfg.set(c);
                            },
                        }
                        "允许整夹打包下载"
                    }
                }
            }
        }

        section { class: "card",
            h3 { "访问密码（留空 = 不设密码）" }
            div { class: "field",
                label { "账号" }
                input {
                    r#type: "text",
                    placeholder: "如 boss",
                    value: "{cfg().auth_user}",
                    oninput: move |e| {
                        let mut c = cfg();
                        c.auth_user = e.value();
                        c.save();
                        cfg.set(c);
                    },
                }
            }
            div { class: "field",
                label { "密码" }
                input {
                    r#type: "password",
                    value: "{cfg().auth_pass}",
                    oninput: move |e| {
                        let mut c = cfg();
                        c.auth_pass = e.value();
                        c.save();
                        cfg.set(c);
                    },
                }
            }
            if cfg().auth_enabled() {
                div { class: "field",
                    label { "" }
                    label { class: "check",
                        input {
                            r#type: "checkbox",
                            checked: cfg().guest_readonly,
                            onchange: move |e| {
                                let mut c = cfg();
                                c.guest_readonly = e.value().parse().unwrap_or(true);
                                c.save();
                                cfg.set(c);
                            },
                        }
                        "没登录的访客可以只读浏览"
                    }
                }
            }
            p { class: "hint",
                "设了密码后，上传/删除等写操作只有登录后才能做；访客要么只读、要么完全进不来，由上面的勾决定。"
            }
        }

        section { class: "card",
            h3 { "HTTPS 证书（留空 = 明文 HTTP）" }
            div { class: "field",
                label { "证书" }
                input {
                    r#type: "text",
                    readonly: true,
                    placeholder: "cert.pem / fullchain.pem",
                    value: "{cfg().tls_cert}",
                }
                button {
                    class: "btn small",
                    onclick: move |_| {
                        spawn(async move {
                            if let Some(f) = rfd::AsyncFileDialog::new()
                                .add_filter("证书", &["pem", "crt", "cer"])
                                .pick_file().await
                            {
                                let mut c = cfg();
                                c.tls_cert = f.path().to_string_lossy().to_string();
                                c.save();
                                cfg.set(c);
                            }
                        });
                    },
                    "选择…"
                }
            }
            div { class: "field",
                label { "私钥" }
                input {
                    r#type: "text",
                    readonly: true,
                    placeholder: "key.pem / privkey.pem",
                    value: "{cfg().tls_key}",
                }
                button {
                    class: "btn small",
                    onclick: move |_| {
                        spawn(async move {
                            if let Some(f) = rfd::AsyncFileDialog::new()
                                .add_filter("私钥", &["pem", "key"])
                                .pick_file().await
                            {
                                let mut c = cfg();
                                c.tls_key = f.path().to_string_lossy().to_string();
                                c.save();
                                cfg.set(c);
                            }
                        });
                    },
                    "选择…"
                }
            }
            if !cfg().tls_cert.is_empty() || !cfg().tls_key.is_empty() {
                div { class: "field",
                    label { "" }
                    button {
                        class: "btn small",
                        onclick: move |_| {
                            let mut c = cfg();
                            c.tls_cert = String::new();
                            c.tls_key = String::new();
                            c.save();
                            cfg.set(c);
                        },
                        "清除，回到 HTTP"
                    }
                }
            }
            if cfg().tls_cert.is_empty() != cfg().tls_key.is_empty() {
                p { class: "hint warn", "证书和私钥要成对选，只有一个不会生效。" }
            }
            p { class: "hint",
                "开了 HTTPS 后，经中继转发的也是加密流量，中继服务器看不见内容。"
                "自签证书浏览器会告警，正式用建议上正经证书（如 Let's Encrypt）。"
            }
        }

        section { class: "card",
            h3 { "通用" }
            div { class: "field",
                label { "" }
                label { class: "check",
                    input {
                        r#type: "checkbox",
                        checked: autostart_on(),
                        onchange: move |e| {
                            let want = e.value().parse::<bool>().unwrap_or(false);
                            match crate::autostart::set_enabled(want) {
                                Ok(()) => {
                                    autostart_err.set(String::new());
                                    // 落定后以 OS 实际状态为准，防止写失败但勾变了
                                    autostart_on.set(crate::autostart::is_enabled());
                                }
                                Err(msg) => autostart_err.set(msg),
                            }
                        },
                    }
                    "开机自动启动（静默进托盘，不弹窗口）"
                }
            }
            if !autostart_err().is_empty() {
                p { class: "hint err", "{autostart_err}" }
            }
        }
    }
}
