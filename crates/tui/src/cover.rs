//! The window's startup cover. See `docs/notes/tui-cover.md`.

use font8x8::{BASIC_FONTS, UnicodeFonts};
use image::{Rgba, RgbaImage};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Paragraph};
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::{FontSize, Resize, StatefulImage};

/// How much of the picture's short side is spent fading its edge into the black around it.
const FEATHER_FRACTION: f64 = 0.14;

/// How far the grain at the very edge can carry a pixel away from the fade it would have had.
const GRAIN: f64 = 0.30;

/// The name this window opens with, stamped into the picture itself.
const WORDMARK: &str = "Gongga Harness";

/// The share of the picture's height the wordmark's letters take.
const WORDMARK_HEIGHT: f64 = 0.07;

/// Where the wordmark's letters end, as a share of the picture's height.
const WORDMARK_BASELINE: f64 = 0.86;

/// One glyph's side, in the font's own pixels.
const GLYPH_SIDE: usize = 8;

/// The picture the window opens on, and the one terminal it opens on.
pub struct Cover {
    /// The picture, encoded once per size: this terminal is asked for pixels, not for cells.
    picture: StatefulProtocol,
    /// The pixels one cell covers here, reckoned the same way the protocol reckons its size.
    cell: FontSize,
    /// The picture's own width divided by its own height.
    aspect: f64,
    /// The area the last frame drew it in, so a frame after a size is known is owed.
    painted: Option<(u16, u16)>,
}

impl Cover {
    /// Decodes the asset the executable carries, then asks `picker` how to draw it.
    pub fn new(mut picker: Picker) -> Result<Self, image::ImageError> {
        let source = image::load_from_memory(include_bytes!("../../../cover.png"))?.to_rgba8();
        let picture = stamp(feather(source));
        let aspect = f64::from(picture.width()) / f64::from(picture.height());
        let cell = picker.font_size();
        // The protocol pads the picture out to whole cells; the pad is painted with this.
        picker.set_background_color(Some(image::Rgba([0, 0, 0, 255])));
        Ok(Self {
            picture: picker.new_resize_protocol(picture.into()),
            cell,
            aspect,
            painted: None,
        })
    }

    /// What area this cover has already been drawn in, if any.
    ///
    /// # Invariant
    ///
    /// The window's as much as the cover's: a frame drawn before the terminal reports
    /// its size paints nothing, so the area it drew in is how the loop knows whether
    /// the cover has been seen yet.
    pub fn painted(&self) -> Option<(u16, u16)> {
        self.painted
    }

