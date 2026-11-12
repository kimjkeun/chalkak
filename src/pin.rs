//! 캡쳐를 화면 위에 띄워 두는 작은 창. 다른 작업을 하면서 참고용으로 본다.
//!
//! 항상 다른 창 위에 있고, 안쪽을 끌어서 옮기고, 가장자리를 끌거나 휠을 돌려 크기를 바꾸고,
//! 오른쪽 클릭이나 Esc로 닫는다.

use std::num::NonZeroU32;
use std::rc::Rc;

use anyhow::{Context, Result, anyhow};
use image::RgbaImage;
use image::imageops::FilterType;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::platform::windows::WindowAttributesExtWindows;
use winit::window::{CursorIcon, Window, WindowId, WindowLevel};

use crate::overlay::cursor_position;

const BORDER_COLOR: u32 = 0x0025_63EB;
const BORDER: usize = 2;
const MAX_ZOOM: f32 = 4.0;
/// 휠로 줄여도 이보다 작아지지는 않는다 (긴 변 기준, 픽셀).
const MIN_LONG_SIDE: f32 = 120.0;

pub struct PinWindow {
    window: Rc<Window>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
    image: RgbaImage,
    zoom: f32,
    /// 지금 창 크기로 맞춰 둔 그림 (0RGB 픽셀).
    pixels: Vec<u32>,
    size: (u32, u32),
    /// 커서가 놓인 가장자리 (크기 조절 손잡이).
    grip: Grip,
    /// 가장자리를 끌어 크기를 바꾸는 중일 때의 시작 상태.
    resizing: Option<Resizing>,
}

/// 창의 어느 가장자리를 잡았는지. 둘 다 0이면 가장자리가 아니다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Grip {
    /// -1: 왼쪽, 1: 오른쪽.
    horizontal: i32,
    /// -1: 위, 1: 아래.
    vertical: i32,
}

impl Grip {
    fn at(position: (i32, i32), size: (u32, u32), margin: i32) -> Self {
        let side = |v: i32, length: u32| {
            if v < margin {
                -1
            } else if v >= length as i32 - margin {
                1
            } else {
                0
            }
        };
        Self { horizontal: side(position.0, size.0), vertical: side(position.1, size.1) }
    }

    fn is_none(self) -> bool {
        self.horizontal == 0 && self.vertical == 0
    }

    fn cursor(self) -> CursorIcon {
        match (self.horizontal, self.vertical) {
            (0, 0) => CursorIcon::Move,
            (_, 0) => CursorIcon::EwResize,
            (0, _) => CursorIcon::NsResize,
            (h, v) if h == v => CursorIcon::NwseResize,
            _ => CursorIcon::NeswResize,
        }
    }
}

struct Resizing {
    grip: Grip,
    /// 끌기 시작한 순간의 커서 위치 (화면 좌표).
    cursor: (i32, i32),
    size: (u32, u32),
    position: (i32, i32),
}

/// 가장자리를 `(dx, dy)`만큼 끌었을 때의 새 배율. 그림의 가로세로 비율은 유지한다.
fn zoom_after_drag(image: (u32, u32), start: (u32, u32), grip: Grip, dx: i32, dy: i32) -> f32 {
    let by_width = (start.0 as i32 + dx * grip.horizontal).max(1) as f32 / image.0 as f32;
    let by_height = (start.1 as i32 + dy * grip.vertical).max(1) as f32 / image.1 as f32;
    let zoom = match (grip.horizontal != 0, grip.vertical != 0) {
        (true, false) => by_width,
        (false, true) => by_height,
        // 모서리를 잡았으면 더 많이 움직인 쪽을 따른다.
        _ => by_width.max(by_height),
    };
    clamp_zoom(image, zoom)
}

