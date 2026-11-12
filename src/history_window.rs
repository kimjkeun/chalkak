//! 히스토리 창: 최근 캡쳐를 썸네일 격자로 보여 주고 하나를 고르게 한다.
//!
//! 클릭하거나 숫자 키(1~9)를 누르면 선택, Shift를 함께 누르면 "경로로 복사" 선택이다.
//! Esc를 누르거나 다른 창을 클릭하면 닫힌다.

use std::num::NonZeroU32;
use std::rc::Rc;

use anyhow::{Context, Result, anyhow, bail};
use image::RgbaImage;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::platform::windows::WindowAttributesExtWindows;
use winit::window::{Window, WindowButtons, WindowId, WindowLevel};

use crate::history::fit;

const TITLE: &str =
    "캡쳐 히스토리   |   클릭·숫자 키: 이미지 복사   |   Shift+클릭·Shift+숫자: 파일 경로 복사   |   Esc: 닫기";

const COLUMNS: usize = 4;
const BACKGROUND: u32 = 0x001E_1E1E;
const CELL_COLOR: u32 = 0x002D_2D2D;
const ACCENT: u32 = 0x0025_63EB;
const WHITE: u32 = 0x00FF_FFFF;

/// 숫자 1~9의 5x7 점 글꼴. 각 줄의 아래 5비트가 왼쪽부터의 점이다.
const DIGITS: [[u8; 7]; 9] = [
    [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
    [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
    [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110],
    [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
    [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
    [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
    [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
    [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
    [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
];

pub enum HistoryOutcome {
    Closed,
    /// `index`번째(0부터) 항목을 골랐다. `as_path`면 이미지 대신 파일 경로를 복사한다.
    Picked { index: usize, as_path: bool },
}

/// 격자 배치 계산. 모든 값은 물리 픽셀이다.
#[derive(Debug, Clone, Copy)]
struct Layout {
    count: usize,
    cell_w: u32,
    cell_h: u32,
    pad: u32,
    inset: u32,
    badge: u32,
    border: u32,
}

impl Layout {
    fn new(count: usize, scale: f64) -> Self {
        let px = |v: f64| ((v * scale).round() as u32).max(1);
        Self {
            count,
            cell_w: px(240.0),
            cell_h: px(160.0),
            pad: px(12.0),
            inset: px(6.0),
            badge: px(24.0),
            border: px(3.0),
        }
    }

    fn columns(&self) -> u32 {
        self.count.clamp(1, COLUMNS) as u32
    }

    fn rows(&self) -> u32 {
        self.count.div_ceil(COLUMNS).max(1) as u32
    }

    fn width(&self) -> u32 {
        self.pad + self.columns() * (self.cell_w + self.pad)
    }

    fn height(&self) -> u32 {
        self.pad + self.rows() * (self.cell_h + self.pad)
    }

    fn cell_origin(&self, index: usize) -> (u32, u32) {
        let (col, row) = ((index % COLUMNS) as u32, (index / COLUMNS) as u32);
        (
            self.pad + col * (self.cell_w + self.pad),
            self.pad + row * (self.cell_h + self.pad),
        )
    }

    fn cell_at(&self, x: i32, y: i32) -> Option<usize> {
        (0..self.count).find(|&i| {
            let (cx, cy) = self.cell_origin(i);
            let (cx, cy) = (cx as i32, cy as i32);
            x >= cx && x < cx + self.cell_w as i32 && y >= cy && y < cy + self.cell_h as i32
        })
    }
}

/// 창 크기에 맞게 줄여 둔 썸네일 (0RGB 픽셀).
struct Thumb {
    width: u32,
    height: u32,
    pixels: Vec<u32>,
}

pub struct HistoryWindow {
    window: Rc<Window>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
    layout: Layout,
    thumbs: Vec<Thumb>,
    hover: Option<usize>,
    shift: bool,
}

impl HistoryWindow {
    /// `thumbs`를 최신 순 격자로 보여 주는 창을, `area`(모니터의 위치와 크기) 가운데에 띄운다.
    pub fn open(
        event_loop: &ActiveEventLoop,
        thumbs: &[&RgbaImage],
        area: (PhysicalPosition<i32>, PhysicalSize<u32>),
        scale: f64,
    ) -> Result<Self> {
        if thumbs.is_empty() {
            bail!("보여 줄 캡쳐가 없습니다");
        }
        let layout = Layout::new(thumbs.len(), scale);
        let (width, height) = (layout.width(), layout.height());
        let (origin, monitor) = area;
        let position = PhysicalPosition::new(
            origin.x + (monitor.width as i32 - width as i32) / 2,
            origin.y + (monitor.height as i32 - height as i32) / 2,
        );
        let attrs = Window::default_attributes()
            .with_title(TITLE)
            .with_resizable(false)
            .with_enabled_buttons(WindowButtons::CLOSE)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_skip_taskbar(true)
            .with_visible(false)
            .with_position(position)
            .with_inner_size(PhysicalSize::new(width, height));
        let window = Rc::new(
            event_loop
                .create_window(attrs)
                .context("히스토리 창을 만들지 못했습니다")?,
        );
        let context = softbuffer::Context::new(window.clone())
            .map_err(|e| anyhow!("그리기 컨텍스트 생성 실패: {e}"))?;
        let surface = softbuffer::Surface::new(&context, window.clone())
            .map_err(|e| anyhow!("그리기 표면 생성 실패: {e}"))?;

        let max = (
            layout.cell_w - 2 * layout.inset,
            layout.cell_h - 2 * layout.inset,
        );
        let thumbs = thumbs
            .iter()
            .map(|image| {
                let (w, h) = fit(image.dimensions(), max);
                let small = image::imageops::thumbnail(*image, w, h);
                Thumb {
                    width: w,
                    height: h,
                    pixels: small
                        .as_raw()
                        .chunks_exact(4)
                        .map(|p| (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32)
                        .collect(),
                }
            })
            .collect();

        let mut this = Self {
            window,
            surface,
            layout,
            thumbs,
            hover: None,
            shift: false,
        };
        this.draw()?;
        this.window.set_visible(true);
        this.window.focus_window();
        Ok(this)
    }

    pub fn window_id(&self) -> WindowId {
        self.window.id()
    }

    pub fn handle(&mut self, event: WindowEvent) -> Result<Option<HistoryOutcome>> {
        match event {
            WindowEvent::CloseRequested | WindowEvent::Focused(false) => {
                return Ok(Some(HistoryOutcome::Closed));
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.shift = modifiers.state().shift_key();
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    if code == KeyCode::Escape {
                        return Ok(Some(HistoryOutcome::Closed));
                    }
                    if let Some(index) = digit_index(code).filter(|&i| i < self.layout.count) {
                        return Ok(Some(HistoryOutcome::Picked { index, as_path: self.shift }));
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.set_hover(self.layout.cell_at(position.x as i32, position.y as i32));
            }
            WindowEvent::CursorLeft { .. } => self.set_hover(None),
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(index) = self.hover {
                    return Ok(Some(HistoryOutcome::Picked { index, as_path: self.shift }));
                }
            }
            WindowEvent::ScaleFactorChanged { mut inner_size_writer, .. } => {
                let _ = inner_size_writer
                    .request_inner_size(PhysicalSize::new(self.layout.width(), self.layout.height()));
            }
            WindowEvent::RedrawRequested => self.draw()?,
            _ => {}
        }
        Ok(None)
    }

    fn set_hover(&mut self, hover: Option<usize>) {
        if self.hover != hover {
            self.hover = hover;
            self.window.request_redraw();
        }
    }

    fn draw(&mut self) -> Result<()> {
        let layout = self.layout;
        let (width, height) = (layout.width(), layout.height());
        self.surface
            .resize(
                NonZeroU32::new(width).context("창 너비가 0입니다")?,
                NonZeroU32::new(height).context("창 높이가 0입니다")?,
            )
            .map_err(|e| anyhow!("그리기 표면 크기 조정 실패: {e}"))?;
        let mut buffer = self
            .surface
            .buffer_mut()
            .map_err(|e| anyhow!("그리기 버퍼를 얻지 못했습니다: {e}"))?;
        if buffer.len() != (width * height) as usize {
            bail!("그리기 버퍼 크기가 창과 다릅니다");
        }
        let mut frame = Frame { pixels: &mut buffer, width, height };
        frame.fill(0, 0, width, height, BACKGROUND);

        for (i, thumb) in self.thumbs.iter().enumerate() {
            let (cx, cy) = layout.cell_origin(i);
            let hovered = self.hover == Some(i);
            frame.fill(cx, cy, layout.cell_w, layout.cell_h, if hovered { ACCENT } else { CELL_COLOR });
            if hovered {
                // 테두리만 강조색으로 남긴다.
                let b = layout.border;
                frame.fill(cx + b, cy + b, layout.cell_w - 2 * b, layout.cell_h - 2 * b, CELL_COLOR);
            }
            frame.blit(
                cx + (layout.cell_w - thumb.width) / 2,
                cy + (layout.cell_h - thumb.height) / 2,
                thumb,
            );
            if let Some(glyph) = DIGITS.get(i) {
                frame.fill(cx, cy, layout.badge, layout.badge, ACCENT);
                let dot = (layout.badge / 10).max(1);
                let (gx, gy) = (cx + (layout.badge - 5 * dot) / 2, cy + (layout.badge - 7 * dot) / 2);
                for (row, bits) in glyph.iter().enumerate() {
                    for col in 0..5u32 {
                        if bits >> (4 - col) & 1 == 1 {
                            frame.fill(gx + col * dot, gy + row as u32 * dot, dot, dot, WHITE);
                        }
                    }
                }
            }
        }
        buffer
            .present()
            .map_err(|e| anyhow!("화면에 그리지 못했습니다: {e}"))
    }
}

/// 숫자 키(위쪽 줄과 숫자 패드)를 0부터 시작하는 항목 번호로 바꾼다.
fn digit_index(code: KeyCode) -> Option<usize> {
    use KeyCode::*;
    Some(match code {
        Digit1 | Numpad1 => 0,
        Digit2 | Numpad2 => 1,
        Digit3 | Numpad3 => 2,
        Digit4 | Numpad4 => 3,
        Digit5 | Numpad5 => 4,
        Digit6 | Numpad6 => 5,
        Digit7 | Numpad7 => 6,
        Digit8 | Numpad8 => 7,
        Digit9 | Numpad9 => 8,
        _ => return None,
    })
}

/// 그리기 대상 버퍼. 범위를 벗어난 부분은 잘라서 그린다.
struct Frame<'a> {
    pixels: &'a mut [u32],
    width: u32,
    height: u32,
}

impl Frame<'_> {
    fn fill(&mut self, x: u32, y: u32, w: u32, h: u32, color: u32) {
        let (x1, y1) = ((x + w).min(self.width), (y + h).min(self.height));
        for row in y.min(y1)..y1 {
            let start = (row * self.width) as usize;
            self.pixels[start + x.min(x1) as usize..start + x1 as usize].fill(color);
        }
    }

    fn blit(&mut self, x: u32, y: u32, thumb: &Thumb) {
        let w = thumb.width.min(self.width.saturating_sub(x)) as usize;
        let h = thumb.height.min(self.height.saturating_sub(y));
        for row in 0..h {
            let src = (row * thumb.width) as usize;
            let dst = ((y + row) * self.width + x) as usize;
            self.pixels[dst..dst + w].copy_from_slice(&thumb.pixels[src..src + w]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_grid_size() {
        let one = Layout::new(1, 1.0);
        assert_eq!((one.columns(), one.rows()), (1, 1));
        assert_eq!((one.width(), one.height()), (12 + 252, 12 + 172));
        let five = Layout::new(5, 1.0);
        assert_eq!((five.columns(), five.rows()), (4, 2));
        let twelve = Layout::new(12, 2.0);
        assert_eq!((twelve.columns(), twelve.rows()), (4, 3));
        assert_eq!(twelve.width(), 24 + 4 * (480 + 24));
    }

    #[test]
    fn cell_hit_test() {
        let layout = Layout::new(6, 1.0);
        assert_eq!(layout.cell_at(12, 12), Some(0));
        assert_eq!(layout.cell_at(251, 171), Some(0));
        assert_eq!(layout.cell_at(5, 5), None); // 바깥 여백
        assert_eq!(layout.cell_at(255, 50), None); // 칸 사이
        assert_eq!(layout.cell_at(264, 12), Some(1));
        assert_eq!(layout.cell_at(12, 184), Some(4));
        // 둘째 줄의 빈 자리(7번째 칸)는 선택되지 않는다.
        assert_eq!(layout.cell_at(12 + 2 * 252, 184), None);
        assert_eq!(layout.cell_at(-1, -1), None);
    }

    #[test]
    fn digit_keys_map_to_indices() {
        assert_eq!(digit_index(KeyCode::Digit1), Some(0));
        assert_eq!(digit_index(KeyCode::Numpad9), Some(8));
        assert_eq!(digit_index(KeyCode::Digit0), None);
        assert_eq!(digit_index(KeyCode::KeyA), None);
    }
}
