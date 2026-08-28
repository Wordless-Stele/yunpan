//! 云链盘中继端 —— 跑在公网 Linux 服务器上的常驻进程。
//!
//! 一个进程、一个 TOML 配置、一个 systemd 服务，没有数据库也没有状态文件。
//! 客户端从内网主动连上来，所以内网侧不需要任何端口映射；中继只需在防火墙上
//! 放行控制端口和各客户端申请的公网端口。
//!
//! ```bash
//! yunpan-relay --config /etc/yunpan/relay.toml   # 正常启动
//! yunpan-relay token                             # 生成一个随机令牌，填进配置
//! ```

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rand::RngCore;
use std::path::PathBuf;
use std::sync::Arc;
use yunpan_tunnel::{Relay, RelayConfig};

#[derive(Parser)]
#[command(name = "yunpan-relay", version, about = "云链盘中继端")]
struct Cli {
    /// 配置文件路径
    #[arg(short, long, default_value = "/etc/yunpan/relay.toml")]
    config: PathBuf,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// 生成一个随机令牌（32 字节，十六进制）
    Token,
    /// 只检查配置文件是否合法，不启动服务
    Check,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if let Some(Command::Token) = cli.command {
        let mut buf = [0u8; 32];
        rand::rng().fill_bytes(&mut buf);
        println!("{}", buf.iter().map(|b| format!("{b:02x}")).collect::<String>());
        return Ok(());
    }

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cfg = load_config(&cli.config)?;

    if let Some(Command::Check) = cli.command {
        println!(
            "配置合法：控制端口 {}（{}），{} 个客户端",
            cfg.control_port,
            if cfg.tls_cert.is_some() { "TLS" } else { "明文——建议配上 tls_cert/tls_key" },
            cfg.clients.len()
        );
        for client in &cfg.clients {
            println!("  - {} 可用端口 {:?}", client.id, client.ports);
        }
        return Ok(());
    }

    Arc::new(Relay::new(cfg)?).run().await
}

fn load_config(path: &PathBuf) -> Result<RelayConfig> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("读取配置 {} 失败", path.display()))?;
    let cfg: RelayConfig =
        toml::from_str(&text).with_context(|| format!("解析配置 {} 失败", path.display()))?;

    match (&cfg.tls_cert, &cfg.tls_key) {
        (Some(_), Some(_)) | (None, None) => {}
        _ => anyhow::bail!("tls_cert 和 tls_key 要么都给要么都不给"),
    }
    if cfg.clients.is_empty() {
        anyhow::bail!("配置里一个客户端都没有，中继起来也没人能用");
    }
    for client in &cfg.clients {
        if client.token.len() < 16 {
            anyhow::bail!(
                "客户端 {} 的令牌太短（{} 字符）。用 `yunpan-relay token` 生成一个",
                client.id,
                client.token.len()
            );
        }
        if client.ports.is_empty() && cfg.path_router_port.is_none() {
            anyhow::bail!(
                "客户端 {} 没有可用端口。要么给它端口白名单，要么开启 path_router_port 走路径路由",
                client.id
            );
        }
        if Some(cfg.control_port) == cfg.path_router_port {
            anyhow::bail!("path_router_port 不能与控制端口相同");
        }
        if client.ports.contains(&cfg.control_port) {
            anyhow::bail!(
                "客户端 {} 的端口白名单里有控制端口 {}，会把中继自己顶掉",
                client.id,
                cfg.control_port
            );
        }
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn 写临时配置(body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("yunpan-relay-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.toml", body.len()));
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn 令牌太短的配置会被拒绝() {
        let path = 写临时配置(
            r#"
control_port = 7100
[[clients]]
id = "office"
token = "short"
ports = [8080]
"#,
        );
        let err = load_config(&path).unwrap_err().to_string();
        assert!(err.contains("令牌太短"), "实际报错：{err}");
    }

    #[test]
    fn 白名单里混进控制端口会被拒绝() {
        let path = 写临时配置(
            r#"
control_port = 7100
[[clients]]
id = "office"
token = "0123456789abcdef0123"
ports = [7100]
"#,
        );
        let err = load_config(&path).unwrap_err().to_string();
        assert!(err.contains("控制端口"), "实际报错：{err}");
    }

    #[test]
    fn 只给一半_tls_配置会被拒绝() {
        let path = 写临时配置(
            r#"
control_port = 7100
tls_cert = "/etc/yunpan/fullchain.pem"
[[clients]]
id = "office"
token = "0123456789abcdef0123"
ports = [8080]
"#,
        );
        let err = load_config(&path).unwrap_err().to_string();
        assert!(err.contains("要么都给"), "实际报错：{err}");
    }

    #[test]
    fn 一个合法配置能读进来() {
        let path = 写临时配置(
            r#"
bind = "0.0.0.0"
control_port = 7100
public_host = "yz.example.com"
[[clients]]
id = "office"
token = "0123456789abcdef0123"
ports = [8080, 8443]
"#,
        );
        let cfg = load_config(&path).unwrap();
        assert_eq!(cfg.control_port, 7100);
        assert_eq!(cfg.clients[0].ports, vec![8080, 8443]);
    }
}
