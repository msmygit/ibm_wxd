//! `wxd` — launches the local installer web server and serves the UI.
//!
//! Binds 127.0.0.1 only. Generates a session token and prints a ready-to-click
//! URL that carries the token, so the static UI can authenticate without a build
//! step. Override the port with `WXD_PORT` and the UI directory with `WXD_UI_DIR`.

use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port: u16 = std::env::var("WXD_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(4178);

    let ui_dir: PathBuf = std::env::var_os("WXD_UI_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/ui")));

    // Token auth is optional. By default (no WXD_TOKEN) the loopback-only server
    // needs no token, so the bare URL just works. Set WXD_TOKEN to require one
    // (recommended if you expose the port beyond localhost).
    let token = std::env::var("WXD_TOKEN").unwrap_or_default();
    let orch = sw_api::default_orchestrator();
    let app = sw_api::app(orch, token.clone(), ui_dir);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;

    println!("\n  IBM self-managed software Easy Installer");
    println!("  ---------------------------------------");
    if token.is_empty() {
        println!("  Open this URL in your browser:\n");
        println!("    http://127.0.0.1:{}/\n", bound.port());
        println!("  (Auth disabled — loopback only. Set WXD_TOKEN to require a token.)");
    } else {
        println!("  Open this URL in your browser (the token authenticates the UI):\n");
        println!("    http://127.0.0.1:{}/?token={}\n", bound.port(), token);
    }
    println!("  API docs:  http://127.0.0.1:{}/api/openapi.yaml", bound.port());
    println!("  Press Ctrl-C to stop.\n");

    axum::serve(listener, app).await?;
    Ok(())
}
