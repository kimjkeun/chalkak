//! 트레이 상주 프로그램. 전역 단축키와 트레이 메뉴로 캡쳐한다.
#![windows_subsystem = "windows"]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use image::RgbaImage;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::monitor::MonitorHandle;
use winit::window::WindowId;

use capture_tool::config::{self, Config};
use capture_tool::flash::Flash;
use capture_tool::history::{self, ThumbCache};
use capture_tool::history_window::{HistoryOutcome, HistoryWindow};
use capture_tool::output::{self, CopyMode};
use capture_tool::overlay::{Canvas, Outcome, Overlay};
use capture_tool::{autostart, capture, capture_canvas, crop_from_canvas, state};

const APP_NAME: &str = "capture_tool";

/// 메뉴가 닫히는 애니메이션이 캡쳐에 찍히지 않도록 기다리는 시간.
const MENU_CLOSE_DELAY: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Region,
    Again,
    Full,
    History,
    OpenFolder,
    OpenConfig,
    ReloadConfig,
    Autostart,
    Quit,
}

impl Action {
    const ALL: [Action; 9] = [
        Action::Region,
        Action::Again,
        Action::Full,
        Action::History,
        Action::OpenFolder,
        Action::OpenConfig,
        Action::ReloadConfig,
        Action::Autostart,
        Action::Quit,
    ];

    fn id(self) -> &'static str {
        match self {
            Action::Region => "region",
            Action::Again => "again",
            Action::Full => "full",
            Action::History => "history",
            Action::OpenFolder => "open_folder",
            Action::OpenConfig => "open_config",
            Action::ReloadConfig => "reload_config",
            Action::Autostart => "autostart",
            Action::Quit => "quit",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.id() == id)
    }

    fn text(self) -> &'static str {
        match self {
            Action::Region => "영역 캡쳐",
            Action::Again => "같은 영역 다시 캡쳐",
            Action::Full => "주 모니터 전체 캡쳐",
            Action::History => "캡쳐 히스토리",
            Action::OpenFolder => "저장 폴더 열기",
            Action::OpenConfig => "설정 파일 열기",
            Action::ReloadConfig => "설정 다시 불러오기",
            Action::Autostart => "Windows 시작 시 자동 실행",
            Action::Quit => "종료",
        }
    }

    /// 설정 파일에 적힌 이 동작의 단축키. 단축키가 없는 동작이면 `None`.
    fn hotkey_text(self, config: &Config) -> Option<&str> {
        let text = match self {
            Action::Region => &config.hotkeys.region,
            Action::Again => &config.hotkeys.again,
            Action::History => &config.hotkeys.history,
            _ => return None,
        };
        Some(text.trim()).filter(|t| !t.is_empty())
    }

    fn label(self, config: &Config) -> String {
        match self.hotkey_text(config) {
            Some(keys) => format!("{}\t{keys}", self.text()),
            None => self.text().to_string(),
        }
    }

    /// 화면을 찍는 동작인지 (메뉴에서 골랐을 때 메뉴가 사라지길 기다려야 한다).
    fn captures_screen(self) -> bool {
        matches!(self, Action::Region | Action::Again | Action::Full)
    }
}

#[derive(Debug)]
enum AppEvent {
    /// 단축키나 트레이 왼쪽 클릭: 바로 실행.
    Run(Action),
    /// 트레이 메뉴에서 고름: 메뉴가 사라진 뒤 실행.
    RunFromMenu(Action),
}

/// 등록된 단축키 ID와 동작의 대응표. 단축키 이벤트 처리기와 함께 쓴다.
type HotkeyTable = Arc<Mutex<Vec<(u32, Action)>>>;

fn main() {
    capture_tool::make_dpi_aware();
    if let Err(e) = run() {
        show_error(&format!("{e:#}"));
    }
}

