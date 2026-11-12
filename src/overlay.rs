//! 영역 선택 오버레이.
//!
//! 캡쳐해 둔 정지 화면을 모든 모니터에 걸친 테두리 없는 창에 어둡게 깔고,
//! 드래그한 사각형만 원래 밝기로 보여 준다. 마우스를 놓으면 확정, Esc나
//! 오른쪽 클릭은 취소다.

use std::num::NonZeroU32;
use std::rc::Rc;

use anyhow::{Context, Result, anyhow, bail};
use image::RgbaImage;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::platform::windows::WindowAttributesExtWindows;
use winit::window::{CursorIcon, Window, WindowId, WindowLevel};

use crate::capture::{Rect, Shot};

/// 이보다 작은 드래그는 실수로 보고 무시한다.
const MIN_SELECTION: u32 = 4;
const BORDER_COLOR: u32 = 0x00FF_FFFF;
/// 안내선: 가운데 선과, 그 양옆에 붙는 띠의 색과 두께.
const GUIDE_COLOR: u32 = 0x00FF_FFFF;
const GUIDE_EDGE_COLOR: u32 = 0x0000_0000;
const GUIDE_HALF: i32 = 1;

/// 모든 모니터의 캡쳐를 한 장으로 합친 것. `bounds`는 가상 데스크톱에서의 위치다.
pub struct Canvas {
    pub image: RgbaImage,
    pub bounds: Rect,
}

impl Canvas {
    pub fn from_shots(shots: &[Shot]) -> Result<Self> {
        if shots.is_empty() {
            bail!("캡쳐된 모니터가 없습니다");
        }
        let left = shots.iter().map(|s| s.bounds.x).min().unwrap();
        let top = shots.iter().map(|s| s.bounds.y).min().unwrap();
        let right = shots
            .iter()
            .map(|s| s.bounds.x + s.bounds.width as i32)
            .max()
            .unwrap();
        let bottom = shots
            .iter()
            .map(|s| s.bounds.y + s.bounds.height as i32)
            .max()
            .unwrap();
        let bounds = Rect {
            x: left,
            y: top,
            width: (right - left) as u32,
            height: (bottom - top) as u32,
        };
        let mut image = RgbaImage::from_pixel(bounds.width, bounds.height, image::Rgba([0, 0, 0, 255]));
        for shot in shots {
            image::imageops::replace(
                &mut image,
                &shot.image,
                (shot.bounds.x - left) as i64,
                (shot.bounds.y - top) as i64,
            );
        }
        Ok(Self { image, bounds })
    }
}

pub enum Outcome {
    /// 선택된 영역 (가상 데스크톱 좌표).
    Selected(Rect),
    Cancelled,
}

pub struct Overlay {
    window: Rc<Window>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
    origin: (i32, i32),
    width: u32,
    height: u32,
    /// 0RGB 픽셀. `bright`는 원본, `dim`은 어둡게 만든 것.
    bright: Vec<u32>,
    dim: Vec<u32>,
    cursor: (i32, i32),
    drag_start: Option<(i32, i32)>,
    /// 직전 프레임에서 `dim`과 다르게 칠한 영역. 다음 프레임에서 여기만 되돌린다.
    painted: Vec<Area>,
    /// 다음 다시 그리기가 우리 요청(커서 이동 등)으로 생긴 것인지.
    partial_redraw: bool,
    /// 창이 뜬 직후에는 요청이 합쳐질 수 있으므로, 처음 몇 번은 무조건 창 전체를 보낸다.
    full_presents_left: u8,
}

/// 캔버스 안의 사각형 `[x0, x1) x [y0, y1)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Area {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
}

impl Area {
    fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }
}

/// 커서를 지나는 안내선: 가운데 흰 선 1픽셀과 양옆의 검은 선. 어떤 배경에서도 보인다.
/// 가로선과 세로선이 차지하는 영역을 돌려준다.
fn guide_areas(cursor: (i32, i32), width: usize, height: usize) -> [Area; 2] {
    let band = |c: i32, max: usize| {
        let lo = (c - GUIDE_HALF).clamp(0, max as i32) as usize;
        let hi = (c + GUIDE_HALF + 1).clamp(0, max as i32) as usize;
        (lo, hi)
    };
    let (x0, x1) = band(cursor.0, width);
    let (y0, y1) = band(cursor.1, height);
    [
        Area { x0, y0: 0, x1, y1: height },
        Area { x0: 0, y0, x1: width, y1 },
    ]
}

