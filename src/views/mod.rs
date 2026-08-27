mod logs;
mod relay;
mod share;

pub use logs::LogsView;
pub use relay::RelayView;
pub use share::ShareView;

/// 把一段文字复制进系统剪贴板。
///
/// 走 webview 的 execCommand 兜底路径而不是 `navigator.clipboard`：后者在
/// 非 https 上下文里直接是 undefined，桌面 webview 的自定义协议正踩在这条线上。
pub fn copy_text(text: &str) {
    let escaped = text.replace('\\', "\\\\").replace('`', "\\`");
    let js = format!(
        r#"(function() {{
            const ta = document.createElement('textarea');
            ta.value = `{escaped}`;
            ta.style.position = 'fixed';
            ta.style.opacity = '0';
            document.body.appendChild(ta);
            ta.select();
            document.execCommand('copy');
            document.body.removeChild(ta);
        }})()"#
    );
    let _ = dioxus::document::eval(&js);
}

/// 把 URL 画成二维码 SVG。失败（超长之类）就返回 None，界面上不画。
pub fn qr_svg(url: &str) -> Option<String> {
    use qrcode::render::svg;
    let code = qrcode::QrCode::new(url.as_bytes()).ok()?;
    Some(
        code.render::<svg::Color>()
            .quiet_zone(false)
            .min_dimensions(168, 168)
            .dark_color(svg::Color("#111111"))
            .light_color(svg::Color("#ffffff"))
            .build(),
    )
}
