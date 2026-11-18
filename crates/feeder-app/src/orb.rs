//! Draws a project's [`Orb`] as stacked gradients: a pattern body, then the same rim shade, key
//! light and bounce light on every orb, so they all read as one family of glass spheres lit from
//! the upper left.

use feeder_core::{Orb, Pattern};
use rune::{Color, El, el, oklch};

/// Harmonize secondary/tertiary hues so even legacy orbs with distant hues stay clean and cohesive.
fn harmonized_hue(o: &Orb, i: usize) -> f32 {
    let h0 = o.hues[0];
    let hi = o.hues[i % 3];
    if i.is_multiple_of(3) {
        return h0;
    }
    let delta = (hi - h0 + 540.0).rem_euclid(360.0) - 180.0;
    (h0 + delta.clamp(-42.0, 42.0)).rem_euclid(360.0)
}

/// A colour from the orb's palette: hue `i` (0..3) at lightness `l` and `c` × its chroma.
fn tone(o: &Orb, i: usize, l: f32, c: f32) -> Color {
    let base_c = o.chroma.max(0.15);
    oklch(l, (base_c * c).min(0.24), harmonized_hue(o, i))
}

/// The orb's signature colour, for dots, glows and accents elsewhere.
pub fn accent(o: &Orb) -> Color {
    match o.pattern {
        Pattern::Pearl => tone(o, 0, 0.8, 0.9),
        _ => tone(o, 0, 0.72, 1.0),
    }
}

/// Soft breathing halo placed behind an orb when an agent session has an active pulse.
pub fn pulse_halo(o: &Orb, size: f32, phase: f32) -> El {
    let pad = (size * 0.22).clamp(14.0, 26.0);
    let total = size + pad * 2.0;
    let c = accent(o);
    let strength = 0.24 + 0.16 * phase.clamp(0.0, 1.0);
    el().size(total, total).absolute(-pad, -pad).radius(total / 2.0).radial_gradient(
        [0.5, 0.5],
        total / 2.0,
        [(0.0, c.alpha(0.0)), (0.68, c.alpha(0.0)), (0.80, c.alpha(strength)), (1.0, c.alpha(0.0))],
    )
}

pub fn orb(o: &Orb, size: f32) -> El {
    let r = size / 2.0;
    let layer = || el().size(size, size).absolute(0.0, 0.0).radius(r);
    let mut body = el().size(size, size).radius(r).fill(base(o));

    for paint in pattern_layers(o, size) {
        body = body.child(paint(layer()));
    }

    // Shared lighting: a subtle rim shade, soft bottom bounce, crisp edge highlight, and key glint.
    let rim = layer().radial_gradient([0.5, 0.5], r, [(0.0, black(0.0)), (0.68, black(0.0)), (0.9, black(0.12)), (1.0, black(0.28))]);
    let caustic_tone = tone(o, 1, 0.86, 0.75);
    let caustic = layer().radial_gradient(
        [0.62, 0.82],
        [r * 0.58, r * 0.34],
        [(0.0, caustic_tone.alpha(0.45)), (0.55, caustic_tone.alpha(0.12)), (1.0, caustic_tone.alpha(0.0))],
    );
    let edge = layer().radial_gradient([0.5, 0.5], r, [(0.0, white(0.0)), (0.91, white(0.0)), (0.965, white(0.14)), (1.0, white(0.0))]);
    let key = layer().radial_gradient([0.34, 0.25], [r * 0.56, r * 0.40], [(0.0, white(0.42)), (0.38, white(0.14)), (1.0, white(0.0))]);
    let glint = layer().radial_gradient([0.31, 0.21], [r * 0.14, r * 0.10], [(0.0, white(0.88)), (0.6, white(0.28)), (1.0, white(0.0))]);
    let second = layer().radial_gradient([0.72, 0.3], r * 0.06, [(0.0, white(0.3)), (1.0, white(0.0))]);
    body.child(rim).child(caustic).child(edge).child(key).child(glint).child(second)
}

fn base(o: &Orb) -> Color {
    match o.pattern {
        Pattern::Nebula => tone(o, 0, 0.44, 0.95),
        Pattern::Aurora => tone(o, 0, 0.48, 0.95),
        Pattern::Pearl => tone(o, 0, 0.78, 0.55),
        Pattern::Eclipse => tone(o, 0, 0.28, 0.7),
        Pattern::Tide => tone(o, 1, 0.46, 0.95),
        Pattern::Ember => tone(o, 1, 0.36, 0.9),
    }
}

type Paint = Box<dyn Fn(El) -> El>;

