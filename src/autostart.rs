//! 系统级开机自启动。**OS 是单一真相**（plist / regkey / .desktop 存在与否），
//! 不进 `AppConfig`——避免「配置说开了，文件却被手删」的不一致。写法承自 ProxyZms。
//!
//! 自启动条目一律带 `--hidden` 参数：登录时静默进托盘，不弹主窗口。
//!
//! - **macOS**：`~/Library/LaunchAgents/top.zhoumaosen.yunpan.plist`，`RunAtLoad`。
//! - **Windows**：`HKCU\...\Run` 注册表值，用 `reg add/delete/query`，不引 winreg。
//! - **Linux**：XDG 自启动 `~/.config/autostart/yunpan.desktop`。

#[allow(dead_code)]
const LAUNCH_AGENT_LABEL: &str = "top.zhoumaosen.yunpan";
#[allow(dead_code)]
const WINDOWS_RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
#[allow(dead_code)]
const WINDOWS_RUN_VALUE: &str = "YunPan";

/// 登录时静默启动的参数；main() 里认它则不显示窗口。
pub const HIDDEN_FLAG: &str = "--hidden";

// ─────────────────────────────────────────────────────────────────────────────
// 纯渲染函数：产出各平台的自启动文件内容，可单测

#[allow(dead_code)]
fn render_plist(exe_path: &str) -> String {
    fn xml_escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>{flag}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>
"#,
        label = LAUNCH_AGENT_LABEL,
        exe = xml_escape(exe_path),
        flag = HIDDEN_FLAG,
    )
}

#[allow(dead_code)]
fn render_desktop_entry(exe_path: &str) -> String {
    // Exec 行按 Desktop Entry 规范转义：路径含空格要引号包裹
    format!(
        "[Desktop Entry]\nType=Application\nName=云链盘\nExec=\"{exe_path}\" {HIDDEN_FLAG}\nX-GNOME-Autostart-enabled=true\n"
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// macOS

#[cfg(target_os = "macos")]
mod imp {
    use super::{render_plist, LAUNCH_AGENT_LABEL};
    use std::path::PathBuf;

    fn plist_path() -> Result<PathBuf, String> {
        let home = dirs::home_dir().ok_or_else(|| "无法定位用户主目录".to_string())?;
        Ok(home
            .join("Library/LaunchAgents")
            .join(format!("{LAUNCH_AGENT_LABEL}.plist")))
    }

    pub fn is_enabled() -> bool {
        plist_path().map(|p| p.exists()).unwrap_or(false)
    }

    pub fn set_enabled(enable: bool) -> Result<(), String> {
        let path = plist_path()?;
        if enable {
            let exe = std::env::current_exe()
                .map_err(|e| format!("无法获取当前可执行文件路径: {e}"))?;
            let exe_str = exe
                .to_str()
                .ok_or_else(|| "可执行文件路径含非 UTF-8 字符".to_string())?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("创建 LaunchAgents 目录失败: {e}"))?;
            }
            std::fs::write(&path, render_plist(exe_str))
                .map_err(|e| format!("写入 plist 失败: {e}"))?;
        } else if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("删除 plist 失败: {e}"))?;
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Windows

#[cfg(target_os = "windows")]
mod imp {
    use super::{HIDDEN_FLAG, WINDOWS_RUN_KEY, WINDOWS_RUN_VALUE};
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    /// 隐藏 reg.exe 的控制台窗口
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    pub fn is_enabled() -> bool {
        Command::new("reg")
            .args(["query", WINDOWS_RUN_KEY, "/v", WINDOWS_RUN_VALUE])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    pub fn set_enabled(enable: bool) -> Result<(), String> {
        if enable {
            let exe = std::env::current_exe()
                .map_err(|e| format!("无法获取当前可执行文件路径: {e}"))?;
            let exe_str = exe
                .to_str()
                .ok_or_else(|| "可执行文件路径含非 UTF-8 字符".to_string())?;
            // 路径用引号包住，Windows 启动器才不会把带空格的路径拆开
            let value = format!("\"{exe_str}\" {HIDDEN_FLAG}");
            let status = Command::new("reg")
                .args([
                    "add", WINDOWS_RUN_KEY, "/v", WINDOWS_RUN_VALUE,
                    "/t", "REG_SZ", "/d", &value, "/f",
                ])
                .creation_flags(CREATE_NO_WINDOW)
                .status()
                .map_err(|e| format!("调用 reg add 失败: {e}"))?;
            if !status.success() {
                return Err(format!("reg add 退出码 {status}"));
            }
        } else {
            // delete 不存在的值会返回非 0——目标态一致，视为成功
            let _ = Command::new("reg")
                .args(["delete", WINDOWS_RUN_KEY, "/v", WINDOWS_RUN_VALUE, "/f"])
                .creation_flags(CREATE_NO_WINDOW)
                .status();
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Linux（XDG 自启动）

#[cfg(target_os = "linux")]
mod imp {
    use super::render_desktop_entry;
    use std::path::PathBuf;

    fn desktop_path() -> Result<PathBuf, String> {
        let config = dirs::config_dir().ok_or_else(|| "无法定位配置目录".to_string())?;
        Ok(config.join("autostart").join("yunpan.desktop"))
    }

    pub fn is_enabled() -> bool {
        desktop_path().map(|p| p.exists()).unwrap_or(false)
    }

    pub fn set_enabled(enable: bool) -> Result<(), String> {
        let path = desktop_path()?;
        if enable {
            let exe = std::env::current_exe()
                .map_err(|e| format!("无法获取当前可执行文件路径: {e}"))?;
            let exe_str = exe
                .to_str()
                .ok_or_else(|| "可执行文件路径含非 UTF-8 字符".to_string())?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("创建 autostart 目录失败: {e}"))?;
            }
            std::fs::write(&path, render_desktop_entry(exe_str))
                .map_err(|e| format!("写入 .desktop 失败: {e}"))?;
        } else if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("删除 .desktop 失败: {e}"))?;
        }
        Ok(())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod imp {
    pub fn is_enabled() -> bool {
        false
    }
    pub fn set_enabled(_enable: bool) -> Result<(), String> {
        Err("当前平台暂不支持开机自启动".to_string())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 公共 API

/// 当前是否已设为开机自启。同步、廉价（读文件 / 查注册表）。
pub fn is_enabled() -> bool {
    imp::is_enabled()
}

/// 设置开机自启。失败返回可直接展示的中文错误。
pub fn set_enabled(enable: bool) -> Result<(), String> {
    imp::set_enabled(enable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_里带了静默启动参数并转义了特殊字符() {
        let p = render_plist("/Applications/Yun&Pan.app/Contents/MacOS/yunpan");
        assert!(p.contains("<string>--hidden</string>"));
        assert!(p.contains("Yun&amp;Pan"), "& 必须转义，否则 plist 非法");
        assert!(p.contains(LAUNCH_AGENT_LABEL));
    }

    #[test]
    fn desktop_条目路径带引号且有静默参数() {
        let d = render_desktop_entry("/opt/yun pan/yunpan");
        assert!(d.contains("Exec=\"/opt/yun pan/yunpan\" --hidden"), "实际内容：\n{d}");
    }
}
