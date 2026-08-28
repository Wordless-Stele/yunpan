//! 云链盘 MCP Agent —— 让 AI 通过 MCP 工具往共享盘里传文件、取文件、检索。
//!
//! 注册到 Claude Code：
//! ```bash
//! claude mcp add yunpan -- /path/to/yunpan-agent
//! # 远程（AI 在别的机器、走中继地址）：
//! claude mcp add yunpan -e YUNPAN_BASE_URL=https://relay.example.com:8080 \
//!     -e YUNPAN_USER=boss -e YUNPAN_PASS=xxx -- /path/to/yunpan-agent
//! ```
//! 不带环境变量时读桌面端的 config.json，直连本机 dufs。

mod api;
mod mcp;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let target = api::Target::resolve()?;
    eprintln!("[yunpan-agent] 目标：{}", target.base_url);

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(resp) = mcp::handle_line(&target, &line).await {
            stdout.write_all(resp.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}