fn run() -> Result<()> {
    if already_running() {
        show_message(&format!("{APP_NAME}이 이미 실행 중입니다."), false);
        return Ok(());
    }
    let event_loop = EventLoop::<AppEvent>::with_user_event()
        .build()
        .context("이벤트 루프를 만들지 못했습니다")?;

    let config = Config::load().unwrap_or_else(|e| {
        show_error(&format!("{e:#}\n\n기본 설정으로 실행합니다."));
        Config::default()
    });

    let hotkey_table: HotkeyTable = Arc::default();
    let proxy = event_loop.create_proxy();
    let table = hotkey_table.clone();
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        if event.state != HotKeyState::Pressed {
            return;
        }
        let action = table
            .lock()
            .ok()
            .and_then(|t| t.iter().find(|(id, _)| *id == event.id).map(|(_, a)| *a));
        if let Some(action) = action {
            let _ = proxy.send_event(AppEvent::Run(action));
        }
    }));

    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if let Some(action) = Action::from_id(event.id.0.as_str()) {
            let _ = proxy.send_event(AppEvent::RunFromMenu(action));
        }
    }));

    let proxy = event_loop.create_proxy();
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            let _ = proxy.send_event(AppEvent::Run(Action::Region));
        }
    }));

    let mut app = App {
        config,
        hotkey_manager: GlobalHotKeyManager::new().context("단축키 관리자를 만들지 못했습니다")?,
        hotkey_table,
        registered: Vec::new(),
        tray: None,
        autostart_item: None,
        selecting: None,
        flash: None,
        history: None,
        thumbs: ThumbCache::default(),
    };
    app.apply_hotkeys();
    event_loop.run_app(&mut app).context("이벤트 루프 오류")
}

struct App {
    config: Config,
    hotkey_manager: GlobalHotKeyManager,
    hotkey_table: HotkeyTable,
    /// 지금 등록되어 있는 단축키 (설정을 다시 읽을 때 해제한다).
    registered: Vec<HotKey>,
    tray: Option<TrayIcon>,
    autostart_item: Option<CheckMenuItem>,
    /// 영역 선택 중일 때의 오버레이와 그 바탕 화면.
    selecting: Option<(Overlay, Canvas)>,
    /// 캡쳐 직후 잠깐 떠 있는 번쩍임.
    flash: Option<Flash>,
    /// 열려 있는 히스토리 창과, 거기 보이는 파일들(보이는 순서대로).
    history: Option<(HistoryWindow, Vec<PathBuf>)>,
    thumbs: ThumbCache,
}

impl App {
    /// 설정에 적힌 단축키를 등록한다. 등록하지 못한 것이 있으면 알린다.
    fn apply_hotkeys(&mut self) {
        for hotkey in self.registered.drain(..) {
            let _ = self.hotkey_manager.unregister(hotkey);
        }
        let mut table = Vec::new();
        let mut problems = Vec::new();
        for action in Action::ALL {
            let Some(text) = action.hotkey_text(&self.config) else {
                continue;
            };
            let hotkey = match text.parse::<HotKey>() {
                Ok(hotkey) => hotkey,
                Err(_) => {
                    problems.push(format!("{} ({text}): 단축키 표기를 이해하지 못했습니다", action.text()));
                    continue;
                }
            };
            match self.hotkey_manager.register(hotkey) {
                Ok(()) => {
                    self.registered.push(hotkey);
                    table.push((hotkey.id(), action));
                }
                Err(_) => problems.push(format!(
                    "{} ({text}): 다른 프로그램이 쓰고 있거나 중복된 단축키입니다",
                    action.text()
                )),
            }
        }
        if let Ok(mut shared) = self.hotkey_table.lock() {
            *shared = table;
        }
        if !problems.is_empty() {
            show_error(&format!(
                "다음 단축키를 등록하지 못했습니다.\n\n{}\n\n트레이 메뉴로는 계속 쓸 수 있습니다.",
                problems.join("\n")
            ));
        }
    }

    fn run_action(&mut self, event_loop: &ActiveEventLoop, action: Action) {
        // 영역 선택 중에는 종료 외의 다른 동작을 받지 않는다.
        if self.selecting.is_some() && action != Action::Quit {
            return;
        }
        // 히스토리 창이 열려 있으면 닫는다. 히스토리 단축키를 다시 누른 경우는 닫기만 한다.
        if self.history.take().is_some() && action == Action::History {
            return;
        }
        // 이전 번쩍임이 새 캡쳐에 찍히지 않게 먼저 닫는다.
        self.flash = None;
        let result = match action {
            Action::Region => self.start_region(event_loop),
            Action::Again => state::load_last_region().and_then(|rect| {
                let canvas = capture_canvas()?;
                let image = crop_from_canvas(&canvas, rect)?;
                self.finish(&image)?;
                // 화면 밖으로 나간 부분은 잘렸으므로 번쩍임도 화면 안쪽에서 시작한다.
                let (x, y) = (rect.x.max(canvas.bounds.x), rect.y.max(canvas.bounds.y));
                self.show_flash(event_loop, x, y, &image)
            }),
            Action::Full => capture::primary_monitor().and_then(|shot| {
                self.finish(&shot.image)?;
                self.show_flash(event_loop, shot.bounds.x, shot.bounds.y, &shot.image)
            }),
            Action::History => self.open_history(event_loop),
            Action::OpenFolder => self.config.save_dir().and_then(|dir| {
                std::fs::create_dir_all(&dir)?;
                open_with("explorer.exe", &dir)
            }),
            Action::OpenConfig => config::path().and_then(|path| open_with("notepad.exe", &path)),
            Action::ReloadConfig => self.reload_config(),
            Action::Autostart => self.toggle_autostart(),
            Action::Quit => {
                self.selecting = None;
                event_loop.exit();
                Ok(())
            }
        };
        if let Err(e) = result {
            show_error(&format!("{e:#}"));
        }
    }

