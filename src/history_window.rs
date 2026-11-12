//! 히스토리 창: 최근 캡쳐를 골라 크게 보고, 간단히 표시를 그린 뒤 다시 클립보드로 보낸다.
//!
//! 화면 구성 (전체 화면):
//! - 위: 도구 모음 (사각형, 밑줄, 텍스트, 색, 되돌리기, 복사)
//! - 왼쪽: 고른 캡쳐의 큰 미리보기. 여기에 주석을 그린다.
//! - 오른쪽(설정에 따라 위쪽): 캡쳐 목록. 마우스 휠로 스크롤한다.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use image::RgbaImage;
use image::imageops::FilterType;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::annotate::{self, Annotation, Color, Font, Target};
use crate::config::HistoryLayout;
use crate::history::fit;

const TITLE: &str =
    "캡쳐 히스토리   |   목록 클릭: 선택   ·   더블클릭·숫자 키: 바로 복사   ·   방향키: 이동   ·   Esc: 닫기";

const BACKGROUND: u32 = 0x001E_1E1E;
const TOOLBAR_COLOR: u32 = 0x0026_2626;
const STRIP_COLOR: u32 = 0x0018_1818;
const CELL_COLOR: u32 = 0x002D_2D2D;
const PANE_COLOR: u32 = 0x0012_1212;
const BUTTON_COLOR: u32 = 0x003A_3A3A;
const ACCENT: u32 = 0x0025_63EB;
const WHITE: u32 = 0x00FF_FFFF;
const LABEL: Color = [0xF0, 0xF0, 0xF0];

/// 주석에 쓸 수 있는 색. 첫 번째가 기본값이다.
const PALETTE: [Color; 4] = [
    [0xE5, 0x39, 0x35], // 빨강
    [0x25, 0x63, 0xEB], // 파랑
    [0xFD, 0xD8, 0x35], // 노랑
    [0x11, 0x11, 0x11], // 검정
];

