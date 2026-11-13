//! 캡쳐 효과음. 소리 파일을 따로 두지 않고 "찰칵" 소리를 계산해서 만든다.

use std::sync::OnceLock;

const SAMPLE_RATE: u32 = 44_100;
/// 전체 길이.
const LENGTH_MS: u32 = 190;
/// 가장 큰 순간의 크기 (1.0이 최대).
const VOLUME: f32 = 0.32;

/// 셔터 소리를 이루는 딸깍 소리 하나.
struct Click {
    /// 시작 시각(초).
    start: f32,
    /// 소리가 줄어드는 빠르기(초). 작을수록 짧고 날카롭다.
    decay: f32,
    gain: f32,
    /// 잡음을 얼마나 부드럽게 할지 (0: 날카로움, 1에 가까울수록 둔탁함).
    smooth: f32,
    /// 울림의 높이(Hz).
    tone: f32,
}

/// "찰"(짧고 날카롭게)과 "칵"(조금 길고 낮게).
const CLICKS: [Click; 2] = [
    Click { start: 0.004, decay: 0.004, gain: 0.75, smooth: 0.15, tone: 3200.0 },
    Click { start: 0.072, decay: 0.011, gain: 1.0, smooth: 0.55, tone: 1500.0 },
];

/// 셔터 소리의 파형 (-1.0 ~ 1.0).
fn shutter_samples() -> Vec<f32> {
    let count = (SAMPLE_RATE * LENGTH_MS / 1000) as usize;
    let mut samples = vec![0.0f32; count];
    // 실행할 때마다 같은 소리가 나도록 고정된 값에서 시작하는 난수.
    let mut seed: u32 = 0x1234_5678;
    let mut noise = move || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 8) as f32 / (1u32 << 23) as f32 - 1.0
    };
    for click in &CLICKS {
        let first = (click.start * SAMPLE_RATE as f32) as usize;
        let mut smoothed = 0.0f32;
        for (i, sample) in samples.iter_mut().enumerate().skip(first) {
            let t = (i - first) as f32 / SAMPLE_RATE as f32;
            let envelope = (-t / click.decay).exp();
            if envelope < 0.001 {
                break;
            }
            smoothed = smoothed * click.smooth + noise() * (1.0 - click.smooth);
            let ring = (std::f32::consts::TAU * click.tone * t).sin() * 0.35;
            *sample += (smoothed + ring) * envelope * click.gain;
        }
    }
    let peak = samples.iter().fold(0.0f32, |max, s| max.max(s.abs())).max(f32::EPSILON);
    for sample in &mut samples {
        *sample = *sample / peak * VOLUME;
    }
    samples
}

/// 셔터 소리를 WAV 파일 형식(16비트, 모노)으로 만든다.
fn shutter_wav() -> Vec<u8> {
    let samples = shutter_samples();
    let data_len = (samples.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // fmt 덩어리 크기
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // 모노
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // 초당 바이트
    wav.extend_from_slice(&2u16.to_le_bytes()); // 샘플 하나의 바이트
    wav.extend_from_slice(&16u16.to_le_bytes()); // 비트
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&((sample * i16::MAX as f32) as i16).to_le_bytes());
    }
    wav
}

/// 셔터 소리를 낸다. 소리가 끝나길 기다리지 않고 바로 돌아온다.
/// 소리 장치가 없는 등 재생하지 못해도 캡쳐에는 지장이 없으므로 조용히 넘어간다.
pub fn play_shutter() {
    use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
    // 재생이 끝날 때까지 메모리가 살아 있어야 해서 프로그램이 끝날 때까지 들고 있는다.
    static WAV: OnceLock<Vec<u8>> = OnceLock::new();
    let wav = WAV.get_or_init(shutter_wav);
    // SAFETY: SND_MEMORY일 때 첫 인자는 WAV 데이터의 주소이고, 그 데이터는 static이라 계속 유효하다.
    unsafe {
        PlaySoundW(wav.as_ptr().cast(), std::ptr::null_mut(), SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_matches_data() {
        let wav = shutter_wav();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        assert_eq!(&wav[36..40], b"data");
        let riff_len = u32::from_le_bytes(wav[4..8].try_into().unwrap()) as usize;
        let data_len = u32::from_le_bytes(wav[40..44].try_into().unwrap()) as usize;
        assert_eq!(riff_len, wav.len() - 8);
        assert_eq!(data_len, wav.len() - 44);
        assert_eq!(data_len, (SAMPLE_RATE * LENGTH_MS / 1000) as usize * 2);
    }

    #[test]
    fn samples_are_audible_and_end_quietly() {
        let samples = shutter_samples();
        let peak = samples.iter().fold(0.0f32, |max, s| max.max(s.abs()));
        assert!((peak - VOLUME).abs() < 0.001, "최대 크기: {peak}");
        // 끝이 뚝 끊겨 틱 소리가 나지 않도록 마지막은 거의 무음이어야 한다.
        let tail = &samples[samples.len() - 200..];
        assert!(tail.iter().all(|s| s.abs() < 0.01));
    }

    #[test]
    fn both_clicks_are_present() {
        let samples = shutter_samples();
        let loudest = |from_ms: u32, to_ms: u32| {
            let at = |ms: u32| (SAMPLE_RATE * ms / 1000) as usize;
            samples[at(from_ms)..at(to_ms)].iter().fold(0.0f32, |max, s| max.max(s.abs()))
        };
        assert!(loudest(0, 30) > 0.1);
        assert!(loudest(70, 110) > 0.1);
        // 두 소리 사이에는 틈이 있다.
        assert!(loudest(45, 70) < 0.02);
    }
}
