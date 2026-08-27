# 云链盘（YunPan）

把 Windows/Mac 电脑上的一个文件夹变成网页网盘：局域网同事直接访问，外网访客经
Linux 中继穿透进来。文件服务用 [dufs](https://github.com/sigoden/dufs)，但**不是**
拉起 dufs.exe——dufs 的源码被 vendor 成库编进了 GUI 进程，安装包里只有一个可执行
文件，任务管理器里也只有一个进程。GUI 是 Dioxus 0.7 桌面端（Windows / macOS /
Linux 全平台），关窗即收进系统托盘，图标随共享状态变色（灰 = 未共享，朱砂红 = 共享中）。

```text
┌───────────── 桌面端（yunpan，一个 exe）─────────────┐
│  Dioxus GUI ── 托盘图标 ── 日志页                    │
│  dufs-core：进程内起 HTTP 文件服务（端口 5000）      │      ┌── Linux 公网机 ──┐
│  yunpan-tunnel::client：主动连出去 ──────────────────┼─────→│ yunpan-relay      │←── 外网访客
└─────────────────────────────────────────────────────┘ 7100 │ 8080 → 转发回来   │    :8080
             内网（无需任何端口映射）                          └───────────────────┘
```

## Workspace 成员

| crate | 是什么 |
| --- | --- |
| `.`（yunpan） | 桌面 GUI。共享 / 中继 / 日志三页 + 系统托盘 |
| `crates/dufs-core` | dufs v0.46.0 的库化内嵌版。相对上游只有四处改动，见其 README |
| `crates/tunnel` | 自研穿透协议（frp 形态：挑战-应答鉴权、令牌不上线、中继只转发裸字节） |
| `crates/relay-server` | `yunpan-relay` 二进制，跑在 Linux 公网机上 |

## 常用命令

```bash
cargo test --workspace                    # 全部测试（含隧道端到端：本机环回起真实中继）
cargo clippy --workspace --all-targets    # CI 不跑 clippy，提交前自己过
dx serve                                  # 开发（桌面窗口 + 控制台日志）
dx bundle --release --platform windows --package-types nsis   # Windows 安装包（要在 Windows 上跑）
cargo build --release -p yunpan-relay     # 中继端（Linux 上跑；Mac 编译只为验证）
```

**Windows/macOS 安装包在原生 runner 上出**（`.github/workflows/release.yml`，推 `v*`
标签触发）：ring、liblzma 这些 C 依赖从 Mac 交叉编 MSVC 编不过，本机 `cargo check
--target x86_64-pc-windows-msvc` 会死在 build script 上，不是代码问题。

## 中继端部署（Linux）

```bash
# 1. 出二进制：CI relay-linux-* 产物，或在服务器上 cargo build --release -p yunpan-relay
cp yunpan-relay /usr/local/bin/

# 2. 配置 + 生成令牌
mkdir -p /etc/yunpan
cp deploy/relay.toml.example /etc/yunpan/relay.toml
yunpan-relay token          # 生成令牌，粘进 relay.toml 的 clients.token
yunpan-relay --config /etc/yunpan/relay.toml check

# 3. systemd
cp deploy/yunpan-relay.service /etc/systemd/system/
systemctl daemon-reload && systemctl enable --now yunpan-relay

# 4. 防火墙放行 control_port（7100）和白名单里的公网端口（如 8080）
```

桌面端「中继」页里填：服务器地址、控制端口 7100、客户端 ID、令牌、公网端口 8080，
勾上「启动共享时自动打通公网中继」即可。

## 安全模型（一句话版）

- 令牌永不上线：挑战-应答 HMAC-SHA256，重放无效；数据连接的签名绑定 conn_id，劫不走。
- TLS 终结在中继：配了证书后访客 HTTPS、隧道 TLS 一把闸全开，路上没人能看；
  中继解得开流量，但它就是你自己的服务器。
- 端口白名单：客户端只能申请中继配置里列给它的端口，令牌泄露也抢不走 22/443。
- 文件访问控制归 dufs：账号密码（Digest）、访客只读、上传/删除开关，都在共享页。

## 桌面端功能一览

- 共享页：选文件夹、端口、权限开关、账号密码（Digest）、**开机自启**（写
  plist / 注册表 / XDG autostart，登录时带 `--hidden` 静默进托盘）。
- 中继页：服务器地址、令牌、**TLS 加密开关**。证书只配在中继上（certbot 跟域名签），
  开着加密时访客→中继是正经 HTTPS、客户端→中继的隧道同样 TLS——**桌面端零证书文件**；
  本地 dufs 只对 127.0.0.1/局域网说话，保持明文。
- **单实例**（仅发布版）：再次双击不重开，通知已有实例把窗口拉到前台（127.0.0.1:17654
  握手，端口避开临时端口范围）。
- 图标：`logos/` 里是品牌概念稿与预览页，`assets/icons/` 是选定稿导出的成品
  （`app.svg` 源、窗口 128px、托盘两态 32px），`assets/icon.ico|png` 给打包器。
  重新导出：`resvg -w <尺寸> assets/icons/app.svg out.png`，ICO 用 PIL 打包。

## 已知取舍

- `panic = "abort"`（release）：dufs 里靠 unwind 兜底的路径会直接崩——宁可明确崩溃，
  不带半坏状态继续跑。
- 中继掉线自动重连（指数退避 1→30s）；令牌错、端口不在白名单这类**配置错误不重试**，
  界面上停在红色 Fatal 状态等人改。
- 升级 dufs：照 `crates/dufs-core/README.md` 的四处改动清单重做，别顺手改 vendor 文件。
