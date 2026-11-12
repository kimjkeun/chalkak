//! 명령줄 도구. 에이전트나 스크립트가 직접 캡쳐를 실행할 때 쓴다.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use image::RgbaImage;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::WindowId;

use capture_tool::autostart;
use capture_tool::capture::{self, Rect};
use capture_tool::config::{self, Config};
use capture_tool::output::{self, CopyMode};
use capture_tool::overlay::{Canvas, Outcome, Overlay};
use capture_tool::{capture_canvas, crop_from_canvas, ocr, state};

const USAGE: &str = "\
사용법:
  capture_tool region [옵션]     드래그로 영역을 선택해 캡쳐 (Esc/오른쪽 클릭: 취소)
  capture_tool again [옵션]      직전에 선택한 영역을 그대로 다시 캡쳐
  capture_tool full [옵션]       주 모니터 전체를 캡쳐
  capture_tool ocr <이미지 파일>  이미지의 글자를 읽어 출력
  capture_tool monitors          연결된 모니터 목록 출력
  capture_tool autostart <on|off|status>
                                 Windows 시작 시 트레이 프로그램 자동 실행
  capture_tool config            설정 파일 위치 출력
  capture_tool install           고정 위치에 설치하고 그곳의 트레이 프로그램을 실행

옵션:
  --out <파일>              저장할 파일 (png, jpg). 생략하면 설정의 저장 폴더에
                            날짜·시간 이름으로 저장하고 latest.png 도 갱신
  --copy <image|path|none>  클립보드에 올릴 것. 생략하면 설정을 따름

저장된 파일 경로를 표준 출력에 씁니다. 선택을 취소하면 아무것도 쓰지 않고
종료 코드 1로 끝납니다.

단축키와 트레이 아이콘으로 쓰려면 capture_tool_tray 를 실행하세요.";

struct Options {
    out: Option<PathBuf>,
    /// `None`이면 설정 파일의 값을 쓴다.
    copy: Option<CopyMode>,
}

