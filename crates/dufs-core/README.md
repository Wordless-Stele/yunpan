# dufs-core —— dufs 的库化内嵌版

上游：[sigoden/dufs](https://github.com/sigoden/dufs) **v0.46.0**，MIT OR Apache-2.0。

上游是纯二进制 crate（没有 `[lib]` 目标），无法作为依赖引入，所以把源码搬了进来。
搬进来的代价是**升级要人工做**，好处是云链盘只有一个 exe：没有随包的 dufs.exe、
没有子进程存活与孤儿清理、任务管理器里也只有一个进程。

## 相对上游的改动只有四处

| 文件 | 改动 |
| --- | --- |
| `main.rs` → `lib.rs` | `serve()` 返回 `RunningServer` 句柄，支持优雅停机；上游是等 Ctrl-C 跑到进程结束 |
| `args.rs` | 剥掉 clap；`Args::parse(ArgMatches)` 换成 `Args::from_yaml` + `Args::finalize` |
| `server.rs` | 静态资源前缀里的 `env!("CARGO_PKG_VERSION")` 换成 `crate::DUFS_VERSION` 常量 |
| —— | 不带 `logger.rs`：全局 logger 归宿主应用管（`yunpan::logbus`） |

其余文件（`auth.rs` / `server.rs` / `http_logger.rs` / `http_utils.rs` / `noscript.rs` /
`utils.rs` / `assets/`）与上游 v0.46.0 **逐字节一致**——不要顺手格式化或改中文注释，
否则下次升级就无法用 `diff` 分辨「哪些是我们的改动」。

## 升级步骤

```bash
curl -sL https://github.com/sigoden/dufs/archive/refs/tags/vX.Y.Z.tar.gz | tar xz
cp dufs-X.Y.Z/src/{args,auth,http_logger,http_utils,server,utils,noscript}.rs src/
cp dufs-X.Y.Z/assets/* assets/
# 然后照上表重做四处改动，并把 lib.rs 的 DUFS_VERSION 与本文件的版本号一起改掉
```

`DUFS_VERSION` 忘了改的症状：浏览器缓存的 `index.js` 与新版 `index.html` 对不上，
页面白屏或功能错乱，而硬刷新就好——典型的静态资源前缀没换。
