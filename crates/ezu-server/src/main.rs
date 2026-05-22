//! Live editor + tile server for the Ezu Style Spec.
//!
//! - `GET  /`                       → inline HTML editor
//! - `GET  /style`                  → current style JSON
//! - `PUT  /style`                  → validate + rebuild graph; returns `{ version }`
//! - `GET  /tiles/{z}/{x}/{y}.png`  → render and serve the tile
//! - `GET  /schemas/ezu-style.json` → (optional) JSON Schema for client validation
//!
//! Each style version owns its own intermediate cache; an edit invalidates
//! everything in one swap.

mod handlers;
mod pmtiles;
mod state;

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use crate::state::{AppState, StyleSnapshot};

#[derive(Parser, Debug)]
#[command(about = "Live editor + tile server for the Ezu Style Spec")]
struct Args {
    /// PMTiles archive URL (HTTP range requests).
    #[arg(
        long,
        default_value = "https://build.protomaps.com/20260520.pmtiles",
        env = "EZU_PMTILES_URL"
    )]
    pmtiles_url: String,
    /// Path to the initial Ezu Style JSON document.
    #[arg(
        long,
        default_value = "crates/ezu/examples/watercolor-basic.json",
        env = "EZU_STYLE"
    )]
    style: PathBuf,
    /// Directory containing `.myb` brush files.
    #[arg(long, default_value = "assets/brushes", env = "EZU_BRUSHES")]
    brushes: PathBuf,
    /// Bind address.
    #[arg(long, default_value = "127.0.0.1:8080", env = "EZU_BIND")]
    bind: SocketAddr,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let args = Args::parse();

    tracing::info!("opening PMTiles {}", args.pmtiles_url);
    let archive = pmtiles::PmTilesArchive::open_url(&args.pmtiles_url).await?;

    let style_text = std::fs::read_to_string(&args.style)?;
    let snapshot = StyleSnapshot::build(style_text, 1, &args.brushes).await?;
    tracing::info!(
        "loaded style {} ({} nodes, tile={}, pad={}, {} brushes, {} images)",
        snapshot.doc.name,
        snapshot.doc.nodes.len(),
        snapshot.doc.tile_size,
        snapshot.doc.pad,
        snapshot.assets.bank.len(),
        snapshot.assets.images.len(),
    );

    let state = AppState::new(archive, snapshot, args.brushes.clone());

    let mut app = handlers::router().with_state(state);

    for (route, dir) in [
        ("/wasm-demo", "crates/ezu-wasm/www"),
        ("/wasm/scalar", "target/wasm/scalar"),
        ("/wasm/simd", "target/wasm/simd"),
        ("/assets", "assets"),
    ] {
        if std::path::Path::new(dir).is_dir() {
            tracing::info!("serving {} from {}", route, dir);
            app = app.nest_service(route, ServeDir::new(dir));
        }
    }

    let app = app
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());

    tracing::info!("listening on http://{}", args.bind);
    let listener = tokio::net::TcpListener::bind(args.bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