fn pattern_layers(o: &Orb, size: f32) -> Vec<Paint> {
    let o = *o;
    let r = size / 2.0;
    let n = move |i: u64| o.noise(i);
    match o.pattern {
        Pattern::Nebula => (0..4u64)
            .map(|i| {
                let c = tone(&o, i as usize, 0.62 + n(i * 7) * 0.14, 1.0);
                let center = [0.2 + n(i * 3) * 0.6, 0.2 + n(i * 3 + 1) * 0.6];
                let rad = r * (0.6 + n(i * 3 + 2) * 0.5);
                Box::new(move |l: El| l.radial_gradient(center, rad, [(0.0, c.alpha(0.85)), (0.6, c.alpha(0.28)), (1.0, c.alpha(0.0))]))
                    as Paint
            })
            .collect(),
        Pattern::Aurora => {
            let bands = [
                (0.0, tone(&o, 0, 0.46, 0.95)),
                (0.35, tone(&o, 1, 0.64, 1.0)),
                (0.55, tone(&o, 2, 0.76, 0.9)),
                (0.75, tone(&o, 0, 0.58, 1.0)),
                (1.0, tone(&o, 1, 0.42, 0.95)),
            ];
            let haze = tone(&o, 2, 0.74, 0.95);
            let at = [0.32 + n(1) * 0.36, 0.32 + n(2) * 0.36];
            vec![
                Box::new(move |l: El| l.linear_gradient(o.angle, bands)),
                Box::new(move |l: El| l.radial_gradient(at, r * 0.75, [(0.0, haze.alpha(0.45)), (1.0, haze.alpha(0.0))])),
            ]
        }
        Pattern::Pearl => (0..4u64)
            .map(|i| {
                let c = tone(&o, i as usize, 0.82 + n(i) * 0.06, 0.72);
                let center = [0.15 + n(i * 5) * 0.7, 0.15 + n(i * 5 + 1) * 0.7];
                let rad = r * (0.65 + n(i * 5 + 2) * 0.45);
                Box::new(move |l: El| l.radial_gradient(center, rad, [(0.0, c.alpha(0.85)), (1.0, c.alpha(0.0))])) as Paint
            })
            .collect(),
        Pattern::Eclipse => {
            let ring = tone(&o, 1, 0.74, 1.0);
            let ring_outer = tone(&o, 0, 0.56, 1.0);
            let core = tone(&o, 2, 0.86, 0.65);
            let core_mid = tone(&o, 2, 0.66, 1.0);
            let cc = [0.44 + n(1) * 0.12, 0.44 + n(2) * 0.12];
            vec![
                Box::new(move |l: El| {
                    l.radial_gradient(
                        [0.5, 0.5],
                        r,
                        [
                            (0.0, ring.alpha(0.0)),
                            (0.56, ring.alpha(0.0)),
                            (0.72, ring.alpha(0.8)),
                            (0.86, ring_outer.alpha(0.42)),
                            (1.0, ring_outer.alpha(0.0)),
                        ],
                    )
                }),
                Box::new(move |l: El| {
                    l.radial_gradient(cc, r * 0.66, [(0.0, core), (0.4, core_mid.alpha(0.85)), (1.0, core_mid.alpha(0.0))])
                }),
            ]
        }
        Pattern::Tide => {
            let horizon = 0.44 + n(4) * 0.12;
            let stops = [
                (0.0, tone(&o, 0, 0.76, 0.9)),
                (horizon - 0.1, tone(&o, 0, 0.64, 1.0)),
                (horizon + 0.1, tone(&o, 1, 0.5, 1.0)),
                (1.0, tone(&o, 1, 0.38, 0.95)),
            ];
            let glow = tone(&o, 2, 0.76, 0.95);
            let angle = std::f32::consts::PI + (o.angle - std::f32::consts::PI) * 0.25;
            vec![
                Box::new(move |l: El| l.linear_gradient(angle, stops)),
                Box::new(move |l: El| {
                    l.radial_gradient([0.5, horizon], [r * 1.1, r * 0.24], [(0.0, glow.alpha(0.5)), (1.0, glow.alpha(0.0))])
                }),
            ]
        }
        Pattern::Ember => {
            let c = [0.42 + n(1) * 0.16, 0.44 + n(2) * 0.16];
            let stops = [
                (0.0, tone(&o, 2, 0.92, 0.55)),
                (0.28, tone(&o, 0, 0.78, 1.0)),
                (0.65, tone(&o, 1, 0.56, 1.0)),
                (1.0, tone(&o, 1, 0.36, 0.9).alpha(0.0)),
            ];
            vec![Box::new(move |l: El| l.radial_gradient(c, r * 1.15, stops))]
        }
    }
}

fn white(a: f32) -> Color {
    oklch(1.0, 0.0, 0.0).alpha(a)
}
fn black(a: f32) -> Color {
    oklch(0.0, 0.0, 0.0).alpha(a)
}
