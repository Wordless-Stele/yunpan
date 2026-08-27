//! 云链盘中继协议 v1。
//!
//! 形态跟 frp 一样：客户端在内网、中继在公网，**连接方向永远是客户端 → 中继**，
//! 所以内网侧不需要任何端口映射。中继上一个端口同时收两种连接（控制连接和数据连接），
//! 靠第一帧 [`Role`] 区分——这样防火墙只需放行 `control_port` 加各业务公网端口。
//!
//! ## 一次访问的完整时序
//!
//! ```text
//! 客户端                          中继                        访客浏览器
//!   │ ──Hello{Control}──────────→ │
//!   │ ←─────────Challenge{nonce}─ │
//!   │ ──Auth{mac}───────────────→ │  校验 HMAC
//!   │ ←─────────────────AuthOk──  │
//!   │ ──Bind{name,port}─────────→ │  监听 0.0.0.0:port
//!   │ ←─────────────────BindOk──  │
//!   │                             │ ←──────────── TCP 连接 ──│
//!   │ ←────NewConn{conn_id}────── │  访客 socket 挂进待配对表
//!   │ ──Hello{Data{conn_id}}────→ │
//!   │ ←─────────Challenge{nonce}─ │
//!   │ ──Auth{mac}───────────────→ │  校验 HMAC（mac 里含 conn_id）
//!   │ ←─────────────────AuthOk──  │  取出访客 socket，两条流对接
//!   │ ═══════════ 双向裸字节转发 ═══════════════════════════════│
//! ```
//!
//! ## 令牌不上线
//!
//! 鉴权是挑战-应答：中继发随机 nonce，客户端回 `HMAC-SHA256(token, 上下文||nonce)`。
//! 令牌本身永远不出现在网络上，重放也没用（nonce 一次性）。数据连接同样要过这一关，
//! 且 mac 的上下文里含 `conn_id`——否则任何人猜到 conn_id 就能把访客的连接劫走。
//!
//! ## TLS 终结在中继
//!
//! 中继配了证书（跟它的域名走，certbot 签）后，访客→中继、客户端→中继的
//! 控制/数据连接全部套 TLS（见 `tls` 模块）——路上没人能看，客户端侧零证书文件。
//! 中继自己解得开流量，但它本来就是你自己的服务器；本模块的帧格式在 TLS 之内
//! 原样不变，协议版本不因加密与否而变。

use anyhow::{bail, Context, Result};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// 协议版本。两端不一致直接拒绝——宁可报错，也别让半兼容的两版互相猜对方的意思。
pub const PROTO_VERSION: u16 = 1;

/// 单帧上限。控制帧都是几十字节的 JSON，64 KiB 足够到离谱；设上限是为了防止
/// 有人往控制端口上灌一个天文数字的长度前缀，把中继的内存吃光。
pub const MAX_FRAME_LEN: usize = 64 * 1024;

/// 访客连上来之后，等客户端来接管的最长时间。超时就把访客 socket 丢掉。
pub const PAIR_TIMEOUT_SECS: u64 = 10;

/// 客户端连上来时声明自己是哪种连接。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Role {
    /// 控制连接：一个客户端同时只该有一条，负责鉴权、申请端口、心跳、接收新连接通知。
    Control { client_id: String },
    /// 数据连接：一条只服务一个 `conn_id`，配对完成后退化成裸字节管道。
    Data { client_id: String, conn_id: u64 },
}

impl Role {
    /// 参与 HMAC 计算的上下文串。把身份和 `conn_id` 都绑进签名里，
    /// 使得「控制连接的签名」不能拿来冒充「某条数据连接」。
    pub fn context(&self) -> String {
        match self {
            Role::Control { client_id } => format!("control:{client_id}"),
            Role::Data { client_id, conn_id } => format!("data:{client_id}:{conn_id}"),
        }
    }

