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
    // hook 统一取在组件顶部，不散落在 rsx 里
    let mut tab = use_context::<Signal<crate::Tab>>();

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
                // 中继状态徽章：一眼看到公网通没通，点击跳中继页
                {
                    let (chip_cls, chip_txt): (&str, String) = if !cfg().relay.enabled {
                        ("chip", "中继服务器未启用".into())
                    } else {
                        match relay_state() {
                            RelayStatus::Idle => ("chip", "中继服务器待命".into()),
                            RelayStatus::Connecting => ("chip busy", "连接中继服务器…".into()),
                            RelayStatus::Online { .. } => ("chip on", "中继服务器正常".into()),
                            RelayStatus::Retrying { attempt, .. } => {
                                ("chip busy", format!("中继服务器重连中 {attempt}"))
                            }
                            RelayStatus::Fatal { .. } => ("chip err", "中继服务器异常".into()),
                        }
                    };
                    rsx! {
                        button {
                            class: "{chip_cls}",
                            title: "点击查看中继详情",
                            onclick: move |_| tab.set(crate::Tab::Relay),
                            span { class: "mini" }
                            "{chip_txt}"
                        }
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
                p { class: "hint", style: "margin-top:8px", "请先在下方选择需要共享的文件夹。" }
            }
            if dirty {
                p { class: "hint warn", style: "margin-top:8px",
                    "配置已修改，重新启动共享后生效。"
                }
            }
        }

        if let ShareStatus::Running { urls } = &status {
            section { class: "card",
                h3 { "访问地址（点击地址显示二维码）" }
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
                    placeholder: "未选择",
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
                label { "本机端口" }
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
                label { "访问范围" }
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
                        "允许局域网设备访问"
                    }
                }
            }
            div { class: "field",
                label { "功能权限" }
                div { class: "checks",
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
                        "允许打包下载"
                    }
                }
            }
            p { class: "hint",
                "本机端口为文件服务在本机的监听端口，局域网访问与公网中继均经由该端口。"
                "功能权限对所有访问者生效；设置访问密码后，上传、删除等写入操作仅登录用户可执行。"
            }
        }

        section { class: "card",
            h3 { "访问密码（留空表示不设密码）" }
            div { class: "field",
                label { "账号" }
                input {
                    r#type: "text",
                    placeholder: "用户名",
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
                        "允许未登录访客只读浏览"
                    }
                }
            }
            p { class: "hint",
                "设置密码后，上传、删除等写入操作仅登录用户可执行；未登录访客能否只读浏览，由上方选项决定。"
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
                    "开机自动启动（启动后驻留系统托盘，不显示主窗口）"
                }
            }
            if !autostart_err().is_empty() {
                p { class: "hint err", "{autostart_err}" }
            }
        }
    }
}
