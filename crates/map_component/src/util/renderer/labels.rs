use std::collections::HashMap;

use crate::style::{SymbolLayer, TextPlacement, TextTransform};
use crate::util::labels::label_identity;

use super::compile::{CompiledLabel, LabelAnchor};
use super::{FeatureGeometry, Properties, RenderableTile};

/// Pulls ready-to-place labels out of one symbol layer's matching features. Zoom gating here is
/// deliberately loose (±1 around the compile `zoom_z`): the exact `[min_zoom, max_zoom]` gate
/// runs per-frame against the continuous zoom, so labels fade in/out smoothly instead of popping
/// only when `zoom_z` changes. Tiles at the source's deepest zoom (`max_real_zoom`) skip the lower
/// bound entirely: they're what every deeper zoom over-zooms, so a layer starting at e.g. 15.5
/// (footpath names) would otherwise never be shown at all.
pub fn extract_labels(
    tile: &RenderableTile,
    symbol_layer: &SymbolLayer,
    style_index: usize,
    zoom: f64,
    max_real_zoom: u32,
    labels: &mut Vec<CompiledLabel>,
) {
    let deepest = zoom >= max_real_zoom as f64;
    if (!deepest && zoom + 1.0 < symbol_layer.min_zoom) || zoom - 1.0 > symbol_layer.max_zoom {
        return;
    }
    let Some(text_field) = &symbol_layer.text_field else {
        return;
    };
    let Some(features) = tile.source_layers.get(&symbol_layer.source_layer) else {
        return;
    };

    // Line-placed labels grouped by evaluated text: (properties, sort_value, polyline fragments).
    let mut line_groups: HashMap<String, (std::sync::Arc<Properties>, f64, Vec<Vec<(f32, f32)>>)> =
        HashMap::new();

    for feature in features {
        if !symbol_layer
            .filter
            .as_ref()
            .is_none_or(|f| f.eval(&feature.properties))
        {
            continue;
        }

        let text = single_line(&text_field.eval(&feature.properties));
        if text.is_empty() {
            continue;
        }
        let text = match symbol_layer.text_transform {
            TextTransform::None => text,
            TextTransform::Uppercase => text.to_uppercase(),
            TextTransform::Lowercase => text.to_lowercase(),
        };

        let sort_value = symbol_layer
            .sort_key
            .as_ref()
            .map(|key| key.eval(&feature.properties))
            .unwrap_or(f64::MAX);

        match symbol_layer.placement {
            TextPlacement::Point => {
                if let FeatureGeometry::Points(points) = &feature.geometry {
                    for &(x, y) in points {
                        labels.push(CompiledLabel {
                            identity: label_identity(&text, style_index),
                            text: text.clone(),
                            style_index,
                            anchor: LabelAnchor::Point { x, y },
                            sort_value,
                            properties: feature.properties.clone(),
                            feature_id: feature.id,
                        });
                    }
                }
            }
            // Line-placed labels are grouped by name and merged below — a street is split into
            // many small features per tile (at intersections/attribute changes), and labeling
            // each fragment separately gives short, unstable baselines that flap as zoom
            // re-splits the geometry.
            TextPlacement::Line => {
                if let Some(lines) = &feature.line_coords {
                    line_groups
                        .entry(text)
                        .or_insert_with(|| (feature.properties.clone(), sort_value, Vec::new()))
                        .2
                        .extend(lines.iter().cloned());
                }
            }
        }
    }

    for (text, (properties, sort_value, lines)) in line_groups {
        // Chain fragments that share endpoints into long continuous polylines, then label only
        // the longest chain — one stable, well-fitting label per street name per tile.
        let chains = merge_polylines(lines);
        let Some(anchor) = line_label_anchor(&chains) else {
            continue;
        };
        labels.push(CompiledLabel {
            identity: label_identity(&text, style_index),
            text,
            style_index,
            anchor,
            sort_value,
            properties,
            feature_id: None,
        });
    }
}

/// Splices polyline fragments that share an endpoint (within `EPS` in normalized tile space)
/// into longer chains — MVT splits one street into many features, and label placement wants the
/// street's full run, not a fragment.
fn merge_polylines(mut lines: Vec<Vec<(f32, f32)>>) -> Vec<Vec<(f32, f32)>> {
    const EPS: f32 = 1e-4;
    let close = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).abs() < EPS && (a.1 - b.1).abs() < EPS;

    lines.retain(|line| line.len() >= 2);
    let mut merged: Vec<Vec<(f32, f32)>> = Vec::new();

    while let Some(mut chain) = lines.pop() {
        loop {
            let chain_start = *chain.first().unwrap();
            let chain_end = *chain.last().unwrap();

            let Some(index) = lines.iter().position(|other| {
                let other_start = *other.first().unwrap();
                let other_end = *other.last().unwrap();
                close(chain_end, other_start)
                    || close(chain_end, other_end)
                    || close(chain_start, other_start)
                    || close(chain_start, other_end)
            }) else {
                break;
            };

            let mut other = lines.swap_remove(index);
            let other_start = *other.first().unwrap();
            let other_end = *other.last().unwrap();

            if close(chain_end, other_start) {
                chain.extend(other.into_iter().skip(1));
            } else if close(chain_end, other_end) {
                other.reverse();
                chain.extend(other.into_iter().skip(1));
            } else if close(chain_start, other_end) {
                other.pop();
                other.extend(chain);
                chain = other;
            } else {
                other.reverse();
                other.pop();
                other.extend(chain);
                chain = other;
            }
        }
        merged.push(chain);
    }
    merged
}

fn polyline_length(line: &[(f32, f32)]) -> f32 {
    line.windows(2)
        .map(|w| {
            let (x0, y0) = w[0];
            let (x1, y1) = w[1];
            ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt()
        })
        .sum()
}

/// Midpoint + tangent of the feature's longest linestring — the anchor for straight rotated
/// street-name text. The angle is flipped into `(-π/2, π/2]` so text never renders upside-down.
/// Labels are drawn as one line: a multi-line `text-field` (osm-bright's
/// `"{name:latin}\n{name:nonlatin}"`) is joined with spaces, skipping empty lines — otherwise the
/// newline was drawn as part of the text.
fn single_line(text: &str) -> String {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let Some(first) = lines.next() else {
        return String::new();
    };
    lines.fold(first.to_string(), |mut out, line| {
        out.push(' ');
        out.push_str(line);
        out
    })
}

/// Flips a segment angle into `[-π/2, π/2]` so text along it is never upside-down.
pub fn readable_angle(mut angle: f32) -> f32 {
    if angle > std::f32::consts::FRAC_PI_2 {
        angle -= std::f32::consts::PI;
    } else if angle < -std::f32::consts::FRAC_PI_2 {
        angle += std::f32::consts::PI;
    }
    angle
}

fn line_label_anchor(lines: &[Vec<(f32, f32)>]) -> Option<LabelAnchor> {
    let (line, total) = lines
        .iter()
        .map(|line| (line, polyline_length(line)))
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    if total <= 0.0 || line.len() < 2 {
        return None;
    }

    let mut remaining = total / 2.0;
    for w in line.windows(2) {
        let (x0, y0) = w[0];
        let (x1, y1) = w[1];
        let seg = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
        if seg >= remaining && seg > 0.0 {
            let t = remaining / seg;
            let angle = (y1 - y0).atan2(x1 - x0);
            return Some(LabelAnchor::Line {
                x: x0 + (x1 - x0) * t,
                y: y0 + (y1 - y0) * t,
                angle: readable_angle(angle),
                len_norm: total,
                chain: line.clone(),
            });
        }
        remaining -= seg;
    }
    None
}
