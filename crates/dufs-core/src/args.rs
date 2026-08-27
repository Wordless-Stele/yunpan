use anyhow::{bail, Context, Result};
use async_zip::Compression;
use serde::{Deserialize, Deserializer};
use smart_default::SmartDefault;
use std::env;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use crate::auth::AccessControl;
use crate::http_logger::HttpLogger;
use crate::utils::encode_uri;

#[derive(Debug, Deserialize, SmartDefault, PartialEq)]
#[serde(default)]
#[serde(rename_all = "kebab-case")]
pub struct Args {
    #[serde(default = "default_serve_path")]
    #[default(default_serve_path())]
    pub serve_path: PathBuf,
    #[serde(deserialize_with = "deserialize_bind_addrs")]
    #[serde(rename = "bind")]
    #[serde(default = "default_addrs")]
    #[default(default_addrs())]
    pub addrs: Vec<BindAddr>,
    #[serde(default = "default_port")]
    #[default(default_port())]
    pub port: u16,
    #[serde(skip)]
    pub path_is_file: bool,
    pub path_prefix: String,
    #[serde(skip)]
    pub uri_prefix: String,
    #[serde(deserialize_with = "deserialize_string_or_vec")]
    pub hidden: Vec<String>,
    #[serde(deserialize_with = "deserialize_access_control")]
    pub auth: AccessControl,
    pub allow_all: bool,
    pub allow_upload: bool,
    pub allow_delete: bool,
    pub allow_search: bool,
    pub allow_symlink: bool,
    pub allow_archive: bool,
    pub allow_hash: bool,
    pub render_index: bool,
    pub render_spa: bool,
    pub render_try_index: bool,
    pub enable_cors: bool,
    pub assets: Option<PathBuf>,
    pub error_page: Option<PathBuf>,
    #[serde(deserialize_with = "deserialize_log_http")]
    #[serde(rename = "log-format")]
    pub http_logger: HttpLogger,
    pub log_file: Option<PathBuf>,
    pub compress: Compress,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
}

impl Args {
    /// 从 dufs 配置文件（YAML）构造参数。
    ///
    /// GUI 不走命令行，配置由界面生成后序列化成 YAML 再进这里 —— 与上游
    /// `dufs -c config.yaml` 是同一条解析路径，字段语义不会随版本漂移。
    pub fn from_yaml(contents: &str) -> Result<Args> {
        let mut args: Args =
            serde_yaml::from_str(contents).with_context(|| "解析 dufs 配置失败")?;
        args.finalize()?;
        Ok(args)
    }

    /// 补齐 serde 反序列化拿不到的派生字段。
    ///
    /// 对应上游 `Args::parse` 里除「读 clap 取值」以外的全部后处理：路径规范化、
    /// `path_is_file`、`uri_prefix`、`hidden` 拆分、`allow-all` 传播、assets 校验、
    /// TLS 成对性检查。少做任何一步，`Server` 拿到的都是半成品参数。
    pub fn finalize(&mut self) -> Result<()> {
        self.serve_path = Self::sanitize_path(std::mem::take(&mut self.serve_path))?;
        self.path_is_file = self.serve_path.metadata()?.is_file();

        self.path_prefix = self.path_prefix.trim_matches('/').to_string();
        self.uri_prefix = if self.path_prefix.is_empty() {
            "/".to_owned()
        } else {
            format!("/{}/", &encode_uri(&self.path_prefix))
        };

        // 上游允许 `hidden: tmp,*.log` 这种一行写多条，这里照样拆开
        let hidden = std::mem::take(&mut self.hidden);
        self.hidden = hidden
            .into_iter()
            .flat_map(|v| v.split(',').map(|v| v.to_string()).collect::<Vec<String>>())
            .collect();

        if self.allow_all {
            self.allow_upload = true;
            self.allow_delete = true;
            self.allow_search = true;
            self.allow_symlink = true;
            self.allow_hash = true;
            self.allow_archive = true;
        }

        if let Some(assets_path) = &self.assets {
            let assets_path = Self::sanitize_assets_path(assets_path)?;
            let error_page = assets_path.join("404.html");
            self.error_page = error_page.exists().then_some(error_page);
            self.assets = Some(assets_path);
        }

        match (&self.tls_cert, &self.tls_key) {
            (Some(_), Some(_)) | (None, None) => {}
            (Some(_), None) => bail!("只给了 tls-cert，没给 tls-key"),
            (None, Some(_)) => bail!("只给了 tls-key，没给 tls-cert"),
        }

        Ok(())
    }

