# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

文档、注释、测试名、错误文案全中文，沿用。

## 是什么

云链盘：Dioxus 0.7 全平台桌面 GUI（主要交付 Windows），内嵌 dufs 做文件共享 +
自研 frp 形态隧道穿透到 Linux 中继。**一个 exe 就是全部**——dufs 不是子进程，
是 vendor 成库编进来的（`crates/dufs-core`，上游 v0.46.0，只有四处改动，
清单在其 README；升级照清单重做，其余文件必须与上游逐字节一致，别格式化它们）。

## 命令

```bash
cargo test --workspace                    # 29 项，含隧道端到端（环回起真实中继）
cargo clippy --workspace --all-targets    # 没有 CI 闸门，提交前必须自己跑
dx serve                                  # 开发跑 GUI（dx 0.7.10）
```

Windows 包只能在原生 Windows 上出（ring/liblzma 交叉编不过 MSVC，是 build
script 问题不是代码问题）；推 `v*` 标签走 `.github/workflows/release.yml`。

## 结构与关键约束

- `src/engine.rs`：dufs 与隧道跑在**专属 tokio 运行时**（`RT`）上，不借 Dioxus 的
  执行器——大目录打包不能卡 UI。所有启停都是 `RT.spawn(...).await`，幂等（先停旧的）。
- `src/logbus.rs`：全进程唯一的 `log` logger。dufs-core 特意不带上游 logger.rs，
  谁也不要再 `set_boxed_logger`。
- `crates/tunnel/src/protocol.rs` 顶部注释是协议全貌（时序图 + 为什么令牌不上线）。
  改协议先改 `PROTO_VERSION`，两端不同版本直接拒连，没有兼容模式。
- 中继侧端口白名单是安全边界不是摆设（见 `ClientAuth::ports` 注释）；
  数据连接的 HMAC 绑 conn_id，防接管别人的访客连接——这两处别「简化」。
- `RelayStatus::Fatal`（令牌错/端口不在白名单）**不重试也不许被 Idle 盖掉**，
  测试 `令牌错误的客户端会停在_fatal_而不是无限重试` 盯着这条。
- GUI 配置改动即存盘（`config.json`）；共享跑着时改配置不热生效，界面提示重启。
- `src/autostart.rs`：OS 是唯一真相（plist/注册表/.desktop 存在与否），不进
  AppConfig；自启条目带 `--hidden`。单实例在 main.rs（仅 release 编译，端口 17654，
  比 ProxyZms 的 17653 加一，两应用同机不打架）。
- TLS 终结在中继（用户定的架构，别改回客户端配证书）：`RelayConfig.tls_cert/key`
  一把闸控三种连接（访客 HTTPS + 控制 + 数据），客户端 `ClientConfig.tls` 用系统
  根证书验中继域名，`extra_trust_der` 仅供测试/自签（界面不暴露）。本地 dufs 恒明文
  （dufs-core 的 tls 能力保留未用）。帧格式在 TLS 之内不变，PROTO_VERSION 不动。
- 图标源在 `logos/`（概念稿+preview.html），成品在 `assets/icons/`（include_bytes
  进二进制）。换 logo：改 `assets/icons/app.svg` 与 tray-on/off.svg 后用 resvg 重导，
  尺寸认准 128/32/32，`assets/icon.ico|png` 用 PIL 重打。
- 托盘写法抄的 ProxyZms（muda + tray 两个 handler 都挂、共用处理函数），
  动托盘前先看那边 main.rs 的注释，坑都写在那里。
