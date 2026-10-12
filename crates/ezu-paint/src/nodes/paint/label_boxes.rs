//! `label-boxes` — `Labels -> Features`. The boxes a `label-placement` node
//! worked with, as ordinary polygons.
//!
//! Placement measures every label as boxes: the laid-out text block, the
//! icon, the `background-color` box, and the padded box it actually tests
//! for collision. This node hands those boxes to the feature graph, so a
//! recipe can draw them with any geometry and paint op — a bordered or
//! rounded badge (`buffer` → `stroke`), an offset shadow (`transform`), or a
//! debug overlay of what placed and what lost.
//!
//! It reads the decisions a `label-placement` node already made, so the
//! labels are placed once and the boxes match what `text-draw` draws.
//! Labels placed from a neighbour tile's candidates come through like the
//! text does, so a box straddling a tile edge continues across it.

use std::collections::BTreeMap;
use std::sync::Arc;

use ezu_graph::{
    schema_frag, take_input_ref, take_optional_input_ref, BuiltNode, Connection, CoordSpace,
    EvalCtx, EvalError, FactoryCtx, FactoryError, Node, NodeFactory, PortKind, PortSpec, PortValue,
};
use serde_json::Value;
use xxhash_rust::xxh3::Xxh3;

use super::labels::{labels_input, text_rect, LabelDraw, LabelSet, PlacedLabels, ACCEPTS_LABELS};
use crate::nodes::common::{features_value_culled, FeatureGroup};
use ezu_core::text::{collide::Aabb, LabelCandidate};

/// Coordinate extent of the emitted geometry when the recipe gives none.
const DEFAULT_EXTENT: u32 = 4096;

/// One kind of box a label is measured by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    /// A point label's laid-out text block, no padding.
    Text,
    /// A point symbol's icon, rotated by `icon-rotate`.
    Icon,
    /// The box `background-color` draws behind a point label.
    Background,
    /// One glyph of a line label, rotated along the path.
    Glyph,
    /// A box placement tests for collision, padding included.
    Collision,
}

impl Part {
    const ALL: [Part; 5] = [
        Part::Text,
        Part::Icon,
        Part::Background,
        Part::Glyph,
        Part::Collision,
    ];

    fn name(self) -> &'static str {
        match self {
            Part::Text => "text",
            Part::Icon => "icon",
            Part::Background => "background",
            Part::Glyph => "glyph",
            Part::Collision => "collision",
        }
    }

    fn bit(self) -> u8 {
        1 << self as u8
    }
}

struct LabelBoxesNode {
    /// Bit set of the [`Part`]s to emit.
    parts: u8,
    /// Also emit the candidates that lost their collision test.
    include_rejected: bool,
    extent: u32,
    ports: Vec<PortSpec>,
}

/// Converts the label frame (px, this tile's origin at 0) to the emitted
/// geometry's extent units, and collects the boxes.
struct Emitter {
    sx: f32,
    sy: f32,
    groups: Vec<FeatureGroup>,
}

impl Emitter {
    fn unit(&self, (x, y): (f32, f32)) -> (i32, i32) {
        ((x * self.sx).round() as i32, (y * self.sy).round() as i32)
    }

    /// Emit one ring (px, clockwise on screen) with `props` and `part`.
    fn ring(&mut self, corners: [(f32, f32); 4], props: &Props<'_>, part: Part) {
        let exterior: Vec<(i32, i32)> = corners.iter().map(|&c| self.unit(c)).collect();
        let mut properties = (*props.source).clone();
        properties.insert(
            "part".into(),
            maplibre_expr::Value::String(part.name().into()),
        );
        properties.insert("placed".into(), maplibre_expr::Value::Bool(props.placed));
        properties.insert(
            "label-id".into(),
            maplibre_expr::Value::Number(props.label_id),
        );
        self.groups.push(FeatureGroup {
            properties: Arc::new(properties),
            polygons: vec![ezu_features::Polygon {
                exterior,
                holes: vec![],
            }],
            lines: vec![],
            points: vec![],
        });
    }

