//! 명령줄 도구(`capture_tool`)와 트레이 상주 프로그램(`capture_tool_tray`)이 함께 쓰는 코드.

pub mod autostart;
pub mod capture;
pub mod config;
pub mod flash;
pub mod history;
pub mod history_window;
pub mod output;
pub mod overlay;
pub mod state;

use anyhow::Result;
use image::RgbaImage;

use capture::Rect;
use overlay::Canvas;

/// 모니터 좌표와 창 좌표가 모두 물리 픽셀로 나오도록 한다. 프로그램 시작 시 한 번 부른다.
pub fn make_dpi_aware() {
    use windows_sys::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    // SAFETY: 인자 없는 단순 Win32 호출. 실패해도(이미 설정된 경우 등) 무시해도 된다.
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// 모든 모니터를 찍어 한 장으로 합친다.
pub fn capture_canvas() -> Result<Canvas> {
    Canvas::from_shots(&capture::all_monitors()?)
}

/// 가상 데스크톱 좌표의 사각형을 캔버스에서 잘라낸다.
pub fn crop_from_canvas(canvas: &Canvas, rect: Rect) -> Result<RgbaImage> {
    capture::crop(
        &canvas.image,
        Rect {
            x: rect.x - canvas.bounds.x,
            y: rect.y - canvas.bounds.y,
            ..rect
        },
    )
}
