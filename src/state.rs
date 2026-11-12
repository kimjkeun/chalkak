//! 실행 사이에 기억해야 하는 값. 지금은 "직전에 선택한 영역"뿐이다.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::capture::Rect;

fn last_region_path() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "capture_tool")
        .context("프로그램 데이터 폴더를 찾지 못했습니다")?;
    Ok(dirs.data_local_dir().join("last_region.txt"))
}

pub fn save_last_region(rect: Rect) -> Result<()> {
    let path = last_region_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, format_rect(rect))
        .with_context(|| format!("직전 영역을 기록하지 못했습니다: {}", path.display()))
}

pub fn load_last_region() -> Result<Rect> {
    let path = last_region_path()?;
    let text = fs::read_to_string(&path)
        .context("직전에 선택한 영역이 없습니다. 먼저 `region`으로 한 번 캡쳐하세요")?;
    parse_rect(&text)
}

fn format_rect(r: Rect) -> String {
    format!("{} {} {} {}", r.x, r.y, r.width, r.height)
}

fn parse_rect(text: &str) -> Result<Rect> {
    let parts: Vec<&str> = text.split_whitespace().collect();
    let [x, y, w, h] = parts.as_slice() else {
        bail!("직전 영역 기록이 손상되었습니다");
    };
    Ok(Rect {
        x: x.parse()?,
        y: y.parse()?,
        width: w.parse()?,
        height: h.parse()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_round_trip() {
        let r = Rect { x: -1920, y: -843, width: 300, height: 200 };
        assert_eq!(parse_rect(&format_rect(r)).unwrap(), r);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_rect("1 2 3").is_err());
        assert!(parse_rect("a b c d").is_err());
    }
}
