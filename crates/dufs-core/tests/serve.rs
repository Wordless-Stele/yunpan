//! 库化改造后的冒烟测试：`serve()` 起来真能应答 HTTP，`shutdown()` 真能把端口还回来。
//! 上游自己的行为测试（webdav、上传、鉴权……）没搬过来——那些测的是没动过的代码。

use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    l.local_addr().unwrap().port()
}

async fn http_get(port: u16, path: &str) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf).await;
    String::from_utf8_lossy(&buf).into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn 起服务_能拿到目录页_停机后端口立即可复用() {
    // 准备一个有辨识度的目录
    let dir = std::env::temp_dir().join(format!("yunpan-dufs-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("你好.txt"), "云链盘冒烟测试").unwrap();

    let port = free_port().await;
    let yaml = format!("serve-path: {}\nbind: 127.0.0.1\nport: {port}\n", dir.display());
    let args = dufs_core::Args::from_yaml(&yaml).unwrap();
    let server = dufs_core::serve(args).unwrap();
    assert!(
        server.urls().iter().any(|u| u.contains(&port.to_string())),
        "地址列表里没有监听端口：{:?}",
        server.urls()
    );

    // 目录页应当 200，且 __INDEX_DATA__ 里带着我们放的文件名（JSON 转义为 \u 序列也认）
    let resp = http_get(port, "/").await;
    assert!(resp.starts_with("HTTP/1.1 200"), "响应头不对：{}", &resp[..resp.len().min(200)]);
    // 目录数据以 base64 嵌在 <template id="index-data"> 里，解出来才看得到文件名
    let marker = "<template id=\"index-data\">";
    let b64_start = resp.find(marker).expect("目录页里没有 index-data 模板") + marker.len();
    let b64 = &resp[b64_start..b64_start + resp[b64_start..].find('<').unwrap()];
    use base64::Engine;
    let index_json = String::from_utf8(
        base64::engine::general_purpose::STANDARD.decode(b64).expect("index-data 不是合法 base64"),
    )
    .unwrap();
    assert!(index_json.contains("你好.txt"), "目录数据里找不到测试文件：{index_json}");

    // 文件本体也下得动
    let file = http_get(port, "/%E4%BD%A0%E5%A5%BD.txt").await;
    assert!(file.contains("云链盘冒烟测试"), "文件内容不对");

    // 停机后端口必须立刻能重新 bind——「停止共享再启动」全靠这一点
    server.shutdown().await;
    let rebind = TcpListener::bind(("127.0.0.1", port)).await;
    assert!(rebind.is_ok(), "shutdown 后端口没释放");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn 端口被占时_serve_直接报错而不是悄悄失败() {
    let holder = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = holder.local_addr().unwrap().port();

    let dir = std::env::temp_dir();
    let yaml = format!("serve-path: {}\nbind: 127.0.0.1\nport: {port}\n", dir.display());
    let args = dufs_core::Args::from_yaml(&yaml).unwrap();
    let err = dufs_core::serve(args).map(|_| ()).map_err(|e| format!("{e:#}"));
    assert!(err.is_err(), "端口明明被占了却没报错");
    assert!(err.unwrap_err().contains("绑定"), "报错文案应当说明是绑定失败");
    drop(Arc::new(holder)); // 只是压住未使用告警
}
