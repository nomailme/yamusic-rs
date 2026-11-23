//! Обновления через релизы GitHub, как у оригинала: проверка раз в полчаса, тост
//! «Доступна версия …» с кнопкой «Обновить». Приложение — один exe, поэтому кнопка
//! скачивает `yamusic-rs.exe` нового релиза, сверяет SHA-256 с тем, что показывает GitHub,
//! и подменяет собой запущенный файл: Windows не даёт перезаписать работающий exe, но даёт
//! его переименовать. Старый файл (`*.old.exe`) удаляется при следующем запуске.
//! Если папка недоступна для записи, кнопка открывает страницу релиза. На Linux — всегда её.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Как часто спрашивать GitHub. У оригинала UPDATE_POLL_INTERVAL_MS — те же 30 минут.
pub const POLL: Duration = Duration::from_secs(30 * 60);

/// Имя exe в релизе.
const ASSET: &str = concat!(env!("CARGO_PKG_NAME"), ".exe");
/// APK в релизе. На Android «Обновить» ведёт прямо на него: браузер скачает и предложит установить.
const APK: &str = "yamusic-rs-arm64-v8a.apk";

/// Проверка включена в настройках («Проверять обновления»).
pub static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Страница GitHub со всеми изменениями между запущенной версией и новой.
pub fn changes_url(version: &str) -> String {
    format!("{}/compare/v{}...v{version}", env!("CARGO_PKG_REPOSITORY"), env!("CARGO_PKG_VERSION"))
}

/// Аргумент, с которым обновлённый exe ждёт, пока закроется старый.
pub const AFTER_UPDATE: &str = "--after-update";

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    /// Страница релиза на GitHub.
    pub page: String,
    /// Exe: адрес, размер и SHA-256. Нет — обновить можно только руками.
    pub exe: Option<(String, u64, String)>,
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        // GitHub API без User-Agent отвечает 403.
        .user_agent(concat!("yamusic-rs/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())
}

/// «v1.2.3» / «1.2.3» → (1, 2, 3). Пре-релизы (`/releases/latest` их и так не отдаёт) не нужны.
fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let mut it = s.trim().trim_start_matches('v').split(['.', '-']).map(|p| p.parse::<u32>());
    Some((it.next()?.ok()?, it.next()?.ok()?, it.next().unwrap_or(Ok(0)).ok()?))
}

/// Авто-проверка отключена: сообщаем, что обновление не требуется, без сетевого запроса.
pub fn check() -> Result<Option<Release>, String> {
    Ok(None)
}

fn old_path(exe: &Path) -> PathBuf {
    exe.with_extension("old.exe")
}

/// Скачать новый exe, проверить и поставить на место запущенного. Долго: звать из фонового
/// потока. После успеха нужно запустить [`relaunch`] и закрыться.
pub fn install(r: &Release) -> Result<PathBuf, String> {
    let (url, size, sha) = r.exe.as_ref().ok_or("у релиза нет exe")?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let bytes = client()?
        .get(url)
        .timeout(Duration::from_secs(600))
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.bytes())
        .map_err(|e| format!("загрузка: {}", crate::config::chain(&e)))?;
    if bytes.len() as u64 != *size {
        return Err("файл скачался не целиком".into());
    }
    let got: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    if &got != sha {
        return Err("контрольная сумма не совпала".into());
    }
    let new = exe.with_extension("new.exe");
    std::fs::write(&new, &bytes).map_err(|e| format!("нет доступа к папке {}: {e}", exe.parent().unwrap_or(&exe).display()))?;
    let old = old_path(&exe);
    let _ = std::fs::remove_file(&old);
    if let Err(e) = std::fs::rename(&exe, &old) {
        let _ = std::fs::remove_file(&new);
        return Err(format!("не удалось заменить exe: {e}"));
    }
    if let Err(e) = std::fs::rename(&new, &exe) {
        let _ = std::fs::rename(&old, &exe);
        return Err(format!("не удалось заменить exe: {e}"));
    }
    Ok(exe)
}

/// Запустить обновлённый exe. Он дождётся, пока этот процесс закроется.
pub fn relaunch(exe: &Path) -> Result<(), String> {
    std::process::Command::new(exe)
        .arg(AFTER_UPDATE)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("новая версия не запустилась: {e}"))
}

/// Убрать exe прошлой версии, оставшийся после обновления. Старый процесс может ещё
/// завершаться и держать файл, поэтому пробуем несколько раз в фоне.
pub fn cleanup() {
    let Ok(exe) = std::env::current_exe() else { return };
    let old = old_path(&exe);
    if !old.exists() {
        return;
    }
    std::thread::spawn(move || {
        for _ in 0..10 {
            if std::fs::remove_file(&old).is_ok() || !old.exists() {
                return;
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse_version("1.10"), Some((1, 10, 0)));
        assert!(parse_version("v0.10.0") > parse_version("0.9.9"));
        assert_eq!(parse_version("latest"), None);
    }

    #[test]
    fn update_check_is_disabled_without_network() {
        assert!(check().unwrap().is_none());
    }

    #[test]
    fn paths() {
        assert_eq!(old_path(Path::new(r"C:\a\yamusic-rs.exe")), Path::new(r"C:\a\yamusic-rs.old.exe"));
        assert_eq!(ASSET, "yamusic-rs.exe");
    }
}
