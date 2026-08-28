//! 端到端：进程内起一个真 dufs（dufs-core），MCP 工具挨个打真实 HTTP。
//! 上传→列目录→搜索→下载→删除全链路，外加鉴权（Basic 对 dufs 的 Digest 默认）验证。

use serde_json::{json, Value};
use yunpan_agent_test_helpers::*;

// 把 agent 的模块直接编进测试（bin crate 没有 lib target，用 include 共享源码）
#[path = "../src/api.rs"]
mod api;
#[path = "../src/mcp.rs"]
mod mcp;

mod yunpan_agent_test_helpers {
    /// 挑一个可用端口。不用「bind 0 读回端口再释放」——workspace 并发跑多个测试
    /// 进程时，释放到复用的空当里内核会把同一个端口分给别人（实测撞过）。
    /// 改成 PID 加盐的候选序列 + 探测循环：各进程各扫各的序列，互不相撞；
    /// 候选压在 49152 以下，内核给出站连接随机分配的临时端口也永远撞不进来。
    pub fn free_port() -> u16 {
        use std::net::TcpListener;
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        loop {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let candidate = (17000 + (std::process::id().wrapping_mul(61) + n * 13) % 30000) as u16;
            if TcpListener::bind(("127.0.0.1", candidate)).is_ok() {
                return candidate;
            }
        }
    }
}

fn start_dufs(dir: &std::path::Path, port: u16, auth: Option<(&str, &str)>) -> dufs_core::RunningServer {
    let auth_line = match auth {
        Some((u, p)) => format!("auth:\n  - {u}:{p}@/:rw\n"),
        None => String::new(),
    };
    let yaml = format!(
        "serve-path: {}\nbind: 127.0.0.1\nport: {port}\nallow-upload: true\nallow-delete: true\nallow-search: true\n{auth_line}",
        dir.display()
    );
    let args = dufs_core::Args::from_yaml(&yaml).unwrap();
    dufs_core::serve(args).unwrap()
}

async fn call(t: &api::Target, name: &str, args: Value) -> (bool, String) {
    let line = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": name, "arguments": args }
    })
    .to_string();
    let resp: Value = serde_json::from_str(&mcp::handle_line(t, &line).await.unwrap()).unwrap();
    (
        resp["result"]["isError"].as_bool().unwrap(),
        resp["result"]["content"][0]["text"].as_str().unwrap().to_string(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn 七个工具对真_dufs_全链路跑通() {
    let share = std::env::temp_dir().join(format!("yunpan-agent-e2e-{}", std::process::id()));
    let local = share.join("_本地侧");
    std::fs::create_dir_all(&local).unwrap();
    let src = local.join("源文件.txt");
    std::fs::write(&src, "AI 经 MCP 上传的内容").unwrap();

    let port = free_port();
    let server = start_dufs(&share, port, None);
    let t = api::Target {
        base_url: format!("http://127.0.0.1:{port}"),
        user: None,
        pass: None,
    };

    let (err, text) = call(&t, "status", json!({})).await;
    assert!(!err, "{text}");
    assert!(text.contains("在线"), "{text}");

    let (err, text) = call(&t, "make_dir", json!({ "remote_path": "报告" })).await;
    assert!(!err, "{text}");

    let (err, text) = call(&t, "upload_file", json!({
        "local_path": src.to_str().unwrap(),
        "remote_path": "报告/月报.txt"
    })).await;
    assert!(!err, "{text}");
    assert!(share.join("报告/月报.txt").exists(), "上传的文件没落盘");

    let (err, text) = call(&t, "list_dir", json!({ "path": "报告" })).await;
    assert!(!err, "{text}");
    assert!(text.contains("月报.txt"), "列表里没有刚传的文件：{text}");

    let (err, text) = call(&t, "search", json!({ "query": "月报" })).await;
    assert!(!err, "{text}");
    assert!(text.contains("月报.txt"), "搜索没找到：{text}");

    let dst = local.join("取回.txt");
    let (err, text) = call(&t, "download_file", json!({
        "remote_path": "报告/月报.txt",
        "local_path": dst.to_str().unwrap()
    })).await;
    assert!(!err, "{text}");
    assert_eq!(std::fs::read_to_string(&dst).unwrap(), "AI 经 MCP 上传的内容");

    let (err, text) = call(&t, "delete", json!({ "remote_path": "报告/月报.txt" })).await;
    assert!(!err, "{text}");
    assert!(!share.join("报告/月报.txt").exists(), "删除后文件还在");

    server.shutdown().await;
    let _ = std::fs::remove_dir_all(&share);
}

#[tokio::test(flavor = "multi_thread")]
async fn 开了鉴权后_basic_能过_不带凭据被拦() {
    let share = std::env::temp_dir().join(format!("yunpan-agent-auth-{}", std::process::id()));
    std::fs::create_dir_all(&share).unwrap();
    let src = share.join("s.txt");
    std::fs::write(&src, "x").unwrap();

    let port = free_port();
    let server = start_dufs(&share, port, Some(("boss", "12345678")));

    // 带对的 Basic 凭据：能传
    let t_ok = api::Target {
        base_url: format!("http://127.0.0.1:{port}"),
        user: Some("boss".into()),
        pass: Some("12345678".into()),
    };
    let (err, text) = call(&t_ok, "upload_file", json!({
        "local_path": src.to_str().unwrap(),
        "remote_path": "a.txt"
    })).await;
    assert!(!err, "{text}");

    // 不带凭据：被拦，且文案指路
    let t_anon = api::Target {
        base_url: format!("http://127.0.0.1:{port}"),
        user: None,
        pass: None,
    };
    let (err, text) = call(&t_anon, "upload_file", json!({
        "local_path": src.to_str().unwrap(),
        "remote_path": "b.txt"
    })).await;
    assert!(err, "没鉴权居然传上了");
    assert!(text.contains("账号密码"), "文案没指路：{text}");

    server.shutdown().await;
    let _ = std::fs::remove_dir_all(&share);
}
