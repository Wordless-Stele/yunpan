//! 应用配置，持久化到 `<config_dir>/yunpan/config.json`。
//!
//! 界面改一项存一项；「启动共享」时把整份配置翻译成 dufs 的 YAML（见
//! [`AppConfig::to_dufs_yaml`]）——走上游 `-c config.yaml` 同一条解析路径，
//! 字段语义与 dufs 文档一字不差。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// 要共享的文件夹。空串表示还没选过，界面上禁用「启动」。
    pub serve_path: String,
    pub port: u16,
    /// true = 绑 0.0.0.0（局域网可见），false = 只绑 127.0.0.1（仅本机，配中继用）。
    pub lan_visible: bool,
    pub allow_upload: bool,
    pub allow_delete: bool,
    pub allow_search: bool,
    pub allow_archive: bool,
    /// 账号密码。都非空才启用鉴权；启用后匿名访客按 `guest_readonly` 决定能不能只读。
    pub auth_user: String,
    pub auth_pass: String,
    pub guest_readonly: bool,
    /// 打开软件即自动开启共享。与「开机自动启动」配合，登录后即在托盘后台共享。
    pub auto_start_share: bool,
    pub relay: RelaySettings,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RelaySettings {
    /// 启动共享时是否顺带拉起中继隧道。
    pub enabled: bool,
    /// 中继服务器地址（Linux 公网机的域名或 IP）。
    pub host: String,
    pub control_port: u16,
    pub client_id: String,
    pub token: String,
    /// 想占用的中继公网端口，需在中继侧白名单里。
    pub remote_port: u16,
    /// 与中继之间走 TLS（中继配了证书就开着）。开着时访客侧就是正经 HTTPS，
    /// 客户端不需要任何证书文件。
    pub tls: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            serve_path: String::new(),
            port: 5000,
            lan_visible: true,
            allow_upload: false,
            allow_delete: false,
            allow_search: true,
            allow_archive: true,
            auth_user: String::new(),
            auth_pass: String::new(),
            guest_readonly: true,
            auto_start_share: false,
            relay: RelaySettings::default(),
        }
    }
}

impl Default for RelaySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            control_port: 7100,
            client_id: String::new(),
            token: String::new(),
            remote_port: 8080,
            // 默认开：正经部署中继都该有证书；内网联调再手动关
            tls: true,
        }
    }
}

fn config_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("yunpan").join("config.json"))
}

impl AppConfig {
    pub fn load() -> Self {
        config_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// 存盘失败只记日志不打断操作——配置丢了下次还能再填，弹窗打断共享才是事故。
    pub fn save(&self) {
        let Some(path) = config_path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    log::warn!("保存配置失败：{e}");
                }
            }
            Err(e) => log::warn!("序列化配置失败：{e}"),
        }
    }

    /// 鉴权是否启用（账号密码都填了才算）。
    pub fn auth_enabled(&self) -> bool {
        !self.auth_user.trim().is_empty() && !self.auth_pass.trim().is_empty()
    }


    /// 翻译成 dufs 的配置 YAML。
    pub fn to_dufs_yaml(&self) -> String {
        /// 只为序列化 YAML 而生的镜像结构；字段名与 dufs 文档一致。
        #[derive(Serialize)]
        #[serde(rename_all = "kebab-case")]
        struct DufsYaml {
            serve_path: String,
            bind: String,
            port: u16,
            allow_upload: bool,
            allow_delete: bool,
            allow_search: bool,
            allow_archive: bool,
            #[serde(skip_serializing_if = "Vec::is_empty")]
            auth: Vec<String>,
        }

        let mut auth = vec![];
        if self.auth_enabled() {
            auth.push(format!(
                "{}:{}@/:rw",
                self.auth_user.trim(),
                self.auth_pass.trim()
            ));
            if self.guest_readonly {
                auth.push("@/".to_string());
            }
        }

        let doc = DufsYaml {
            serve_path: self.serve_path.clone(),
            // 开了中继但不想露局域网时绑回环；否则全网卡
            bind: if self.lan_visible { "0.0.0.0" } else { "127.0.0.1" }.to_string(),
            port: self.port,
            allow_upload: self.allow_upload,
            allow_delete: self.allow_delete,
            allow_search: self.allow_search,
            allow_archive: self.allow_archive,
            auth,
        };
        serde_yaml::to_string(&doc).expect("配置镜像结构必然可序列化")
    }

    /// 供隧道客户端用的连接参数。None 表示中继没配全。
    pub fn tunnel_config(&self) -> Option<yunpan_tunnel::ClientConfig> {
        let r = &self.relay;
        if !r.enabled
            || r.host.trim().is_empty()
            || r.client_id.trim().is_empty()
            || r.token.trim().is_empty()
        {
            return None;
        }
        Some(yunpan_tunnel::ClientConfig {
            relay_host: r.host.trim().to_string(),
            control_port: r.control_port,
            client_id: r.client_id.trim().to_string(),
            token: r.token.trim().to_string(),
            name: "dufs".to_string(),
            remote_port: r.remote_port,
            local_port: self.port,
            tls: r.tls,
            extra_trust_der: None,
        })
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)] // 测试里逐字段摆条件更直观
mod tests {
    use super::*;

