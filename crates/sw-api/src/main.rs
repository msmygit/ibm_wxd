//! `wxd` — launches the local installer web server and serves the UI.
//!
//! Binds 127.0.0.1 only. Generates a session token and prints a ready-to-click
//! URL that carries the token, so the static UI can authenticate without a build
//! step. Override the port with `WXD_PORT` and the UI directory with `WXD_UI_DIR`.

use std::path::PathBuf;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port: u16 = std::env::var("WXD_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(4178);

    let ui_dir: PathBuf = std::env::var_os("WXD_UI_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/ui")));

    let token = Uuid::new_v4().to_string();
    let orch = sw_api::default_orchestrator();
    let app = sw_api::app(orch, token.clone(), ui_dir);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;

    println!("\n  watsonx.data Easy Installer");
    println!("  ---------------------------");
    println!("  Open this URL in your browser (the token authenticates the UI):\n");
    println!("    http://127.0.0.1:{}/?token={}\n", bound.port(), token);
    println!("  API docs:  http://127.0.0.1:{}/api/openapi.yaml", bound.port());
    println!("  Press Ctrl-C to stop.\n");

    axum::serve(listener, app).await?;
    Ok(())
}
