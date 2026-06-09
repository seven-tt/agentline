use crate::config::AppConfig;
use crate::state::AppState;
use agentline_im_wechat::{HttpClient, request_qr, wait_for_scan};
use anyhow::{Context, Result};

/// Terminal-side login: drives `request_qr` + `wait_for_scan`, prints
/// status to stderr, writes the PNG to `/tmp` and (on macOS) pops it
/// in Preview. The web dashboard does NOT call this — it talks to the
/// library directly so it can stream PNG bytes over HTTP without going
/// through a subprocess.
pub async fn run(cfg: AppConfig) -> Result<()> {
    let http = HttpClient::new().context("build http client")?;
    let state_path = cfg.state_path()?;

    eprintln!("→ 启动微信 iLink 扫码登录…");
    let qr = request_qr(&http).await.context("fetch QR")?;

    eprintln!();
    eprintln!("📱 用微信扫码以下链接登录（复制到浏览器或用二维码扫描）:");
    eprintln!("   {}", qr.login_url);
    eprintln!();

    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&qr.login_url).spawn();
    }

    let result = wait_for_scan(&http, &qr).await.context("iLink scan")?;

    let mut state = AppState::load_or_default(&state_path).unwrap_or_default();
    state.im.wechat.bot_token = Some(result.bot_token.clone());
    state.im.wechat.bot_baseurl = result.baseurl.clone();
    // Reset cursor on fresh login — old cursors are tied to the old token.
    state.im.wechat.get_updates_buf = String::new();
    state.save(&state_path)?;

    eprintln!("✅ 已登录，token 已保存到 {}", state_path.display());
    if let Some(b) = result.baseurl {
        eprintln!("   baseurl = {b}");
    }
    Ok(())
}
