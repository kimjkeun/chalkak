//! 이미지에서 글자 추출. Windows에 들어 있는 OCR 기능을 불러 쓰므로
//! 프로그램에 인식 모델을 넣지 않는다.

use anyhow::{Context, Result, anyhow, bail};
use image::RgbaImage;
use image::imageops::FilterType;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;

/// 화면 글자는 작아서, 키워서 넘기면 더 잘 읽는다.
const UPSCALE: u32 = 2;

/// 이미지의 글자를 줄 단위로 읽어 돌려준다. 글자가 없으면 빈 문자열.
///
/// Windows OCR은 창 이벤트를 처리하는 스레드에서 기다리면 멈출 수 있어서
/// 별도 스레드에서 실행한다.
pub fn recognize(image: &RgbaImage) -> Result<String> {
    let image = image.clone();
    std::thread::spawn(move || recognize_blocking(&image))
        .join()
        .map_err(|_| anyhow!("글자 추출 중 오류가 발생했습니다"))?
}

fn recognize_blocking(image: &RgbaImage) -> Result<String> {
    let engine = OcrEngine::TryCreateFromUserProfileLanguages()
        .context("Windows 글자 인식 기능을 쓸 수 없습니다 (설정 > 시간 및 언어에서 언어 팩의 광학 문자 인식을 설치해야 합니다)")?;
    let max = OcrEngine::MaxImageDimension().context("글자 인식 한계를 알 수 없습니다")?;
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        bail!("빈 이미지입니다");
    }

    // 한계 안에서 키우고, 원본이 한계를 넘으면 줄인다.
    let longest = width.max(height);
    let scaled;
    let image = if longest * UPSCALE <= max {
        scaled = image::imageops::resize(image, width * UPSCALE, height * UPSCALE, FilterType::CatmullRom);
        &scaled
    } else if longest > max {
        let ratio = max as f64 / longest as f64;
        let dim = |v: u32| ((v as f64 * ratio) as u32).max(1);
        scaled = image::imageops::resize(image, dim(width), dim(height), FilterType::Triangle);
        &scaled
    } else {
        image
    };

    // RGBA를 Windows가 받는 BGRA로 바꾼다.
    let mut bgra = image.as_raw().clone();
    for pixel in bgra.chunks_exact_mut(4) {
        pixel.swap(0, 2);
        pixel[3] = 255;
    }
    let writer = DataWriter::new()?;
    writer.WriteBytes(&bgra)?;
    let bitmap = SoftwareBitmap::CreateCopyFromBuffer(
        &writer.DetachBuffer()?,
        BitmapPixelFormat::Bgra8,
        image.width() as i32,
        image.height() as i32,
    )
    .context("이미지를 글자 인식용으로 바꾸지 못했습니다")?;

    let result = engine
        .RecognizeAsync(&bitmap)
        .and_then(|operation| operation.join())
        .context("글자를 인식하지 못했습니다")?;
    let mut lines = Vec::new();
    for line in result.Lines()? {
        lines.push(line.Text()?.to_string());
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotate::{Annotation, Font, apply};

    #[test]
    fn reads_rendered_text() {
        let font = Font::load().unwrap();
        let white = RgbaImage::from_pixel(520, 70, image::Rgba([255, 255, 255, 255]));
        let text = Annotation::Text {
            x: 16.0,
            y: 18.0,
            size: 26.0,
            text: "화면 캡쳐 도구 2026".into(),
            color: [0, 0, 0],
        };
        let found = recognize(&apply(&white, &[text], &font)).unwrap();
        // 설치된 인식 언어에 따라 일부 글자는 달라질 수 있어서 또렷한 부분만 확인한다.
        assert!(found.contains("화면") && found.contains("2026"), "읽은 결과: {found:?}");
    }

    #[test]
    fn blank_image_has_no_text() {
        let white = RgbaImage::from_pixel(200, 80, image::Rgba([255, 255, 255, 255]));
        assert_eq!(recognize(&white).unwrap(), "");
    }
}
