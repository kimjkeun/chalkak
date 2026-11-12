//! 캡쳐 위에 그리는 주석(사각형, 밑줄, 글자)과 그것을 그리는 코드.
//!
//! 주석의 좌표와 크기는 모두 원본 이미지의 픽셀 기준이다. 화면 미리보기에는 배율과
//! 위치를 주어 그리고, 내보낼 때는 배율 1로 원본 이미지에 직접 그린다.

use std::path::PathBuf;

use ab_glyph::{Font as _, FontVec, OutlinedGlyph, PxScale, ScaleFont, point};
use anyhow::{Context, Result, bail};
use image::RgbaImage;

pub type Color = [u8; 3];

/// 그림을 그릴 수 있는 대상 (화면 버퍼나 이미지).
pub trait Target {
    /// `(x, y)` 픽셀에 `color`를 `alpha`(0~255)만큼 섞는다. 범위를 벗어나면 무시한다.
    fn blend(&mut self, x: i32, y: i32, color: Color, alpha: u8);
}

impl Target for RgbaImage {
    fn blend(&mut self, x: i32, y: i32, color: Color, alpha: u8) {
        if x < 0 || y < 0 || x >= self.width() as i32 || y >= self.height() as i32 {
            return;
        }
        let pixel = self.get_pixel_mut(x as u32, y as u32);
        for (channel, new) in pixel.0.iter_mut().zip(color) {
            *channel = mix(*channel, new, alpha);
        }
        pixel.0[3] = 255;
    }
}

pub fn mix(old: u8, new: u8, alpha: u8) -> u8 {
    let a = alpha as u32;
    ((old as u32 * (255 - a) + new as u32 * a + 127) / 255) as u8
}

#[derive(Debug, Clone, PartialEq)]
pub enum Annotation {
    /// 테두리만 있는 사각형. `width`는 선 두께.
    Rect { x0: f32, y0: f32, x1: f32, y1: f32, width: f32, color: Color },
    /// `y` 높이에 긋는 가로선.
    Underline { x0: f32, x1: f32, y: f32, width: f32, color: Color },
    /// `(x, y)`가 글자 줄의 왼쪽 위. `size`는 글자 높이.
    Text { x: f32, y: f32, size: f32, text: String, color: Color },
    /// `(x0, y0)`에서 시작해 `(x1, y1)`을 가리키는 화살표. `width`는 선 두께.
    Arrow { x0: f32, y0: f32, x1: f32, y1: f32, width: f32, color: Color },
    /// `(x, y)`가 중심인 원 안에 번호를 쓴 표시.
    Marker { x: f32, y: f32, radius: f32, number: u32, color: Color },
}

