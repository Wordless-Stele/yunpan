//! 与 dufs 对话的 HTTP 客户端。走的全是 dufs 自己的公开 API：
//! `PUT` 上传、`GET` 下载、`?json` 列目录、`?q=&json` 搜索、`DELETE` 删除、
//! `MKCOL` 建目录。鉴权用 Basic——dufs 虽默认发 Digest 质询，但 Basic 一直认
//! （见 dufs-core/src/auth.rs 对 `Basic ` 前缀的处理）。

use anyhow::{bail, Context as _, Result};
use serde::Deserialize;

/// 连接目标。优先环境变量（AI 在别的机器上、走中继地址时用），
/// 否则读桌面端落盘的 config.json，指向本机 dufs。
#[derive(Debug, Clone)]
pub struct Target {
    pub base_url: String,
    pub user: Option<String>,
    pub pass: Option<String>,
}

impl Target {
    // e2e 测试用 #[path] 把本文件编进测试二进制，测试里 Target 直接手填，
    // resolve 只有 main 用——在测试那个编译单元里是死代码，属预期
    #[allow(dead_code)]
    pub fn resolve() -> Result<Self> {
        if let Ok(base_url) = std::env::var("YUNPAN_BASE_URL") {
            return Ok(Self {
                base_url: base_url.trim_end_matches('/').to_string(),
                user: std::env::var("YUNPAN_USER").ok().filter(|s| !s.is_empty()),
                pass: std::env::var("YUNPAN_PASS").ok().filter(|s| !s.is_empty()),
            });
        }
        // 桌面端的配置：拿端口和账号密码，凑出本机地址。
        // 松散解析（Value 而不是共享结构体）——agent 不依赖 GUI crate，
        // 配置文件多几个字段少几个字段都不至于把它弄崩。
        let path = dirs::config_dir()
            .context("找不到系统配置目录")?
            .join("yunpan")
            .join("config.json");
        let cfg: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&path).with_context(|| {
                format!(
                    "读不到云链盘配置 {}。要么先在桌面端跑一次云链盘，\
                     要么用 YUNPAN_BASE_URL 环境变量直接指定地址",
                    path.display()
                )
            })?,
        )?;
        let port = cfg.get("port").and_then(|v| v.as_u64()).unwrap_or(5000);
        let user = cfg
            .get("auth_user")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from);
        let pass = cfg
            .get("auth_pass")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from);
        // 账号密码必须成对：只有密码没账号等于没配
        let pass = pass.filter(|_| user.is_some());
        Ok(Self {
            base_url: format!("http://127.0.0.1:{port}"),
            user,
            pass,
        })
    }

    /// 远程路径 → 完整 URL。逐段百分号编码，中文文件名照样走。
    pub fn url(&self, remote_path: &str) -> String {
        let encoded: Vec<String> = remote_path
            .split('/')
            .filter(|s| !s.is_empty())
            .map(|seg| urlencoding::encode(seg).into_owned())
            .collect();
        format!("{}/{}", self.base_url, encoded.join("/"))
    }

    fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        let rb = reqwest::Client::new().request(method, url);
        match (&self.user, &self.pass) {
            (Some(u), Some(p)) => rb.basic_auth(u, Some(p)),
            _ => rb,
        }
    }
}

