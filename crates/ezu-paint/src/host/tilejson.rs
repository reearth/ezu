//! Minimal TileJSON resolution shared by the tile-pyramid fetchers
//! (`raster`, `dem`): extract the first `tiles[]` template and the
//! upstream `attribution`.

use reqwest::Client;

/// Fetch a TileJSON document and return `(template, attribution)`.
/// The template must contain `{z}` / `{x}` / `{y}` placeholders.
pub async fn resolve_tilejson(
    client: &Client,
    url: &str,
) -> Result<(String, Option<String>), String> {
    let resp = client
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("tilejson {url}: {e}"))?;
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("tilejson {url}: {e}"))?;
    let body: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("tilejson {url}: {e}"))?;
    let template = body
        .get("tiles")
        .and_then(|t| t.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("tilejson {url}: no `tiles[0]` entry"))?
        .to_string();
    if !(template.contains("{z}") && template.contains("{x}") && template.contains("{y}")) {
        return Err(format!(
            "tilejson {url}: tiles[0] lacks {{z}}/{{x}}/{{y}}: {template}"
        ));
    }
    let attribution = body
        .get("attribution")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Ok((template, attribution))
}