fn main() -> Result<()> {
    capture_tool::make_dpi_aware();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first() else {
        println!("{USAGE}");
        return Ok(());
    };
    match command.as_str() {
        "region" => {
            let opts = parse_options(&args[1..])?;
            let Some((image, rect)) = select_region()? else {
                std::process::exit(1);
            };
            state::save_last_region(rect)?;
            finish(&image, &opts)
        }
        "again" => {
            let opts = parse_options(&args[1..])?;
            let rect = state::load_last_region()?;
            let image = crop_from_canvas(&capture_canvas()?, rect)?;
            finish(&image, &opts)
        }
        "full" => {
            let opts = parse_options(&args[1..])?;
            finish(&capture::primary_monitor()?.image, &opts)
        }
        "monitors" => {
            for (i, shot) in capture::all_monitors()?.iter().enumerate() {
                let b = shot.bounds;
                println!("{}: {}x{} @ ({}, {})", i + 1, b.width, b.height, b.x, b.y);
            }
            Ok(())
        }
        "ocr" => {
            let path = args.get(1).with_context(|| format!("이미지 파일을 지정하세요

{USAGE}"))?;
            let image = image::open(path)
                .with_context(|| format!("이미지를 열지 못했습니다: {path}"))?
                .to_rgba8();
            println!("{}", ocr::recognize(&image)?);
            Ok(())
        }
        "install" => install(),
        "autostart" => autostart_command(args.get(1).map(String::as_str)),
        "config" => {
            Config::load()?; // 없으면 기본 파일을 만든다.
            println!("{}", config::path()?.display());
            Ok(())
        }
        "-h" | "--help" => {
            println!("{USAGE}");
            Ok(())
        }
        other => bail!("알 수 없는 명령: {other}\n\n{USAGE}"),
    }
}

fn parse_options(args: &[String]) -> Result<Options> {
    let mut opts = Options { out: None, copy: None };
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        let mut value = || {
            iter.next()
                .with_context(|| format!("{flag} 뒤에 값이 필요합니다\n\n{USAGE}"))
        };
        match flag.as_str() {
            "--out" => opts.out = Some(PathBuf::from(value()?)),
            "--copy" => {
                let v = value()?;
                let mode = CopyMode::parse(v)
                    .with_context(|| format!("--copy 값은 image, path, none 중 하나여야 합니다: {v}"))?;
                opts.copy = Some(mode);
            }
            other => bail!("알 수 없는 옵션: {other}\n\n{USAGE}"),
        }
    }
    Ok(opts)
}

/// 저장하고, 클립보드에 올리고, 경로를 출력한다.
fn finish(image: &RgbaImage, opts: &Options) -> Result<()> {
    let config = Config::load()?;
    let path = match &opts.out {
        Some(path) => {
            output::save(image, path)?;
            std::path::absolute(path).unwrap_or_else(|_| path.clone())
        }
        None => output::save_to_dir(image, &config.save_dir()?)?,
    };
    output::copy(opts.copy.unwrap_or(config.copy), image, &path)?;
    println!("{}", path.display());
    Ok(())
}

/// 두 프로그램을 `%LOCALAPPDATA%\capture_tool\bin`에 복사하고 그곳의 트레이 프로그램을 실행한다.
/// 빌드 폴더를 지우거나 다시 빌드해도 쓰던 프로그램이 영향을 받지 않게 된다.
fn install() -> Result<()> {
    const FILES: [&str; 2] = ["capture_tool.exe", "capture_tool_tray.exe"];
    let source = std::env::current_exe()
        .context("실행 파일 위치를 알 수 없습니다")?
        .parent()
        .context("실행 파일의 폴더를 알 수 없습니다")?
        .to_path_buf();
    let target = directories::BaseDirs::new()
        .context("사용자 폴더를 찾지 못했습니다")?
        .data_local_dir()
        .join("capture_tool")
        .join("bin");
    std::fs::create_dir_all(&target)
        .with_context(|| format!("폴더를 만들지 못했습니다: {}", target.display()))?;
    if std::fs::canonicalize(&source)? == std::fs::canonicalize(&target)? {
        bail!("이미 설치된 위치에서 실행 중입니다. 새로 빌드한 폴더의 capture_tool 로 실행하세요.");
    }
    for name in FILES {
        if !source.join(name).exists() {
            bail!("{name} 이 {} 에 없습니다. 먼저 cargo build --release 를 실행하세요.", source.display());
        }
    }
    let had_autostart = autostart::registered()?.is_some();

    // 실행 중인 트레이 프로그램은 파일을 잡고 있으므로 먼저 끝낸다.
    let _ = std::process::Command::new("taskkill")
        .args(["/IM", "capture_tool_tray.exe", "/F"])
        .output();
    for name in FILES {
        let mut tries = 0;
        loop {
            match std::fs::copy(source.join(name), target.join(name)) {
                Ok(_) => break,
                Err(_) if tries < 10 => {
                    tries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                Err(e) => return Err(e).with_context(|| format!("{name} 을 복사하지 못했습니다")),
            }
        }
    }
    let tray = target.join("capture_tool_tray.exe");
    if had_autostart {
        autostart::enable(&tray)?;
    }
    // 이 명령의 출력 통로를 물려주지 않아야, 호출한 쪽이 트레이 프로그램이 끝나길 기다리지 않는다.
    std::process::Command::new(&tray)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("설치한 트레이 프로그램을 실행하지 못했습니다")?;
    println!("설치 위치: {}", target.display());
    println!("자동 실행: {}", if had_autostart { "설치 위치로 갱신함" } else { "꺼져 있음 (그대로 둠)" });
    println!("트레이 프로그램을 설치 위치에서 실행했습니다.");
    Ok(())
}

fn autostart_command(arg: Option<&str>) -> Result<()> {
    let exe = autostart::tray_exe()?;
    match arg {
        Some("on") => autostart::enable(&exe)?,
        Some("off") => autostart::disable()?,
        Some("status") => {}
        _ => bail!("autostart 뒤에는 on, off, status 중 하나가 와야 합니다\n\n{USAGE}"),
    }
    match autostart::registered()? {
        Some(command) if autostart::is_enabled_for(&exe)? => println!("켜짐: {command}"),
        Some(command) => println!("다른 위치의 프로그램이 등록되어 있음: {command}"),
        None => println!("꺼짐"),
    }
    Ok(())
}

/// 오버레이를 띄워 영역을 고르게 한다. 취소하면 `None`.
fn select_region() -> Result<Option<(RgbaImage, Rect)>> {
    let event_loop = EventLoop::new().context("이벤트 루프를 만들지 못했습니다")?;
    // 오버레이가 뜨기 전의 화면을 찍어 둔다.
    let canvas = capture_canvas()?;
    let mut app = RegionApp { canvas, overlay: None, result: None };
    event_loop.run_app(&mut app).context("이벤트 루프 오류")?;
    match app.result.take() {
        Some(Ok(Outcome::Selected(rect))) => Ok(Some((crop_from_canvas(&app.canvas, rect)?, rect))),
        Some(Ok(Outcome::Cancelled)) | None => Ok(None),
        Some(Err(e)) => Err(e),
    }
}

struct RegionApp {
    canvas: Canvas,
    overlay: Option<Overlay>,
    result: Option<Result<Outcome>>,
}

impl RegionApp {
    fn end(&mut self, event_loop: &ActiveEventLoop, result: Result<Outcome>) {
        self.result = Some(result);
        self.overlay = None;
        event_loop.exit();
    }
}

impl ApplicationHandler for RegionApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.overlay.is_some() || self.result.is_some() {
            return;
        }
        match Overlay::open(event_loop, &self.canvas) {
            Ok(overlay) => self.overlay = Some(overlay),
            Err(e) => self.end(event_loop, Err(e)),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(overlay) = self.overlay.as_mut().filter(|o| o.window_id() == id) else {
            return;
        };
        match overlay.handle(event) {
            Ok(None) => {}
            Ok(Some(outcome)) => self.end(event_loop, Ok(outcome)),
            Err(e) => self.end(event_loop, Err(e)),
        }
    }
}
