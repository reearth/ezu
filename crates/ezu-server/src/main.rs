//! Live editor + tile server for the Ezu Style Spec.
//!
//! - `GET  /`                    → inline HTML editor (textarea + Leaflet)
//! - `GET  /style`               → current style as raw JSON
//! - `PUT  /style`               → validate + replace style; returns `{ version }`
//! - `GET  /tiles/{z}/{x}/{y}.png` → render and serve the tile
//! - `GET  /schemas/ezu-style.json` → JSON Schema for client-side validation
//!
//! The upstream MVT bytes are cached in-process so style edits don't refetch
//! from the PMTiles archive.

mod handlers;
mod state;

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;
use ezu::paint::Brush;
use std::collections::HashMap;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use crate::state::AppState;

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
        default_value = "crates/ezu/styles/watercolor-basic.json",
        env = "EZU_STYLE"
    )]
    style: PathBuf,
    /// Directory containing `.myb` brush files.
    #[arg(long, default_value = "assets/brushes", env = "EZU_BRUSHES")]
    brushes: PathBuf,
    /// Path to the JSON schema served at `/schemas/ezu-style.json`.
    #[arg(long, default_value = "schemas/ezu-style.json", env = "EZU_SCHEMA")]
    schema: PathBuf,
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
    let archive = ezu::pmtiles::PmTilesArchive::open_url(&args.pmtiles_url).await?;

    let style_text = std::fs::read_to_string(&args.style)?;
    let parsed = ezu::style::Style::from_json(&style_text)?;
    tracing::info!(
        "loaded style {} ({} layers, tile={}, pad={})",
        parsed.name,
        parsed.layers.len(),
        parsed.tile_size,
        parsed.pad
    );

    let brushes = load_brushes(&args.brushes)?;
    tracing::info!("loaded {} brushes from {}", brushes.len(), args.brushes.display());

    let state = AppState::new(archive, parsed, style_text, brushes, args.schema.clone());

    let mut app = handlers::router().with_state(state);

    // Static directories — present only when they exist on disk so the
    // server stays useful even when run from a published binary.
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

fn load_brushes(dir: &PathBuf) -> Result<HashMap<String, Brush>, Box<dyn std::error::Error>> {
    let mut bank = HashMap::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("myb") {
            continue;
        }
        let json = std::fs::read_to_string(&path)?;
        let brush = hokusai::myb::from_str(&json)?;
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or("bad brush filename")?
            .to_string();
        bank.insert(name, brush);
    }
    Ok(bank)
}
