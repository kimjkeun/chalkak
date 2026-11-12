//! 캡쳐 결과 내보내기: 파일 저장과 클립보드 복사.

use std::borrow::Cow;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use image::RgbaImage;

/// 에이전트가 "최신 캡쳐"를 바로 읽을 수 있도록 항상 덮어쓰는 고정 파일 이름.
pub const LATEST_NAME: &str = "latest.png";

/// 기본 저장 폴더: `사진\Captures`.
pub fn default_dir() -> Result<PathBuf> {
    let dirs = directories::UserDirs::new().context("사용자 폴더를 찾지 못했습니다")?;
    let pictures = dirs
        .picture_dir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| dirs.home_dir().join("Pictures"));
    Ok(pictures.join("Captures"))
}

/// 날짜·시간 이름으로 `dir`에 저장하고, 같은 내용을 `latest.png`로도 남긴다.
/// 저장한 파일의 경로를 돌려준다.
pub fn save_to_dir(image: &RgbaImage, dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(dir).with_context(|| format!("폴더를 만들지 못했습니다: {}", dir.display()))?;
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S_%3f");
    let path = dir.join(format!("capture_{stamp}.png"));
    save(image, &path)?;
    fs::copy(&path, dir.join(LATEST_NAME)).context("latest.png를 갱신하지 못했습니다")?;
    Ok(path)
}

/// 지정한 경로에 저장한다. 형식은 확장자(png, jpg)로 정해진다.
pub fn save(image: &RgbaImage, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .with_context(|| format!("폴더를 만들지 못했습니다: {}", parent.display()))?;
    }
    let is_jpeg = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg"));
    let result = if is_jpeg {
        // JPEG는 투명도를 지원하지 않는다.
        image::DynamicImage::ImageRgba8(image.clone()).to_rgb8().save(path)
    } else {
        image.save(path)
    };
    result.with_context(|| format!("저장하지 못했습니다: {}", path.display()))
}

/// 클립보드에 무엇을 올릴지.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CopyMode {
    /// 이미지 자체 (채팅 창 등에 바로 붙여넣기).
    Image,
    /// 저장된 파일의 경로 (터미널 에이전트에 붙여넣기).
    Path,
    None,
}

impl CopyMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "image" => Some(Self::Image),
            "path" => Some(Self::Path),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

/// 파일을 휴지통으로 보낸다 (탐색기에서 되살릴 수 있다).
pub fn move_to_recycle_bin(path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::{
        FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, SHFILEOPSTRUCTW,
        SHFileOperationW,
    };
    let absolute = std::path::absolute(path)
        .with_context(|| format!("경로를 알 수 없습니다: {}", path.display()))?;
    // 경로 목록 형식이라 0을 두 번 붙여 끝낸다.
    let from: Vec<u16> = absolute.as_os_str().encode_wide().chain([0, 0]).collect();
    let mut operation = SHFILEOPSTRUCTW {
        hwnd: std::ptr::null_mut(),
        wFunc: FO_DELETE,
        pFrom: from.as_ptr(),
        pTo: std::ptr::null(),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT) as u16,
        fAnyOperationsAborted: 0,
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: std::ptr::null(),
    };
    // SAFETY: 구조체와 그 안의 경로 버퍼는 호출이 끝날 때까지 살아 있다.
    let code = unsafe { SHFileOperationW(&mut operation) };
    if code != 0 || operation.fAnyOperationsAborted != 0 || absolute.exists() {
        anyhow::bail!("휴지통으로 보내지 못했습니다: {}", absolute.display());
    }
    Ok(())
}

/// 글자를 클립보드에 올린다.
pub fn copy_text(text: &str) -> Result<()> {
    arboard::Clipboard::new()
        .context("클립보드를 열지 못했습니다")?
        .set_text(text)
        .context("클립보드에 복사하지 못했습니다")
}

/// 파일 경로만 클립보드에 올린다. 이미지를 읽을 필요가 없을 때 쓴다.
pub fn copy_path(path: &Path) -> Result<()> {
    arboard::Clipboard::new()
        .context("클립보드를 열지 못했습니다")?
        .set_text(path.display().to_string())
        .context("클립보드에 복사하지 못했습니다")
}

pub fn copy(mode: CopyMode, image: &RgbaImage, path: &Path) -> Result<()> {
    if mode == CopyMode::None {
        return Ok(());
    }
    let mut clipboard = arboard::Clipboard::new().context("클립보드를 열지 못했습니다")?;
    match mode {
        CopyMode::Image => clipboard.set_image(arboard::ImageData {
            width: image.width() as usize,
            height: image.height() as usize,
            bytes: Cow::Borrowed(image.as_raw()),
        }),
        CopyMode::Path => clipboard.set_text(path.display().to_string()),
        CopyMode::None => unreachable!(),
    }
    .context("클립보드에 복사하지 못했습니다")
}
