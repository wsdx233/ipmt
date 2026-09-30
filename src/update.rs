use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use directories::BaseDirs;
use reqwest::blocking::Client;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::{NamedTempFile, tempdir};

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const LATEST_RELEASE: &str = "https://api.github.com/repos/wsdx233/ipmt/releases/latest";

#[derive(Debug)]
pub struct UpdateInfo {
    pub version: String,
    pub release_url: String,
    archive: ReleaseAsset,
    checksum: ReleaseAsset,
}

#[derive(Debug, Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<ReleaseAsset>,
}

#[derive(Default, Deserialize, Serialize)]
struct UpdatePreferences {
    #[serde(default)]
    ignored_version: Option<String>,
}

fn client() -> Result<Client> {
    Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("ipmt/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("无法初始化 GitHub 更新客户端")
}

fn release_target() -> Result<&'static str> {
    if cfg!(all(
        target_os = "linux",
        target_arch = "x86_64",
        target_env = "gnu"
    )) {
        Ok("x86_64-unknown-linux-gnu")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Ok("x86_64-apple-darwin")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok("aarch64-apple-darwin")
    } else if cfg!(all(
        target_os = "windows",
        target_arch = "x86_64",
        target_env = "msvc"
    )) {
        Ok("x86_64-pc-windows-msvc")
    } else {
        bail!("当前平台没有可用的 GitHub 发布包，请从源码安装")
    }
}

fn parse_version(version: &str) -> Result<Version> {
    Version::parse(version.strip_prefix('v').unwrap_or(version))
        .with_context(|| format!("无效的版本号：{version}"))
}

fn update_from_release(
    release: GitHubRelease,
    current_version: &str,
    target: &str,
) -> Result<Option<UpdateInfo>> {
    if release.draft || release.prerelease {
        return Ok(None);
    }
    let version = parse_version(&release.tag_name)?;
    let current = parse_version(current_version)?;
    if !version.pre.is_empty() || !version.cmp_precedence(&current).is_gt() {
        return Ok(None);
    }

    let extension = if target.ends_with("windows-msvc") {
        "zip"
    } else {
        "tar.gz"
    };
    let archive_name = format!("ipmt-{target}.{extension}");
    let checksum_name = format!("ipmt-{target}.sha256");
    let mut archive = None;
    let mut checksum = None;
    for asset in release.assets {
        if asset.name == archive_name {
            archive = Some(asset);
        } else if asset.name == checksum_name {
            checksum = Some(asset);
        }
    }
    let archive = archive.with_context(|| format!("最新 Release 缺少 {archive_name}"))?;
    let checksum = checksum.with_context(|| format!("最新 Release 缺少 {checksum_name}"))?;
    Ok(Some(UpdateInfo {
        version: version.to_string(),
        release_url: release.html_url,
        archive,
        checksum,
    }))
}

pub fn check_for_update() -> Result<Option<UpdateInfo>> {
    let target = release_target()?;
    let release = client()?
        .get(LATEST_RELEASE)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .context("无法连接 GitHub 检查更新")?
        .error_for_status()
        .context("GitHub 更新检查失败")?
        .json::<GitHubRelease>()
        .context("无法读取 GitHub Release 信息")?;
    update_from_release(release, CURRENT_VERSION, target)
}

fn preferences_path() -> Result<PathBuf> {
    Ok(BaseDirs::new()
        .context("无法确定用户配置目录")?
        .config_dir()
        .join("ipmt/update.json"))
}

fn load_preferences(path: &Path) -> Result<UpdatePreferences> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("无法解析更新设置 {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(UpdatePreferences::default())
        }
        Err(error) => Err(error).with_context(|| format!("无法读取更新设置 {}", path.display())),
    }
}

fn filter_ignored(info: Option<UpdateInfo>, preferences: &UpdatePreferences) -> Option<UpdateInfo> {
    info.filter(|info| preferences.ignored_version.as_deref() != Some(info.version.as_str()))
}

pub fn check_for_startup() -> Result<Option<UpdateInfo>> {
    let info = check_for_update()?;
    if info.is_none() {
        return Ok(None);
    }
    Ok(filter_ignored(
        info,
        &load_preferences(&preferences_path()?)?,
    ))
}

fn save_ignored_version(path: &Path, version: &str) -> Result<()> {
    let mut preferences = load_preferences(path)?;
    preferences.ignored_version = Some(parse_version(version)?.to_string());
    let parent = path.parent().context("更新设置缺少父目录")?;
    fs::create_dir_all(parent).context("无法创建更新设置目录")?;
    let mut temporary = NamedTempFile::new_in(parent).context("无法创建更新设置临时文件")?;
    serde_json::to_writer_pretty(&mut temporary, &preferences)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).context("无法保存更新设置")?;
    Ok(())
}

