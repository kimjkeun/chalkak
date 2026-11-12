//! 설정 파일 (`%APPDATA%\capture_tool\config\config.toml`).
//! 처음 실행하면 설명이 달린 기본 파일을 만들어 둔다.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::history;
use crate::output::{self, CopyMode};

const DEFAULT_FILE: &str = r#"# capture_tool 설정
# 고친 뒤 트레이 메뉴의 "설정 다시 불러오기"를 누르면 바로 적용됩니다.

# 캡쳐를 저장할 폴더. 비워 두면 사진\Captures 폴더에 저장합니다.
# 경로는 작은따옴표로 감싸세요. 예: save_dir = 'D:\캡쳐'
save_dir = ''

# 캡쳐 직후 클립보드에 올릴 것.
#   "image" : 이미지 (채팅 창 등에 바로 붙여넣기)
#   "path"  : 저장된 파일의 경로 (터미널 에이전트에 붙여넣기)
#   "none"  : 올리지 않음
copy = "image"

# 히스토리 창에 보여 줄 최근 캡쳐 개수 (1~100).
history_size = 20

# 히스토리 창에서 캡쳐 목록을 둘 자리.
#   "right" : 오른쪽에 세로로 (큰 미리보기는 왼쪽)
#   "top"   : 위쪽에 가로로 (큰 미리보기는 아래)
history_layout = "right"

# 전역 단축키. Ctrl, Alt, Shift, Win 과 글자·숫자·F1~F12 를 + 로 잇습니다.
# 비워 두면 그 단축키는 쓰지 않습니다.
[hotkeys]
region = "Ctrl+Alt+A"        # 영역 캡쳐
again = "Ctrl+Alt+S"         # 같은 영역 다시 캡쳐
history = "Ctrl+Alt+D"       # 캡쳐 히스토리 (편집기)
region_edit = "Ctrl+Alt+E"   # 영역 캡쳐 후 바로 편집기 열기
"#;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    save_dir: String,
    pub copy: CopyMode,
    pub history_size: usize,
    pub history_layout: HistoryLayout,
    pub hotkeys: Hotkeys,
}

/// 히스토리 창에서 캡쳐 목록이 놓이는 자리.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HistoryLayout {
    Right,
    Top,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Hotkeys {
    pub region: String,
    pub again: String,
    pub history: String,
    pub region_edit: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            save_dir: String::new(),
            copy: CopyMode::Image,
            history_size: 20,
            history_layout: HistoryLayout::Right,
            hotkeys: Hotkeys::default(),
        }
    }
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            region: "Ctrl+Alt+A".into(),
            again: "Ctrl+Alt+S".into(),
            history: "Ctrl+Alt+D".into(),
            region_edit: "Ctrl+Alt+E".into(),
        }
    }
}

impl Config {
    /// 설정 파일을 읽는다. 파일이 없으면 기본 파일을 만들고 기본값을 돌려준다.
    pub fn load() -> Result<Self> {
        let path = path()?;
        if !path.exists() {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, DEFAULT_FILE)
                .with_context(|| format!("설정 파일을 만들지 못했습니다: {}", path.display()))?;
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("설정 파일을 읽지 못했습니다: {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("설정 파일에 오류가 있습니다: {}", path.display()))
    }

    fn parse(text: &str) -> Result<Self> {
        let config: Config = toml::from_str(text)?;
        if !(1..=history::MAX_ENTRIES).contains(&config.history_size) {
            bail!("history_size 는 1~{} 사이여야 합니다", history::MAX_ENTRIES);
        }
        Ok(config)
    }

    /// 캡쳐를 저장할 폴더.
    pub fn save_dir(&self) -> Result<PathBuf> {
        match self.save_dir.trim() {
            "" => output::default_dir(),
            dir => Ok(PathBuf::from(dir)),
        }
    }
}

/// 설정 파일의 위치.
pub fn path() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "capture_tool")
        .context("설정 폴더를 찾지 못했습니다")?;
    Ok(dirs.config_dir().join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_file_matches_defaults() {
        assert_eq!(Config::parse(DEFAULT_FILE).unwrap(), Config::default());
    }

    #[test]
    fn empty_file_gives_defaults() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn overrides_are_read() {
        let config = Config::parse(
            "save_dir = 'D:\\캡쳐'\ncopy = \"path\"\nhistory_size = 5\n[hotkeys]\nagain = \"\"\n",
        )
        .unwrap();
        assert_eq!(config.save_dir().unwrap(), PathBuf::from("D:\\캡쳐"));
        assert_eq!(config.copy, CopyMode::Path);
        assert_eq!(config.history_size, 5);
        assert_eq!(config.hotkeys.again, "");
        assert_eq!(config.hotkeys.region, "Ctrl+Alt+A");
    }

    #[test]
    fn bad_values_are_rejected() {
        assert!(Config::parse("copy = \"jpg\"").is_err());
        assert!(Config::parse("history_size = 0").is_err());
        assert!(Config::parse("history_size = 101").is_err());
        assert!(Config::parse("history_layout = \"left\"").is_err());
        assert_eq!(
            Config::parse("history_layout = \"top\"").unwrap().history_layout,
            HistoryLayout::Top
        );
        assert!(Config::parse("hotkey = 1").is_err()); // 오타 난 항목 이름
    }
}