    fn reload_config(&mut self) -> Result<()> {
        // 파일에 오류가 있으면 지금 설정을 그대로 둔다.
        self.config = Config::load()?;
        self.apply_hotkeys();
        if let Some(tray) = &self.tray {
            let (menu, autostart_item) = build_menu(&self.config)?;
            tray.set_menu(Some(Box::new(menu)));
            self.autostart_item = Some(autostart_item);
        }
        Ok(())
    }

    fn toggle_autostart(&mut self) -> Result<()> {
        let exe = autostart::tray_exe()?;
        let result = if autostart::is_enabled_for(&exe)? {
            autostart::disable()
        } else {
            autostart::enable(&exe)
        };
        // 메뉴의 체크 표시는 실제 등록 상태를 따른다.
        if let Some(item) = &self.autostart_item {
            item.set_checked(autostart::is_enabled_for(&exe).unwrap_or(false));
        }
        result
    }

    fn show_flash(&mut self, event_loop: &ActiveEventLoop, x: i32, y: i32, image: &RgbaImage) -> Result<()> {
        self.flash = Some(Flash::show(event_loop, x, y, image)?);
        Ok(())
    }

    fn start_region(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        let canvas = capture_canvas()?;
        let overlay = Overlay::open(event_loop, &canvas)?;
        self.selecting = Some((overlay, canvas));
        Ok(())
    }

    /// 저장하고 클립보드에 올린 뒤, 트레이 도움말에 마지막 파일 이름을 보여 준다.
    fn finish(&mut self, image: &RgbaImage) -> Result<()> {
        let path = output::save_to_dir(image, &self.config.save_dir()?)?;
        output::copy(self.config.copy, image, &path)?;
        if let (Some(tray), Some(name)) = (&self.tray, path.file_name()) {
            let _ = tray.set_tooltip(Some(format!(
                "{APP_NAME}\n마지막 캡쳐: {}",
                name.to_string_lossy()
            )));
        }
        self.thumbs.insert(path, image);
        Ok(())
    }

    fn open_history(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        let files = history::recent(&self.config.save_dir()?, self.config.history_size)?;
        self.thumbs.retain(&files);
        // 열 수 없는 파일(손상 등)은 목록에서 뺀다.
        let files: Vec<PathBuf> = files
            .into_iter()
            .filter(|path| self.thumbs.get(path).is_ok())
            .collect();
        if files.is_empty() {
            show_message("아직 캡쳐한 것이 없습니다.", false);
            return Ok(());
        }
        let thumbs: Vec<&RgbaImage> = files
            .iter()
            .filter_map(|path| self.thumbs.cached(path))
            .collect();
        let monitor = cursor_monitor(event_loop).context("모니터를 찾지 못했습니다")?;
        let window = HistoryWindow::open(
            event_loop,
            &thumbs,
            (monitor.position(), monitor.size()),
            monitor.scale_factor(),
        )?;
        self.history = Some((window, files));
        Ok(())
    }

    /// 히스토리에서 고른 캡쳐를 다시 클립보드에 올린다.
    fn copy_from_history(&mut self, path: &Path, as_path: bool) -> Result<()> {
        if as_path {
            return output::copy_path(path);
        }
        let image = image::open(path)
            .with_context(|| format!("이미지를 열지 못했습니다: {}", path.display()))?
            .to_rgba8();
        output::copy(CopyMode::Image, &image, path)
    }
}