/// 작은 캡쳐를 미리보기에서 키우는 최대 배율.
const MAX_ZOOM: f32 = 2.0;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// 숫자 1~9의 5x7 점 글꼴. 각 줄의 아래 5비트가 왼쪽부터의 점이다.
const DIGITS: [[u8; 7]; 9] = [
    [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
    [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
    [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110],
    [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
    [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
    [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
    [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
    [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
    [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
];

pub enum HistoryOutcome {
    Closed,
    /// `index`번째(0부터) 캡쳐를 클립보드로 보낸다. `as_path`면 이미지 대신 파일 경로를,
    /// `edited`가 있으면 주석을 그린 그 이미지를 새 캡쳐로 저장해서 보낸다.
    Picked { index: usize, as_path: bool, edited: Option<RgbaImage> },
    /// `index`번째 캡쳐의 글자를 읽어 클립보드로 보낸다.
    Ocr { index: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Rect,
    Underline,
    Arrow,
    Marker,
    Text,
}

const TOOLS: [Tool; 5] = [Tool::Rect, Tool::Underline, Tool::Arrow, Tool::Marker, Tool::Text];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Button {
    Tool(Tool),
    Color(usize),
    Undo,
    Ocr,
    CopyImage,
    CopyPath,
}

/// 도구 모음 안의 버튼 하나와 그 가로 위치.
#[derive(Debug, Clone, Copy)]
struct ButtonBox {
    button: Button,
    x: i32,
    width: i32,
}

/// 창 안의 사각형 (물리 픽셀).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pane {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl Pane {
    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

/// 창 안의 배치 계산. 모든 값은 물리 픽셀이다.
#[derive(Debug, Clone, Copy)]
struct Layout {
    width: i32,
    height: i32,
    count: usize,
    /// 목록이 세로로 쌓이는지 (오른쪽 배치). 아니면 가로로 늘어선다 (위쪽 배치).
    vertical: bool,
    toolbar: Pane,
    strip: Pane,
    preview: Pane,
    cell_w: i32,
    cell_h: i32,
    pad: i32,
    inset: i32,
    badge: i32,
    border: i32,
}

impl Layout {
    fn new(size: (u32, u32), scale: f64, side: HistoryLayout, count: usize) -> Self {
        let px = |v: f64| ((v * scale).round() as i32).max(1);
        let (width, height) = ((size.0 as i32).max(1), (size.1 as i32).max(1));
        let pad = px(12.0);
        let toolbar = Pane { x: 0, y: 0, width, height: px(48.0).min(height) };
        let below = toolbar.height;
        let vertical = side == HistoryLayout::Right;
        let (strip, cell_w, cell_h) = if vertical {
            let strip_w = px(280.0).min(width / 2);
            (
                Pane { x: width - strip_w, y: below, width: strip_w, height: height - below },
                strip_w - 2 * pad,
                px(158.0),
            )
        } else {
            let strip_h = px(176.0).min((height - below) / 2);
            (
                Pane { x: 0, y: below, width, height: strip_h },
                px(224.0),
                strip_h - 2 * pad,
            )
        };
        let preview = if vertical {
            Pane {
                x: pad,
                y: below + pad,
                width: strip.x - 2 * pad,
                height: height - below - 2 * pad,
            }
        } else {
            let top = strip.y + strip.height + pad;
            Pane { x: pad, y: top, width: width - 2 * pad, height: height - top - pad }
        };
        Self {
            width,
            height,
            count,
            vertical,
            toolbar,
            strip,
            preview: Pane { width: preview.width.max(1), height: preview.height.max(1), ..preview },
            cell_w: cell_w.max(1),
            cell_h: cell_h.max(1),
            pad,
            inset: px(6.0),
            badge: px(24.0),
            border: px(3.0),
        }
    }

    /// 목록에서 칸 하나가 차지하는 길이 (칸 + 간격).
    fn step(&self) -> i32 {
        (if self.vertical { self.cell_h } else { self.cell_w }) + self.pad
    }

    /// 목록이 보이는 창의 길이.
    fn viewport(&self) -> i32 {
        if self.vertical { self.strip.height } else { self.strip.width }
    }

    fn max_scroll(&self) -> i32 {
        (self.pad + self.count as i32 * self.step() - self.viewport()).max(0)
    }

    fn cell_origin(&self, index: usize, scroll: i32) -> (i32, i32) {
        let along = self.pad + index as i32 * self.step() - scroll;
        if self.vertical {
            (self.strip.x + self.pad, self.strip.y + along)
        } else {
            (self.strip.x + along, self.strip.y + self.pad)
        }
    }

    fn cell_at(&self, x: i32, y: i32, scroll: i32) -> Option<usize> {
        if !self.strip.contains(x, y) {
            return None;
        }
        (0..self.count).find(|&i| {
            let (cx, cy) = self.cell_origin(i, scroll);
            x >= cx && x < cx + self.cell_w && y >= cy && y < cy + self.cell_h
        })
    }

    /// `index`번째 칸이 다 보이도록 맞춘 스크롤 값.
    fn scroll_to_show(&self, index: usize, scroll: i32) -> i32 {
        let start = index as i32 * self.step();
        let end = start + self.step() + self.pad;
        let scroll = if start < scroll {
            start
        } else if end > scroll + self.viewport() {
            end - self.viewport()
        } else {
            scroll
        };
        scroll.clamp(0, self.max_scroll())
    }
}

/// 그릴 크기로 맞춰 둔 이미지 (0RGB 픽셀).
struct Thumb {
    width: i32,
    height: i32,
    pixels: Vec<u32>,
}

impl Thumb {
    fn from_image(image: &RgbaImage) -> Self {
        Self {
            width: image.width() as i32,
            height: image.height() as i32,
            pixels: image
                .as_raw()
                .chunks_exact(4)
                .map(|p| (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32)
                .collect(),
        }
    }

    /// `max` 안에 들어가도록 줄인다. 작은 이미지를 키우지는 않는다.
    fn fit_into(image: &RgbaImage, max: (i32, i32)) -> Self {
        let (width, height) = fit(image.dimensions(), (max.0.max(1) as u32, max.1.max(1) as u32));
        Self::from_image(&image::imageops::thumbnail(image, width, height))
    }
}

/// 미리보기 칸에 그려진 캡쳐. `scale`은 원본 1픽셀이 화면에서 몇 픽셀인지.
struct Preview {
    thumb: Thumb,
    x: i32,
    y: i32,
    scale: f32,
}

struct Entry {
    path: PathBuf,
    /// 목록 칸 크기로 줄이기 전의 썸네일.
    source: RgbaImage,
    cell: Thumb,
}

/// 입력 중인 글자. 좌표는 원본 이미지 기준이고 `(x, y)`가 줄의 왼쪽 위다.
struct TextEntry {
    x: f32,
    y: f32,
    size: f32,
    text: String,
    /// 한글 등 조합 중인 글자.
    preedit: String,
}

pub struct HistoryWindow {
    window: Rc<Window>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
    font: Rc<Font>,
    side: HistoryLayout,
    scale: f64,
    layout: Layout,
    entries: Vec<Entry>,
    selected: usize,
    /// 고른 캡쳐의 원본. 읽지 못했으면 `None`.
    current: Option<RgbaImage>,
    preview: Option<Preview>,
    annotations: Vec<Annotation>,
    tool: Option<Tool>,
    color: usize,
    /// 사각형·밑줄을 끄는 중일 때의 시작점 (원본 이미지 좌표).
    drag: Option<(f32, f32)>,
    text_entry: Option<TextEntry>,
    cursor: (i32, i32),
    scroll: i32,
    shift: bool,
    ctrl: bool,
    last_click: Option<(Instant, usize)>,
}

impl HistoryWindow {
    /// `entries`(파일과 그 썸네일, 최신 순)를 보여 주는 창을
    /// `area`(모니터의 위치와 크기)에 전체 화면으로 띄운다.
    pub fn open(
        event_loop: &ActiveEventLoop,
        entries: &[(&Path, &RgbaImage)],
        area: (PhysicalPosition<i32>, PhysicalSize<u32>),
        scale: f64,
        side: HistoryLayout,
        font: Rc<Font>,
    ) -> Result<Self> {
        if entries.is_empty() {
            bail!("보여 줄 캡쳐가 없습니다");
        }
        let (origin, monitor) = area;
        // 최대화가 풀렸을 때의 크기와 위치. 최대화는 이 위치가 속한 모니터에서 이뤄진다.
        let restored = PhysicalSize::new(monitor.width * 4 / 5, monitor.height * 4 / 5);
        let attrs = Window::default_attributes()
            .with_title(TITLE)
            .with_visible(false)
            .with_position(PhysicalPosition::new(
                origin.x + (monitor.width / 10) as i32,
                origin.y + (monitor.height / 10) as i32,
            ))
            .with_inner_size(restored)
            .with_maximized(true);
        let window = Rc::new(
            event_loop
                .create_window(attrs)
                .context("히스토리 창을 만들지 못했습니다")?,
        );
        let context = softbuffer::Context::new(window.clone())
            .map_err(|e| anyhow!("그리기 컨텍스트 생성 실패: {e}"))?;
        let surface = softbuffer::Surface::new(&context, window.clone())
            .map_err(|e| anyhow!("그리기 표면 생성 실패: {e}"))?;

        let size = window.inner_size();
        let mut this = Self {
            window,
            surface,
            font,
            side,
            scale,
            layout: Layout::new((size.width, size.height), scale, side, entries.len()),
            entries: entries
                .iter()
                .map(|(path, thumb)| Entry {
                    path: path.to_path_buf(),
                    source: (*thumb).clone(),
                    cell: Thumb { width: 0, height: 0, pixels: Vec::new() },
                })
                .collect(),
            selected: 0,
            current: None,
            preview: None,
            annotations: Vec::new(),
            tool: None,
            color: 0,
            drag: None,
            text_entry: None,
            cursor: (-1, -1),
            scroll: 0,
            shift: false,
            ctrl: false,
            last_click: None,
        };
        this.load_current();
        this.relayout((size.width, size.height));
        this.draw()?;
        this.window.set_visible(true);
        this.window.focus_window();
        Ok(this)
    }

    pub fn window_id(&self) -> WindowId {
        self.window.id()
    }

    pub fn handle(&mut self, event: WindowEvent) -> Result<Option<HistoryOutcome>> {
        match event {
            WindowEvent::CloseRequested => return Ok(Some(HistoryOutcome::Closed)),
            WindowEvent::Resized(size) => {
                self.relayout((size.width, size.height));
                self.window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.scale = scale_factor;
                let size = self.window.inner_size();
                self.relayout((size.width, size.height));
                self.window.request_redraw();
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.shift = modifiers.state().shift_key();
                self.ctrl = modifiers.state().control_key();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x as i32, position.y as i32);
                if self.drag.is_some() {
                    self.window.request_redraw();
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let amount = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (y + x) * self.layout.step() as f32 * 0.5,
                    MouseScrollDelta::PixelDelta(p) => (p.y + p.x) as f32,
                };
                let scroll = (self.scroll - amount.round() as i32).clamp(0, self.layout.max_scroll());
                if scroll != self.scroll {
                    self.scroll = scroll;
                    self.window.request_redraw();
                }
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                let outcome = match state {
                    ElementState::Pressed => self.mouse_down(),
                    ElementState::Released => {
                        self.mouse_up();
                        None
                    }
                };
                self.window.request_redraw();
                return Ok(outcome);
            }
            WindowEvent::Ime(ime) => {
                if let Some(entry) = self.text_entry.as_mut() {
                    match ime {
                        Ime::Preedit(text, _) => entry.preedit = text,
                        Ime::Commit(text) => {
                            entry.preedit.clear();
                            entry.text.push_str(&text);
                        }
                        _ => {}
                    }
                    self.window.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let outcome = if self.text_entry.is_some() {
                    self.type_key(&event.logical_key, event.text.as_deref());
                    None
                } else {
                    self.shortcut(event.physical_key, &event.logical_key)
                };
                self.window.request_redraw();
                return Ok(outcome);
            }
            WindowEvent::RedrawRequested => self.draw()?,
            _ => {}
        }
        Ok(None)
    }

    // ---- 입력 처리 ----

    fn mouse_down(&mut self) -> Option<HistoryOutcome> {
        let (x, y) = self.cursor;
        if self.layout.toolbar.contains(x, y) {
            let hit = self.buttons().0.into_iter().find(|b| x >= b.x && x < b.x + b.width);
            return hit.and_then(|b| self.press(b.button));
        }
        if let Some(index) = self.layout.cell_at(x, y, self.scroll) {
            let now = Instant::now();
            let double = self
                .last_click
                .is_some_and(|(at, i)| i == index && now.duration_since(at) < DOUBLE_CLICK);
            self.last_click = Some((now, index));
            if double {
                return Some(self.pick(index, self.shift));
            }
            self.select(index);
            return None;
        }
        if let (Some(tool), Some(point)) = (self.tool, self.image_point(x, y, false)) {
            self.finish_text();
            match tool {
                Tool::Rect | Tool::Underline | Tool::Arrow => self.drag = Some(point),
                Tool::Marker => self.add_marker(point),
                Tool::Text => self.start_text(point),
            }
        }
        None
    }

    fn mouse_up(&mut self) {
        if let Some(shape) = self.pending_shape() {
            // 실수로 찍은 점 같은 아주 작은 것은 버린다.
            let big_enough = match &shape {
                Annotation::Rect { x0, y0, x1, y1, width, .. } => {
                    (x1 - x0).abs() > width * 2.0 && (y1 - y0).abs() > width * 2.0
                }
                Annotation::Underline { x0, x1, width, .. } => (x1 - x0).abs() > width * 2.0,
                Annotation::Arrow { x0, y0, x1, y1, width, .. } => (x1 - x0).hypot(y1 - y0) > width * 3.0,
                Annotation::Text { .. } | Annotation::Marker { .. } => true,
            };
            if big_enough {
                self.annotations.push(shape);
            }
        }
        self.drag = None;
    }

    fn press(&mut self, button: Button) -> Option<HistoryOutcome> {
        match button {
            Button::Tool(tool) => self.toggle_tool(tool),
            Button::Color(index) => self.color = index,
            Button::Undo => self.undo(),
            Button::Ocr => return Some(HistoryOutcome::Ocr { index: self.selected }),
            Button::CopyImage => return Some(self.pick(self.selected, false)),
            Button::CopyPath => return Some(self.pick(self.selected, true)),
        }
        None
    }

    /// 글자 입력 중이 아닐 때의 단축키.
    fn shortcut(&mut self, key: PhysicalKey, logical: &Key) -> Option<HistoryOutcome> {
        let PhysicalKey::Code(code) = key else {
            return None;
        };
        match code {
            KeyCode::Escape => return Some(HistoryOutcome::Closed),
            KeyCode::Enter | KeyCode::NumpadEnter => return Some(self.pick(self.selected, self.shift)),
            KeyCode::KeyC if self.ctrl => return Some(self.pick(self.selected, self.shift)),
            KeyCode::KeyZ if self.ctrl => self.undo(),
            KeyCode::KeyR if !self.ctrl => self.toggle_tool(Tool::Rect),
            KeyCode::KeyU if !self.ctrl => self.toggle_tool(Tool::Underline),
            KeyCode::KeyT if !self.ctrl => self.toggle_tool(Tool::Text),
            KeyCode::KeyA if !self.ctrl => self.toggle_tool(Tool::Arrow),
            KeyCode::KeyN if !self.ctrl => self.toggle_tool(Tool::Marker),
            KeyCode::KeyO if !self.ctrl => return Some(HistoryOutcome::Ocr { index: self.selected }),
            KeyCode::ArrowUp | KeyCode::ArrowLeft => self.select(self.selected.saturating_sub(1)),
            KeyCode::ArrowDown | KeyCode::ArrowRight => {
                self.select((self.selected + 1).min(self.entries.len() - 1));
            }
            _ => {
                // NumLock이 꺼진 숫자 패드는 방향키로 동작하므로 숫자로 치지 않는다.
                let types_a_digit = matches!(logical, Key::Character(_));
                if let Some(index) =
                    digit_index(code).filter(|&i| types_a_digit && i < self.entries.len() && !self.ctrl)
                {
                    return Some(self.pick(index, self.shift));
                }
            }
        }
        None
    }

    /// 글자 입력 중의 키 처리.
    fn type_key(&mut self, key: &Key, text: Option<&str>) {
        match key {
            Key::Named(NamedKey::Enter) => self.finish_text(),
            Key::Named(NamedKey::Escape) => self.cancel_text(),
            Key::Named(NamedKey::Backspace) => {
                // 조합 중인 글자는 입력기가 직접 지운다.
                if let Some(entry) = self.text_entry.as_mut().filter(|e| e.preedit.is_empty()) {
                    entry.text.pop();
                }
            }
            _ => {
                if let (Some(entry), Some(text), false) = (self.text_entry.as_mut(), text, self.ctrl) {
                    entry.text.extend(text.chars().filter(|c| !c.is_control()));
                }
            }
        }
    }

    fn toggle_tool(&mut self, tool: Tool) {
        self.finish_text();
        self.tool = if self.tool == Some(tool) { None } else { Some(tool) };
    }

    fn undo(&mut self) {
        if self.text_entry.is_some() {
            self.cancel_text();
        } else {
            self.annotations.pop();
        }
    }

    /// 클릭한 자리에 다음 번호의 표시를 놓는다.
    fn add_marker(&mut self, point: (f32, f32)) {
        let Some(scale) = self.preview.as_ref().map(|p| p.scale) else {
            return;
        };
        let used = self
            .annotations
            .iter()
            .filter(|a| matches!(a, Annotation::Marker { .. }))
            .count();
        self.annotations.push(Annotation::Marker {
            x: point.0,
            y: point.1,
            radius: (14.0 * self.scale as f32 / scale.min(1.0)).max(8.0),
            number: used as u32 + 1,
            color: PALETTE[self.color],
        });
    }

    fn start_text(&mut self, point: (f32, f32)) {
        let Some(scale) = self.preview.as_ref().map(|p| p.scale) else {
            return;
        };
        let size = (22.0 * self.scale as f32 / scale.min(1.0)).max(12.0);
        self.text_entry = Some(TextEntry {
            x: point.0,
            // 클릭한 곳이 글자 줄의 가운데 높이가 되게 한다.
            y: point.1 - size / 2.0,
            size,
            text: String::new(),
            preedit: String::new(),
        });
        // 글자를 입력하는 동안에만 한글 입력기를 켠다.
        self.window.set_ime_allowed(true);
        self.window.set_ime_cursor_area(
            PhysicalPosition::new(self.cursor.0, self.cursor.1),
            PhysicalSize::new(1, (size * scale) as u32),
        );
    }

    /// 입력 중인 글자를 주석으로 확정한다.
    fn finish_text(&mut self) {
        if let Some(entry) = self.text_entry.take() {
            let text = format!("{}{}", entry.text, entry.preedit);
            if !text.trim().is_empty() {
                self.annotations.push(Annotation::Text {
                    x: entry.x,
                    y: entry.y,
                    size: entry.size,
                    text,
                    color: PALETTE[self.color],
                });
            }
            self.window.set_ime_allowed(false);
        }
    }

    fn cancel_text(&mut self) {
        if self.text_entry.take().is_some() {
            self.window.set_ime_allowed(false);
        }
    }

    /// 다른 캡쳐를 고른다. 그리던 주석은 그 캡쳐의 것이므로 버린다.
    fn select(&mut self, index: usize) {
        if index == self.selected || index >= self.entries.len() {
            return;
        }
        self.cancel_text();
        self.drag = None;
        self.annotations.clear();
        self.selected = index;
        self.load_current();
        self.rebuild_preview();
        self.scroll = self.layout.scroll_to_show(index, self.scroll);
    }

    fn pick(&mut self, index: usize, as_path: bool) -> HistoryOutcome {
        self.finish_text();
        let edited = (index == self.selected && !self.annotations.is_empty())
            .then(|| self.current.as_ref())
            .flatten()
            .map(|image| annotate::apply(image, &self.annotations, &self.font));
        HistoryOutcome::Picked { index, as_path, edited }
    }

    // ---- 배치와 좌표 ----

    fn load_current(&mut self) {
        self.current = self
            .entries
            .get(self.selected)
            .and_then(|entry| image::open(&entry.path).ok())
            .map(|image| image.to_rgba8());
    }

    fn relayout(&mut self, size: (u32, u32)) {
        self.layout = Layout::new(size, self.scale, self.side, self.entries.len());
        let max = (
            self.layout.cell_w - 2 * self.layout.inset,
            self.layout.cell_h - 2 * self.layout.inset,
        );
        for entry in &mut self.entries {
            entry.cell = Thumb::fit_into(&entry.source, max);
        }
        self.scroll = self.scroll.clamp(0, self.layout.max_scroll());
        self.rebuild_preview();
    }

    fn rebuild_preview(&mut self) {
        let pane = self.layout.preview;
        self.preview = self.current.as_ref().map(|image| {
            let (iw, ih) = (image.width() as f32, image.height() as f32);
            let scale = (pane.width as f32 / iw)
                .min(pane.height as f32 / ih)
                .min(MAX_ZOOM);
            let width = ((iw * scale).round() as u32).max(1);
            let height = ((ih * scale).round() as u32).max(1);
            let thumb = if scale < 1.0 {
                Thumb::from_image(&image::imageops::thumbnail(image, width, height))
            } else if scale > 1.0 {
                // 키울 때는 뭉개지지 않게 픽셀을 그대로 늘린다.
                Thumb::from_image(&image::imageops::resize(image, width, height, FilterType::Nearest))
            } else {
                Thumb::from_image(image)
            };
            Preview {
                x: pane.x + (pane.width - width as i32) / 2,
                y: pane.y + (pane.height - height as i32) / 2,
                scale: width as f32 / iw,
                thumb,
            }
        });
    }

    /// 창 좌표를 원본 이미지 좌표로 바꾼다. `clamp`가 거짓이면 이미지 밖은 `None`.
    fn image_point(&self, x: i32, y: i32, clamp: bool) -> Option<(f32, f32)> {
        let (preview, image) = (self.preview.as_ref()?, self.current.as_ref()?);
        let inside = x >= preview.x
            && x < preview.x + preview.thumb.width
            && y >= preview.y
            && y < preview.y + preview.thumb.height;
        if !inside && !clamp {
            return None;
        }
        Some((
            ((x - preview.x) as f32 / preview.scale).clamp(0.0, image.width() as f32),
            ((y - preview.y) as f32 / preview.scale).clamp(0.0, image.height() as f32),
        ))
    }

    /// 지금 끌고 있는 사각형이나 밑줄.
    fn pending_shape(&self) -> Option<Annotation> {
        let (x0, y0) = self.drag?;
        let (x1, y1) = self.image_point(self.cursor.0, self.cursor.1, true)?;
        let scale = self.preview.as_ref()?.scale;
        // 화면에서 보이는 굵기가 일정하도록, 축소해서 보는 큰 캡쳐일수록 선을 굵게 한다.
        // 작은 캡쳐를 키워서 볼 때는 가늘게 하지 않는다 (저장된 그림에서 너무 작아진다).
        let stroke = |screen_px: f32| (screen_px * self.scale as f32 / scale.min(1.0)).max(2.0);
        let color = PALETTE[self.color];
        match self.tool? {
            Tool::Rect => Some(Annotation::Rect { x0, y0, x1, y1, width: stroke(3.0), color }),
            Tool::Underline => Some(Annotation::Underline { x0, x1, y: y0, width: stroke(4.0), color }),
            Tool::Arrow => Some(Annotation::Arrow { x0, y0, x1, y1, width: stroke(4.0), color }),
            Tool::Marker | Tool::Text => None,
        }
    }

    fn label_size(&self) -> f32 {
        15.0 * self.scale as f32
    }

    /// 도구 모음의 버튼 배치와, 짧은 이름을 썼는지 여부.
    /// 창이 좁아 버튼이 겹치면 단축키 안내를 뺀 짧은 이름으로 바꾼다.
    fn buttons(&self) -> (Vec<ButtonBox>, bool) {
        let (boxes, fits) = self.place_buttons(false);
        if fits { (boxes, false) } else { (self.place_buttons(true).0, true) }
    }

    /// 도구와 색은 왼쪽부터, 내보내기 버튼은 오른쪽 끝에 놓는다. 서로 겹치지 않으면 참.
    fn place_buttons(&self, short: bool) -> (Vec<ButtonBox>, bool) {
        let px = |v: f32| (v * self.scale as f32).round() as i32;
        let (gap, padding) = (px(8.0), px(if short { 9.0 } else { 14.0 }));
        let text_width =
            |button: Button| self.font.measure(button_label(button, short), self.label_size()).ceil() as i32;
        let mut boxes = Vec::new();
        let mut x = gap;
        for button in TOOLS.map(Button::Tool) {
            let width = text_width(button) + 2 * padding;
            boxes.push(ButtonBox { button, x, width });
            x += width + gap;
        }
        x += gap;
        let swatch = self.layout.toolbar.height - 2 * gap;
        for index in 0..PALETTE.len() {
            boxes.push(ButtonBox { button: Button::Color(index), x, width: swatch });
            x += swatch + gap;
        }
        x += gap;
        let width = text_width(Button::Undo) + 2 * padding;
        boxes.push(ButtonBox { button: Button::Undo, x, width });
        let left_end = x + width;

        let mut right = self.layout.width - gap;
        for button in [Button::CopyPath, Button::CopyImage, Button::Ocr] {
            let width = text_width(button) + 2 * padding;
            right -= width;
            boxes.push(ButtonBox { button, x: right, width });
            right -= gap;
        }
        (boxes, left_end <= right)
    }

    // ---- 그리기 ----

    fn draw(&mut self) -> Result<()> {
        let layout = self.layout;
        let (buttons, short_labels) = self.buttons();
        let pending = self.pending_shape();
        self.surface
            .resize(
                NonZeroU32::new(layout.width as u32).context("창 너비가 0입니다")?,
                NonZeroU32::new(layout.height as u32).context("창 높이가 0입니다")?,
            )
            .map_err(|e| anyhow!("그리기 표면 크기 조정 실패: {e}"))?;
        let mut buffer = self
            .surface
            .buffer_mut()
            .map_err(|e| anyhow!("그리기 버퍼를 얻지 못했습니다: {e}"))?;
        if buffer.len() != (layout.width * layout.height) as usize {
            // 크기가 바뀌는 중이다. 곧 올 Resized 뒤에 다시 그린다.
            return Ok(());
        }
        let mut frame = Frame::new(&mut buffer, layout.width, layout.height);
        frame.fill(0, 0, layout.width, layout.height, BACKGROUND);

        // 미리보기와 주석.
        let pane = layout.preview;
        frame.fill(pane.x, pane.y, pane.width, pane.height, PANE_COLOR);
        if let Some(preview) = &self.preview {
            frame.clip = pane;
            frame.blit(preview.x, preview.y, &preview.thumb);
            let (ox, oy) = (preview.x as f32, preview.y as f32);
            for annotation in self.annotations.iter().chain(pending.iter()) {
                annotate::draw(&mut frame, annotation, preview.scale, ox, oy, &self.font);
            }
            if let Some(entry) = &self.text_entry {
                let color = PALETTE[self.color];
                let size = entry.size * preview.scale;
                let (x, y) = (ox + entry.x * preview.scale, oy + entry.y * preview.scale);
                let text = format!("{}{}", entry.text, entry.preedit);
                let end = self.font.draw(&mut frame, &text, x, y, size, color);
                // 입력 위치를 알려 주는 커서.
                let caret = (size / 12.0).round().max(1.0) as i32;
                annotate::fill(&mut frame, end as i32 + 1, y as i32, end as i32 + 1 + caret, (y + size) as i32, color);
            }
            frame.clip = frame.full();
        }

        // 캡쳐 목록.
        let strip = layout.strip;
        frame.fill(strip.x, strip.y, strip.width, strip.height, STRIP_COLOR);
        frame.clip = strip;
        for (i, entry) in self.entries.iter().enumerate() {
            let (cx, cy) = layout.cell_origin(i, self.scroll);
            let selected = self.selected == i;
            frame.fill(cx, cy, layout.cell_w, layout.cell_h, if selected { ACCENT } else { CELL_COLOR });
            if selected {
                let b = layout.border;
                frame.fill(cx + b, cy + b, layout.cell_w - 2 * b, layout.cell_h - 2 * b, CELL_COLOR);
            }
            frame.blit(
                cx + (layout.cell_w - entry.cell.width) / 2,
                cy + (layout.cell_h - entry.cell.height) / 2,
                &entry.cell,
            );
            if let Some(glyph) = DIGITS.get(i) {
                frame.fill(cx, cy, layout.badge, layout.badge, ACCENT);
                let dot = (layout.badge / 10).max(1);
                let (gx, gy) = (cx + (layout.badge - 5 * dot) / 2, cy + (layout.badge - 7 * dot) / 2);
                for (row, bits) in glyph.iter().enumerate() {
                    for col in 0..5 {
                        if bits >> (4 - col) & 1 == 1 {
                            frame.fill(gx + col * dot, gy + row as i32 * dot, dot, dot, WHITE);
                        }
                    }
                }
            }
        }
        // 스크롤 위치 표시.
        let max_scroll = layout.max_scroll();
        if max_scroll > 0 {
            let viewport = layout.viewport();
            let total = viewport + max_scroll;
            let length = (viewport * viewport / total).max(layout.badge);
            let offset = (viewport - length) * self.scroll / max_scroll;
            let thick = (layout.border * 2).max(2);
            if layout.vertical {
                frame.fill(strip.x + strip.width - thick, strip.y + offset, thick, length, BUTTON_COLOR);
            } else {
                frame.fill(strip.x + offset, strip.y + strip.height - thick, length, thick, BUTTON_COLOR);
            }
        }
        frame.clip = frame.full();

        // 도구 모음.
        let bar = layout.toolbar;
        frame.fill(bar.x, bar.y, bar.width, bar.height, TOOLBAR_COLOR);
        let margin = ((bar.height as f32) * 0.17).round() as i32;
        let label_size = 15.0 * self.scale as f32;
        for b in &buttons {
            let (top, height) = (bar.y + margin, bar.height - 2 * margin);
            match b.button {
                Button::Color(index) => {
                    let [r, g, bl] = PALETTE[index];
                    if index == self.color {
                        frame.fill(b.x - 2, top - 2, b.width + 4, height + 4, WHITE);
                    }
                    frame.fill(b.x, top, b.width, height, (r as u32) << 16 | (g as u32) << 8 | bl as u32);
                }
                button => {
                    let active = matches!(button, Button::Tool(tool) if self.tool == Some(tool));
                    frame.fill(b.x, top, b.width, height, if active { ACCENT } else { BUTTON_COLOR });
                    let label = button_label(button, short_labels);
                    let text_w = self.font.measure(label, label_size);
                    self.font.draw(
                        &mut frame,
                        label,
                        b.x as f32 + (b.width as f32 - text_w) / 2.0,
                        top as f32 + (height as f32 - label_size) / 2.0,
                        label_size,
                        LABEL,
                    );
                }
            }
        }
        buffer
            .present()
            .map_err(|e| anyhow!("화면에 그리지 못했습니다: {e}"))
    }
}

fn button_label(button: Button, short: bool) -> &'static str {
    let (long, brief) = match button {
        Button::Tool(Tool::Rect) => ("사각형 (R)", "사각형"),
        Button::Tool(Tool::Underline) => ("밑줄 (U)", "밑줄"),
        Button::Tool(Tool::Arrow) => ("화살표 (A)", "화살표"),
        Button::Tool(Tool::Marker) => ("번호 (N)", "번호"),
        Button::Tool(Tool::Text) => ("텍스트 (T)", "텍스트"),
        Button::Color(_) => ("", ""),
        Button::Undo => ("되돌리기 (Ctrl+Z)", "되돌리기"),
        Button::Ocr => ("글자 추출 (O)", "글자 추출"),
        Button::CopyImage => ("이미지 복사 (Enter)", "이미지 복사"),
        Button::CopyPath => ("경로 복사 (Shift+Enter)", "경로 복사"),
    };
    if short { brief } else { long }
}

/// 숫자 키(위쪽 줄과 숫자 패드)를 0부터 시작하는 항목 번호로 바꾼다.
fn digit_index(code: KeyCode) -> Option<usize> {
    use KeyCode::*;
    Some(match code {
        Digit1 | Numpad1 => 0,
        Digit2 | Numpad2 => 1,
        Digit3 | Numpad3 => 2,
        Digit4 | Numpad4 => 3,
        Digit5 | Numpad5 => 4,
        Digit6 | Numpad6 => 5,
        Digit7 | Numpad7 => 6,
        Digit8 | Numpad8 => 7,
        Digit9 | Numpad9 => 8,
        _ => return None,
    })
}

/// 그리기 대상 버퍼. `clip` 밖에는 그려지지 않는다.
struct Frame<'a> {
    pixels: &'a mut [u32],
    width: i32,
    height: i32,
    clip: Pane,
}

impl<'a> Frame<'a> {
    fn new(pixels: &'a mut [u32], width: i32, height: i32) -> Self {
        Self { pixels, width, height, clip: Pane { x: 0, y: 0, width, height } }
    }

    fn full(&self) -> Pane {
        Pane { x: 0, y: 0, width: self.width, height: self.height }
    }

    /// 그릴 수 있는 범위 `(x0, y0, x1, y1)`: 버퍼와 `clip`의 겹치는 부분.
    fn bounds(&self) -> (i32, i32, i32, i32) {
        (
            self.clip.x.max(0),
            self.clip.y.max(0),
            (self.clip.x + self.clip.width).min(self.width),
            (self.clip.y + self.clip.height).min(self.height),
        )
    }

    fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        let (bx0, by0, bx1, by1) = self.bounds();
        let (x0, x1) = (x.max(bx0), (x + w).min(bx1));
        let (y0, y1) = (y.max(by0), (y + h).min(by1));
        if x0 >= x1 {
            return;
        }
        for row in y0..y1 {
            let start = (row * self.width) as usize;
            self.pixels[start + x0 as usize..start + x1 as usize].fill(color);
        }
    }

    fn blit(&mut self, x: i32, y: i32, thumb: &Thumb) {
        let (bx0, by0, bx1, by1) = self.bounds();
        let (x0, x1) = (x.max(bx0), (x + thumb.width).min(bx1));
        let (y0, y1) = (y.max(by0), (y + thumb.height).min(by1));
        if x0 >= x1 {
            return;
        }
        let len = (x1 - x0) as usize;
        for row in y0..y1 {
            let src = ((row - y) * thumb.width + (x0 - x)) as usize;
            let dst = (row * self.width + x0) as usize;
            self.pixels[dst..dst + len].copy_from_slice(&thumb.pixels[src..src + len]);
        }
    }
}

impl Target for Frame<'_> {
    fn blend(&mut self, x: i32, y: i32, color: Color, alpha: u8) {
        let (bx0, by0, bx1, by1) = self.bounds();
        if x < bx0 || y < by0 || x >= bx1 || y >= by1 {
            return;
        }
        let pixel = &mut self.pixels[(y * self.width + x) as usize];
        let old = [(*pixel >> 16) as u8, (*pixel >> 8) as u8, *pixel as u8];
        let [r, g, b] = [0, 1, 2].map(|i| annotate::mix(old[i], color[i], alpha) as u32);
        *pixel = r << 16 | g << 8 | b;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDE: (u32, u32) = (1920, 1009);

    fn no_overlap(a: Pane, b: Pane) -> bool {
        a.x + a.width <= b.x || b.x + b.width <= a.x || a.y + a.height <= b.y || b.y + b.height <= a.y
    }

    #[test]
    fn right_layout_puts_list_on_the_right_and_preview_on_the_left() {
        let layout = Layout::new(WIDE, 1.0, HistoryLayout::Right, 20);
        assert!(layout.vertical);
        assert_eq!(layout.strip.x + layout.strip.width, 1920);
        assert!(layout.preview.x < layout.strip.x && layout.preview.width > 1500);
        assert!(no_overlap(layout.preview, layout.strip));
        assert!(no_overlap(layout.preview, layout.toolbar));
        assert!(no_overlap(layout.strip, layout.toolbar));
    }

    #[test]
    fn top_layout_puts_list_above_preview() {
        let layout = Layout::new(WIDE, 1.0, HistoryLayout::Top, 20);
        assert!(!layout.vertical);
        assert_eq!(layout.strip.width, 1920);
        assert!(layout.preview.y >= layout.strip.y + layout.strip.height);
        assert!(no_overlap(layout.preview, layout.strip));
    }

    #[test]
    fn list_scrolls_only_when_it_overflows() {
        let few = Layout::new(WIDE, 1.0, HistoryLayout::Right, 3);
        assert_eq!(few.max_scroll(), 0);
        let many = Layout::new(WIDE, 1.0, HistoryLayout::Right, 20);
        assert_eq!(many.max_scroll(), 12 + 20 * 170 - (1009 - 48));
    }

    #[test]
    fn cell_hit_test_follows_scroll() {
        let layout = Layout::new(WIDE, 1.0, HistoryLayout::Right, 20);
        let x = layout.strip.x + 50;
        let top = layout.strip.y;
        assert_eq!(layout.cell_at(x, top + 12, 0), Some(0));
        assert_eq!(layout.cell_at(x, top + 12 + 170, 0), Some(1));
        assert_eq!(layout.cell_at(x, top + 5, 0), None); // 위쪽 여백
        assert_eq!(layout.cell_at(x, top + 12, 170), Some(1)); // 한 칸 스크롤
        assert_eq!(layout.cell_at(100, top + 12, 0), None); // 미리보기 쪽
        assert_eq!(layout.cell_at(x, 10, 0), None); // 도구 모음
    }

    #[test]
    fn scroll_to_show_brings_cell_into_view() {
        let layout = Layout::new(WIDE, 1.0, HistoryLayout::Right, 20);
        assert_eq!(layout.scroll_to_show(0, 0), 0);
        assert_eq!(layout.scroll_to_show(0, 500), 0);
        let last = layout.scroll_to_show(19, 0);
        assert_eq!(last, layout.max_scroll());
        // 이미 보이는 칸이면 그대로 둔다.
        assert_eq!(layout.scroll_to_show(2, 100), 100);
    }

    #[test]
    fn frame_clips_drawing() {
        let mut pixels = vec![0u32; 100];
        let mut frame = Frame::new(&mut pixels, 10, 10);
        frame.clip = Pane { x: 2, y: 2, width: 4, height: 4 };
        frame.fill(-5, -5, 30, 30, 7);
        frame.blend(0, 0, [255, 255, 255], 255);
        frame.blend(3, 3, [255, 0, 0], 255);
        assert_eq!(pixels.iter().filter(|&&p| p != 0).count(), 16);
        assert_eq!(pixels[3 * 10 + 3], 0x00FF_0000);
        assert_eq!(pixels[0], 0);
    }

    #[test]
    fn digit_keys_map_to_indices() {
        assert_eq!(digit_index(KeyCode::Digit1), Some(0));
        assert_eq!(digit_index(KeyCode::Numpad9), Some(8));
        assert_eq!(digit_index(KeyCode::Digit0), None);
        assert_eq!(digit_index(KeyCode::KeyA), None);
    }
}
