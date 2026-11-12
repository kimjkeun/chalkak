//! 화면 캡쳐. UI와 무관하게 동작해서 단축키·오버레이·CLI가 모두 이 모듈을 쓴다.
//! 좌표는 전부 물리 픽셀 기준이다.

use anyhow::{Context, Result, bail};
use image::RgbaImage;
use xcap::Monitor;

/// 가상 데스크톱 위의 사각형 (물리 픽셀).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// 모니터 한 대를 찍은 결과.
pub struct Shot {
    pub image: RgbaImage,
    /// 이 이미지가 가상 데스크톱에서 차지하는 위치.
    pub bounds: Rect,
}

/// 주 모니터 전체를 캡쳐한다.
pub fn primary_monitor() -> Result<Shot> {
    let monitors = Monitor::all().context("모니터 목록을 가져오지 못했습니다")?;
    let monitor = monitors
        .iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .or(monitors.first());
    let Some(monitor) = monitor else {
        bail!("연결된 모니터가 없습니다");
    };
    shoot(monitor)
}

/// 연결된 모든 모니터를 각각 캡쳐한다.
pub fn all_monitors() -> Result<Vec<Shot>> {
    Monitor::all()
        .context("모니터 목록을 가져오지 못했습니다")?
        .iter()
        .map(shoot)
        .collect()
}

fn shoot(monitor: &Monitor) -> Result<Shot> {
    let image = monitor
        .capture_image()
        .context("화면을 캡쳐하지 못했습니다")?;
    let bounds = Rect {
        x: monitor.x()?,
        y: monitor.y()?,
        width: image.width(),
        height: image.height(),
    };
    Ok(Shot { image, bounds })
}

/// 캡쳐한 이미지에서 사각형 영역을 잘라낸다. `area`는 이미지 안쪽 좌표이며,
/// 이미지 밖으로 나간 부분은 잘려 나간다.
pub fn crop(image: &RgbaImage, area: Rect) -> Result<RgbaImage> {
    let x0 = area.x.max(0) as u32;
    let y0 = area.y.max(0) as u32;
    let x1 = (area.x.saturating_add_unsigned(area.width).max(0) as u32).min(image.width());
    let y1 = (area.y.saturating_add_unsigned(area.height).max(0) as u32).min(image.height());
    if x1 <= x0 || y1 <= y0 {
        bail!("선택한 영역이 화면 밖이거나 크기가 0입니다");
    }
    Ok(image::imageops::crop_imm(image, x0, y0, x1 - x0, y1 - y0).to_image())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, width: u32, height: u32) -> Rect {
        Rect { x, y, width, height }
    }

    #[test]
    fn crop_inside() {
        let img = RgbaImage::new(100, 50);
        let out = crop(&img, rect(10, 5, 30, 20)).unwrap();
        assert_eq!((out.width(), out.height()), (30, 20));
    }

    #[test]
    fn crop_clamps_to_image() {
        let img = RgbaImage::new(100, 50);
        let out = crop(&img, rect(-10, 40, 50, 100)).unwrap();
        assert_eq!((out.width(), out.height()), (40, 10));
    }

    #[test]
    fn crop_outside_fails() {
        let img = RgbaImage::new(100, 50);
        assert!(crop(&img, rect(200, 0, 10, 10)).is_err());
        assert!(crop(&img, rect(0, 0, 0, 10)).is_err());
    }
}
