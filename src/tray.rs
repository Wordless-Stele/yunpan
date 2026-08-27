//! 状态图标：程序化画出来的 RGBA，不依赖任何图片资源。
//!
//! 图形是「圆盘 + 横槽」——一眼像个硬盘。灰色 = 未共享，朱砂红 = 共享中。
//! 之所以不用 PNG 资源：图标总共两种状态四个尺寸，代码画比管理八张图省事，
//! 而且颜色改一处常量就全变。

/// 生成一个 size×size 的 RGBA 图标缓冲。
pub fn status_icon_rgba(active: bool, size: u32) -> Vec<u8> {
    let s = size as f32;
    let center = s / 2.0;
    let radius = s * 0.45;
    // 朱砂红 / 中性灰
    let (cr, cg, cb) = if active { (0xE3, 0x42, 0x34) } else { (0x8E, 0x8E, 0x93) };

    let mut buf = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 + 0.5 - center;
            let dy = y as f32 + 0.5 - center;
            let dist = (dx * dx + dy * dy).sqrt();
            // 圆的边缘做 1px 抗锯齿，托盘那么小的图标不做会毛
            let alpha = (radius - dist + 0.5).clamp(0.0, 1.0);
            if alpha <= 0.0 {
                continue;
            }
            let in_slot = dy.abs() < s * 0.07 && dx.abs() < radius * 0.60;
            let idx = ((y * size + x) * 4) as usize;
            if in_slot {
                buf[idx] = 0xFF;
                buf[idx + 1] = 0xFF;
                buf[idx + 2] = 0xFF;
            } else {
                buf[idx] = cr;
                buf[idx + 1] = cg;
                buf[idx + 2] = cb;
            }
            buf[idx + 3] = (alpha * 255.0) as u8;
        }
    }
    buf
}

/// 托盘图标（32×32，Windows 通知区域与 macOS 菜单栏都合适）。
#[cfg(feature = "desktop")]
pub fn tray_icon(active: bool) -> Option<dioxus::desktop::trayicon::Icon> {
    dioxus::desktop::trayicon::Icon::from_rgba(status_icon_rgba(active, 32), 32, 32).ok()
}

/// 窗口图标（Windows 任务栏/标题栏、Linux 标题栏）。
#[cfg(feature = "desktop")]
pub fn window_icon() -> Option<dioxus::desktop::tao::window::Icon> {
    dioxus::desktop::tao::window::Icon::from_rgba(status_icon_rgba(true, 128), 128, 128).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 图标缓冲大小与尺寸一致() {
        assert_eq!(status_icon_rgba(true, 32).len(), 32 * 32 * 4);
        assert_eq!(status_icon_rgba(false, 128).len(), 128 * 128 * 4);
    }

    #[test]
    fn 激活与未激活的图标颜色确实不同() {
        assert_ne!(status_icon_rgba(true, 32), status_icon_rgba(false, 32));
    }

    #[test]
    fn 四角是透明的() {
        let buf = status_icon_rgba(true, 32);
        assert_eq!(buf[3], 0, "左上角应当透明");
        let last = buf.len() - 1;
        assert_eq!(buf[last], 0, "右下角应当透明");
    }
}