    pub fn client_id(&self) -> &str {
        match self {
            Role::Control { client_id } | Role::Data { client_id, .. } => client_id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMsg {
    /// 每条连接的第一帧。
    Hello { proto: u16, role: Role, agent: String },
    /// 对 [`ServerMsg::Challenge`] 的应答，`mac` 是十六进制的 HMAC-SHA256。
    Auth { mac: String },
    /// 申请把中继的某个公网端口指给自己。同一条控制连接可以申请多个。
    Bind { name: String, remote_port: u16 },
    /// 心跳应答，`ts` 原样回送 [`ServerMsg::Ping`] 里的值。
    Pong { ts: u64 },
    /// 主动下线，让中继立刻释放端口，而不是等心跳超时。
    Bye { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    Challenge { nonce: String },
    AuthOk { server: String, heartbeat_secs: u64 },
    AuthErr { reason: String },
    BindOk { name: String, remote_port: u16 },
    BindErr { name: String, reason: String },
    /// 有访客进来了，客户端应当立刻发起一条 [`Role::Data`] 连接来接管。
    NewConn { conn_id: u64, name: String, peer: String },
    Ping { ts: u64 },
    Bye { reason: String },
}

/// 计算鉴权签名。两端必须用同一个函数，否则永远对不上。
pub fn sign(token: &str, proto: u16, role: &Role, nonce_hex: &str) -> String {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(token.as_bytes())
        .expect("HMAC 接受任意长度密钥");
    mac.update(format!("{proto}|{}|{nonce_hex}", role.context()).as_bytes());
    hex_encode(&mac.finalize().into_bytes())
}

/// 定长比较，避免按字节短路比较泄露「前几位对了」的信息。
pub fn verify(token: &str, proto: u16, role: &Role, nonce_hex: &str, mac_hex: &str) -> bool {
    let expected = sign(token, proto, role, nonce_hex);
    if expected.len() != mac_hex.len() {
        return false;
    }
    expected
        .bytes()
        .zip(mac_hex.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// 帧编解码：u32 大端长度 + JSON 正文
//
// 选 JSON 不选二进制格式，是因为出问题时能直接 tcpdump 看懂。控制面的帧数以「每分钟
// 几条」计，编码开销无关紧要；数据面根本不走这套编码，是裸字节。

pub async fn write_msg<W, T>(w: &mut W, msg: &T) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let body = serde_json::to_vec(msg)?;
    if body.len() > MAX_FRAME_LEN {
        bail!("控制帧过大：{} 字节", body.len());
    }
    w.write_all(&(body.len() as u32).to_be_bytes()).await?;
    w.write_all(&body).await?;
    w.flush().await?;
    Ok(())
}

pub async fn read_msg<R, T>(r: &mut R) -> Result<T>
where
    R: AsyncRead + Unpin,
    T: for<'de> Deserialize<'de>,
{
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)
        .await
        .with_context(|| "对端已断开")?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME_LEN {
        bail!("控制帧长度越界：{len} 字节");
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    serde_json::from_slice(&body).with_context(|| "控制帧不是合法 JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn control(id: &str) -> Role {
        Role::Control {
            client_id: id.to_string(),
        }
    }

    #[test]
    fn 正确的令牌能通过校验() {
        let role = control("office");
        let mac = sign("s3cret", PROTO_VERSION, &role, "abcd");
        assert!(verify("s3cret", PROTO_VERSION, &role, "abcd", &mac));
    }

    #[test]
    fn 错误的令牌通不过校验() {
        let role = control("office");
        let mac = sign("s3cret", PROTO_VERSION, &role, "abcd");
        assert!(!verify("wrong", PROTO_VERSION, &role, "abcd", &mac));
    }

    #[test]
    fn 换了随机数的签名不能重放() {
        let role = control("office");
        let mac = sign("s3cret", PROTO_VERSION, &role, "abcd");
        assert!(!verify("s3cret", PROTO_VERSION, &role, "ef01", &mac));
    }

    #[test]
    fn 控制连接的签名不能冒充数据连接() {
        let token = "s3cret";
        let mac = sign(token, PROTO_VERSION, &control("office"), "abcd");
        let data = Role::Data {
            client_id: "office".into(),
            conn_id: 7,
        };
        assert!(!verify(token, PROTO_VERSION, &data, "abcd", &mac));
    }

    #[test]
    fn 换个客户端身份的签名也不通过() {
        let token = "s3cret";
        let mac = sign(token, PROTO_VERSION, &control("office"), "abcd");
        assert!(!verify(token, PROTO_VERSION, &control("home"), "abcd", &mac));
    }

    #[tokio::test]
    async fn 帧收发能原样还原() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        let sent = ServerMsg::NewConn {
            conn_id: 42,
            name: "dufs".into(),
            peer: "1.2.3.4:5678".into(),
        };
        write_msg(&mut a, &sent).await.unwrap();
        let got: ServerMsg = read_msg(&mut b).await.unwrap();
        match got {
            ServerMsg::NewConn { conn_id, name, peer } => {
                assert_eq!(conn_id, 42);
                assert_eq!(name, "dufs");
                assert_eq!(peer, "1.2.3.4:5678");
            }
            other => panic!("收到了别的帧：{other:?}"),
        }
    }

    #[tokio::test]
    async fn 超长的长度前缀会被拒绝而不是照着分配内存() {
        let (mut a, mut b) = tokio::io::duplex(64);
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let _ = a.write_all(&u32::MAX.to_be_bytes()).await;
        });
        let got: Result<ClientMsg> = read_msg(&mut b).await;
        assert!(got.is_err(), "越界长度必须报错");
    }
}
