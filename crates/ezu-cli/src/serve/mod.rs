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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::Args;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

use crate::source::{SourceSpec, TileSource};
use state::{AppState, StyleReload, StyleSnapshot};

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
    /// Accepts either a positional argument (`ezu serve foo.json`) or
    /// `--style`. Positional wins when both are given.
    #[arg(value_name = "STYLE")]
    style_arg: Option<String>,
    /// Same as the positional `STYLE` argument; kept for back-compat
    /// and so `EZU_STYLE` still works.
    #[arg(
        long = "style",
        default_value = "crates/ezu/examples/styles/watercolor-basic.json",
        env = "EZU_STYLE"
    )]
    style_flag: String,
    /// Base directory for resolving asset `src` paths (brushes, images).
    #[arg(long, default_value = "assets/brushes", env = "EZU_ASSETS")]
    assets_dir: PathBuf,
    /// Bind address.
    #[arg(long, default_value = "127.0.0.1:8080", env = "EZU_BIND")]
    bind: SocketAddr,
}

pub async fn run(args: ServeCmd) -> Result<(), Box<dyn std::error::Error>> {
    let cli_source = match (&args.pmtiles, &args.mvt) {
        (Some(p), None) => Some((SourceSpec::PmTiles(p.clone()), "--pmtiles flag")),
        (None, Some(u)) => Some((SourceSpec::Mvt(u.clone()), "--mvt flag")),
        (None, None) => None,
        _ => return Err("--pmtiles and --mvt are mutually exclusive".into()),
    };

    let style_src = args
        .style_arg
        .as_deref()
        .unwrap_or(args.style_flag.as_str());
    tracing::info!("loading style from {style_src}");
    let style_text = crate::fetch_text(style_src).await?;
    let snapshot = StyleSnapshot::build(style_text, 1, &args.assets_dir).await?;
    tracing::info!(
        "loaded style {} ({} nodes, tile={}, pad={}, {} brushes, {} images, {} dem source(s))",
        snapshot.doc.name,
        snapshot.doc.nodes.len(),
        snapshot.doc.tile_size,
        snapshot.doc.pad,
        snapshot.assets.bank.len(),
        snapshot.assets.images.len(),
        snapshot.dem_sources.len(),
    );

    // Resolve the feature-tile source: CLI flags win, then a
    // style-declared mvt / pmtiles entry, and as a last resort the
    // public Protomaps daily build — but only when the style isn't a
    // pure terrain document (in that case rendering without an MVT is
    // exactly what the author asked for).
    let style_source = crate::feature_source_from_doc(&snapshot.doc);
    let needs_features = style_references_features(&snapshot.doc);
    let fallback = (cli_source.is_none() && style_source.is_none() && needs_features).then(|| {
        (
            SourceSpec::PmTiles("https://build.protomaps.com/20260520.pmtiles".into()),
            "default Protomaps build",
        )
    });
    let source = match cli_source.or(style_source).or(fallback) {
        Some((spec, origin)) => {
            tracing::info!("opening tile source ({origin}): {spec:?}");
            Some(TileSource::open(&spec).await?)
        }
        None => {
            tracing::info!("no MVT source — `tile.<feature>` bindings will be empty");
            None
        }
    };

    let state = AppState::new(source, snapshot, args.assets_dir.clone());

    // Spawn a polling watcher when the style was loaded from a local
    // path. URL-sourced styles aren't watched (we don't know how the
    // remote would notify us). The watcher reloads on mtime change and
    // broadcasts to /style/events subscribers.
    if !is_url(style_src) {
        let path = PathBuf::from(style_src);
        let state_for_watch = state.clone();
        tokio::spawn(watch_style_file(path, state_for_watch));
    }

    let mut app = handlers::router().with_state(state);
    for (route, dir) in [
        ("/wasm-demo", "crates/ezu-wasm/examples/wasm-demo"),
        ("/wasm/scalar", "target/wasm/scalar"),
        ("/wasm/simd", "target/wasm/simd"),
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

/// Does the style reference any `features` node? Used to decide whether
/// the legacy Protomaps fallback applies — a pure-terrain document
/// (only `dem` sources) renders fine without an MVT pyramid.
fn style_references_features(doc: &ezu::style::Document) -> bool {
    doc.nodes.values().any(|n| n.op == "features")
}

fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

fn mtime_ms(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// Poll the file mtime once a second and reload the live snapshot
/// whenever it advances. Polling (vs. an OS-native notify watcher)
/// keeps the dependency surface zero and works the same on macOS,
/// Linux, and Windows — fine for an interactive dev tool where the
/// only watcher is the editor that's already attached to it.
async fn watch_style_file(path: PathBuf, state: AppState) {
    let mut last_mtime: Option<SystemTime> = tokio::fs::metadata(&path)
        .await
        .ok()
        .and_then(|m| m.modified().ok());
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tracing::info!("watching {} for live reload", path.display());
    loop {
        ticker.tick().await;
        let meta = match tokio::fs::metadata(&path).await {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime = match meta.modified() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if last_mtime == Some(mtime) {
            continue;
        }
        last_mtime = Some(mtime);

        let text = match tokio::fs::read_to_string(&path).await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("watch: read {} failed: {e}", path.display());
                continue;
            }
        };
        let next_version = state.style.read().await.version + 1;
        let snap = match StyleSnapshot::build(text.clone(), next_version, state.assets_dir.as_ref())
            .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("watch: rebuild failed: {e}");
                continue;
            }
        };
        let v = snap.version;
        *state.style.write().await = snap;
        let _ = state.events.send(StyleReload {
            version: v,
            text,
            mtime_ms: mtime_ms(mtime),
        });
        tracing::info!("style reloaded from {} (v{v})", path.display());
    }
}