/// 주석 하나를 그린다. 이미지 좌표에 `scale`을 곱하고 `(ox, oy)`를 더한 곳에 그려진다.
pub fn draw<T: Target>(target: &mut T, annotation: &Annotation, scale: f32, ox: f32, oy: f32, font: &Font) {
    let tx = |x: f32| ox + x * scale;
    let ty = |y: f32| oy + y * scale;
    let thickness = |w: f32| (w * scale).round().max(1.0) as i32;
    match annotation {
        Annotation::Rect { x0, y0, x1, y1, width, color } => {
            let t = thickness(*width);
            let (left, right) = (tx(x0.min(*x1)).round() as i32, tx(x0.max(*x1)).round() as i32);
            let (top, bottom) = (ty(y0.min(*y1)).round() as i32, ty(y0.max(*y1)).round() as i32);
            // 선은 사각형 안쪽으로 두껍게 한다.
            let t = t.min((right - left + 1) / 2).min((bottom - top + 1) / 2).max(1);
            fill(target, left, top, right, top + t, *color);
            fill(target, left, bottom - t, right, bottom, *color);
            fill(target, left, top, left + t, bottom, *color);
            fill(target, right - t, top, right, bottom, *color);
        }
        Annotation::Underline { x0, x1, y, width, color } => {
            let t = thickness(*width);
            let top = ty(*y).round() as i32 - t / 2;
            fill(
                target,
                tx(x0.min(*x1)).round() as i32,
                top,
                tx(x0.max(*x1)).round() as i32,
                top + t,
                *color,
            );
        }
        Annotation::Text { x, y, size, text, color } => {
            font.draw(target, text, tx(*x), ty(*y), size * scale, *color);
        }
        Annotation::Arrow { x0, y0, x1, y1, width, color } => {
            let (ax, ay, bx, by) = (tx(*x0), ty(*y0), tx(*x1), ty(*y1));
            let w = (width * scale).max(1.0);
            let length = (bx - ax).hypot(by - ay);
            if length < 0.5 {
                return;
            }
            let (ux, uy) = ((bx - ax) / length, (by - ay) / length);
            // 화살촉은 선 굵기에 비례하되, 짧은 화살표에서는 전체 길이를 넘지 않는다.
            let head_length = (w * 4.5).min(length);
            let head_half = head_length * 0.5;
            let (hx, hy) = (bx - ux * head_length, by - uy * head_length);
            stroke_line(target, (ax, ay), (hx, hy), w, *color);
            fill_triangle(
                target,
                [
                    (bx, by),
                    (hx - uy * head_half, hy + ux * head_half),
                    (hx + uy * head_half, hy - ux * head_half),
                ],
                *color,
            );
        }
        Annotation::Marker { x, y, radius, number, color } => {
            let (cx, cy, r) = (tx(*x), ty(*y), (radius * scale).max(2.0));
            fill_circle(target, cx, cy, r, *color);
            // 밝은 색 위에는 검은 글자, 어두운 색 위에는 흰 글자.
            let luminance = 0.299 * color[0] as f32 + 0.587 * color[1] as f32 + 0.114 * color[2] as f32;
            let ink = if luminance > 150.0 { [0, 0, 0] } else { [255, 255, 255] };
            let label = number.to_string();
            let size = if label.len() > 1 { r * 1.15 } else { r * 1.5 };
            font.draw_centered(target, &label, cx, cy, size, ink);
        }
    }
}

