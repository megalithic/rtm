//! Text uses the terminal's palette (ANSI 0–15), so it follows the Rex theme.
//! Surfaces need explicit backgrounds: a Rex floating layer shows the pane beneath through
//! any cell left at the default background. They are neutral tints of megaforest's #272b30.
use ratatui::style::{Color, Modifier, Style};

pub const PAGE: Color = Color::Rgb(0x27, 0x2b, 0x30);
/// The preview card.
pub const CARD: Color = Color::Rgb(0x2d, 0x32, 0x37);
/// The highlighted row.
pub const RAISED: Color = Color::Rgb(0x38, 0x3e, 0x44);

pub const RED: Color = Color::Indexed(1);
pub const GREEN: Color = Color::Indexed(2);
pub const YELLOW: Color = Color::Indexed(3);
pub const BLUE: Color = Color::Indexed(4);
pub const PURPLE: Color = Color::Indexed(5);
pub const AQUA: Color = Color::Indexed(6);
pub const FAINT: Color = Color::Indexed(8);

pub fn text() -> Style {
    Style::new().fg(Color::Reset)
}

pub fn muted() -> Style {
    Style::new().fg(Color::Reset).add_modifier(Modifier::DIM)
}

pub fn faint() -> Style {
    Style::new().fg(FAINT)
}

pub fn blend(from: Color, to: Color, t: f32) -> Color {
    match (from, to) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            let lerp =
                |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
            Color::Rgb(lerp(r1, r2), lerp(g1, g2), lerp(b1, b2))
        }
        _ if t < 0.5 => from,
        _ => to,
    }
}