pub fn ignore_version(version: &str) -> Result<()> {
    save_ignored_version(&preferences_path()?, version)
}

fn expected_checksum(text: &str, archive_name: &str) -> Result<String> {
    for line in text.lines() {
        let Some((hash, name)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let name = name.trim().strip_prefix('*').unwrap_or(name.trim());
        if name != archive_name {
            continue;
        }
        if hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(hash.to_ascii_lowercase());
        }
        bail!("发布包的 SHA-256 校验值无效");
    }
    bail!("SHA-256 校验文件中没有 {archive_name}")
}

fn copy_verified_archive(
    source: &mut impl Read,
    destination: &mut impl Write,
    expected: &str,
) -> Result<()> {
    let mut hasher = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = source.read(&mut buffer).context("下载发布包失败")?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        destination
            .write_all(&buffer[..count])
            .context("无法写入发布包")?;
    }
    if format!("{:x}", hasher.finalize()) != expected {
        bail!("SHA-256 校验失败，未替换现有程序");
    }
    Ok(())
}

#[cfg(unix)]
fn extract_executable(archive_path: &Path, destination: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let decoder = flate2::read::GzDecoder::new(File::open(archive_path)?);
    let mut archive = tar::Archive::new(decoder);
    let mut found = false;
    for entry in archive.entries().context("无法读取发布包")? {
        let mut entry = entry?;
        let path = entry.path()?;
        if path.as_ref() != Path::new("ipmt") && path.as_ref() != Path::new("./ipmt") {
            continue;
        }
        if found || !entry.header().entry_type().is_file() {
            bail!("发布包中的 ipmt 不是唯一的普通文件");
        }
        let mut output = File::create(destination)?;
        if std::io::copy(&mut entry, &mut output)? == 0 {
            bail!("发布包中的 ipmt 是空文件");
        }
        output.sync_all()?;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o755))?;
        found = true;
    }
    if !found {
        bail!("发布包中没有找到 ipmt");
    }
    Ok(())
}

