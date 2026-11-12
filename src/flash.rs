//! 캡쳐 직후 찍힌 영역을 잠깐 하얗게 번쩍여서 "찍혔다"는 것과 "어디가 찍혔는지"를 알려 준다.
//! 오버레이 없이 바로 찍는 동작(같은 영역 다시, 전체 화면)에 쓴다.

use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use image::RgbaImage;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event_loop::ActiveEventLoop;
use winit::platform::windows::WindowAttributesExtWindows;
use winit::window::{Window, WindowId, WindowLevel};

const DURATION: Duration = Duration::from_millis(220);
const BORDER: usize = 3;
const BORDER_COLOR: u32 = 0x0025_63EB;

pub struct Flash {
    window: Rc<Window>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
    pixels: Vec<u32>,
    width: u32,
    height: u32,
    until: Instant,
}

impl Flash {
    /// `image`가 찍힌 위치(가상 데스크톱 좌표 `x`, `y`)에 번쩍임을 띄운다.
    pub fn show(event_loop: &ActiveEventLoop, x: i32, y: i32, image: &RgbaImage) -> Result<Self> {
        let (width, height) = image.dimensions();
        let attrs = Window::default_attributes()
            .with_title("capture_tool")
            .with_decorations(false)
            .with_resizable(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_skip_taskbar(true)
            // 작업 중인 창의 포커스를 빼앗지 않는다.
            .with_active(false)
            .with_visible(false)
            .with_position(PhysicalPosition::new(x, y))
            .with_inner_size(PhysicalSize::new(width, height));
        let window = Rc::new(
            event_loop
                .create_window(attrs)
                .context("번쩍임 창을 만들지 못했습니다")?,
        );
        let _ = window.set_cursor_hittest(false);

        let context = softbuffer::Context::new(window.clone())
            .map_err(|e| anyhow!("그리기 컨텍스트 생성 실패: {e}"))?;
        let surface = softbuffer::Surface::new(&context, window.clone())
            .map_err(|e| anyhow!("그리기 표면 생성 실패: {e}"))?;

        let (w, h) = (width as usize, height as usize);
        // 원본과 흰색을 반씩 섞는다.
        let mut pixels: Vec<u32> = image
            .as_raw()
            .chunks_exact(4)
            .map(|p| {
                let mix = |c: u8| (c as u32 + 255) / 2;
                mix(p[0]) << 16 | mix(p[1]) << 8 | mix(p[2])
            })
            .collect();
        for y in 0..h {
            for x in 0..w {
                if x < BORDER || y < BORDER || x + BORDER >= w || y + BORDER >= h {
                    pixels[y * w + x] = BORDER_COLOR;
                }
            }
        }

        let mut flash = Self {
            window,
            surface,
            pixels,
            width,
            height,
            until: Instant::now() + DURATION,
        };
        flash.draw()?;
        flash.window.set_visible(true);
        Ok(flash)
    }

    pub fn window_id(&self) -> WindowId {
        self.window.id()
    }

    /// 이 시각이 지나면 창을 닫아야(값을 버려야) 한다.
    pub fn until(&self) -> Instant {
        self.until
    }

    pub fn draw(&mut self) -> Result<()> {
        self.surface
            .resize(
                NonZeroU32::new(self.width).context("이미지 너비가 0입니다")?,
                NonZeroU32::new(self.height).context("이미지 높이가 0입니다")?,
            )
            .map_err(|e| anyhow!("그리기 표면 크기 조정 실패: {e}"))?;
        let mut buffer = self
            .surface
            .buffer_mut()
            .map_err(|e| anyhow!("그리기 버퍼를 얻지 못했습니다: {e}"))?;
        if buffer.len() != self.pixels.len() {
            bail!("그리기 버퍼 크기가 이미지와 다릅니다");
        }
        buffer.copy_from_slice(&self.pixels);
        buffer
            .present()
            .map_err(|e| anyhow!("화면에 그리지 못했습니다: {e}"))
    }
}