/// 가장자리가 부드러운 굵은 선.
fn stroke_line<T: Target>(target: &mut T, a: (f32, f32), b: (f32, f32), width: f32, color: Color) {
    let half = width / 2.0;
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length_sq = dx * dx + dy * dy;
    let reach = half + 1.0;
    for y in (a.1.min(b.1) - reach).floor() as i32..=(a.1.max(b.1) + reach).ceil() as i32 {
        for x in (a.0.min(b.0) - reach).floor() as i32..=(a.0.max(b.0) + reach).ceil() as i32 {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let t = if length_sq > 0.0 {
                (((px - a.0) * dx + (py - a.1) * dy) / length_sq).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let distance = (px - a.0 - t * dx).hypot(py - a.1 - t * dy);
            cover(target, x, y, half + 0.5 - distance, color);
        }
    }
}

fn fill_triangle<T: Target>(target: &mut T, points: [(f32, f32); 3], color: Color) {
    let [a, b, c] = points;
    let area = (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
    if area.abs() < 0.01 {
        return;
    }
    let sign = area.signum();
    let edges = [(a, b), (b, c), (c, a)];
    let xs = [a.0, b.0, c.0];
    let ys = [a.1, b.1, c.1];
    let min = |v: [f32; 3]| v.into_iter().fold(f32::MAX, f32::min).floor() as i32 - 1;
    let max = |v: [f32; 3]| v.into_iter().fold(f32::MIN, f32::max).ceil() as i32 + 1;
    for y in min(ys)..=max(ys) {
        for x in min(xs)..=max(xs) {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            // 세 변까지의 (안쪽이 양수인) 거리 중 가장 작은 값.
            let inside = edges
                .iter()
                .map(|(p, q)| {
                    let (ex, ey) = (q.0 - p.0, q.1 - p.1);
                    sign * (ex * (py - p.1) - ey * (px - p.0)) / ex.hypot(ey).max(0.001)
                })
                .fold(f32::MAX, f32::min);
            cover(target, x, y, inside + 0.5, color);
        }
    }
}

fn fill_circle<T: Target>(target: &mut T, cx: f32, cy: f32, radius: f32, color: Color) {
    let reach = radius + 1.0;
    for y in (cy - reach).floor() as i32..=(cy + reach).ceil() as i32 {
        for x in (cx - reach).floor() as i32..=(cx + reach).ceil() as i32 {
            let distance = (x as f32 + 0.5 - cx).hypot(y as f32 + 0.5 - cy);
            cover(target, x, y, radius + 0.5 - distance, color);
        }
    }
}

/// `coverage`(0~1, 범위 밖은 잘림)만큼 픽셀을 덮는다.
fn cover<T: Target>(target: &mut T, x: i32, y: i32, coverage: f32, color: Color) {
    let alpha = (coverage.clamp(0.0, 1.0) * 255.0) as u8;
    if alpha > 0 {
        target.blend(x, y, color, alpha);
    }
}

/// `[x0, x1) x [y0, y1)`를 색으로 채운다.
pub fn fill<T: Target>(target: &mut T, x0: i32, y0: i32, x1: i32, y1: i32, color: Color) {
    for y in y0..y1 {
        for x in x0..x1 {
            target.blend(x, y, color, 255);
        }
    }
}

/// 주석을 모두 그린 새 이미지를 만든다.
pub fn apply(image: &RgbaImage, annotations: &[Annotation], font: &Font) -> RgbaImage {
    let mut out = image.clone();
    for annotation in annotations {
        draw(&mut out, annotation, 1.0, 0.0, 0.0, font);
    }
    out
}

/// 글자를 그리는 글꼴. 한글이 들어 있는 Windows 기본 글꼴을 쓴다.
pub struct Font {
    font: FontVec,
}

impl Font {
    /// Windows 글꼴 폴더에서 맑은 고딕을 읽는다. 없으면 다른 기본 글꼴을 쓴다.
    pub fn load() -> Result<Self> {
        let dir = PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into()))
            .join("Fonts");
        for name in ["malgun.ttf", "malgunsl.ttf", "segoeui.ttf", "arial.ttf"] {
            if let Ok(data) = std::fs::read(dir.join(name))
                && let Ok(font) = FontVec::try_from_vec(data)
            {
                return Ok(Self { font });
            }
        }
        bail!("글자를 그릴 글꼴을 찾지 못했습니다: {}", dir.display());
    }

    /// 지정한 글꼴 파일을 읽는다.
    pub fn from_file(path: &std::path::Path) -> Result<Self> {
        let data = std::fs::read(path)
            .with_context(|| format!("글꼴을 읽지 못했습니다: {}", path.display()))?;
        let font = FontVec::try_from_vec(data).context("글꼴 파일 형식이 올바르지 않습니다")?;
        Ok(Self { font })
    }

    /// `size` 높이로 썼을 때 `text`의 너비.
    pub fn measure(&self, text: &str, size: f32) -> f32 {
        let scaled = self.font.as_scaled(PxScale::from(size));
        let mut width = 0.0;
        let mut previous = None;
        for ch in text.chars() {
            let id = scaled.glyph_id(ch);
            if let Some(prev) = previous {
                width += scaled.kern(prev, id);
            }
            width += scaled.h_advance(id);
            previous = Some(id);
        }
        width
    }

    /// `(x, y)`를 줄의 왼쪽 위로 해서 글자를 그린다. 그린 뒤의 x 위치를 돌려준다.
    pub fn draw<T: Target>(&self, target: &mut T, text: &str, x: f32, y: f32, size: f32, color: Color) -> f32 {
        let ascent = self.font.as_scaled(PxScale::from(size)).ascent();
        let (glyphs, end) = self.outlines(text, x, y + ascent, size);
        paint(target, &glyphs, color);
        end
    }

    /// 글자의 보이는 부분이 `(cx, cy)`에 가운데 오도록 그린다.
    pub fn draw_centered<T: Target>(&self, target: &mut T, text: &str, cx: f32, cy: f32, size: f32, color: Color) {
        let (glyphs, _) = self.outlines(text, 0.0, 0.0, size);
        let Some(first) = glyphs.first().map(|g| g.px_bounds()) else {
            return;
        };
        let (mut left, mut top, mut right, mut bottom) = (first.min.x, first.min.y, first.max.x, first.max.y);
        for bounds in glyphs.iter().map(|g| g.px_bounds()) {
            left = left.min(bounds.min.x);
            top = top.min(bounds.min.y);
            right = right.max(bounds.max.x);
            bottom = bottom.max(bounds.max.y);
        }
        let (glyphs, _) = self.outlines(text, cx - (left + right) / 2.0, cy - (top + bottom) / 2.0, size);
        paint(target, &glyphs, color);
    }

    /// `x`에서 시작해 `baseline` 높이에 놓인 글자들의 윤곽과, 다 쓴 뒤의 x 위치.
    fn outlines(&self, text: &str, x: f32, baseline: f32, size: f32) -> (Vec<OutlinedGlyph>, f32) {
        let scale = PxScale::from(size);
        let scaled = self.font.as_scaled(scale);
        let mut glyphs = Vec::new();
        let mut pen = x;
        let mut previous = None;
        for ch in text.chars() {
            let id = scaled.glyph_id(ch);
            if let Some(prev) = previous {
                pen += scaled.kern(prev, id);
            }
            let glyph = id.with_scale_and_position(scale, point(pen, baseline));
            glyphs.extend(self.font.outline_glyph(glyph));
            pen += scaled.h_advance(id);
            previous = Some(id);
        }
        (glyphs, pen)
    }
}

