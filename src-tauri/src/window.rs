//! 0.5.0: the window remembers its size and place (settings.json `window`), fits small laptop screens at 125-150 %
//! scaling, and (when the player turns it on in «Настройки») minimizes to the tray next to the clock.
//!
//! All sizes here are logical pixels (what the page sees): a 1366 x 768 laptop at 150 % is about 910 x 480 of them
//! (minus the taskbar), smaller than the default 1120 x 680 window.

use serde::{Deserialize, Serialize};

/// What settings.json keeps about the window (logical pixels; x, y = the outer top-left corner).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Geometry {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub maximized: bool,
}

/// A monitor's work area (without the taskbar), logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Area {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Where to put the window at start.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// None: centre it on the main monitor.
    pub pos: Option<(f64, f64)>,
    pub size: (f64, f64),
    pub maximized: bool,
}

/// The smallest size the window allows (tauri.conf.json minWidth/minHeight) and the default size.
pub const MIN: (f64, f64) = (640.0, 460.0);
pub const DEFAULT: (f64, f64) = (1120.0, 680.0);
/// At least this much of the saved window must be on some monitor, or it is put back in the centre.
const VISIBLE: f64 = 120.0;

fn overlap(a: &Area, x: f64, y: f64, w: f64, h: f64) -> f64 {
    let ox = (a.x + a.w).min(x + w) - a.x.max(x);
    let oy = (a.y + a.h).min(y + h) - a.y.max(y);
    if ox <= 0.0 || oy <= 0.0 {
        0.0
    } else {
        ox.min(oy)
    }
}

/// The saved place if it is still on a monitor (a monitor may be gone), cut to fit that monitor; else the default
/// size, made smaller to fit the main monitor (`areas[0]`), centred.
pub fn place(saved: Option<Geometry>, areas: &[Area]) -> Placement {
    let fit = |w: f64, h: f64, a: &Area| (w.min(a.w).max(MIN.0.min(a.w)), h.min(a.h).max(MIN.1.min(a.h)));
    if let Some(g) = saved.filter(|g| g.w >= 100.0 && g.h >= 100.0 && g.w.is_finite() && g.h.is_finite()) {
        if let Some(a) = areas.iter().find(|a| overlap(a, g.x, g.y, g.w, g.h) >= VISIBLE) {
            let (w, h) = fit(g.w, g.h, a);
            // keep it inside the monitor it is on
            let x = g.x.max(a.x).min(a.x + a.w - w);
            let y = g.y.max(a.y).min(a.y + a.h - h);
            return Placement { pos: Some((x, y)), size: (w, h), maximized: g.maximized };
        }
    }
    let size = match areas.first() {
        // a little air around it on a small screen
        Some(a) if a.w < DEFAULT.0 + 40.0 || a.h < DEFAULT.1 + 40.0 => fit((a.w - 24.0).min(DEFAULT.0), (a.h - 24.0).min(DEFAULT.1), a),
        _ => DEFAULT,
    };
    Placement { pos: None, size, maximized: saved.map_or(false, |g| g.maximized) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL_HD: Area = Area { x: 0.0, y: 0.0, w: 1920.0, h: 1032.0 };
    // 1366 x 768 at 125 % and 150 %, minus the taskbar
    const LAPTOP_125: Area = Area { x: 0.0, y: 0.0, w: 1093.0, h: 582.0 };
    const LAPTOP_150: Area = Area { x: 0.0, y: 0.0, w: 911.0, h: 480.0 };

    #[test]
    fn first_start_fits_the_screen() {
        assert_eq!(place(None, &[FULL_HD]), Placement { pos: None, size: DEFAULT, maximized: false });
        let p = place(None, &[LAPTOP_125]);
        assert_eq!(p.pos, None);
        assert!(p.size.0 <= LAPTOP_125.w && p.size.1 <= LAPTOP_125.h, "{p:?}");
        assert!(p.size.0 >= MIN.0 && p.size.1 >= MIN.1, "{p:?}");
        let p = place(None, &[LAPTOP_150]);
        assert!(p.size.0 <= LAPTOP_150.w && p.size.1 <= LAPTOP_150.h, "{p:?}");
        // no monitor known: the default
        assert_eq!(place(None, &[]).size, DEFAULT);
    }

    #[test]
    fn saved_place_comes_back_if_it_is_on_a_monitor() {
        let g = Geometry { x: 300.0, y: 200.0, w: 1300.0, h: 760.0, maximized: false };
        assert_eq!(place(Some(g), &[FULL_HD]), Placement { pos: Some((300.0, 200.0)), size: (1300.0, 760.0), maximized: false });
        // a second monitor on the right that is gone now: back to the centre
        let right = Geometry { x: 2100.0, y: 100.0, ..g };
        assert_eq!(place(Some(right), &[FULL_HD]).pos, None);
        let second = Area { x: 1920.0, y: 0.0, w: 1920.0, h: 1032.0 };
        assert_eq!(place(Some(right), &[FULL_HD, second]).pos, Some((2100.0, 100.0)));
        // hanging off the bottom-right edge: pulled back in
        let off = Geometry { x: 1500.0, y: 800.0, w: 1120.0, h: 680.0, maximized: false };
        let p = place(Some(off), &[FULL_HD]);
        assert_eq!(p.pos, Some((800.0, 352.0)));
        // the same saved size on a small laptop: cut to the screen
        let p = place(Some(Geometry { x: 0.0, y: 0.0, ..g }), &[LAPTOP_150]);
        assert_eq!(p.size, (911.0, 480.0));
        // maximized stays maximized; nonsense sizes are ignored
        assert!(place(Some(Geometry { maximized: true, ..g }), &[FULL_HD]).maximized);
        assert_eq!(place(Some(Geometry { w: 0.0, ..g }), &[FULL_HD]).pos, None);
        assert_eq!(place(Some(Geometry { w: f64::NAN, ..g }), &[FULL_HD]).pos, None);
    }
}