    fn rect(&mut self, b: Aabb, props: &Props<'_>, part: Part) {
        self.ring(
            [
                (b.min_x, b.min_y),
                (b.max_x, b.min_y),
                (b.max_x, b.max_y),
                (b.min_x, b.max_y),
            ],
            props,
            part,
        );
    }

    /// A `w`×`h` rectangle whose local frame sits at `centre` turned
    /// `angle` radians clockwise; `(x0, y0)`–`(x1, y1)` span it in that
    /// frame.
    fn rotated(
        &mut self,
        centre: (f32, f32),
        angle: f32,
        (x0, y0, x1, y1): (f32, f32, f32, f32),
        props: &Props<'_>,
        part: Part,
    ) {
        let (s, c) = (libm::sinf(angle), libm::cosf(angle));
        let at = |x: f32, y: f32| (centre.0 + x * c - y * s, centre.1 + x * s + y * c);
        self.ring(
            [at(x0, y0), at(x1, y0), at(x1, y1), at(x0, y1)],
            props,
            part,
        );
    }
}

/// What every box of one label carries besides its own part.
struct Props<'a> {
    source: &'a BTreeMap<String, maplibre_expr::Value>,
    placed: bool,
    label_id: f64,
}

/// A label's identity, the same on every tile that sees it: its layer's
/// place in the `label-placement` list and the world-space quantities the
/// placement itself keys on. Cut to 53 bits so it survives as an exact
/// expression number.
fn label_id(layer: usize, c: &LabelCandidate) -> f64 {
    let mut h = Xxh3::new();
    h.update(&(layer as u64).to_le_bytes());
    h.update(&c.world_ax.to_le_bytes());
    h.update(&c.world_ay.to_le_bytes());
    h.update(c.text.as_bytes());
    h.update(&[0]);
    h.update(&c.style_id.to_le_bytes());
    h.update(&c.rank.tile.0.to_le_bytes());
    h.update(&c.rank.tile.1.to_le_bytes());
    h.update(&c.rank.feature.to_le_bytes());
    h.update(&c.rank.symbol.to_le_bytes());
    (h.digest() & ((1u64 << 53) - 1)) as f64
}

impl LabelBoxesNode {
    fn wants(&self, part: Part) -> bool {
        self.parts & part.bit() != 0
    }

    /// Emit the boxes of candidate `cand` of `set` at collision variant
    /// `variant`.
    fn emit_label(
        &self,
        out: &mut Emitter,
        layer: usize,
        set: &LabelSet,
        cand: usize,
        variant: usize,
        placed: bool,
    ) {
        let (Some(c), Some(draw)) = (set.candidates.get(cand), set.draws.get(cand)) else {
            return;
        };
        let source = match draw {
            LabelDraw::Point { properties, .. } | LabelDraw::Line { properties, .. } => properties,
        };
        let props = Props {
            source,
            placed,
            label_id: label_id(layer, c),
        };
        match draw {
            LabelDraw::Point {
                blocks,
                variants,
                anchor,
                paint,
                icons,
                background,
                ..
            } => {
                let v = variants.get(variant);
                let block = v.and_then(|v| v.block).and_then(|ix| blocks.get(ix));
                let icon = v.and_then(|v| v.icon).and_then(|ix| icons.get(ix));
                let at = |r: Aabb| Aabb {
                    min_x: anchor.0 + r.min_x,
                    min_y: anchor.1 + r.min_y,
                    max_x: anchor.0 + r.max_x,
                    max_y: anchor.1 + r.max_y,
                };
                if let (true, Some(icon)) = (self.wants(Part::Icon), icon) {
                    let (w, h) = (
                        icon.image.width as f32 * icon.scale,
                        icon.image.height as f32 * icon.scale,
                    );
                    out.rotated(
                        (anchor.0 + icon.offset.0, anchor.1 + icon.offset.1),
                        icon.rotation_deg.to_radians(),
                        (-0.5 * w, -0.5 * h, 0.5 * w, 0.5 * h),
                        &props,
                        Part::Icon,
                    );
                }
                if let Some(block) = block {
                    if let (true, Some(bg)) = (self.wants(Part::Background), background) {
                        out.rect(at(bg.rect(block, paint.size_px)), &props, Part::Background);
                    }
                    if self.wants(Part::Text) {
                        out.rect(at(text_rect(block, paint.size_px)), &props, Part::Text);
                    }
                }
            }
            LabelDraw::Line {
                block,
                placements,
                perp_px,
                paint,
                ..
            } => {
                if self.wants(Part::Glyph) {
                    let size = paint.size_px;
                    // Each glyph's line box, as `draw_line` stamps it: half
                    // an advance either side of its centre sample, the
                    // block's line height across, shifted by the
                    // perpendicular offset.
                    let (y0, y1) = (
                        block.bbox.min_y * size + perp_px,
                        block.bbox.max_y * size + perp_px,
                    );
                    for (g, p) in block.glyphs.iter().zip(placements) {
                        let hw = 0.5 * g.advance * size;
                        out.rotated((p.x, p.y), p.angle, (-hw, y0, hw, y1), &props, Part::Glyph);
                    }
                }
            }
        }
        if self.wants(Part::Collision) {
            for b in c.variants.get(variant).into_iter().flatten() {
                out.rect(*b, &props, Part::Collision);
            }
        }
    }
}

