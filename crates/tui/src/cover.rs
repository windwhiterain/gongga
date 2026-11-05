//! The window's startup cover. See `docs/notes/tui-cover.md`.

use crossterm::style::available_color_count;
use crossterm::terminal::window_size;
use image::{RgbImage, imageops};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::Paragraph;

/// A cell is two pixels high when its terminal does not report its pixel size.
const DEFAULT_CELL_ASPECT: f64 = 0.5;
const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
const MONOCHROME: &[u8] = b" .:-=+*#%@";
const ANSI16: [(Color, [u8; 3]); 16] = [
    (Color::Black, [0, 0, 0]),
    (Color::Red, [128, 0, 0]),
    (Color::Green, [0, 128, 0]),
    (Color::Yellow, [128, 128, 0]),
    (Color::Blue, [0, 0, 128]),
    (Color::Magenta, [128, 0, 128]),
    (Color::Cyan, [0, 128, 128]),
    (Color::Gray, [192, 192, 192]),
    (Color::DarkGray, [128, 128, 128]),
    (Color::LightRed, [255, 0, 0]),
    (Color::LightGreen, [0, 255, 0]),
    (Color::LightYellow, [255, 255, 0]),
    (Color::LightBlue, [0, 0, 255]),
    (Color::LightMagenta, [255, 0, 255]),
    (Color::LightCyan, [0, 255, 255]),
    (Color::White, [255, 255, 255]),
];

/// The source and the one palette this terminal can express.
pub struct Cover {
    source: RgbImage,
    colors: Colors,
    cell_aspect: f64,
}

#[derive(Clone, Copy)]
enum Colors {
    True,
    Indexed,
    Basic,
    Monochrome,
}

impl Cover {
    /// Decodes the asset carried by the executable; no file is read at startup.
    pub fn new() -> Result<Self, image::ImageError> {
        let source = image::load_from_memory(include_bytes!("../../../cover.png"))?.to_rgb8();
        let colors = if std::env::var_os("NO_COLOR").is_some() {
            Colors::Monochrome
        } else {
            match available_color_count() {
                u16::MAX => Colors::True,
                count if count >= 256 => Colors::Indexed,
                _ => Colors::Basic,
            }
        };
        Ok(Self {
            source,
            colors,
            cell_aspect: cell_aspect(),
        })
    }

    /// Paints a centered crop over the whole frame, with the dismissal hint on its last row.
    pub fn draw(&self, frame: &mut Frame) {
        let area = frame.area();
        if area.width == 0 || area.height == 0 || self.source.width() == 0 {
            return;
        }
        let Some(crop) = self.crop(area) else {
            return;
        };
        let resized = imageops::resize(
            &*crop,
            u32::from(area.width),
            u32::from(area.height) * 2,
            imageops::FilterType::Triangle,
        );
        {
            let buffer = frame.buffer_mut();
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    let Some(pixel_x) = x.checked_sub(area.x) else {
                        continue;
                    };
                    let Some(pixel_y) = y.checked_sub(area.y) else {
                        continue;
                    };
                    let pixel_y = u32::from(pixel_y) * 2;
                    let top = resized.get_pixel(u32::from(pixel_x), pixel_y).0;
                    let bottom = resized.get_pixel(u32::from(pixel_x), pixel_y + 1).0;
                    let Some(cell) = buffer.cell_mut((x, y)) else {
                        continue;
                    };
                    match self.colors {
                        Colors::Monochrome => {
                            let luminance =
                                (u16::from(luminance(top)) + u16::from(luminance(bottom))) / 2;
                            let index =
                                (usize::from(luminance) * (MONOCHROME.len() - 1) + 127) / 255;
                            let symbol = MONOCHROME.get(index).copied().unwrap_or(b' ');
                            cell.set_char(char::from(symbol))
                                .set_fg(Color::Reset)
                                .set_bg(Color::Reset);
                        }
                        _ => {
                            cell.set_char('▀')
                                .set_fg(self.color(top))
                                .set_bg(self.color(bottom));
                        }
                    }
                }
            }
        }
        let hint = Rect {
            y: area.bottom().saturating_sub(1),
            height: 1,
            ..area
        };
        frame.render_widget(
            Paragraph::new("press any key")
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::White).bg(Color::Black)),
            hint,
        );
    }

    /// Crops in physical units, so unequal cell widths and heights do not distort the picture.
    fn crop(&self, area: Rect) -> Option<image::SubImage<&RgbImage>> {
        let source_width = self.source.width();
        let source_height = self.source.height();
        if source_height == 0 || area.height == 0 {
            return None;
        }
        let aspect = f64::from(area.width) * self.cell_aspect / f64::from(area.height);
        if !aspect.is_finite() || aspect <= 0.0 {
            return None;
        }
        let source_aspect = f64::from(source_width) / f64::from(source_height);
        let (width, height) = if source_aspect > aspect {
            (
                (f64::from(source_height) * aspect)
                    .round()
                    .clamp(1.0, f64::from(source_width)) as u32,
                source_height,
            )
        } else {
            (
                source_width,
                (f64::from(source_width) / aspect)
                    .round()
                    .clamp(1.0, f64::from(source_height)) as u32,
            )
        };
        let cropped = imageops::crop_imm(
            &self.source,
            (source_width - width) / 2,
            (source_height - height) / 2,
            width,
            height,
        );
        Some(cropped)
    }

    fn color(&self, rgb: [u8; 3]) -> Color {
        match self.colors {
            Colors::True => Color::Rgb(rgb[0], rgb[1], rgb[2]),
            Colors::Indexed => ansi256(rgb),
            Colors::Basic | Colors::Monochrome => ansi16(rgb),
        }
    }
}