impl ApplicationHandler<AppEvent> for App {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        // 트레이 아이콘은 이벤트 루프가 돌기 시작한 뒤에 만들어야 한다.
        if cause == StartCause::Init {
            match build_tray(&self.config) {
                Ok((tray, autostart_item)) => {
                    self.tray = Some(tray);
                    self.autostart_item = Some(autostart_item);
                }
                Err(e) => {
                    show_error(&format!("{e:#}"));
                    event_loop.exit();
                }
            }
        }
    }

    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: AppEvent) {
        match event {
            AppEvent::Run(action) => self.run_action(event_loop, action),
            AppEvent::RunFromMenu(action) => {
                if action.captures_screen() {
                    std::thread::sleep(MENU_CLOSE_DELAY);
                }
                self.run_action(event_loop, action);
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        match &self.flash {
            Some(flash) if Instant::now() < flash.until() => {
                event_loop.set_control_flow(ControlFlow::WaitUntil(flash.until()));
            }
            _ => {
                self.flash = None;
                event_loop.set_control_flow(ControlFlow::Wait);
            }
        }
    }

    fn window_event(&mut self, _event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if let Some(flash) = self.flash.as_mut().filter(|f| f.window_id() == id) {
            if event == WindowEvent::RedrawRequested {
                let _ = flash.draw();
            }
            return;
        }
        if let Some((window, _)) = self.history.as_mut().filter(|(w, _)| w.window_id() == id) {
            let outcome = match window.handle(event) {
                Ok(None) => return,
                Ok(Some(outcome)) => Ok(outcome),
                Err(e) => Err(e),
            };
            let Some((_, files)) = self.history.take() else {
                return;
            };
            let result = outcome.and_then(|outcome| match outcome {
                HistoryOutcome::Closed => Ok(()),
                HistoryOutcome::Picked { index, as_path } => match files.get(index) {
                    Some(path) => self.copy_from_history(path, as_path),
                    None => Ok(()),
                },
            });
            if let Err(e) = result {
                show_error(&format!("{e:#}"));
            }
            return;
        }
        let Some((overlay, _)) = self.selecting.as_mut().filter(|(o, _)| o.window_id() == id) else {
            return;
        };
        let outcome = match overlay.handle(event) {
            Ok(None) => return,
            Ok(Some(outcome)) => Ok(outcome),
            Err(e) => Err(e),
        };
        // 오버레이를 먼저 닫아 화면이 바로 돌아오게 한 뒤 저장한다.
        let Some((overlay, canvas)) = self.selecting.take() else {
            return;
        };
        drop(overlay);
        let result = outcome.and_then(|outcome| match outcome {
            Outcome::Cancelled => Ok(()),
            Outcome::Selected(rect) => {
                state::save_last_region(rect)?;
                let image = crop_from_canvas(&canvas, rect)?;
                self.finish(&image)
            }
        });
        if let Err(e) = result {
            show_error(&format!("{e:#}"));
        }
    }
}

/// 트레이 메뉴와, 그 안의 "자동 실행" 체크 항목을 만든다.
fn build_menu(config: &Config) -> Result<(Menu, CheckMenuItem)> {
    let item = |action: Action| MenuItem::with_id(action.id(), action.label(config), true, None);
    let autostart_on = autostart::tray_exe()
        .and_then(|exe| autostart::is_enabled_for(&exe))
        .unwrap_or(false);
    let autostart_item = CheckMenuItem::with_id(
        Action::Autostart.id(),
        Action::Autostart.text(),
        true,
        autostart_on,
        None,
    );
    let menu = Menu::new();
    menu.append_items(&[
        &item(Action::Region),
        &item(Action::Again),
        &item(Action::Full),
        &PredefinedMenuItem::separator(),
        &item(Action::History),
        &item(Action::OpenFolder),
        &PredefinedMenuItem::separator(),
        &item(Action::OpenConfig),
        &item(Action::ReloadConfig),
        &autostart_item,
        &PredefinedMenuItem::separator(),
        &item(Action::Quit),
    ])
    .context("트레이 메뉴를 만들지 못했습니다")?;
    Ok((menu, autostart_item))
}

fn build_tray(config: &Config) -> Result<(TrayIcon, CheckMenuItem)> {
    let (menu, autostart_item) = build_menu(config)?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip(APP_NAME)
        .with_icon(tray_icon_image()?)
        .build()
        .context("트레이 아이콘을 만들지 못했습니다")?;
    Ok((tray, autostart_item))
}

/// 트레이 아이콘을 코드로 그린다: 파란 바탕에 흰색 선택 영역 모서리 표시.
fn tray_icon_image() -> Result<Icon> {
    const SIZE: u32 = 32;
    const BG: [u8; 4] = [0x25, 0x63, 0xEB, 0xFF];
    const FG: [u8; 4] = [0xFF, 0xFF, 0xFF, 0xFF];
    // 모서리 표시가 놓이는 사각형과 팔 길이, 선 두께.
    const INSET: u32 = 7;
    const ARM: u32 = 7;
    const THICK: u32 = 3;

    let far = SIZE - 1 - INSET;
    let near_edge = |v: u32| v >= INSET && v < INSET + THICK || v <= far && v + THICK > far;
    let in_arm = |v: u32| v >= INSET && v < INSET + ARM || v <= far && v + ARM > far;

    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let corner = (near_edge(x) && in_arm(y)) || (near_edge(y) && in_arm(x));
            // 바깥 모서리를 둥글게 깎는다.
            let rounded_out = (x < 2 || x >= SIZE - 2) && (y < 2 || y >= SIZE - 2);
            rgba.extend_from_slice(if rounded_out {
                &[0, 0, 0, 0]
            } else if corner {
                &FG
            } else {
                &BG
            });
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).context("트레이 아이콘 이미지를 만들지 못했습니다")
}