/// dufs `?json` 返回的目录项（只取要用的字段）。
#[derive(Debug, Deserialize)]
pub struct PathItem {
    pub path_type: String,
    pub name: String,
    pub size: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct IndexData {
    #[serde(default)]
    paths: Vec<PathItem>,
}

fn ensure_ok(resp: &reqwest::Response, doing: &str) -> Result<()> {
    let code = resp.status();
    if code.is_success() {
        return Ok(());
    }
    bail!(match code.as_u16() {
        401 => format!("{doing}失败：需要账号密码（设 YUNPAN_USER / YUNPAN_PASS，或与共享页一致）"),
        403 => format!("{doing}失败：没有权限——共享页对应的开关（允许上传/删除）没开"),
        404 => format!("{doing}失败：路径不存在"),
        _ => format!("{doing}失败：HTTP {code}"),
    })
}

pub async fn upload(t: &Target, local_path: &str, remote_path: &str) -> Result<String> {
    let file = tokio::fs::File::open(local_path)
        .await
        .with_context(|| format!("打不开本地文件 {local_path}"))?;
    let size = file.metadata().await?.len();
    let stream = tokio_util::io::ReaderStream::new(file);
    let url = t.url(remote_path);
    let resp = t
        .request(reqwest::Method::PUT, &url)
        .body(reqwest::Body::wrap_stream(stream))
        .send()
        .await
        .with_context(|| "连不上云链盘（共享启动了吗？）")?;
    ensure_ok(&resp, "上传")?;
    Ok(format!("已上传 {local_path}（{size} 字节）→ {url}"))
}

pub async fn download(t: &Target, remote_path: &str, local_path: &str) -> Result<String> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;
    let resp = t
        .request(reqwest::Method::GET, &t.url(remote_path))
        .send()
        .await
        .with_context(|| "连不上云链盘（共享启动了吗？）")?;
    ensure_ok(&resp, "下载")?;
    if let Some(parent) = std::path::Path::new(local_path).parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    let mut file = tokio::fs::File::create(local_path)
        .await
        .with_context(|| format!("建不了本地文件 {local_path}"))?;
    let mut stream = resp.bytes_stream();
    let mut total = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        total += chunk.len() as u64;
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    Ok(format!("已下载 {remote_path}（{total} 字节）→ {local_path}"))
}

async fn fetch_index(t: &Target, url: String, doing: &str) -> Result<Vec<PathItem>> {
    let resp = t
        .request(reqwest::Method::GET, &url)
        .send()
        .await
        .with_context(|| "连不上云链盘（共享启动了吗？）")?;
    ensure_ok(&resp, doing)?;
    Ok(resp.json::<IndexData>().await?.paths)
}

fn render_items(items: &[PathItem], empty_hint: &str) -> String {
    if items.is_empty() {
        return empty_hint.to_string();
    }
    items
        .iter()
        .map(|p| {
            let kind = if p.path_type.starts_with("Dir") { "目录" } else { "文件" };
            match p.size {
                Some(size) if kind == "文件" => format!("{kind}  {}  {size} 字节", p.name),
                _ => format!("{kind}  {}", p.name),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub async fn list_dir(t: &Target, path: &str) -> Result<String> {
    let items = fetch_index(t, format!("{}?json", t.url(path)), "列目录").await?;
    Ok(render_items(&items, "（空目录）"))
}

pub async fn search(t: &Target, query: &str, path: &str) -> Result<String> {
    let url = format!("{}?q={}&json", t.url(path), urlencoding::encode(query));
    let items = fetch_index(t, url, "搜索").await?;
    Ok(render_items(&items, "没搜到"))
}

pub async fn delete(t: &Target, remote_path: &str) -> Result<String> {
    let resp = t
        .request(reqwest::Method::DELETE, &t.url(remote_path))
        .send()
        .await
        .with_context(|| "连不上云链盘（共享启动了吗？）")?;
    ensure_ok(&resp, "删除")?;
    Ok(format!("已删除 {remote_path}"))
}

pub async fn make_dir(t: &Target, remote_path: &str) -> Result<String> {
    let resp = t
        .request(reqwest::Method::from_bytes(b"MKCOL")?, &t.url(remote_path))
        .send()
        .await
        .with_context(|| "连不上云链盘（共享启动了吗？）")?;
    ensure_ok(&resp, "建目录")?;
    Ok(format!("已建目录 {remote_path}"))
}

pub async fn status(t: &Target) -> Result<String> {
    let url = format!("{}/__dufs__/health", t.base_url);
    match t.request(reqwest::Method::GET, &url).send().await {
        Ok(resp) if resp.status().is_success() => {
            Ok(format!("共享在线：{}", t.base_url))
        }
        _ => Ok(format!(
            "共享不在线（{}）。到云链盘桌面端点「启动共享」，或检查 YUNPAN_BASE_URL",
            t.base_url
        )),
    }
}
