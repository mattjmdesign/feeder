//! The app icon: a squircle tile filled with marbles, one per project, drawn with the same orbs
//! the Home screen uses. `scripts/mac-app.sh` renders it with
//! `--scene app-icon --size 1024x1024 --scale 1` and turns the PNG into an `.icns`.

use feeder_core::{Orb, Pattern};
use rune::{El, el, oklch};

use crate::orb;

const fn marble(seed: u64, hues: [f32; 3], chroma: f32, angle: f32, pattern: Pattern) -> Orb {
    Orb { seed, hues, chroma, angle, pattern }
}

/// Marbles packed inside the tile: centre x, centre y, radius (tile is 824 wide), and the orb.
const MARBLES: [(f32, f32, f32, Orb); 9] = [
    (420.0, 409.0, 183.0, marble(0x5EED_F00D, [292.0, 205.0, 350.0], 0.21, 2.2, Pattern::Nebula)),
    (182.0, 181.0, 139.0, marble(0xA11C_E001, [32.0, 12.0, 58.0], 0.2, 0.9, Pattern::Ember)),
    (179.0, 641.0, 140.0, marble(0xB0BA_0002, [168.0, 196.0, 140.0], 0.18, 1.6, Pattern::Tide)),
    (647.0, 645.0, 135.0, marble(0xC0DE_0003, [248.0, 270.0, 220.0], 0.2, 2.6, Pattern::Aurora)),
    (649.0, 178.0, 133.0, marble(0xD00D_0004, [310.0, 280.0, 340.0], 0.12, 0.4, Pattern::Pearl)),
    (414.0, 692.0, 93.0, marble(0xE66E_0005, [96.0, 120.0, 70.0], 0.19, 1.1, Pattern::Nebula)),
    (700.0, 418.0, 90.0, marble(0xF00D_0006, [350.0, 10.0, 325.0], 0.2, 2.0, Pattern::Eclipse)),
    (412.0, 126.0, 90.0, marble(0x1CE0_0007, [205.0, 230.0, 185.0], 0.2, 1.3, Pattern::Ember)),
    (143.0, 411.0, 86.0, marble(0x2BAD_0008, [60.0, 40.0, 85.0], 0.17, 0.7, Pattern::Aurora)),
];

pub fn view() -> El {
    let tile = 824.0;
    let radius = tile * 0.225;

    let mut inner = el().size(tile, tile).radius(radius).continuous_corners().gradient(oklch(0.25, 0.02, 280.0), oklch(0.13, 0.012, 266.0));

    for (x, y, r, o) in MARBLES {
        let d = r * 2.0;
        // Contact shadow under each marble so it sits in the tray instead of floating on it.
        let shadow = el().size(d * 1.2, d * 1.2).absolute(x - r * 1.2, y - r * 1.2 + r * 0.16).radial_gradient(
            [0.5, 0.5],
            r * 1.2,
            [(0.0, oklch(0.0, 0.0, 0.0).alpha(0.55)), (0.72, oklch(0.0, 0.0, 0.0).alpha(0.22)), (1.0, oklch(0.0, 0.0, 0.0).alpha(0.0))],
        );
        inner = inner.child(shadow).child(el().size(d, d).absolute(x - r, y - r).child(orb::orb(&o, d)));
    }

    let rim = el().size(tile, tile).absolute(0.0, 0.0).radius(radius).continuous_corners().border_gradient(
        3.0,
        oklch(1.0, 0.0, 0.0).alpha(0.24),
        oklch(1.0, 0.0, 0.0).alpha(0.05),
    );

    let tile_frame = el()
        .size(tile, tile)
        .radius(radius)
        .continuous_corners()
        .shadow(16.0, 32.0, oklch(0.0, 0.0, 0.0).alpha(0.36))
        .shadow(4.0, 10.0, oklch(0.0, 0.0, 0.0).alpha(0.28))
        .child(inner)
        .child(rim);

    el().key("icon").w_full().h_full().center().child(tile_frame)
}
