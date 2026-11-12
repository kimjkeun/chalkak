//! Windows 시작 시 자동 실행. 현재 사용자의 레지스트리 Run 키에 트레이 프로그램을 등록한다.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "chalkak";
const TRAY_EXE: &str = "chalkak_tray.exe";

/// 지금 실행 중인 프로그램과 같은 폴더에 있는 트레이 프로그램.
pub fn tray_exe() -> Result<PathBuf> {
    let current = std::env::current_exe().context("실행 파일 위치를 알 수 없습니다")?;
    let exe = current.with_file_name(TRAY_EXE);
    if !exe.exists() {
        bail!("트레이 프로그램을 찾지 못했습니다: {}", exe.display());
    }
    Ok(exe)
}

/// 등록되어 있는 실행 명령. 등록되어 있지 않으면 `None`.
pub fn registered() -> Result<Option<String>> {
    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(RUN_KEY, KEY_READ)
        .context("자동 실행 설정을 읽지 못했습니다")?;
    match key.get_value::<String, _>(VALUE_NAME) {
        Ok(command) => Ok(Some(command)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context("자동 실행 설정을 읽지 못했습니다"),
    }
}

/// `exe`가 자동 실행으로 등록되어 있는지.
pub fn is_enabled_for(exe: &Path) -> Result<bool> {
    Ok(registered()?.is_some_and(|command| command.eq_ignore_ascii_case(&command_for(exe))))
}

pub fn enable(exe: &Path) -> Result<()> {
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey(RUN_KEY)
        .context("자동 실행을 등록하지 못했습니다")?;
    key.set_value(VALUE_NAME, &command_for(exe))
        .context("자동 실행을 등록하지 못했습니다")
}

pub fn disable() -> Result<()> {
    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)
        .context("자동 실행을 해제하지 못했습니다")?;
    match key.delete_value(VALUE_NAME) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).context("자동 실행을 해제하지 못했습니다"),
    }
}

fn command_for(exe: &Path) -> String {
    format!("\"{}\"", exe.display())
}