impl Node for LabelBoxesNode {
    fn op_name(&self) -> &'static str {
        "label-boxes"
    }
    fn inputs(&self) -> &[PortSpec] {
        &self.ports
    }
    fn output(&self, _input_kinds: &[Option<PortKind>]) -> PortKind {
        PortKind::Features
    }
    fn coord_space(&self) -> CoordSpace {
        // The boxes come from world-anchored placement decisions.
        CoordSpace::World
    }
    fn eval(
        &self,
        ctx: &EvalCtx<'_>,
        inputs: &[Option<PortValue>],
    ) -> Result<PortValue, EvalError> {
        let placement = labels_input::<PlacedLabels>(inputs[0].as_ref(), "placement")?;
        let only = match inputs.get(1).and_then(Option::as_ref) {
            Some(v) => Some(labels_input::<LabelSet>(Some(v), "labels")?.id),
            None => None,
        };
        if let Some(id) = only {
            if placement.get(id).is_none() {
                return Err(EvalError::Other(
                    "label-boxes: the `labels` layer is absent from the `placement` node — \
                     wire the same `text-labels` node into both"
                        .into(),
                ));
            }
        }
        let mut out = Emitter {
            sx: self.extent as f32 / ctx.canvas.tile_w.max(1) as f32,
            sy: self.extent as f32 / ctx.canvas.tile_h.max(1) as f32,
            groups: Vec::new(),
        };
        for (layer, placed) in placement.layers.iter().enumerate() {
            let set = &placed.set;
            if only.is_some_and(|id| id != set.id) {
                continue;
            }
            for p in &placed.outcome.placed {
                self.emit_label(&mut out, layer, set, p.cand, p.variant, true);
            }
            if self.include_rejected {
                // A losing candidate is shown as it first tried to place.
                for &cand in &placed.outcome.rejected {
                    self.emit_label(&mut out, layer, set, cand, 0, false);
                }
            }
        }
        Ok(features_value_culled(ctx, self.extent, 0, out.groups))
    }
    fn param_hash(&self, h: &mut Xxh3) {
        h.update(b"label-boxes");
        h.update(&[self.parts, self.include_rejected as u8]);
        h.update(&self.extent.to_le_bytes());
        h.update(&(self.ports.len() as u64).to_le_bytes());
    }
}

