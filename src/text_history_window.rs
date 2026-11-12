//! 텍스트 히스토리 창: 최근에 복사한 텍스트 목록을 커서 근처에 띄우고 하나를 고르게 한다.
//!
//! 클릭, 숫자 키(보이는 줄의 1~9), 방향키+Enter로 고른다. Esc나 다른 창 클릭으로 닫힌다.

use std::num::NonZeroU32;
use std::rc::Rc;

use anyhow::{Context, Result, anyhow, bail};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, KeyCode, PhysicalKey};
use winit::platform::windows::WindowAttributesExtWindows;
use winit::window::{Window, WindowId, WindowLevel};

use crate::annotate::{self, Color, Font, Target};
use crate::text_history::one_line;

const HEADER: &str = "복사한 텍스트   ·   클릭 · 숫자 · Enter: 붙여넣기   ·   Esc: 닫기";
/// 한 번에 보이는 최대 줄 수.
const MAX_ROWS: usize = 12;

const BACKGROUND: Color = [0x1E, 0x1E, 0x1E];
const BORDER: Color = [0x55, 0x55, 0x55];
const SELECTED: Color = [0x25, 0x4A, 0x9E];
const BADGE: Color = [0x25, 0x63, 0xEB];
const TEXT: Color = [0xF0, 0xF0, 0xF0];
const MUTED: Color = [0x9A, 0x9A, 0x9A];

pub enum TextOutcome {
    Closed,
    /// 목록의 `index`번째(0부터)를 골랐다.
    Picked(usize),
}

/// 목록의 한 줄에 보여 줄 내용.
struct Row {
    preview: String,
    /// 여러 줄짜리 텍스트일 때 오른쪽에 보여 주는 줄 수.
    hint: String,
}

pub struct TextHistoryWindow {
    window: Rc<Window>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
    font: Rc<Font>,
    rows: Vec<Row>,
    width: i32,
    height: i32,
    header_h: i32,
    row_h: i32,
    pad: i32,
    text_size: f32,
    visible: usize,
    selected: usize,
    /// 맨 위에 보이는 줄의 번호.
    top: usize,
}

