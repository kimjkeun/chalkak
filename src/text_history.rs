//! 복사한 텍스트의 히스토리. 클립보드가 바뀔 때마다 글자를 기억해 두었다가
//! 나중에 골라서 다시 붙여 넣을 수 있게 한다. 메모리에만 두며 파일로 남기지 않는다.

use std::time::Duration;

/// 이보다 긴 텍스트는 기억하지 않는다.
const MAX_TEXT_BYTES: usize = 200_000;

pub struct TextHistory {
    /// 최신 순.
    items: Vec<String>,
    limit: usize,
}

impl TextHistory {
    pub fn new(limit: usize) -> Self {
        Self { items: Vec::new(), limit: limit.max(1) }
    }

    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit.max(1);
        self.items.truncate(self.limit);
    }

    pub fn items(&self) -> &[String] {
        &self.items
    }

    /// 새로 복사된 텍스트를 맨 앞에 넣는다. 이미 있던 것이면 맨 앞으로 옮긴다.
    /// 빈 텍스트나 너무 긴 텍스트는 무시하고 거짓을 돌려준다.
    pub fn push(&mut self, text: String) -> bool {
        if text.trim().is_empty() || text.len() > MAX_TEXT_BYTES {
            return false;
        }
        self.items.retain(|existing| *existing != text);
        self.items.insert(0, text);
        self.items.truncate(self.limit);
        true
    }
}

/// 목록에 한 줄로 보여 줄 미리보기: 줄바꿈과 연속 공백을 공백 하나로 줄인다.
pub fn one_line(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
        if out.chars().count() >= max_chars {
            break;
        }
    }
    out.chars().take(max_chars).collect()
}

/// 지금 클립보드에 있는 텍스트. 텍스트가 아니거나 비밀번호 관리자 등이
/// "기록하지 말 것"으로 표시한 내용이면 `None`.
pub fn read_clipboard_text() -> Option<String> {
    if clipboard_is_private() {
        return None;
    }
    // 복사한 프로그램이 아직 클립보드를 잡고 있을 수 있어서 한 번 더 시도한다.
    for attempt in 0..2 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(40));
        }
        if let Ok(text) = arboard::Clipboard::new().and_then(|mut clipboard| clipboard.get_text()) {
            return Some(text);
        }
    }
    None
}

/// 클립보드 내용이 바뀔 때마다 `on_change`를 부르는 감시 스레드를 띄운다.
pub fn spawn_watcher<F: Fn() + Send + 'static>(on_change: F) {
    use windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber;
    std::thread::spawn(move || {
        // SAFETY: 인자 없는 단순 Win32 호출.
        let mut last = unsafe { GetClipboardSequenceNumber() };
        loop {
            std::thread::sleep(Duration::from_millis(250));
            let now = unsafe { GetClipboardSequenceNumber() };
            if now != last {
                last = now;
                on_change();
            }
        }
    });
}

/// 비밀번호 관리자 등이 클립보드 기록 프로그램에게 남기는 "기록하지 말 것" 표시가 있는지.
fn clipboard_is_private() -> bool {
    use windows_sys::Win32::System::DataExchange::{IsClipboardFormatAvailable, RegisterClipboardFormatW};
    let name: Vec<u16> = "ExcludeClipboardContentFromMonitorProcessing"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: 이름은 0으로 끝나는 UTF-16이고 호출이 끝날 때까지 살아 있다.
    unsafe {
        let format = RegisterClipboardFormatW(name.as_ptr());
        format != 0 && IsClipboardFormatAvailable(format) != 0
    }
}

/// 지금 맨 앞에 있는 창. 붙여 넣을 대상을 기억해 둘 때 쓴다.
pub fn foreground_window() -> isize {
    // SAFETY: 인자 없는 단순 Win32 호출.
    unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow() as isize }
}

/// `window`를 앞으로 가져와 Ctrl+V를 눌러 준다 (클립보드 내용이 붙여 넣어진다).
pub fn paste_into(window: isize) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{KEYEVENTF_KEYUP, VK_CONTROL, keybd_event};
    use windows_sys::Win32::UI::WindowsAndMessaging::SetForegroundWindow;
    if window == 0 {
        return;
    }
    // SAFETY: 창 핸들이 이미 사라졌어도 SetForegroundWindow는 실패할 뿐이다.
    unsafe {
        SetForegroundWindow(window as _);
    }
    // 창이 앞으로 나와 입력을 받을 준비가 될 때까지 잠깐 기다린다.
    std::thread::sleep(Duration::from_millis(120));
    const V: u8 = b'V';
    // SAFETY: 키 입력을 흉내 내는 단순 Win32 호출.
    unsafe {
        keybd_event(VK_CONTROL as u8, 0, 0, 0);
        keybd_event(V, 0, 0, 0);
        keybd_event(V, 0, KEYEVENTF_KEYUP, 0);
        keybd_event(VK_CONTROL as u8, 0, KEYEVENTF_KEYUP, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_first_without_duplicates() {
        let mut history = TextHistory::new(10);
        assert!(history.push("a".into()));
        assert!(history.push("b".into()));
        assert!(history.push("c".into()));
        assert_eq!(history.items(), ["c", "b", "a"]);
        // 다시 복사하면 맨 앞으로 옮겨질 뿐 늘어나지 않는다.
        assert!(history.push("a".into()));
        assert_eq!(history.items(), ["a", "c", "b"]);
    }

    #[test]
    fn keeps_only_the_limit() {
        let mut history = TextHistory::new(2);
        for text in ["a", "b", "c"] {
            history.push(text.into());
        }
        assert_eq!(history.items(), ["c", "b"]);
        history.set_limit(1);
        assert_eq!(history.items(), ["c"]);
    }

    #[test]
    fn ignores_blank_and_huge_text() {
        let mut history = TextHistory::new(5);
        assert!(!history.push("  \n\t".into()));
        assert!(!history.push("x".repeat(MAX_TEXT_BYTES + 1)));
        assert!(history.items().is_empty());
    }

    #[test]
    fn one_line_preview() {
        assert_eq!(one_line("첫 줄\n  둘째\t줄\r\n", 100), "첫 줄 둘째 줄");
        assert_eq!(one_line("abcdefghij", 4), "abcd");
        assert_eq!(one_line("", 10), "");
    }
}
