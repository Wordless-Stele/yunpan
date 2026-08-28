//! 「日志」页：dufs 的访问日志与隧道的连接日志，从 logbus 环形缓冲里读。

use dioxus::prelude::*;

#[component]
pub fn LogsView() -> Element {
    let mut lines = use_signal(crate::logbus::snapshot);

    // 每秒对一次号，变了才拷贝。轮询比回调省事且够用——日志页不追求毫秒级实时。
    use_future(move || async move {
        let mut last_seq = crate::logbus::seq();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let seq = crate::logbus::seq();
            if seq != last_seq {
                last_seq = seq;
                lines.set(crate::logbus::snapshot());
            }
        }
    });

    rsx! {
        section { class: "card",
            h3 { "运行日志（最近 500 条，最新在前）" }
            if lines().is_empty() {
                div { class: "empty", "暂无日志。启动共享后，访问与传输记录将显示于此。" }
            } else {
                div { class: "log-list",
                    for line in lines() {
                        div {
                            class: match line.level {
                                log::Level::Error => "log-line error",
                                log::Level::Warn => "log-line warn",
                                _ => "log-line",
                            },
                            span { class: "log-time", "{line.time}" }
                            span { class: "log-text", "{line.text}" }
                        }
                    }
                }
            }
        }
    }
}
