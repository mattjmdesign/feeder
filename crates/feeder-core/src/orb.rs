//! Each project's orb: a palette and a pattern chosen once, when the project is first seen, and
//! stored with it. New orbs pick the hue farthest from every orb already on the shelf and the
//! pattern used least, so no two read alike. The desktop app turns this into gradients.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Pattern {
    /// Soft clouds of colour drifting over a deep base.
    Nebula,
    /// Bands of light swept across the sphere at an angle.
    Aurora,
    /// Pale, iridescent, mostly highlight.
    Pearl,
    /// A dark body with a bright core and a glowing ring.
    Eclipse,
    /// Two hues meeting along a soft horizon.
    Tide,
    /// Hot centre burning out to a dark rim.
    Ember,
}

impl Pattern {
    pub const ALL: [Pattern; 6] = [Pattern::Nebula, Pattern::Aurora, Pattern::Pearl, Pattern::Eclipse, Pattern::Tide, Pattern::Ember];
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Orb {
    pub seed: u64,
    /// Primary, secondary and tertiary hues in OKLCH degrees.
    pub hues: [f32; 3],
    /// OKLCH chroma of the strongest colour, 0.08..0.24.
    pub chroma: f32,
    /// Angle in radians that bands, horizons and highlights follow.
    pub angle: f32,
    pub pattern: Pattern,
}

impl Orb {
    /// A new orb, as unlike `existing` as the palette allows, with clean analogous harmony.
    pub fn generate(existing: &[Orb]) -> Orb {
        let mut rng = fastrand::Rng::new();
        let hue = pick_hue(existing, &mut rng);
        let (d2, d3) = match rng.u8(0..4) {
            0 => (rng.f32() * 14.0 + 18.0, rng.f32() * 16.0 + 36.0),       // warm step forward
            1 => (-(rng.f32() * 14.0 + 18.0), -(rng.f32() * 16.0 + 36.0)), // cool step back
            2 => (rng.f32() * 16.0 + 16.0, -(rng.f32() * 16.0 + 16.0)),    // balanced flank
            _ => (rng.f32() * 12.0 + 24.0, rng.f32() * 12.0 + 10.0),       // tight tonal pair
        };
        let pattern = pick_pattern(existing, &mut rng);
        Orb {
            seed: rng.u64(..),
            hues: [hue, wrap_hue(hue + d2), wrap_hue(hue + d3)],
            chroma: 0.15 + rng.f32() * 0.06,
            angle: rng.f32() * std::f32::consts::TAU,
            pattern,
        }
    }

    /// Deterministic values in 0..1 drawn from the orb's seed, for blob positions and sizes.
    pub fn noise(&self, i: u64) -> f32 {
        let mut z = self.seed.wrapping_add(i.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 40) as f32 / (1u64 << 24) as f32
    }
}

fn wrap_hue(h: f32) -> f32 {
    h.rem_euclid(360.0)
}

fn hue_distance(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}

fn pick_hue(existing: &[Orb], rng: &mut fastrand::Rng) -> f32 {
    let candidates = (0..48).map(|_| rng.f32() * 360.0);
    if existing.is_empty() {
        return rng.f32() * 360.0;
    }
    candidates
        .map(|h| {
            // Closest primary counts most; the other two hues of each orb count a little.
            let score = existing
                .iter()
                .map(|o| hue_distance(h, o.hues[0]).min(hue_distance(h, o.hues[1]) + 25.0).min(hue_distance(h, o.hues[2]) + 25.0))
                .fold(f32::MAX, f32::min);
            (h, score)
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(h, _)| h)
        .unwrap_or(0.0)
}

fn pick_pattern(existing: &[Orb], rng: &mut fastrand::Rng) -> Pattern {
    let count = |p: Pattern| existing.iter().filter(|o| o.pattern == p).count();
    let fewest = Pattern::ALL.iter().map(|&p| count(p)).min().unwrap_or(0);
    let options: Vec<Pattern> = Pattern::ALL.iter().copied().filter(|&p| count(p) == fewest).collect();
    options[rng.usize(..options.len())]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_orbs_spread_their_hues_and_patterns() {
        let mut shelf = Vec::new();
        for _ in 0..6 {
            let o = Orb::generate(&shelf);
            shelf.push(o);
        }
        let mut patterns: Vec<_> = shelf.iter().map(|o| o.pattern).collect();
        patterns.sort_by_key(|p| *p as u8);
        patterns.dedup();
        assert_eq!(patterns.len(), 6, "six orbs use all six patterns");
        for (i, a) in shelf.iter().enumerate() {
            for b in &shelf[i + 1..] {
                assert!(hue_distance(a.hues[0], b.hues[0]) > 15.0, "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn noise_is_deterministic_and_in_range() {
        let o = Orb::generate(&[]);
        for i in 0..100 {
            let n = o.noise(i);
            assert!((0.0..1.0).contains(&n));
            assert_eq!(n, o.noise(i));
        }
    }
}