    #[test]
    fn 默认配置翻译出来的_yaml_能被_dufs_解析() {
        let mut cfg = AppConfig::default();
        cfg.serve_path = std::env::temp_dir().to_string_lossy().to_string();
        let yaml = cfg.to_dufs_yaml();
        let args = dufs_core::Args::from_yaml(&yaml).expect("dufs 解析失败");
        assert_eq!(args.port, 5000);
        assert!(!args.allow_upload);
        assert!(args.allow_search);
    }

    #[test]
    fn 填了账号密码后_yaml_里有鉴权规则() {
        let mut cfg = AppConfig::default();
        cfg.serve_path = std::env::temp_dir().to_string_lossy().to_string();
        cfg.auth_user = "boss".into();
        cfg.auth_pass = "12345678".into();
        let yaml = cfg.to_dufs_yaml();
        assert!(yaml.contains("boss:12345678@/:rw"), "实际 YAML：\n{yaml}");
        assert!(yaml.contains("'@/'") || yaml.contains("\"@/\""), "访客只读规则丢了：\n{yaml}");
        // 且 dufs 真的认
        dufs_core::Args::from_yaml(&yaml).expect("dufs 解析失败");
    }

    #[test]
    fn 本地_dufs_一律明文_yaml_里不出现_tls() {
        // TLS 终结在中继：本地 dufs 只服务 127.0.0.1/局域网，明文即可
        let mut cfg = AppConfig::default();
        cfg.serve_path = std::env::temp_dir().to_string_lossy().to_string();
        assert!(!cfg.to_dufs_yaml().contains("tls"));
    }

    #[test]
    fn 中继加密开关直通隧道配置() {
        let mut cfg = AppConfig::default();
        cfg.relay = RelaySettings {
            enabled: true,
            host: "relay.example.com".into(),
            control_port: 7100,
            client_id: "office".into(),
            token: "0123456789abcdef".into(),
            remote_port: 8080,
            tls: true,
        };
        assert!(cfg.tunnel_config().unwrap().tls);
        cfg.relay.tls = false;
        assert!(!cfg.tunnel_config().unwrap().tls);
    }

    #[test]
    fn 旧版配置文件没有新字段也能读进来() {
        // serde(default)：升级后首次启动读旧 config.json 不得报错
        let cfg: AppConfig = serde_json::from_str(r#"{"port": 5700}"#).unwrap();
        assert_eq!(cfg.port, 5700);
        assert!(!cfg.auto_start_share, "新字段应默认关闭");
    }

    #[test]
    fn 中继没配全时不产生隧道配置() {
        let mut cfg = AppConfig::default();
        cfg.relay.enabled = true;
        cfg.relay.host = "relay.example.com".into();
        // 缺 client_id 和 token
        assert!(cfg.tunnel_config().is_none());
    }

    #[test]
    fn 中继配全后隧道参数对得上() {
        let mut cfg = AppConfig::default();
        cfg.port = 5001;
        cfg.relay = RelaySettings {
            enabled: true,
            host: "relay.example.com".into(),
            control_port: 7100,
            client_id: "office".into(),
            token: "0123456789abcdef".into(),
            remote_port: 8080,
            tls: true,
        };
        let t = cfg.tunnel_config().expect("应当能生成隧道配置");
        assert_eq!(t.local_port, 5001);
        assert_eq!(t.remote_port, 8080);
    }
}
