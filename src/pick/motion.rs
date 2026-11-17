//! Fades blend each cell's text towards its own background. tachyonfx's colour lerp maps
//! ANSI indices to xterm defaults (and the default foreground to black), so mid-fade
//! colours here go through megaforest's RGB values instead. At rest every cell keeps its
//! palette index, so the theme still comes from Rex.
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier},
};
use std::time::{Duration, Instant};
use tachyonfx::{Effect, Interpolation, fx};

const FG: (u8, u8, u8) = (0xd8, 0xca, 0xac);
const PALETTE: [(u8, u8, u8); 16] = [
    (0x50, 0x5a, 0x60),
    (0xe6, 0x7e, 0x80),
    (0xa7, 0xc0, 0x80),
    (0xd9, 0xbb, 0x80),
    (0x7f, 0xbb, 0xb3),
    (0xd3, 0x9b, 0xb6),
    (0x7e, 0xb4, 0x8b),
    (0xff, 0xff, 0xff),
    (0x5c, 0x63, 0x70),
    (0xe0, 0x6c, 0x75),
    (0xa7, 0xcf, 0x9b),
    (0xe5, 0xc0, 0x7b),
    (0x7f, 0xbb, 0xd0),
    (0xfc, 0xc5, 0xe9),
    (0x78, 0xba, 0x91),
    (0xff, 0xff, 0xff),
];

fn rgb(color: Color, fallback: (u8, u8, u8)) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Indexed(i) if i < 16 => PALETTE[usize::from(i)],
        Color::Black => PALETTE[0],
        Color::Red => PALETTE[1],
        Color::Green => PALETTE[2],
        Color::Yellow => PALETTE[3],
        Color::Blue => PALETTE[4],
        Color::Magenta => PALETTE[5],
        Color::Cyan => PALETTE[6],
        Color::Gray => PALETTE[7],
        Color::DarkGray => PALETTE[8],
        _ => fallback,
    }
}

fn lerp(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> Color {
    let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    Color::Rgb(mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t)
}

/// Text in `area` at `alpha` of the way from invisible (its background) to itself.
pub fn fade_cells(buf: &mut Buffer, area: Rect, alpha: f32) {
    if alpha >= 1.0 {
        return;
    }
    let area = area.intersection(buf.area);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buf[(x, y)];
            if cell.symbol() == " " {
                continue;
            }
            let bg = rgb(cell.bg, (0x27, 0x2b, 0x30));
            let mut fg = rgb(cell.fg, FG);
            if cell.modifier.contains(Modifier::DIM) {
                let Color::Rgb(r, g, b) = lerp(fg, bg, 0.45) else {
                    unreachable!()
                };
                fg = (r, g, b);
                cell.modifier.remove(Modifier::DIM);
            }
            cell.fg = lerp(bg, fg, alpha.max(0.0));
        }
    }
}

/// The window opening: rows fade in top to bottom, each a few ms after the one above.
pub fn open(ms: u32) -> Effect {
    fx::effect_fn_buf((), (ms, Interpolation::Linear), |_, context, buf| {
        let t = context.timer.alpha();
        let area = context.area;
        let rows = f32::from(area.height.max(1));
        for y in area.top()..area.bottom() {
            // The last row starts after 40% of the time; each row eases over the rest.
            let delay = 0.4 * f32::from(y - area.top()) / rows;
            let alpha = ease_out((t - delay) / 0.6);
            fade_cells(buf, Rect::new(area.x, y, area.width, 1), alpha);
        }
    })
}

/// New preview content fading in over its card.
pub fn reveal(area: Rect, ms: u32) -> Effect {
    fx::effect_fn_buf((), (ms, Interpolation::Linear), |_, context, buf| {
        fade_cells(buf, context.area, ease_out(context.timer.alpha()));
    })
    .with_area(area)
}

/// Progress of a hand-rolled transition (selection, entering rows).
pub fn progress(start: Instant, length: Duration, now: Instant) -> f32 {
    ease_out(now.saturating_duration_since(start).as_secs_f32() / length.as_secs_f32())
}