impl Overlay {
    pub fn open(event_loop: &ActiveEventLoop, canvas: &Canvas) -> Result<Self> {
        let Rect { x, y, width, height } = canvas.bounds;
        let attrs = Window::default_attributes()
            .with_title("찰칵")
            .with_decorations(false)
            .with_resizable(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_skip_taskbar(true)
            .with_visible(false)
            .with_position(PhysicalPosition::new(x, y))
            .with_inner_size(PhysicalSize::new(width, height));
        let window = Rc::new(
            event_loop
                .create_window(attrs)
                .context("오버레이 창을 만들지 못했습니다")?,
        );
        window.set_cursor(CursorIcon::Crosshair);

        let context = softbuffer::Context::new(window.clone())
            .map_err(|e| anyhow!("그리기 컨텍스트 생성 실패: {e}"))?;
        let surface = softbuffer::Surface::new(&context, window.clone())
            .map_err(|e| anyhow!("그리기 표면 생성 실패: {e}"))?;

        let bright: Vec<u32> = canvas
            .image
            .as_raw()
            .chunks_exact(4)
            .map(|p| (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32)
            .collect();
        // 채널마다 절반 밝기.
        let dim = bright.iter().map(|p| (p >> 1) & 0x007F_7F7F).collect();

        let mut surface = surface;
        surface
            .resize(
                NonZeroU32::new(width).context("캔버스 너비가 0입니다")?,
                NonZeroU32::new(height).context("캔버스 높이가 0입니다")?,
            )
            .map_err(|e| anyhow!("그리기 표면 크기 조정 실패: {e}"))?;

        let mut overlay = Self {
            window,
            surface,
            origin: (x, y),
            width,
            height,
            bright,
            dim,
            // 창이 뜨자마자 안내선이 보이도록 현재 커서 위치에서 시작한다.
            cursor: cursor_position().map_or((-10, -10), |(cx, cy)| (cx - x, cy - y)),
            drag_start: None,
            painted: Vec::new(),
            partial_redraw: false,
            full_presents_left: 3,
        };
        // 첫 화면을 그린 뒤에 보여 줘서 빈 창이 깜빡이지 않게 한다.
        overlay.draw()?;
        overlay.window.set_visible(true);
        overlay.window.focus_window();
        Ok(overlay)
    }

    pub fn window_id(&self) -> WindowId {
        self.window.id()
    }

    /// 창 이벤트를 처리한다. 선택이 끝나거나 취소되면 결과를 돌려준다.
    pub fn handle(&mut self, event: WindowEvent) -> Result<Option<Outcome>> {
        match event {
            WindowEvent::CloseRequested => return Ok(Some(Outcome::Cancelled)),
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.logical_key == Key::Named(NamedKey::Escape) =>
            {
                return Ok(Some(Outcome::Cancelled));
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x as i32, position.y as i32);
                self.request_partial_redraw();
            }
            WindowEvent::MouseInput { state, button, .. } => match (button, state) {
                (MouseButton::Right, ElementState::Pressed) => {
                    return Ok(Some(Outcome::Cancelled));
                }
                (MouseButton::Left, ElementState::Pressed) => {
                    self.drag_start = Some(self.cursor);
                }
                (MouseButton::Left, ElementState::Released) => {
                    let selection = self.selection();
                    self.drag_start = None;
                    match selection {
                        Some(sel) if sel.width >= MIN_SELECTION && sel.height >= MIN_SELECTION => {
                            return Ok(Some(Outcome::Selected(Rect {
                                x: sel.x + self.origin.0,
                                y: sel.y + self.origin.1,
                                ..sel
                            })));
                        }
                        _ => self.request_partial_redraw(),
                    }
                }
                _ => {}
            },
            // 배율이 다른 모니터에 걸쳐도 창의 물리 픽셀 크기를 그대로 유지한다.
            WindowEvent::ScaleFactorChanged { mut inner_size_writer, .. } => {
                let _ = inner_size_writer.request_inner_size(PhysicalSize::new(self.width, self.height));
            }
            WindowEvent::RedrawRequested => self.draw()?,
            _ => {}
        }
        Ok(None)
    }

    fn request_partial_redraw(&mut self) {
        self.partial_redraw = true;
        self.window.request_redraw();
    }

    /// 현재 드래그 중인 사각형 (캔버스 안쪽 좌표, 캔버스 범위로 잘림).
    fn selection(&self) -> Option<Rect> {
        let (sx, sy) = self.drag_start?;
        let clamp = |v: i32, max: u32| v.clamp(0, max as i32);
        let x0 = clamp(sx.min(self.cursor.0), self.width);
        let x1 = clamp(sx.max(self.cursor.0), self.width);
        let y0 = clamp(sy.min(self.cursor.1), self.height);
        let y1 = clamp(sy.max(self.cursor.1), self.height);
        Some(Rect {
            x: x0,
            y: y0,
            width: (x1 - x0) as u32,
            height: (y1 - y0) as u32,
        })
    }

    fn draw(&mut self) -> Result<()> {
        let selection = self.selection();
        let (w, h) = (self.width as usize, self.height as usize);
        let mut buffer = self
            .surface
            .buffer_mut()
            .map_err(|e| anyhow!("그리기 버퍼를 얻지 못했습니다: {e}"))?;
        if buffer.len() != w * h {
            bail!("그리기 버퍼 크기가 캔버스와 다릅니다");
        }

        // 화면 전체(모니터 여러 대 분량)를 매번 다시 그리면 느리므로,
        // 직전에 칠한 곳만 되돌리고 바뀐 곳만 화면에 보낸다.
        let mut damage: Vec<Area> = Vec::new();
        // 창이 처음 보일 때나 Windows가 다시 그리라고 할 때는 창 전체를 화면에 보내야 한다.
        // 우리가 요청한 다시 그리기(커서 이동 등)일 때만 바뀐 부분만 보낸다.
        let present_all = !self.partial_redraw || self.full_presents_left > 0;
        self.partial_redraw = false;
        self.full_presents_left = self.full_presents_left.saturating_sub(1);
        if present_all {
            damage.push(Area { x0: 0, y0: 0, x1: w, y1: h });
        }
        if buffer.age() == 0 {
            buffer.copy_from_slice(&self.dim);
            self.painted.clear();
            if !present_all {
                damage.push(Area { x0: 0, y0: 0, x1: w, y1: h });
            }
        } else {
            for area in self.painted.drain(..) {
                for y in area.y0..area.y1 {
                    let row = y * w;
                    buffer[row + area.x0..row + area.x1]
                        .copy_from_slice(&self.dim[row + area.x0..row + area.x1]);
                }
                damage.push(area);
            }
        }

        if let Some(sel) = selection.filter(|s| s.width > 0 && s.height > 0) {
            // 드래그 중: 선택 영역을 원래 밝기로 보여 주고 테두리를 두른다.
            let (x0, y0) = (sel.x as usize, sel.y as usize);
            let (x1, y1) = (x0 + sel.width as usize, y0 + sel.height as usize);
            for y in y0..y1 {
                let row = y * w;
                buffer[row + x0..row + x1].copy_from_slice(&self.bright[row + x0..row + x1]);
            }
            for x in x0..x1 {
                buffer[y0 * w + x] = BORDER_COLOR;
                buffer[(y1 - 1) * w + x] = BORDER_COLOR;
            }
            for y in y0..y1 {
                buffer[y * w + x0] = BORDER_COLOR;
                buffer[y * w + x1 - 1] = BORDER_COLOR;
            }
            self.painted.push(Area { x0, y0, x1, y1 });
        } else if self.drag_start.is_none() {
            // 드래그 전: 커서를 지나는 십자 안내선으로 위치를 알려 준다.
            let (cx, cy) = self.cursor;
            let [vertical, horizontal] = guide_areas(self.cursor, w, h);
            for area in [vertical, horizontal] {
                if area.is_empty() {
                    continue;
                }
                for y in area.y0..area.y1 {
                    buffer[y * w + area.x0..y * w + area.x1].fill(GUIDE_EDGE_COLOR);
                }
                self.painted.push(area);
            }
            // 가운데 흰 선은 검은 띠를 다 칠한 뒤에 그려야 교차점에서도 끊기지 않는다.
            if cx >= 0 && (cx as usize) < w {
                for y in 0..h {
                    buffer[y * w + cx as usize] = GUIDE_COLOR;
                }
            }
            if cy >= 0 && (cy as usize) < h {
                buffer[cy as usize * w..(cy as usize + 1) * w].fill(GUIDE_COLOR);
            }
        }
        damage.extend(self.painted.iter().copied());

        let rects: Vec<softbuffer::Rect> = damage
            .iter()
            .filter_map(|a| {
                Some(softbuffer::Rect {
                    x: a.x0 as u32,
                    y: a.y0 as u32,
                    width: NonZeroU32::new((a.x1.checked_sub(a.x0)?) as u32)?,
                    height: NonZeroU32::new((a.y1.checked_sub(a.y0)?) as u32)?,
                })
            })
            .collect();
        buffer
            .present_with_damage(&rects)
            .map_err(|e| anyhow!("화면에 그리지 못했습니다: {e}"))
    }
}

/// 가상 데스크톱에서의 현재 커서 위치 (물리 픽셀).
pub fn cursor_position() -> Option<(i32, i32)> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut point = POINT { x: 0, y: 0 };
    // SAFETY: 유효한 POINT를 가리키는 포인터를 넘긴다.
    (unsafe { GetCursorPos(&mut point) } != 0).then_some((point.x, point.y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guide_lines_cover_cursor_and_stay_inside() {
        let [v, h] = guide_areas((100, 50), 400, 300);
        assert_eq!(v, Area { x0: 99, y0: 0, x1: 102, y1: 300 });
        assert_eq!(h, Area { x0: 0, y0: 49, x1: 400, y1: 52 });

        let [v, h] = guide_areas((0, 299), 400, 300);
        assert_eq!((v.x0, v.x1), (0, 2));
        assert_eq!((h.y0, h.y1), (298, 300));

        // 커서가 캔버스 밖이면 그릴 것이 없다.
        let [v, h] = guide_areas((-10, 1000), 400, 300);
        assert!(v.is_empty() && h.is_empty());
    }
}