#[cfg(windows)]
fn extract_executable(archive_path: &Path, destination: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(File::open(archive_path)?)?;
    let mut found = false;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if entry.name() != "ipmt.exe" {
            continue;
        }
        if found
            || !entry.is_file()
            || entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            bail!("发布包中的 ipmt.exe 不是唯一的普通文件");
        }
        let mut output = File::create(destination)?;
        if std::io::copy(&mut entry, &mut output)? == 0 {
            bail!("发布包中的 ipmt.exe 是空文件");
        }
        output.sync_all()?;
        found = true;
    }
    if !found {
        bail!("发布包中没有找到 ipmt.exe");
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn extract_executable(_archive_path: &Path, _destination: &Path) -> Result<()> {
    bail!("当前平台不支持自动更新")
}

pub fn install_update(info: &UpdateInfo) -> Result<()> {
    let client = client()?;
    let checksum = client
        .get(&info.checksum.browser_download_url)
        .send()?
        .error_for_status()?
        .text()
        .context("无法下载 SHA-256 校验文件")?;
    let expected = expected_checksum(&checksum, &info.archive.name)?;
    let directory = tempdir().context("无法创建更新临时目录")?;
    let archive_path = directory.path().join(&info.archive.name);
    let mut archive = File::create(&archive_path)?;
    let mut response = client
        .get(&info.archive.browser_download_url)
        .timeout(Duration::from_secs(180))
        .send()?
        .error_for_status()
        .context("无法下载 GitHub 发布包")?;
    copy_verified_archive(&mut response, &mut archive, &expected)?;
    archive.sync_all()?;
    drop(archive);
    let executable = directory
        .path()
        .join(if cfg!(windows) { "ipmt.exe" } else { "ipmt" });
    extract_executable(&archive_path, &executable)?;
    let current = std::env::current_exe().context("无法确定当前程序路径")?;
    self_replace::self_replace(&executable)
        .with_context(|| format!("无法替换 {}；请检查安装目录的写入权限", current.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARGET: &str = "x86_64-unknown-linux-gnu";

    fn release(version: &str) -> GitHubRelease {
        GitHubRelease {
            tag_name: version.into(),
            html_url: format!("https://github.com/wsdx233/ipmt/releases/tag/{version}"),
            draft: false,
            prerelease: false,
            assets: ["tar.gz", "sha256"].into_iter().map(|extension| ReleaseAsset {
                name: format!("ipmt-{TARGET}.{extension}"),
                browser_download_url: format!("https://github.com/wsdx233/ipmt/releases/download/{version}/ipmt-{TARGET}.{extension}"),
            }).collect(),
        }
    }

    #[test]
    fn upgrades_use_semver_precedence_and_only_stable_releases() {
        assert_eq!(
            update_from_release(release("v0.10.0"), "0.9.9", TARGET)
                .unwrap()
                .unwrap()
                .version,
            "0.10.0"
        );
        for (remote, local) in [
            ("v0.9.0", "0.10.0"),
            ("v1.0.0", "1.0.0"),
            ("v1.0.0+new", "1.0.0+old"),
            ("v2.0.0-rc.1", "1.0.0"),
        ] {
            assert!(
                update_from_release(release(remote), local, TARGET)
                    .unwrap()
                    .is_none()
            );
        }
        assert!(
            update_from_release(release("v1.0.0"), "1.0.0-rc.1", TARGET)
                .unwrap()
                .is_some()
        );
        let mut draft = release("v2.0.0");
        draft.draft = true;
        assert!(
            update_from_release(draft, "1.0.0", TARGET)
                .unwrap()
                .is_none()
        );
        let mut prerelease = release("v2.0.0");
        prerelease.prerelease = true;
        assert!(
            update_from_release(prerelease, "1.0.0", TARGET)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn newer_release_requires_both_compatible_archive_and_checksum() {
        for missing in ["tar.gz", "sha256"] {
            let mut incomplete = release("v1.0.0");
            incomplete
                .assets
                .retain(|asset| !asset.name.ends_with(missing));
            assert!(update_from_release(incomplete, "0.1.0", TARGET).is_err());
        }
        assert!(update_from_release(release("v1.0.0"), "0.1.0", "aarch64-apple-darwin").is_err());
    }

    #[test]
    fn ignored_version_persists_but_does_not_hide_later_versions() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("ipmt/update.json");
        save_ignored_version(&path, "v1.0.0").unwrap();
        let preferences = load_preferences(&path).unwrap();
        assert!(
            filter_ignored(
                update_from_release(release("v1.0.0"), "0.1.0", TARGET).unwrap(),
                &preferences
            )
            .is_none()
        );
        assert_eq!(
            filter_ignored(
                update_from_release(release("v1.0.1"), "0.1.0", TARGET).unwrap(),
                &preferences
            )
            .unwrap()
            .version,
            "1.0.1"
        );
    }

    #[test]
    fn invalid_preferences_are_not_overwritten() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("update.json");
        fs::write(&path, b"broken json").unwrap();
        assert!(save_ignored_version(&path, "1.0.0").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"broken json");
    }

    #[test]
    fn checksum_is_bound_to_archive_and_rejects_corruption() {
        let hash = format!("{:x}", Sha256::digest(b"archive"));
        let expected = expected_checksum(&format!("{hash}  ipmt.tar.gz\n"), "ipmt.tar.gz").unwrap();
        assert!(expected_checksum(&format!("{hash}  other.tar.gz\n"), "ipmt.tar.gz").is_err());
        assert!(expected_checksum("not-a-hash  ipmt.tar.gz", "ipmt.tar.gz").is_err());
        let mut destination = Vec::new();
        copy_verified_archive(&mut &b"archive"[..], &mut destination, &expected).unwrap();
        assert!(copy_verified_archive(&mut &b"corrupted"[..], &mut Vec::new(), &expected).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn archives_reject_symlinks_and_missing_executable() {
        let directory = tempdir().unwrap();
        let archive_path = directory.path().join("archive.tar.gz");
        let executable = directory.path().join("ipmt");
        let encoder = flate2::write::GzEncoder::new(
            File::create(&archive_path).unwrap(),
            flate2::Compression::default(),
        );
        let mut archive = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o755);
        archive.append_link(&mut header, "ipmt", "/bin/sh").unwrap();
        archive.into_inner().unwrap().finish().unwrap();
        assert!(extract_executable(&archive_path, &executable).is_err());
        assert!(!executable.exists());

        let encoder = flate2::write::GzEncoder::new(
            File::create(&archive_path).unwrap(),
            flate2::Compression::default(),
        );
        tar::Builder::new(encoder)
            .into_inner()
            .unwrap()
            .finish()
            .unwrap();
        assert!(extract_executable(&archive_path, &executable).is_err());
        assert!(!executable.exists());
    }
}
