//! 云链盘的内网穿透隧道，客户端与中继两侧共用一套协议定义。
//!
//! 为什么不直接用 frp：frp 是 Go 写的独立二进制，塞进来就等于 Windows 端要多带一个
//! frpc.exe、多管一个子进程。云链盘的取舍是「一个 exe 就是全部」，所以自己实现了
//! 同形态的协议（见 [`protocol`] 的时序图），客户端直接编在 GUI 进程里。

pub mod client;
pub mod protocol;
pub mod server;

pub use client::{ClientConfig, ClientStats, RelayStatus, TunnelHandle};
pub use protocol::PROTO_VERSION;
pub use server::{ClientAuth, Relay, RelayConfig};
