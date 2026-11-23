use feeder_core::{Orb, Pattern};
use rune::{El, el};

use crate::{orb, style};

const ICON_ORB: Orb = Orb { seed: 0x5EED_F00D, hues: [292.0, 205.0, 350.0], chroma: 0.19, angle: 2.2, pattern: Pattern::Nebula };

pub fn view() -> El {
    let p = style::palette(true);
    let tile = 824.0;
    let sphere = 560.0;
    el().key("icon").w_full().h_full().center().child(
        el().size(tile, tile)
            .radius(tile * 0.225)
            .continuous_corners()
            .fill(p.bg)
            .border(2.0, p.hairline)
            .center()
            .child(orb::orb(&ICON_ORB, sphere)),
    )
}