    fn sanitize_path<P: AsRef<Path>>(path: P) -> Result<PathBuf> {
        let path = path.as_ref();
        if !path.exists() {
            bail!("Path `{}` doesn't exist", path.display());
        }

        env::current_dir()
            .and_then(|mut p| {
                p.push(path); // If path is absolute, it replaces the current path.
                std::fs::canonicalize(p)
            })
            .with_context(|| format!("Failed to access path `{}`", path.display()))
    }

    fn sanitize_assets_path<P: AsRef<Path>>(path: P) -> Result<PathBuf> {
        let path = Self::sanitize_path(path)?;
        if !path.join("index.html").exists() {
            bail!("Path `{}` doesn't contains index.html", path.display());
        }
        Ok(path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum BindAddr {
    IpAddr(IpAddr),
    #[cfg(unix)]
    SocketPath(String),
}

impl BindAddr {
    fn parse_addrs(addrs: &[&str]) -> Result<Vec<Self>> {
        let mut bind_addrs = vec![];
        #[cfg(not(unix))]
        let mut invalid_addrs = vec![];
        for addr in addrs {
            match addr.parse::<IpAddr>() {
                Ok(v) => {
                    bind_addrs.push(BindAddr::IpAddr(v));
                }
                Err(_) => {
                    #[cfg(unix)]
                    bind_addrs.push(BindAddr::SocketPath(addr.to_string()));
                    #[cfg(not(unix))]
                    invalid_addrs.push(*addr);
                }
            }
        }
        #[cfg(not(unix))]
        if !invalid_addrs.is_empty() {
            bail!("Invalid bind address `{}`", invalid_addrs.join(","));
        }
        Ok(bind_addrs)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Compress {
    None,
    #[default]
    Low,
    Medium,
    High,
}

impl Compress {
    pub fn to_compression(self) -> Compression {
        match self {
            Compress::None => Compression::Stored,
            Compress::Low => Compression::Deflate,
            Compress::Medium => Compression::Bz,
            Compress::High => Compression::Xz,
        }
    }
}

fn deserialize_bind_addrs<'de, D>(deserializer: D) -> Result<Vec<BindAddr>, D::Error>
where
    D: Deserializer<'de>,
{
    struct StringOrVec;

    impl<'de> serde::de::Visitor<'de> for StringOrVec {
        type Value = Vec<BindAddr>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("string or list of strings")
        }

        fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            BindAddr::parse_addrs(&[s]).map_err(serde::de::Error::custom)
        }

        fn visit_seq<S>(self, seq: S) -> Result<Self::Value, S::Error>
        where
            S: serde::de::SeqAccess<'de>,
        {
            let addrs: Vec<&'de str> =
                Deserialize::deserialize(serde::de::value::SeqAccessDeserializer::new(seq))?;
            BindAddr::parse_addrs(&addrs).map_err(serde::de::Error::custom)
        }
    }

    deserializer.deserialize_any(StringOrVec)
}

fn deserialize_string_or_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    struct StringOrVec;

    impl<'de> serde::de::Visitor<'de> for StringOrVec {
        type Value = Vec<String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("string or list of strings")
        }

        fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(vec![s.to_owned()])
        }

        fn visit_seq<S>(self, seq: S) -> Result<Self::Value, S::Error>
        where
            S: serde::de::SeqAccess<'de>,
        {
            Deserialize::deserialize(serde::de::value::SeqAccessDeserializer::new(seq))
        }
    }

