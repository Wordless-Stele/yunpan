//! 状态图标：来自正式 logo（`assets/icons/`，源文件是 `logos/` 里选定的概念稿）。
//!
//! 托盘两态是同一个「云+槽」标记的换色版：灰 = 未共享，朱砂红 = 共享中。
//! PNG 在编译期 `include_bytes!` 嵌入，运行期解码成 RGBA——不依赖安装目录里
//! 有没有资源文件，单文件分发也不缺图。

/// 托盘：共享中（朱砂红云）。
const TRAY_ON: &[u8] = include_bytes!("../assets/icons/tray-on-32.png");
/// 托盘：未共享（灰云）。
const TRAY_OFF: &[u8] = include_bytes!("../assets/icons/tray-off-32.png");
/// 窗口图标（Windows 任务栏/标题栏、Linux 标题栏）：带黑色圆角底的完整应用图标。
const WINDOW_ICON: &[u8] = include_bytes!("../assets/icons/app-128.png");

/// 解码 PNG 为 (RGBA 字节, 宽, 高)。
fn decode_rgba(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let rgba = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (w, h) = rgba.dimensions();
    Some((rgba.into_raw(), w, h))
}

/// 托盘图标（32×32）。
#[cfg(feature = "desktop")]
pub fn tray_icon(active: bool) -> Option<dioxus::desktop::trayicon::Icon> {
    let (buf, w, h) = decode_rgba(if active { TRAY_ON } else { TRAY_OFF })?;
    dioxus::desktop::trayicon::Icon::from_rgba(buf, w, h).ok()
}

/// 窗口图标（128×128）。
#[cfg(feature = "desktop")]
pub fn window_icon() -> Option<dioxus::desktop::tao::window::Icon> {
    let (buf, w, h) = decode_rgba(WINDOW_ICON)?;
    dioxus::desktop::tao::window::Icon::from_rgba(buf, w, h).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 三张内嵌图标都解得开且尺寸正确() {
        let (_, w, h) = decode_rgba(TRAY_ON).expect("tray-on 解码失败");
        assert_eq!((w, h), (32, 32));
        let (_, w, h) = decode_rgba(TRAY_OFF).expect("tray-off 解码失败");
        assert_eq!((w, h), (32, 32));
        let (_, w, h) = decode_rgba(WINDOW_ICON).expect("窗口图标解码失败");
        assert_eq!((w, h), (128, 128));
    }

    #[test]
    fn 两态托盘图标像素确实不同() {
        let (on, _, _) = decode_rgba(TRAY_ON).unwrap();
        let (off, _, _) = decode_rgba(TRAY_OFF).unwrap();
        assert_ne!(on, off, "换色版不该一模一样");
    }
}
