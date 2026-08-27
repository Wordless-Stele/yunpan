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
            h3 { "运行日志（最近 500 条，新的在上面）" }
            if lines().is_empty() {
                div { class: "empty", "还没有日志。启动共享后，谁来看过、下过什么都会记在这里。" }
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