fn paint<T: Target>(target: &mut T, glyphs: &[OutlinedGlyph], color: Color) {
    for outlined in glyphs {
        let bounds = outlined.px_bounds();
        let (left, top) = (bounds.min.x as i32, bounds.min.y as i32);
        outlined.draw(|gx, gy, coverage| {
            let alpha = (coverage.clamp(0.0, 1.0) * 255.0) as u8;
            if alpha > 0 {
                target.blend(left + gx as i32, top + gy as i32, color, alpha);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Color = [255, 0, 0];
    const WHITE_PIXEL: image::Rgba<u8> = image::Rgba([255, 255, 255, 255]);

    fn white(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_pixel(w, h, WHITE_PIXEL)
    }

    fn is_red(image: &RgbaImage, x: u32, y: u32) -> bool {
        image.get_pixel(x, y).0 == [255, 0, 0, 255]
    }

    #[test]
    fn rect_draws_only_its_border() {
        let font = Font::load().unwrap();
        let rect = Annotation::Rect { x0: 10.0, y0: 10.0, x1: 50.0, y1: 40.0, width: 3.0, color: RED };
        let out = apply(&white(100, 60), &[rect], &font);
        assert!(is_red(&out, 10, 10) && is_red(&out, 12, 25) && is_red(&out, 49, 39));
        assert!(is_red(&out, 30, 11) && is_red(&out, 30, 38));
        assert_eq!(*out.get_pixel(30, 25), WHITE_PIXEL); // 안쪽은 그대로
        assert_eq!(*out.get_pixel(9, 9), WHITE_PIXEL); // 바깥도 그대로
        assert_eq!(*out.get_pixel(50, 40), WHITE_PIXEL);
    }

    #[test]
    fn rect_drawn_backwards_is_the_same() {
        let font = Font::load().unwrap();
        let a = Annotation::Rect { x0: 10.0, y0: 10.0, x1: 50.0, y1: 40.0, width: 3.0, color: RED };
        let b = Annotation::Rect { x0: 50.0, y0: 40.0, x1: 10.0, y1: 10.0, width: 3.0, color: RED };
        assert_eq!(apply(&white(100, 60), &[a], &font), apply(&white(100, 60), &[b], &font));
    }

    #[test]
    fn underline_is_a_horizontal_bar() {
        let font = Font::load().unwrap();
        let line = Annotation::Underline { x0: 20.0, x1: 80.0, y: 30.0, width: 4.0, color: RED };
        let out = apply(&white(100, 60), &[line], &font);
        assert!(is_red(&out, 20, 28) && is_red(&out, 79, 31));
        assert_eq!(*out.get_pixel(50, 27), WHITE_PIXEL);
        assert_eq!(*out.get_pixel(50, 32), WHITE_PIXEL);
        assert_eq!(*out.get_pixel(19, 30), WHITE_PIXEL);
    }

    #[test]
    fn drawing_outside_the_image_is_ignored() {
        let font = Font::load().unwrap();
        let rect = Annotation::Rect { x0: -50.0, y0: -50.0, x1: 500.0, y1: 500.0, width: 3.0, color: RED };
        let out = apply(&white(20, 20), &[rect], &font);
        assert_eq!(out, white(20, 20));
    }

    #[test]
    fn text_marks_pixels_inside_its_box_and_handles_korean() {
        let font = Font::load().unwrap();
        let width = font.measure("가나다 abc", 24.0);
        assert!(width > 60.0 && width < 200.0);
        let text = Annotation::Text { x: 10.0, y: 10.0, size: 24.0, text: "가나다 abc".into(), color: RED };
        let out = apply(&white(240, 60), &[text], &font);
        let changed: Vec<(u32, u32)> = out
            .enumerate_pixels()
            .filter(|(_, _, p)| **p != WHITE_PIXEL)
            .map(|(x, y, _)| (x, y))
            .collect();
        assert!(changed.len() > 100, "글자가 그려지지 않았습니다");
        assert!(changed.iter().all(|&(x, y)| x >= 9 && x as f32 <= 12.0 + width && (8..=40).contains(&y)));
    }

    #[test]
    fn arrow_has_a_shaft_and_a_wider_head_at_the_end() {
        let font = Font::load().unwrap();
        let arrow = Annotation::Arrow { x0: 20.0, y0: 50.0, x1: 160.0, y1: 50.0, width: 4.0, color: RED };
        let out = apply(&white(200, 100), &[arrow], &font);
        let red_rows = |x: u32| (0..100).filter(|&y| is_red(&out, x, y)).count();
        assert_eq!(red_rows(60), 4); // 몸통은 선 굵기만큼
        assert!(red_rows(145) > 10); // 화살촉은 더 넓다
        assert!(is_red(&out, 157, 50)); // 끝점 근처까지 칠해진다
        assert_eq!(red_rows(165), 0); // 끝점을 넘지 않는다
        assert_eq!(red_rows(10), 0);
    }

    #[test]
    fn marker_is_a_filled_circle_with_a_number() {
        let font = Font::load().unwrap();
        let marker = Annotation::Marker { x: 50.0, y: 50.0, radius: 20.0, number: 3, color: RED };
        let out = apply(&white(100, 100), &[marker], &font);
        assert!(is_red(&out, 33, 50) && is_red(&out, 50, 33) && is_red(&out, 67, 50));
        assert_eq!(*out.get_pixel(50, 28), WHITE_PIXEL); // 원 밖
        assert_eq!(*out.get_pixel(35, 35), WHITE_PIXEL); // 모서리 쪽도 원 밖
        // 원 안에 흰 숫자가 그려져 있다.
        let white_inside = (40..60)
            .flat_map(|y| (40..60).map(move |x| (x, y)))
            .filter(|&(x, y)| out.get_pixel(x, y).0[1] > 128)
            .count();
        assert!(white_inside > 15, "숫자가 보이지 않습니다");
    }

    #[test]
    fn scale_and_offset_move_the_drawing() {
        let font = Font::load().unwrap();
        let rect = Annotation::Rect { x0: 10.0, y0: 10.0, x1: 20.0, y1: 20.0, width: 2.0, color: RED };
        let mut out = white(100, 100);
        draw(&mut out, &rect, 2.0, 5.0, 7.0, &font);
        assert!(is_red(&out, 25, 27) && is_red(&out, 44, 46));
        assert_eq!(*out.get_pixel(24, 27), WHITE_PIXEL);
        assert_eq!(*out.get_pixel(35, 37), WHITE_PIXEL);
    }
}