/// Width divided by height for one terminal cell, when the terminal reports pixels.
fn cell_aspect() -> f64 {
    let Ok(size) = window_size() else {
        return DEFAULT_CELL_ASPECT;
    };
    if size.width == 0 || size.height == 0 || size.columns == 0 || size.rows == 0 {
        return DEFAULT_CELL_ASPECT;
    }
    let cell_width = f64::from(size.width) / f64::from(size.columns);
    let cell_height = f64::from(size.height) / f64::from(size.rows);
    let aspect = cell_width / cell_height;
    if aspect.is_finite() && aspect > 0.0 {
        aspect
    } else {
        DEFAULT_CELL_ASPECT
    }
}

fn luminance(rgb: [u8; 3]) -> u8 {
    ((u32::from(rgb[0]) * 2126 + u32::from(rgb[1]) * 7152 + u32::from(rgb[2]) * 722) / 10000) as u8
}

/// The nearer of the xterm color cube and its grayscale ramp.
fn ansi256(rgb: [u8; 3]) -> Color {
    let [red, green, blue] = rgb.map(nearest_cube_level);
    let cube = [red.1, green.1, blue.1];
    let cube_index = 16 + 36 * red.0 + 6 * green.0 + blue.0;
    let average = (u16::from(rgb[0]) + u16::from(rgb[1]) + u16::from(rgb[2])) / 3;
    let gray_index = if average <= 8 {
        0
    } else {
        ((average - 3) / 10).min(23) as u8
    };
    let gray = 8 + gray_index * 10;
    if distance(rgb, [gray; 3]) < distance(rgb, cube) {
        Color::Indexed(232 + gray_index)
    } else {
        Color::Indexed(cube_index)
    }
}

fn nearest_cube_level(value: u8) -> (u8, u8) {
    CUBE_LEVELS
        .iter()
        .copied()
        .enumerate()
        .map(|(index, level)| (index as u8, level, value.abs_diff(level)))
        .min_by_key(|(_, _, distance)| *distance)
        .map(|(index, level, _)| (index, level))
        .unwrap_or((0, 0))
}

fn ansi16(rgb: [u8; 3]) -> Color {
    ANSI16
        .iter()
        .min_by_key(|(_, candidate)| distance(rgb, *candidate))
        .map(|(color, _)| *color)
        .unwrap_or(Color::Gray)
}

fn distance(left: [u8; 3], right: [u8; 3]) -> u32 {
    left.into_iter()
        .zip(right)
        .map(|(a, b)| {
            let difference = i32::from(a) - i32::from(b);
            (difference * difference) as u32
        })
        .sum()
}
