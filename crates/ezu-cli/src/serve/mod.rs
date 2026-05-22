//! `ezu serve` — live editor + tile server.
//!
//! Hosts the same surface area the now-retired `ezu-server` binary
//! used to: an inline HTML editor at `/`, `GET/PUT /style`, rendered
//! tiles at `/tiles/{z}/{x}/{y}.{png,webp}`, raw MVT bytes for the
//! WASM demo at `/mvt/{z}/{x}/{y}`, and a registry-derived JSON
//! Schema at `/schemas/ezu-style.json`. The editor uses the schema
//! for client-side validation as you type.

mod handlers;
mod state;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use clap::Args;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

use crate::source::{SourceSpec, TileSource};
use state::{AppState, StyleSnapshot};

#[derive(Args, Debug)]
pub struct ServeCmd {
    /// PMTiles archive — local path or http(s):// URL.
    #[arg(long, conflicts_with = "mvt", env = "EZU_PMTILES_URL")]
    pmtiles: Option<String>,
    /// Templated MVT tile source (URL or path) containing `{z}`,
    /// `{x}`, `{y}` placeholders; or a `.json` TileJSON document.
    #[arg(long, conflicts_with = "pmtiles", env = "EZU_MVT_URL")]
    mvt: Option<String>,
    /// Initial Ezu Style document — local path or http(s):// URL.
    #[arg(
        long,
        default_value = "crates/ezu/examples/watercolor-basic.json",
        env = "EZU_STYLE"
    )]
    style: String,
    /// Base directory for resolving asset `src` paths (brushes, images).
    #[arg(long, default_value = "assets/brushes", env = "EZU_ASSETS")]
    assets_dir: PathBuf,
    /// Bind address.
    #[arg(long, default_value = "127.0.0.1:8080", env = "EZU_BIND")]
    bind: SocketAddr,
}

pub async fn run(args: ServeCmd) -> Result<(), Box<dyn std::error::Error>> {
    let spec = match (&args.pmtiles, &args.mvt) {
        (Some(p), None) => SourceSpec::PmTiles(p.clone()),
        (None, Some(u)) => SourceSpec::Mvt(u.clone()),
        // Default when neither flag is given: the public Protomaps daily build.
        (None, None) => SourceSpec::PmTiles("https://build.protomaps.com/20260520.pmtiles".into()),
        _ => return Err("--pmtiles and --mvt are mutually exclusive".into()),
    };
    tracing::info!("opening tile source: {spec:?}");
    let source = TileSource::open(&spec).await?;

    let style_text = crate::fetch_text(&args.style).await?;
    let snapshot = StyleSnapshot::build(style_text, 1, &args.assets_dir).await?;
    tracing::info!(
        "loaded style {} ({} nodes, tile={}, pad={}, {} brushes, {} images)",
        snapshot.doc.name,
        snapshot.doc.nodes.len(),
        snapshot.doc.tile_size,
        snapshot.doc.pad,
        snapshot.assets.bank.len(),
        snapshot.assets.images.len(),
    );

    let state = AppState::new(source, snapshot, args.assets_dir.clone());

    let mut app = handlers::router().with_state(state);
    for (route, dir) in [
        ("/wasm-demo", "crates/ezu-wasm/www"),
        ("/wasm/scalar", "target/wasm/scalar"),
        ("/wasm/simd", "target/wasm/simd"),
        ("/assets", "assets"),
    ] {
        if Path::new(dir).is_dir() {
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