    /// Paints black, then the picture, then the hint on the last row.
    pub fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        if area.width == 0 || area.height == 0 {
            return;
        }
        self.painted = Some((area.width, area.height));
        // Uncovered cells are black, not blank: a blank cell is the terminal's background.
        frame.render_widget(Block::new().style(Style::default().bg(Color::Black)), area);
        // The hint's row carries no pixels, so the picture is fitted above it.
        let rows = area.height.saturating_sub(1);
        if let Some(rect) = self.rect(area, rows) {
            frame.render_stateful_widget(
                StatefulImage::new().resize(Resize::Scale(None)),
                rect,
                &mut self.picture,
            );
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

    /// Where the whole picture fits, in cells, centred in the `rows` it may use.
    ///
    /// # Invariant
    ///
    /// The whole picture: the cover shows the one image the window was given, so nothing
    /// is cropped. What is left over stays black, and the picture's own edge fades into it.
    fn rect(&self, area: Rect, rows: u16) -> Option<Rect> {
        if area.width == 0 || rows == 0 {
            return None;
        }
        let cell_width = f64::from(self.cell.width);
        let cell_height = f64::from(self.cell.height);
        if !(self.aspect.is_finite() && self.aspect > 0.0)
            || !(cell_width > 0.0 && cell_height > 0.0)
        {
            return None;
        }
        let available_width = f64::from(area.width) * cell_width;
        let available_height = f64::from(rows) * cell_height;
        // The picture's shape against the box's: whichever of the two runs out first decides.
        let (width, height) = if available_width / available_height > self.aspect {
            (self.aspect * available_height, available_height)
        } else {
            (available_width, available_width / self.aspect)
        };
        let width = cells(width / cell_width, area.width);
        let height = cells(height / cell_height, rows);
        Some(Rect {
            x: area.x + (area.width - width) / 2,
            y: area.y + (rows - height) / 2,
            width,
            height,
        })
    }
}

/// Stamps the wordmark into the picture: white letters, black edge, so the photograph
/// behind them stays behind them.
///
/// # Invariant
///
/// The name belongs to the picture, not to the cells under it: a terminal cell can only
/// paint a background, so letters drawn as cells would leave black boxes on the photograph.
/// Every letter is one font pixel per `scale` picture pixels, so it stays square and crisp.
fn stamp(mut picture: RgbaImage) -> RgbaImage {
    let Some(scale) = letter_scale(picture.height()) else {
        return picture;
    };
    let letters = letters(WORDMARK, scale);
    let (width, height) = (letters.first().map_or(0, Vec::len), letters.len());
    if width == 0 || height == 0 || width > picture.width() as usize {
        return picture;
    }
    let edge = (scale / 2).max(1);
    let left = (picture.width() as usize - width) / 2;
    let bottom = (f64::from(picture.height()) * WORDMARK_BASELINE) as usize;
    let top = bottom.saturating_sub(height);
    let black = Rgba([0, 0, 0, 255]);
    let white = Rgba([255, 255, 255, 255]);
    // The edge first, then the letters on top of it: what is left of the edge is the outline.
    ink(&mut picture, &letters, left, top, edge, black);
    ink(&mut picture, &letters, left, top, 0, white);
    picture
}

/// How many picture pixels one font pixel is worth here.
fn letter_scale(height: u32) -> Option<usize> {
    let wanted = f64::from(height) * WORDMARK_HEIGHT / GLYPH_SIDE as f64;
    let scale = wanted.round();
    (scale >= 1.0).then_some(scale as usize)
}

/// The wordmark in ink: one row of booleans per picture pixel row, one column per pixel.
fn letters(text: &str, scale: usize) -> Vec<Vec<bool>> {
    let advance = GLYPH_SIDE * scale;
    let mut rows = vec![vec![false; text.chars().count() * advance]; advance];
    for (index, letter) in text.chars().enumerate() {
        let Some(glyph) = BASIC_FONTS.get(letter) else {
            continue;
        };
        for (line, bits) in glyph.iter().enumerate() {
            for column in 0..GLYPH_SIDE {
                if bits & (1 << column) == 0 {
                    continue;
                }
                for down in 0..scale {
                    for across in 0..scale {
                        let y = line * scale + down;
                        let x = index * advance + column * scale + across;
                        if let Some(cell) = rows.get_mut(y).and_then(|row| row.get_mut(x)) {
                            *cell = true;
                        }
                    }
                }
            }
        }
    }
    rows
}

/// Paints every ink of `mask` at `(left, top)`, grown by `edge` picture pixels.
fn ink(
    picture: &mut RgbaImage,
    mask: &[Vec<bool>],
    left: usize,
    top: usize,
    edge: usize,
    colour: Rgba<u8>,
) {
    let width = picture.width() as usize;
    let height = picture.height() as usize;
    for (row, line) in mask.iter().enumerate() {
        for (column, painted) in line.iter().enumerate() {
            if !painted {
                continue;
            }
            let x = left + column;
            let y = top + row;
            if x >= width || y >= height {
                continue;
            }
            let from_x = x.saturating_sub(edge);
            let to_x = (x + edge).min(width - 1);
            let from_y = y.saturating_sub(edge);
            let to_y = (y + edge).min(height - 1);
            for py in from_y..=to_y {
                for px in from_x..=to_x {
                    picture.put_pixel(px as u32, py as u32, colour);
                }
            }
        }
    }
}

/// Rounds a count of cells into the room there is for it, never below one.
fn cells(count: f64, room: u16) -> u16 {
    let room = u32::from(room);
    let rounded = count.round();
    if rounded.is_finite() && rounded >= 1.0 {
        (rounded as u32).min(room) as u16
    } else {
        1.min(room) as u16
    }
}

/// Darkens the picture towards black where it meets its own border.
///
/// # Invariant
///
/// The picture has no frame, so its rectangle would meet the black around it along a hard
/// line. Each pixel is multiplied by a fade reaching full brightness one
/// [`FEATHER_FRACTION`] of the short side in, leaving the edge itself black.
///
/// The same pass composites onto black and makes the pixel opaque: a protocol padding a
/// transparent pixel would otherwise show what lies behind it.
fn feather(mut picture: RgbaImage) -> RgbaImage {
    let width = f64::from(picture.width());
    let height = f64::from(picture.height());
    if !(width > 0.0 && height > 0.0) {
        return picture;
    }
    let reach = FEATHER_FRACTION * width.min(height);
    if !(reach > 0.0) {
        return picture;
    }
    for (x, y, pixel) in picture.enumerate_pixels_mut() {
        let (fx, fy) = (f64::from(x), f64::from(y));
        let border = fx.min(fy).min(width - 1.0 - fx).min(height - 1.0 - fy);
        let curve = smootherstep((border / reach).clamp(0.0, 1.0));
        // Grain, strongest at the edge: the fade breaks into speckle, not a smooth band.
        let grain = (noise(x, y) - 0.5) * GRAIN * (1.0 - curve);
        let fade = (curve + grain).clamp(0.0, 1.0);
        let alpha = f64::from(pixel[3]) / 255.0;
        for channel in 0..3 {
            pixel[channel] = (f64::from(pixel[channel]) * alpha * fade).round() as u8;
        }
        pixel[3] = 255;
    }
    picture
}

/// A reproducible value in `[0, 1)` for one pixel, so the grain is the same on every run.
fn noise(x: u32, y: u32) -> f64 {
    let mut hash = x.wrapping_mul(0x9E37_79B9) ^ y.wrapping_mul(0x85EB_CA6B);
    hash ^= hash >> 15;
    hash = hash.wrapping_mul(0x2545_F491);
    hash ^= hash >> 13;
    f64::from(hash & 0xFFFF) / 65536.0
}

/// The flat middle and the round ends: a fade whose ends leave no visible seam.
fn smootherstep(position: f64) -> f64 {
    position * position * position * (position * (position * 6.0 - 15.0) + 10.0)
}