impl PinWindow {
    /// `image`를 `area`(모니터의 위치와 크기) 가운데에 띄운다. 모니터보다 크면 줄여서 띄운다.
    pub fn open(
        event_loop: &ActiveEventLoop,
        image: RgbaImage,
        area: (PhysicalPosition<i32>, PhysicalSize<u32>),
    ) -> Result<Self> {
        let (origin, monitor) = area;
        let zoom = start_zoom(image.dimensions(), (monitor.width, monitor.height));
        let size = scaled_size(image.dimensions(), zoom);
        let attrs = Window::default_attributes()
            .with_title("찰칵 띄운 캡쳐")
            .with_decorations(false)
            .with_resizable(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_skip_taskbar(true)
            .with_visible(false)
            .with_position(PhysicalPosition::new(
                origin.x + (monitor.width as i32 - size.0 as i32) / 2,
                origin.y + (monitor.height as i32 - size.1 as i32) / 2,
            ))
            .with_inner_size(PhysicalSize::new(size.0, size.1));
        let window = Rc::new(
            event_loop
                .create_window(attrs)
                .context("캡쳐를 띄울 창을 만들지 못했습니다")?,
        );
        window.set_cursor(CursorIcon::Move);
        let context = softbuffer::Context::new(window.clone())
            .map_err(|e| anyhow!("그리기 컨텍스트 생성 실패: {e}"))?;
        let surface = softbuffer::Surface::new(&context, window.clone())
            .map_err(|e| anyhow!("그리기 표면 생성 실패: {e}"))?;

        let mut this = Self {
            window,
            surface,
            image,
            zoom,
            pixels: Vec::new(),
            size,
            grip: Grip::default(),
            resizing: None,
        };
        this.render();
        this.draw()?;
        this.window.set_visible(true);
        this.window.focus_window();
        Ok(this)
    }

    pub fn window_id(&self) -> WindowId {
        self.window.id()
    }

    /// 창 이벤트를 처리한다. 창을 닫아야 하면 참을 돌려준다.
    pub fn handle(&mut self, event: WindowEvent) -> Result<bool> {
        match event {
            WindowEvent::CloseRequested => return Ok(true),
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.logical_key == Key::Named(NamedKey::Escape) =>
            {
                return Ok(true);
            }
            WindowEvent::CursorMoved { position, .. } => {
                if self.resizing.is_some() {
                    self.resize_to_cursor();
                } else {
                    let margin = (8.0 * self.window.scale_factor()).round() as i32;
                    let grip = Grip::at((position.x as i32, position.y as i32), self.size, margin);
                    if grip != self.grip {
                        self.grip = grip;
                        self.window.set_cursor(grip.cursor());
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => match (button, state) {
                (MouseButton::Right, ElementState::Released) => return Ok(true),
                (MouseButton::Left, ElementState::Pressed) => {
                    if self.grip.is_none() {
                        // 안쪽을 잡으면 끌어서 옮긴다.
                        let _ = self.window.drag_window();
                    } else if let (Some(cursor), Ok(position)) =
                        (cursor_position(), self.window.outer_position())
                    {
                        // 가장자리를 잡으면 끌어서 크기를 바꾼다.
                        self.resizing = Some(Resizing {
                            grip: self.grip,
                            cursor,
                            size: self.size,
                            position: (position.x, position.y),
                        });
                    }
                }
                (MouseButton::Left, ElementState::Released) => self.resizing = None,
                _ => {}
            },
            WindowEvent::MouseWheel { delta, .. } => {
                let steps = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 120.0,
                };
                let zoom = clamp_zoom(self.image.dimensions(), self.zoom * 1.1f32.powf(steps));
                let size = scaled_size(self.image.dimensions(), zoom);
                if size != self.size {
                    self.zoom = zoom;
                    self.size = size;
                    let _ = self.window.request_inner_size(PhysicalSize::new(size.0, size.1));
                    self.render();
                    self.window.request_redraw();
                }
            }
            // 배율이 다른 모니터로 옮겨도 그림의 픽셀 크기를 그대로 유지한다.
            WindowEvent::ScaleFactorChanged { mut inner_size_writer, .. } => {
                let _ = inner_size_writer.request_inner_size(PhysicalSize::new(self.size.0, self.size.1));
            }
            WindowEvent::RedrawRequested => self.draw()?,
            _ => {}
        }
        Ok(false)
    }

    /// 가장자리를 끄는 중: 커서가 움직인 만큼 크기를 바꾼다. 반대쪽 가장자리는 제자리에 둔다.
    fn resize_to_cursor(&mut self) {
        let (Some(start), Some(cursor)) = (self.resizing.as_ref(), cursor_position()) else {
            return;
        };
        let (dx, dy) = (cursor.0 - start.cursor.0, cursor.1 - start.cursor.1);
        let zoom = zoom_after_drag(self.image.dimensions(), start.size, start.grip, dx, dy);
        let size = scaled_size(self.image.dimensions(), zoom);
        if size == self.size {
            return;
        }
        // 왼쪽이나 위쪽을 끌면 창의 시작점이 함께 움직여야 한다.
        let x = if start.grip.horizontal < 0 {
            start.position.0 + start.size.0 as i32 - size.0 as i32
        } else {
            start.position.0
        };
        let y = if start.grip.vertical < 0 {
            start.position.1 + start.size.1 as i32 - size.1 as i32
        } else {
            start.position.1
        };
        self.zoom = zoom;
        self.size = size;
        self.window.set_outer_position(PhysicalPosition::new(x, y));
        let _ = self.window.request_inner_size(PhysicalSize::new(size.0, size.1));
        self.render();
        self.window.request_redraw();
    }

    /// 지금 크기에 맞는 그림을 만들어 둔다.
    fn render(&mut self) {
        let (width, height) = self.size;
        let scaled;
        let image = if self.size == self.image.dimensions() {
            &self.image
        } else if self.zoom < 1.0 {
            scaled = image::imageops::thumbnail(&self.image, width, height);
            &scaled
        } else {
            scaled = image::imageops::resize(&self.image, width, height, FilterType::Triangle);
            &scaled
        };
        self.pixels = image
            .as_raw()
            .chunks_exact(4)
            .map(|p| (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32)
            .collect();
        // 바탕과 구별되도록 테두리를 두른다.
        let (w, h) = (width as usize, height as usize);
        for y in 0..h {
            for x in 0..w {
                if x < BORDER || y < BORDER || x + BORDER >= w || y + BORDER >= h {
                    self.pixels[y * w + x] = BORDER_COLOR;
                }
            }
        }
    }

    fn draw(&mut self) -> Result<()> {
        self.surface
            .resize(
                NonZeroU32::new(self.size.0).context("그림 너비가 0입니다")?,
                NonZeroU32::new(self.size.1).context("그림 높이가 0입니다")?,
            )
            .map_err(|e| anyhow!("그리기 표면 크기 조정 실패: {e}"))?;
        let mut buffer = self
            .surface
            .buffer_mut()
            .map_err(|e| anyhow!("그리기 버퍼를 얻지 못했습니다: {e}"))?;
        if buffer.len() != self.pixels.len() {
            return Ok(());
        }
        buffer.copy_from_slice(&self.pixels);
        buffer
            .present()
            .map_err(|e| anyhow!("화면에 그리지 못했습니다: {e}"))
    }
}

/// 처음 띄울 때의 배율: 원래 크기 그대로, 다만 모니터의 80%를 넘으면 줄인다.
fn start_zoom(image: (u32, u32), monitor: (u32, u32)) -> f32 {
    let fit = (monitor.0 as f32 * 0.8 / image.0 as f32).min(monitor.1 as f32 * 0.8 / image.1 as f32);
    clamp_zoom(image, fit.min(1.0))
}

fn clamp_zoom(image: (u32, u32), zoom: f32) -> f32 {
    let long_side = image.0.max(image.1) as f32;
    // 작은 그림은 원래 크기가 이미 최소보다 작을 수 있다.
    let min = (MIN_LONG_SIDE / long_side).min(1.0);
    zoom.clamp(min, MAX_ZOOM)
}

fn scaled_size(image: (u32, u32), zoom: f32) -> (u32, u32) {
    let dim = |v: u32| ((v as f32 * zoom).round() as u32).max(1);
    (dim(image.0), dim(image.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_captures_open_at_original_size() {
        assert_eq!(start_zoom((400, 300), (1920, 1080)), 1.0);
        assert_eq!(scaled_size((400, 300), 1.0), (400, 300));
    }

    #[test]
    fn large_captures_are_shrunk_to_fit_the_monitor() {
        let zoom = start_zoom((3840, 1080), (1920, 1080));
        let (w, h) = scaled_size((3840, 1080), zoom);
        assert_eq!(w, 1536); // 모니터 너비의 80%
        assert!(h <= 864);
    }

    #[test]
    fn grips_are_found_at_the_edges() {
        let size = (400, 300);
        assert!(Grip::at((200, 150), size, 8).is_none());
        assert_eq!(Grip::at((3, 150), size, 8), Grip { horizontal: -1, vertical: 0 });
        assert_eq!(Grip::at((396, 297), size, 8), Grip { horizontal: 1, vertical: 1 });
        assert_eq!(Grip::at((200, 2), size, 8).cursor(), CursorIcon::NsResize);
        assert_eq!(Grip::at((2, 2), size, 8).cursor(), CursorIcon::NwseResize);
        assert_eq!(Grip::at((398, 2), size, 8).cursor(), CursorIcon::NeswResize);
    }

    #[test]
    fn dragging_an_edge_scales_and_keeps_the_ratio() {
        let image = (800, 600);
        let start = (400, 300);
        let right = Grip { horizontal: 1, vertical: 0 };
        assert_eq!(zoom_after_drag(image, start, right, 200, 999), 0.75);
        // 왼쪽을 왼쪽으로 끌면 커진다.
        let left = Grip { horizontal: -1, vertical: 0 };
        assert_eq!(zoom_after_drag(image, start, left, -200, 0), 0.75);
        let bottom = Grip { horizontal: 0, vertical: 1 };
        assert_eq!(zoom_after_drag(image, start, bottom, 0, -150), 0.25);
        // 모서리는 더 많이 움직인 쪽을 따른다.
        let corner = Grip { horizontal: 1, vertical: 1 };
        assert_eq!(zoom_after_drag(image, start, corner, 400, 0), 1.0);
        assert_eq!(scaled_size(image, 0.75), (600, 450));
    }

    #[test]
    fn zoom_stays_within_limits() {
        assert_eq!(clamp_zoom((1000, 500), 10.0), MAX_ZOOM);
        assert_eq!(clamp_zoom((1000, 500), 0.01), 0.12);
        // 원래부터 작은 그림은 더 줄이지 않는다.
        assert_eq!(clamp_zoom((60, 40), 0.5), 1.0);
    }
}