fn open_with(program: &str, target: &Path) -> Result<()> {
    std::process::Command::new(program)
        .arg(target)
        .spawn()
        .with_context(|| format!("{program}을 실행하지 못했습니다"))?;
    Ok(())
}

/// 같은 프로그램이 이미 떠 있는지 이름 붙은 뮤텍스로 확인한다.
/// 뮤텍스 핸들은 일부러 닫지 않아서 프로그램이 끝날 때까지 유지된다.
fn already_running() -> bool {
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name: Vec<u16> = "Local\\capture_tool_tray_single_instance"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: 이름은 0으로 끝나는 UTF-16이고 호출이 끝날 때까지 살아 있다.
    unsafe {
        let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        !handle.is_null() && GetLastError() == ERROR_ALREADY_EXISTS
    }
}

/// 마우스 커서가 있는 모니터. 찾지 못하면 주 모니터.
fn cursor_monitor(event_loop: &ActiveEventLoop) -> Option<MonitorHandle> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut point = POINT { x: 0, y: 0 };
    // SAFETY: 유효한 POINT를 가리키는 포인터를 넘긴다.
    let found = unsafe { GetCursorPos(&mut point) } != 0;
    event_loop
        .available_monitors()
        .find(|m| {
            let (pos, size) = (m.position(), m.size());
            found
                && point.x >= pos.x
                && point.x < pos.x + size.width as i32
                && point.y >= pos.y
                && point.y < pos.y + size.height as i32
        })
        .or_else(|| event_loop.primary_monitor())
        .or_else(|| event_loop.available_monitors().next())
}

/// 콘솔이 없으므로 오류는 메시지 상자로 알린다.
fn show_error(message: &str) {
    show_message(message, true);
}

fn show_message(message: &str, is_error: bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MessageBoxW,
    };
    let icon = if is_error { MB_ICONERROR } else { MB_ICONINFORMATION };
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (text, title) = (wide(message), wide(APP_NAME));
    // SAFETY: 두 문자열 모두 0으로 끝나는 UTF-16이고 호출이 끝날 때까지 살아 있다.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | icon | MB_SETFOREGROUND,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_hotkeys_parse_and_are_distinct() {
        let config = Config::default();
        let ids: Vec<u32> = [Action::Region, Action::Again, Action::History]
            .into_iter()
            .map(|a| a.hotkey_text(&config).unwrap().parse::<HotKey>().unwrap().id())
            .collect();
        assert!(ids[0] != ids[1] && ids[1] != ids[2] && ids[0] != ids[2]);
    }

    #[test]
    fn hotkey_notation() {
        assert!("ctrl+shift+F9".parse::<HotKey>().is_ok());
        assert!("Ctrl+Alt+없는키".parse::<HotKey>().is_err());
    }

    #[test]
    fn action_ids_round_trip() {
        for action in Action::ALL {
            assert_eq!(Action::from_id(action.id()), Some(action));
        }
    }
}
