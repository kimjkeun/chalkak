//! 캡쳐 히스토리. 저장 폴더의 `capture_*.png` 파일이 곧 히스토리라서,
//! 프로그램을 껐다 켜도 이전 캡쳐를 다시 고를 수 있다.

use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use image::RgbaImage;

/// 히스토리 창에 보여 줄 최대 개수.
pub const MAX_ENTRIES: usize = 12;

/// 메모리에 들고 있는 썸네일의 최대 크기. 창에 그릴 때 한 번 더 줄인다.
const THUMB_MAX: (u32, u32) = (480, 320);

/// `dir` 안의 캡쳐 파일을 최신 순으로 `limit`개까지 돌려준다.
pub fn recent(dir: &Path, limit: usize) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(e).with_context(|| format!("폴더를 읽지 못했습니다: {}", dir.display()));
        }
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| is_capture_file(p))
        .collect();
    // 파일 이름이 날짜·시간이라 이름 순서가 곧 시간 순서다.
    files.sort();
    files.reverse();
    files.truncate(limit);
    Ok(files)
}

fn is_capture_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name.starts_with("capture_") && name.to_ascii_lowercase().ends_with(".png")
}

/// 비율을 유지하면서 `max` 안에 들어가는 크기. 확대는 하지 않는다.
pub fn fit(size: (u32, u32), max: (u32, u32)) -> (u32, u32) {
    let scale = (max.0 as f64 / size.0 as f64)
        .min(max.1 as f64 / size.1 as f64)
        .min(1.0);
    let dim = |v: u32| ((v as f64 * scale).round() as u32).max(1);
    (dim(size.0), dim(size.1))
}

/// 파일별 썸네일. 캡쳐할 때 바로 넣어 두고, 없는 것은 처음 필요할 때 파일에서 만든다.
#[derive(Default)]
pub struct ThumbCache {
    thumbs: HashMap<PathBuf, RgbaImage>,
}

impl ThumbCache {
    pub fn insert(&mut self, path: PathBuf, image: &RgbaImage) {
        let (w, h) = fit(image.dimensions(), THUMB_MAX);
        self.thumbs.insert(path, image::imageops::thumbnail(image, w, h));
    }

    pub fn get(&mut self, path: &Path) -> Result<&RgbaImage> {
        if !self.thumbs.contains_key(path) {
            let image = image::open(path)
                .with_context(|| format!("이미지를 열지 못했습니다: {}", path.display()))?
                .to_rgba8();
            self.insert(path.to_path_buf(), &image);
        }
        Ok(&self.thumbs[path])
    }

    /// 이미 만들어 둔 썸네일만 돌려준다. 파일을 읽지 않는다.
    pub fn cached(&self, path: &Path) -> Option<&RgbaImage> {
        self.thumbs.get(path)
    }

    /// 더 이상 히스토리에 없는 파일의 썸네일을 버린다.
    pub fn retain(&mut self, keep: &[PathBuf]) {
        self.thumbs.retain(|path, _| keep.contains(path));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_lists_newest_first_and_ignores_other_files() {
        let dir = std::env::temp_dir().join(format!("capture_tool_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for name in [
            "capture_20261112_100000_000.png",
            "capture_20261112_100500_000.png",
            "capture_20261111_235959_999.png",
            "latest.png",
            "notes.txt",
        ] {
            fs::write(dir.join(name), b"x").unwrap();
        }

        let names = |limit| -> Vec<String> {
            recent(&dir, limit)
                .unwrap()
                .iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(
            names(10),
            [
                "capture_20261112_100500_000.png",
                "capture_20261112_100000_000.png",
                "capture_20261111_235959_999.png",
            ]
        );
        assert_eq!(names(1), ["capture_20261112_100500_000.png"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn recent_on_missing_dir_is_empty() {
        let dir = std::env::temp_dir().join("capture_tool_test_does_not_exist");
        assert!(recent(&dir, 5).unwrap().is_empty());
    }

    #[test]
    fn fit_keeps_ratio_and_never_enlarges() {
        assert_eq!(fit((1920, 1080), (480, 320)), (480, 270));
        assert_eq!(fit((1080, 1920), (480, 320)), (180, 320));
        assert_eq!(fit((100, 50), (480, 320)), (100, 50));
        assert_eq!(fit((5000, 1), (480, 320)), (480, 1));
    }
}