    deserializer.deserialize_any(StringOrVec)
}

fn deserialize_access_control<'de, D>(deserializer: D) -> Result<AccessControl, D::Error>
where
    D: Deserializer<'de>,
{
    let rules: Vec<&str> = Vec::deserialize(deserializer)?;
    AccessControl::new(&rules).map_err(serde::de::Error::custom)
}

fn deserialize_log_http<'de, D>(deserializer: D) -> Result<HttpLogger, D::Error>
where
    D: Deserializer<'de>,
{
    let value: String = Deserialize::deserialize(deserializer)?;
    value.parse().map_err(serde::de::Error::custom)
}

fn default_serve_path() -> PathBuf {
    PathBuf::from(".")
}

fn default_addrs() -> Vec<BindAddr> {
    BindAddr::parse_addrs(&["0.0.0.0", "::"]).unwrap()
}

fn default_port() -> u16 {
    5000
}

#[cfg(test)]
mod tests {
    // 上游的测试走 clap 命令行，这里已剥掉命令行，改测 from_yaml 这条唯一入口。
    // 断言内容与上游 test_args_from_config_file1/2 等价。
    use super::*;

    #[test]
    fn 配置文件里的字段都能进到_args() {
        let tmpdir = std::env::temp_dir();
        let contents = format!(
            "serve-path: {}\nbind: 0.0.0.0\nport: 3000\nallow-upload: true\nhidden: tmp,*.log,*.lock\n",
            tmpdir.display()
        );
        let args = Args::from_yaml(&contents).unwrap();
        assert_eq!(args.serve_path, Args::sanitize_path(&tmpdir).unwrap());
        assert_eq!(args.addrs, vec![BindAddr::IpAddr("0.0.0.0".parse().unwrap())]);
        assert_eq!(args.hidden, ["tmp", "*.log", "*.lock"]);
        assert_eq!(args.port, 3000);
        assert!(args.allow_upload);
        assert_eq!(args.uri_prefix, "/");
    }

    #[test]
    fn 多个绑定地址与逐条列出的隐藏项() {
        let tmpdir = std::env::temp_dir();
        let contents = format!(
            "serve-path: {}\nbind:\n  - 127.0.0.1\n  - 192.168.8.10\nhidden:\n  - tmp\n  - '*.log'\n  - '*.lock'\n",
            tmpdir.display()
        );
        let args = Args::from_yaml(&contents).unwrap();
        assert_eq!(
            args.addrs,
            vec![
                BindAddr::IpAddr("127.0.0.1".parse().unwrap()),
                BindAddr::IpAddr("192.168.8.10".parse().unwrap())
            ]
        );
        assert_eq!(args.hidden, ["tmp", "*.log", "*.lock"]);
    }

    #[test]
    fn allow_all_会传播到各单项开关() {
        let tmpdir = std::env::temp_dir();
        let args = Args::from_yaml(&format!("serve-path: {}\nallow-all: true\n", tmpdir.display())).unwrap();
        assert!(args.allow_upload && args.allow_delete && args.allow_search && args.allow_archive && args.allow_hash);
    }

    #[test]
    fn 只给一半_tls_配置会报错() {
        let tmpdir = std::env::temp_dir();
        let err = Args::from_yaml(&format!("serve-path: {}\ntls-cert: /tmp/x.pem\n", tmpdir.display()));
        assert!(err.is_err());
    }

    #[test]
    fn 路径前缀会拼进_uri_prefix() {
        let tmpdir = std::env::temp_dir();
        let args = Args::from_yaml(&format!("serve-path: {}\npath-prefix: dufs\n", tmpdir.display())).unwrap();
        assert_eq!(args.uri_prefix, "/dufs/");
    }
}