impl TextHistoryWindow {
    /// `items`(최신 순)를 보여 주는 창을 `cursor` 근처에, `area`(모니터) 안에 들어오게 띄운다.
    pub fn open(
        event_loop: &ActiveEventLoop,
        items: &[String],
        cursor: (i32, i32),
        area: (PhysicalPosition<i32>, PhysicalSize<u32>),
        scale: f64,
        font: Rc<Font>,
    ) -> Result<Self> {
        if items.is_empty() {
            bail!("보여 줄 텍스트가 없습니다");
        }
        let px = |v: f64| ((v * scale).round() as i32).max(1);
        let (origin, monitor) = area;
        let width = px(600.0).min(monitor.width as i32);
        let (header_h, row_h, pad) = (px(30.0), px(36.0), px(10.0));
        let visible = items.len().min(MAX_ROWS);
        let height = header_h + visible as i32 * row_h + 1;
        let text_size = 16.0 * scale as f32;

        // 줄 번호와 줄 수 표시를 뺀 자리에 들어가도록 미리보기를 자른다.
        let preview_width = (width - row_h - 2 * pad - px(56.0)) as f32;
        let rows = items
            .iter()
            .map(|text| {
                let lines = text.trim_end().lines().count();
                Row {
                    preview: truncate_to_width(&font, &one_line(text, 300), text_size, preview_width),
                    hint: if lines > 1 { format!("{lines}줄") } else { String::new() },
                }
            })
            .collect();

        // 커서 오른쪽 아래에 두되, 모니터 밖으로 나가지 않게 당긴다.
        let x = cursor.0.clamp(origin.x, (origin.x + monitor.width as i32 - width).max(origin.x));
        let y = cursor.1.clamp(origin.y, (origin.y + monitor.height as i32 - height).max(origin.y));
        let attrs = Window::default_attributes()
            .with_title("찰칵 텍스트 히스토리")
            .with_decorations(false)
            .with_resizable(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_skip_taskbar(true)
            .with_visible(false)
            .with_position(PhysicalPosition::new(x, y))
            .with_inner_size(PhysicalSize::new(width as u32, height as u32));
        let window = Rc::new(
            event_loop
                .create_window(attrs)
                .context("텍스트 히스토리 창을 만들지 못했습니다")?,
        );
        let context = softbuffer::Context::new(window.clone())
            .map_err(|e| anyhow!("그리기 컨텍스트 생성 실패: {e}"))?;
        let surface = softbuffer::Surface::new(&context, window.clone())
            .map_err(|e| anyhow!("그리기 표면 생성 실패: {e}"))?;

        let mut this = Self {
            window,
            surface,
            font,
            rows,
            width,
            height,
            header_h,
            row_h,
            pad,
            text_size,
            visible,
            selected: 0,
            top: 0,
        };
        this.draw()?;
        this.window.set_visible(true);
        this.window.focus_window();
        Ok(this)
    }

    pub fn window_id(&self) -> WindowId {
        self.window.id()
    }

    pub fn handle(&mut self, event: WindowEvent) -> Result<Option<TextOutcome>> {
        match event {
            WindowEvent::CloseRequested | WindowEvent::Focused(false) => {
                return Ok(Some(TextOutcome::Closed));
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let PhysicalKey::Code(code) = event.physical_key else {
                    return Ok(None);
                };
                match code {
                    KeyCode::Escape => return Ok(Some(TextOutcome::Closed)),
                    KeyCode::Enter | KeyCode::NumpadEnter => {
                        return Ok(Some(TextOutcome::Picked(self.selected)));
                    }
                    KeyCode::ArrowUp => self.select(self.selected.saturating_sub(1)),
                    KeyCode::ArrowDown => self.select(self.selected + 1),
                    _ => {
                        // NumLock이 꺼진 숫자 패드는 방향키이므로 숫자로 치지 않는다.
                        let types_a_digit = matches!(event.logical_key, Key::Character(_));
                        if let Some(row) = digit_row(code).filter(|&r| types_a_digit && r < self.visible) {
                            return Ok(Some(TextOutcome::Picked(self.top + row)));
                        }
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(index) = self.row_at(position.y as i32) {
                    self.select(index);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / self.row_h as f32,
                };
                let max_top = self.rows.len() - self.visible;
                let top = (self.top as f32 - lines).round().clamp(0.0, max_top as f32) as usize;
                if top != self.top {
                    self.top = top;
                    self.selected = self.selected.clamp(top, top + self.visible - 1);
                    self.window.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => return Ok(Some(TextOutcome::Picked(self.selected))),
            WindowEvent::ScaleFactorChanged { mut inner_size_writer, .. } => {
                let _ = inner_size_writer
                    .request_inner_size(PhysicalSize::new(self.width as u32, self.height as u32));
            }
            WindowEvent::RedrawRequested => self.draw()?,
            _ => {}
        }
        Ok(None)
    }

    /// `index`번째 줄을 고르고, 보이지 않으면 보이도록 스크롤한다.
    fn select(&mut self, index: usize) {
        let index = index.min(self.rows.len() - 1);
        if index < self.top {
            self.top = index;
        } else if index >= self.top + self.visible {
            self.top = index + 1 - self.visible;
        }
        if index != self.selected {
            self.selected = index;
        }
        self.window.request_redraw();
    }

    fn row_at(&self, y: i32) -> Option<usize> {
        if y < self.header_h {
            return None;
        }
        let row = ((y - self.header_h) / self.row_h) as usize;
        (row < self.visible).then_some(self.top + row)
    }

    fn draw(&mut self) -> Result<()> {
        let (width, height) = (self.width, self.height);
        self.surface
            .resize(
                NonZeroU32::new(width as u32).context("창 너비가 0입니다")?,
                NonZeroU32::new(height as u32).context("창 높이가 0입니다")?,
            )
            .map_err(|e| anyhow!("그리기 표면 크기 조정 실패: {e}"))?;
        let mut buffer = self
            .surface
            .buffer_mut()
            .map_err(|e| anyhow!("그리기 버퍼를 얻지 못했습니다: {e}"))?;
        if buffer.len() != (width * height) as usize {
            return Ok(());
        }
        let mut canvas = Canvas { pixels: &mut buffer, width, height };
        annotate::fill(&mut canvas, 0, 0, width, height, BACKGROUND);

        let small = self.text_size * 0.8;
        self.font.draw(
            &mut canvas,
            HEADER,
            self.pad as f32,
            (self.header_h as f32 - small) / 2.0,
            small,
            MUTED,
        );

        for row in 0..self.visible {
            let index = self.top + row;
            let entry = &self.rows[index];
            let y = self.header_h + row as i32 * self.row_h;
            if index == self.selected {
                annotate::fill(&mut canvas, 1, y, width - 1, y + self.row_h, SELECTED);
            }
            let text_y = y as f32 + (self.row_h as f32 - self.text_size) / 2.0;
            // 숫자 키로 고를 수 있는 줄에는 번호를 붙인다.
            if row < 9 {
                let badge = self.row_h * 3 / 5;
                let (bx, by) = (self.pad, y + (self.row_h - badge) / 2);
                annotate::fill(&mut canvas, bx, by, bx + badge, by + badge, BADGE);
                self.font.draw_centered(
                    &mut canvas,
                    &(row + 1).to_string(),
                    bx as f32 + badge as f32 / 2.0,
                    by as f32 + badge as f32 / 2.0,
                    self.text_size * 0.9,
                    TEXT,
                );
            }
            let text_x = (self.pad + self.row_h) as f32;
            self.font.draw(&mut canvas, &entry.preview, text_x, text_y, self.text_size, TEXT);
            if !entry.hint.is_empty() {
                let hint_width = self.font.measure(&entry.hint, small);
                self.font.draw(
                    &mut canvas,
                    &entry.hint,
                    width as f32 - self.pad as f32 - hint_width,
                    y as f32 + (self.row_h as f32 - small) / 2.0,
                    small,
                    MUTED,
                );
            }
        }

        // 테두리 (창 장식이 없어서 직접 그린다).
        annotate::fill(&mut canvas, 0, 0, width, 1, BORDER);
        annotate::fill(&mut canvas, 0, height - 1, width, height, BORDER);
        annotate::fill(&mut canvas, 0, 0, 1, height, BORDER);
        annotate::fill(&mut canvas, width - 1, 0, width, height, BORDER);
        buffer
            .present()
            .map_err(|e| anyhow!("화면에 그리지 못했습니다: {e}"))
    }
}

/// `width` 안에 들어가도록 글자를 자르고, 잘랐으면 끝에 줄임표를 붙인다.
fn truncate_to_width(font: &Font, text: &str, size: f32, width: f32) -> String {
    if font.measure(text, size) <= width {
        return text.to_string();
    }
    let mut out = String::new();
    for ch in text.chars() {
        out.push(ch);
        if font.measure(&out, size) + font.measure("…", size) > width {
            out.pop();
            break;
        }
    }
    out.push('…');
    out
}

/// 숫자 키(위쪽 줄과 숫자 패드)를 0부터 시작하는 보이는 줄 번호로 바꾼다.
fn digit_row(code: KeyCode) -> Option<usize> {
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

/// 창의 그리기 버퍼 (0RGB 픽셀).
struct Canvas<'a> {
    pixels: &'a mut [u32],
    width: i32,
    height: i32,
}

impl Target for Canvas<'_> {
    fn blend(&mut self, x: i32, y: i32, color: Color, alpha: u8) {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return;
        }
        let pixel = &mut self.pixels[(y * self.width + x) as usize];
        let old = [(*pixel >> 16) as u8, (*pixel >> 8) as u8, *pixel as u8];
        let [r, g, b] = [0, 1, 2].map(|i| annotate::mix(old[i], color[i], alpha) as u32);
        *pixel = r << 16 | g << 8 | b;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_text_is_cut_with_an_ellipsis() {
        let font = Font::load().unwrap();
        let long = "가나다라마바사 ".repeat(30);
        let cut = truncate_to_width(&font, &long, 16.0, 200.0);
        assert!(cut.ends_with('…'));
        assert!(font.measure(&cut, 16.0) <= 200.0);
        assert!(cut.chars().count() > 5);
        assert_eq!(truncate_to_width(&font, "짧다", 16.0, 200.0), "짧다");
    }

    #[test]
    fn digit_keys_map_to_rows() {
        assert_eq!(digit_row(KeyCode::Digit1), Some(0));
        assert_eq!(digit_row(KeyCode::Numpad9), Some(8));
        assert_eq!(digit_row(KeyCode::Digit0), None);
    }
}