pub(super) struct LabelBoxesFactory;
impl NodeFactory for LabelBoxesFactory {
    fn op_name(&self) -> &'static str {
        "label-boxes"
    }
    fn build(
        &self,
        fields: &serde_json::Map<String, Value>,
        _ctx: &FactoryCtx<'_>,
    ) -> Result<BuiltNode, FactoryError> {
        let mut ports = vec![PortSpec::new("placement", ACCEPTS_LABELS)];
        let mut connections = vec![Connection {
            port: "placement".into(),
            src: take_input_ref(fields, "placement")?,
        }];
        if let Some(src) = take_optional_input_ref(fields, "labels")? {
            ports.push(PortSpec::new("labels", ACCEPTS_LABELS));
            connections.push(Connection {
                port: "labels".into(),
                src,
            });
        }
        let parts = match fields.get("parts") {
            None => Part::ALL.iter().fold(0, |m, p| m | p.bit()),
            Some(v) => {
                let bad = |msg: String| FactoryError::BadField {
                    field: "parts".into(),
                    msg,
                };
                let arr = v
                    .as_array()
                    .ok_or_else(|| bad("expected an array of part names".into()))?;
                let mut mask = 0u8;
                for entry in arr {
                    let name = entry.as_str().unwrap_or_default();
                    let part = Part::ALL
                        .iter()
                        .find(|p| p.name() == name)
                        .ok_or_else(|| bad(format!("unknown part {entry}")))?;
                    mask |= part.bit();
                }
                mask
            }
        };
        let include_rejected = match fields.get("include").map(|v| v.as_str()) {
            None | Some(Some("placed")) => false,
            Some(Some("all")) => true,
            Some(_) => {
                return Err(FactoryError::BadField {
                    field: "include".into(),
                    msg: "expected `placed` or `all`".into(),
                })
            }
        };
        let extent = match fields.get("extent") {
            None => DEFAULT_EXTENT,
            Some(v) => v
                .as_u64()
                .filter(|&e| e >= 1 && e <= u32::MAX as u64)
                .ok_or_else(|| FactoryError::BadField {
                    field: "extent".into(),
                    msg: "expected a positive integer".into(),
                })? as u32,
        };
        Ok(BuiltNode {
            node: Box::new(LabelBoxesNode {
                parts,
                include_rejected,
                extent,
                ports,
            }),
            connections,
        })
    }
    fn schema(&self) -> Value {
        let mut labels = schema_frag::node_ref();
        labels["description"] = Value::String(
            "Only this `text-labels` layer of the placement (it must be wired into the \
             `placement` node too). Default: every layer."
                .into(),
        );
        serde_json::json!({
            "description": "The boxes a `label-placement` node measured its labels by, as polygons — one per box — so any geometry and paint op can draw them: a bordered or rounded badge (`buffer` → `stroke`), an offset shadow (`transform`), or a debug overlay of placement. Reads the decisions already made, so nothing is placed twice and the boxes match what `text-draw` draws; labels placed from a neighbour tile's candidates are included, so boxes continue across tile edges. Each polygon carries the labelled feature's properties plus `part` (`text`, `icon`, `background`, `glyph` or `collision`), `placed` (false for a candidate that lost its collision test, with `include: all`) and `label-id` (shared by every box of one label, the same on every tile). A point label gives its `text` block (no padding), `icon` (rotated by `icon-rotate`), `background` box (with `background-color`) and the `collision` box or boxes it was tested with, padding included. A line label gives one `glyph` quad per glyph, rotated along the path, and the axis-aligned `collision` box placement tested for each glyph.",
            "properties": {
                "placement": schema_frag::node_ref(),
                "labels": labels,
                "parts": {
                    "type": "array",
                    "items": { "enum": ["text", "icon", "background", "glyph", "collision"] },
                    "description": "Which boxes to emit. Default: every kind a label has."
                },
                "include": {
                    "enum": ["placed", "all"],
                    "default": "placed",
                    "description": "`placed` emits the labels that placed; `all` adds the candidates that lost their collision test, with `placed: false`, at their first anchor. Duplicate copies of one label are never emitted."
                },
                "extent": { "type": "integer", "minimum": 1, "default": DEFAULT_EXTENT,
                            "description": "Coordinate extent of the emitted geometry, matching the features it is combined with." }
            },
            "required": ["placement"],
        })
    }
}

ezu_graph::submit_node!(LabelBoxesFactory);
