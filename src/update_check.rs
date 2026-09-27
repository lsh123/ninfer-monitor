// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use thiserror::Error;

/// The GitHub releases API endpoint for the latest release (M8).
pub const RELEASES_API_URL: &str =
    "https://api.github.com/repos/lsh123/ninfer-monitor/releases/latest";
/// The GitHub releases page the "Release notes" link points at (M8).
pub const RELEASES_PAGE_URL: &str = "https://github.com/lsh123/ninfer-monitor/releases";

/// The supported update-check period range and the default (M8, in days).
pub const MIN_PERIOD_DAYS: u64 = 1;
pub const MAX_PERIOD_DAYS: u64 = 14;
pub const DEFAULT_PERIOD_DAYS: u64 = 3;

/// One day in milliseconds (the check period is stored in days).
pub const DAY_MS: u64 = 86_400_000;

/// The HTTP timeout for the release check (M8).
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// The total time allowed for the installer download (M8 §5.3): installers
/// are tens of megabytes, so the generous total timeout still bounds a
/// stalled connection.
pub const INSTALLER_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// The `User-Agent` sent with the release check (the GitHub API requires one).
const USER_AGENT: &str = concat!("ninfer-monitor/", env!("CARGO_PKG_VERSION"));

/// An error while checking for the latest release (M8).
#[derive(Debug, Error)]
pub enum CheckError {
    /// The HTTP request failed (network down, non-2xx status, timeout).
    #[error("HTTP request failed: {0}")]
    Http(String),
    /// The response was not a usable release object.
    #[error("malformed release response: {0}")]
    Parse(String),
}

/// An error while preparing the in-app update (M8 §5.3).
#[derive(Debug, Error)]
pub enum InstallError {
    /// The release has no installer asset for the current platform.
    #[error("this release has no installer for {0}")]
    NoAsset(&'static str),
    /// The download or the file write failed (network, timeout, disk).
    #[error("the installer download failed: {0}")]
    Download(String),
}

/// A downloadable release asset (M8 §5.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseAsset {
    /// The asset file name (e.g. `ninfer-monitor_0.3.0_x64-setup.exe`).
    pub name: String,
    /// The direct download URL.
    pub url: String,
}

/// The latest release fetched from the GitHub API (M8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestRelease {
    /// The release tag, as published (e.g. `v0.3.0`).
    pub tag: String,
    /// The normalized version (the tag with a leading `v`/`V` stripped).
    pub version: String,
    /// The release page URL (the release notes link).
    pub url: String,
    /// The downloadable assets; malformed entries are skipped.
    pub assets: Vec<ReleaseAsset>,
}

/// Strip a leading `v`/`V` from a release tag and return it trimmed.
pub fn normalize_version(tag: &str) -> String {
    let tag = tag.trim();
    tag.strip_prefix('v')
        .or_else(|| tag.strip_prefix('V'))
        .unwrap_or(tag)
        .to_owned()
}

/// Whether `candidate` is a strictly newer semantic version than `current`;
/// unparseable versions yield `false` (no false-positive update prompt).
pub fn is_newer(candidate: &str, current: &str) -> bool {
    matches!(
        compare_versions(candidate, current),
        Some(Ordering::Greater)
    )
}

/// Compare two semantic version strings in precedence order.
///
/// Versions with a pre-release part are earlier than the release without one
/// (`0.3.0-beta < 0.3.0`); pre-release identifiers compare dot-by-dot with
/// numeric identifiers ordered before alphanumeric ones (semver §11). Build
/// metadata (after `+`) is ignored. Returns `None` when either side is not a
/// parseable `major.minor.patch` version.
pub fn compare_versions(a: &str, b: &str) -> Option<Ordering> {
    let a = parse_semver(a)?;
    let b = parse_semver(b)?;
    let core = a.0.cmp(&b.0);
    if core != Ordering::Equal {
        return Some(core);
    }
    Some(pre_release_precedence(a.1, b.1))
}

/// The `(major, minor, patch)` core and the pre-release part (if any).
type SemVer<'a> = ((u64, u64, u64), Option<&'a str>);

/// The parsed form of a version string; `None` when the core is not three
/// numeric dot-separated components.
fn parse_semver(s: &str) -> Option<SemVer<'_>> {
    let s = s.trim();
    let s = s
        .strip_prefix('v')
        .or_else(|| s.strip_prefix('V'))
        .unwrap_or(s);
    let (core, pre) = match s.find(['-', '+']) {
        // `+` introduces build metadata, which never affects precedence.
        Some(i) if s.as_bytes()[i] == b'+' => (&s[..i], None),
        // `-` introduces the pre-release part, which runs up to `+` (if any).
        Some(i) => {
            let pre = s[i + 1..].split('+').next().unwrap_or(&s[i + 1..]);
            (&s[..i], if pre.is_empty() { None } else { Some(pre) })
        }
        None => (s, None),
    };
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(((major, minor, patch), pre))
}

/// Pre-release precedence for equal cores: no pre-release is later than any
/// pre-release, and two pre-releases compare their dot-separated
/// identifiers one by one.
fn pre_release_precedence(a: Option<&str>, b: Option<&str>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => {
            let (mut x, mut y) = (x.split('.'), y.split('.'));
            loop {
                match (x.next(), y.next()) {
                    (None, None) => return Ordering::Equal,
                    (Some(_), None) => return Ordering::Greater,
                    (None, Some(_)) => return Ordering::Less,
                    (Some(xi), Some(yi)) => {
                        let c = compare_identifier(xi, yi);
                        if c != Ordering::Equal {
                            return c;
                        }
                    }
                }
            }
        }
    }
}

/// Compare two pre-release identifiers: numeric identifiers are ordered
/// before alphanumeric ones and compare numerically among themselves
/// (semver §11.4).
fn compare_identifier(a: &str, b: &str) -> Ordering {
    match (a.parse::<u64>().ok(), b.parse::<u64>().ok()) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => a.cmp(b),
    }
}

/// Clamp a user-set update-check period (days) to the supported range (M8).
pub fn clamp_period_days(days: u64) -> u64 {
    days.clamp(MIN_PERIOD_DAYS, MAX_PERIOD_DAYS)
}

/// Whether a scheduled update check is due (M8 §5.1): due when no check has
/// succeeded yet, or at least `period_days` (clamped) have elapsed since the
/// last successful check.
pub fn is_check_due(last_check_unix_ms: Option<u64>, period_days: u64, now_unix_ms: u64) -> bool {
    let period_ms = clamp_period_days(period_days) * DAY_MS;
    match last_check_unix_ms {
        None => true,
        Some(last) => now_unix_ms.saturating_sub(last) >= period_ms,
    }
}

/// The current time in unix milliseconds (0 when the clock predates the epoch).
pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Parse the GitHub `/releases/latest` API body into a `LatestRelease` (M8).
///
/// Requires a non-empty `tag_name`; the release notes URL is the `html_url`
/// when present and valid, else the releases page plus the tag. Unknown
/// fields are ignored.
pub fn parse_latest_release(body: &str) -> Result<LatestRelease, CheckError> {
    let object: serde_json::Value =
        serde_json::from_str(body).map_err(|e| CheckError::Parse(e.to_string()))?;
    let object = object
        .as_object()
        .ok_or_else(|| CheckError::Parse("not an object".into()))?;
    let tag = object
        .get("tag_name")
        .and_then(|v| v.as_str())
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| CheckError::Parse("missing tag_name".into()))?;
    let url = object
        .get("html_url")
        .and_then(|v| v.as_str())
        .filter(|u| is_http_url(u))
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{RELEASES_PAGE_URL}/{tag}"));
    let assets = object
        .get("assets")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    let o = a.as_object()?;
                    let name = o.get("name")?.as_str()?.trim();
                    let url = o.get("browser_download_url")?.as_str()?;
                    if name.is_empty() || !is_http_url(url) {
                        return None;
                    }
                    Some(ReleaseAsset {
                        name: name.to_owned(),
                        url: url.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(LatestRelease {
        tag: tag.to_owned(),
        version: normalize_version(tag),
        url,
        assets,
    })
}

/// Whether `url` is an http(s) URL (the scheme filter for asset links).
fn is_http_url(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

/// Fetch and parse the latest release from the GitHub API (M8).
pub fn fetch_latest_release() -> Result<LatestRelease, CheckError> {
    let body = ureq::get(RELEASES_API_URL)
        .timeout(CHECK_TIMEOUT)
        .set("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| CheckError::Http(e.to_string()))?
        .into_string()
        .map_err(|e| CheckError::Http(e.to_string()))?;
    parse_latest_release(&body)
}

/// Check for a release newer than `current_version` (M8): fetches the latest
/// release and returns it when it is strictly newer, else `Ok(None)`.
pub fn check(current_version: &str) -> Result<Option<LatestRelease>, CheckError> {
    let latest = fetch_latest_release()?;
    Ok(if is_newer(&latest.version, current_version) {
        Some(latest)
    } else {
        None
    })
}

/// The installer file name the packager produces for `version` on the
/// current platform (M8 §5.3). The packager builds only the Windows NSIS
/// installer, so other platforms have no installer asset to match.
pub fn installer_asset_name(version: &str) -> Option<String> {
    if !cfg!(windows) {
        return None;
    }
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    };
    Some(format!("ninfer-monitor_{version}_{arch}-setup.exe"))
}

/// The installer asset of `release` for the current platform (M8 §5.3): the
/// exact packager file name first, then any same-version `*-setup.exe` asset
/// as a fallback for naming drift.
pub fn installer_asset(release: &LatestRelease) -> Option<&ReleaseAsset> {
    let exact = installer_asset_name(&release.version)?;
    release.assets.iter().find(|a| a.name == exact).or_else(|| {
        let prefix = format!("ninfer-monitor_{}_", release.version);
        release
            .assets
            .iter()
            .find(|a| a.name.starts_with(&prefix) && a.name.ends_with("-setup.exe"))
    })
}

/// The platform name for user-facing installer errors (M8 §5.3).
fn platform_name() -> &'static str {
    if cfg!(windows) {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else {
        "this platform"
    }
}

/// Download the release installer for the current platform to a file in the
/// system temp directory (M8 §5.3) and return its path. The file is kept for
/// the installer launch; a re-download overwrites any stale copy. The asset
/// name is used only as a file-name component (it comes from the GitHub
/// API), and a failed copy removes the partial file so a retry starts fresh.
pub fn download_installer(release: &LatestRelease) -> Result<PathBuf, InstallError> {
    let asset = installer_asset(release).ok_or(InstallError::NoAsset(platform_name()))?;
    let name = Path::new(&asset.name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned());
    let path = match name {
        Some(name) => std::env::temp_dir().join(name),
        None => std::env::temp_dir().join("installer.exe"),
    };
    let response = ureq::get(&asset.url)
        .timeout(INSTALLER_DOWNLOAD_TIMEOUT)
        .set("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| InstallError::Download(e.to_string()))?;
    let mut reader = response.into_reader();
    let mut file =
        std::fs::File::create(&path).map_err(|e| InstallError::Download(e.to_string()))?;
    let result =
        std::io::copy(&mut reader, &mut file).map_err(|e| InstallError::Download(e.to_string()));
    if result.is_err() {
        let _ = std::fs::remove_file(&path);
    }
    result.map(|_| path)
}

/// Open `url` in the system browser (M8 §5.3): `cmd /c start` on Windows,
/// `xdg-open` on Linux, `open` on macOS.
pub fn open_in_browser(url: &str) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        // `start` treats its first quoted argument as a window title, so an
        // empty title is passed before the URL.
        std::process::Command::new("cmd")
            .args(["/c", "start", "", url])
            .spawn()
            .map(|_| ())
    }
    #[cfg(not(windows))]
    {
        let program = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        std::process::Command::new(program)
            .arg(url)
            .spawn()
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_leading_v() {
        assert_eq!(normalize_version("v0.3.0"), "0.3.0");
        assert_eq!(normalize_version("V0.3.0"), "0.3.0");
        assert_eq!(normalize_version("0.3.0"), "0.3.0");
        assert_eq!(normalize_version("  v0.3.0  "), "0.3.0");
        assert_eq!(normalize_version("vv0.3.0"), "v0.3.0");
    }

    #[test]
    fn compare_semantic_ordering() {
        assert_eq!(compare_versions("0.3.0", "0.3.0"), Some(Ordering::Equal));
        assert_eq!(compare_versions("0.3.0", "0.2.9"), Some(Ordering::Greater));
        assert_eq!(compare_versions("0.2.0", "0.3.0"), Some(Ordering::Less));
        assert_eq!(
            compare_versions("1.0.0", "0.99.99"),
            Some(Ordering::Greater)
        );
        assert_eq!(compare_versions("0.10.0", "0.9.0"), Some(Ordering::Greater));
        assert_eq!(compare_versions("0.3.1", "0.3.0"), Some(Ordering::Greater));
        assert_eq!(compare_versions("v1.2.3", "1.2.3"), Some(Ordering::Equal));
    }

    #[test]
    fn compare_pre_release_is_before_release() {
        assert_eq!(
            compare_versions("0.3.0-beta", "0.3.0"),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare_versions("0.3.0", "0.3.0-beta"),
            Some(Ordering::Greater)
        );
        assert_eq!(
            compare_versions("0.3.0-beta.1", "0.3.0-beta.2"),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare_versions("0.3.0-beta.2", "0.3.0-beta.10"),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare_versions("0.3.0-beta.1", "0.3.0-beta.1"),
            Some(Ordering::Equal)
        );
        // Numeric identifiers sort before alphanumeric ones.
        assert_eq!(
            compare_versions("0.3.0-1", "0.3.0-alpha"),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare_versions("0.3.0-alpha", "0.3.0-beta"),
            Some(Ordering::Less)
        );
        // A longer pre-release with an equal prefix is later.
        assert_eq!(
            compare_versions("0.3.0-beta.2", "0.3.0-beta"),
            Some(Ordering::Greater)
        );
    }

    #[test]
    fn compare_ignores_build_metadata() {
        assert_eq!(
            compare_versions("0.3.0+build.5", "0.3.0"),
            Some(Ordering::Equal)
        );
        assert_eq!(
            compare_versions("0.3.0+build.5", "0.3.0+other"),
            Some(Ordering::Equal)
        );
    }

    #[test]
    fn compare_rejects_non_semver() {
        assert_eq!(compare_versions("abc", "0.3.0"), None);
        assert_eq!(compare_versions("0.3", "0.3.0"), None);
        assert_eq!(compare_versions("", "0.3.0"), None);
        assert_eq!(compare_versions("0.3.0.1", "0.3.0"), None);
        assert_eq!(compare_versions("0.3.-1", "0.3.0"), None);
        assert_eq!(compare_versions("0.3.0", "latest"), None);
    }

    #[test]
    fn is_newer_matrix() {
        assert!(is_newer("0.3.0", "0.2.0"));
        assert!(is_newer("v0.3.0", "0.2.0"));
        assert!(!is_newer("0.2.0", "0.3.0"));
        assert!(!is_newer("0.3.0", "0.3.0"));
        // A pre-release of the next version is still "newer" than the current
        // stable one, and a pre-release of the current version is not.
        assert!(is_newer("0.4.0-beta", "0.3.0"));
        assert!(!is_newer("0.3.0-beta", "0.3.0"));
        assert!(!is_newer("nonsense", "0.3.0"));
    }

    #[test]
    fn clamp_period_days_bounds() {
        assert_eq!(clamp_period_days(0), MIN_PERIOD_DAYS);
        assert_eq!(clamp_period_days(3), 3);
        assert_eq!(clamp_period_days(14), MAX_PERIOD_DAYS);
        assert_eq!(clamp_period_days(15), MAX_PERIOD_DAYS);
        assert_eq!(clamp_period_days(999), MAX_PERIOD_DAYS);
    }

    #[test]
    fn check_due_without_history() {
        assert!(is_check_due(None, 3, 0));
        assert!(is_check_due(None, 3, 1_700_000_000_000));
    }

    #[test]
    fn check_due_respects_period() {
        let now = 1_700_000_000_000u64;
        let period_3d = 3 * DAY_MS;
        // Exactly the period old -> due.
        assert!(is_check_due(Some(now - period_3d), 3, now));
        // One millisecond short -> not due.
        assert!(!is_check_due(Some(now - period_3d + 1), 3, now));
        // Fresh check -> not due.
        assert!(!is_check_due(Some(now), 3, now));
        // The next day after the period -> due.
        assert!(is_check_due(Some(now - period_3d - DAY_MS), 3, now));
    }

    #[test]
    fn check_due_clamps_period() {
        let now = 1_700_000_000_000u64;
        // Period 0 is clamped to 1 day: due after a day, not before.
        assert!(is_check_due(Some(now - DAY_MS), 0, now));
        assert!(!is_check_due(Some(now - 1), 0, now));
        // Period 99 is clamped to 14 days: not due after 10 days.
        assert!(!is_check_due(Some(now - 10 * DAY_MS), 99, now));
        assert!(is_check_due(Some(now - 14 * DAY_MS), 99, now));
    }

    #[test]
    fn check_due_tolerates_clock_regression() {
        // `last` in the future (clock skew) -> not due, no overflow.
        assert!(!is_check_due(Some(u64::MAX), 3, 100));
    }

    #[test]
    fn now_is_reasonable() {
        let now = now_unix_ms();
        // Well after 2020-01-01 and before 2100-01-01.
        assert!(now > 1_577_836_800_000);
        assert!(now < 4_102_444_800_000);
    }

    fn release_body(tag: &str, html_url: Option<&str>) -> String {
        let mut value = serde_json::json!({
            "tag_name": tag,
            "name": "Release",
            "draft": false,
            "prerelease": false,
        });
        if let Some(url) = html_url {
            value["html_url"] = serde_json::json!(url);
        }
        value.to_string()
    }

    #[test]
    fn parse_latest_release_uses_html_url() {
        let body = release_body(
            "v0.3.0",
            Some("https://github.com/lsh123/ninfer-monitor/releases/tag/v0.3.0"),
        );
        let release = parse_latest_release(&body).unwrap();
        assert_eq!(
            release,
            LatestRelease {
                tag: "v0.3.0".to_owned(),
                version: "0.3.0".to_owned(),
                url: "https://github.com/lsh123/ninfer-monitor/releases/tag/v0.3.0".to_owned(),
                assets: Vec::new(),
            }
        );
    }

    #[test]
    fn parse_latest_release_derives_url_without_html_url() {
        let body = release_body("v1.0.0", None);
        let release = parse_latest_release(&body).unwrap();
        assert_eq!(
            release.url,
            "https://github.com/lsh123/ninfer-monitor/releases/v1.0.0"
        );
    }

    #[test]
    fn parse_latest_release_ignores_invalid_html_url() {
        let body = release_body("v1.0.0", Some("javascript:alert(1)"));
        let release = parse_latest_release(&body).unwrap();
        assert_eq!(
            release.url,
            "https://github.com/lsh123/ninfer-monitor/releases/v1.0.0"
        );
    }

    #[test]
    fn parse_latest_release_requires_tag() {
        assert!(matches!(
            parse_latest_release(r#"{"name":"Release"}"#),
            Err(CheckError::Parse(_))
        ));
        assert!(matches!(
            parse_latest_release(r#"{"tag_name":""}"#),
            Err(CheckError::Parse(_))
        ));
        assert!(matches!(
            parse_latest_release(r#"{"tag_name":"   "}"#),
            Err(CheckError::Parse(_))
        ));
        // The tag is trimmed and normalized into the version.
        assert_eq!(
            parse_latest_release(r#"{"tag_name":"v1"}"#)
                .unwrap()
                .version,
            "1"
        );
    }

    #[test]
    fn parse_latest_release_rejects_non_json() {
        assert!(matches!(
            parse_latest_release("<html>rate limited</html>"),
            Err(CheckError::Parse(_))
        ));
        assert!(matches!(
            parse_latest_release("[1, 2]"),
            Err(CheckError::Parse(_))
        ));
    }

    #[test]
    fn parse_latest_release_collects_valid_assets_only() {
        let body = r#"{
            "tag_name": "v0.3.0",
            "assets": [
                {"name": "ninfer-monitor_0.3.0_x64-setup.exe",
                 "browser_download_url": "https://github.com/lsh123/ninfer-monitor/releases/download/v0.3.0/ninfer-monitor_0.3.0_x64-setup.exe"},
                {"name": "source.zip",
                 "browser_download_url": "https://github.com/lsh123/ninfer-monitor/archive/refs/tags/v0.3.0.zip"},
                {"name": "bad-scheme.exe",
                 "browser_download_url": "javascript:alert(1)"},
                {"name": "no-url"},
                "not-an-object"
            ]
        }"#;
        let release = parse_latest_release(body).unwrap();
        assert_eq!(
            release.assets,
            vec![
                ReleaseAsset {
                    name: "ninfer-monitor_0.3.0_x64-setup.exe".to_owned(),
                    url: "https://github.com/lsh123/ninfer-monitor/releases/download/v0.3.0/ninfer-monitor_0.3.0_x64-setup.exe"
                        .to_owned(),
                },
                ReleaseAsset {
                    name: "source.zip".to_owned(),
                    url: "https://github.com/lsh123/ninfer-monitor/archive/refs/tags/v0.3.0.zip"
                        .to_owned(),
                },
            ]
        );
    }

    fn release_with_assets(version: &str, names: &[&str]) -> LatestRelease {
        LatestRelease {
            tag: format!("v{version}"),
            version: version.to_owned(),
            url: format!("{RELEASES_PAGE_URL}/{version}"),
            assets: names
                .iter()
                .map(|name| ReleaseAsset {
                    name: (*name).to_owned(),
                    url: format!("https://github.com/lsh123/ninfer-monitor/releases/download/v{version}/{name}"),
                })
                .collect(),
        }
    }

    #[test]
    fn installer_asset_name_is_the_packager_output() {
        #[cfg(windows)]
        {
            assert_eq!(
                installer_asset_name("0.3.0"),
                Some(format!(
                    "ninfer-monitor_0.3.0_{}-setup.exe",
                    if cfg!(target_arch = "aarch64") {
                        "arm64"
                    } else {
                        "x64"
                    }
                ))
            );
        }
        #[cfg(not(windows))]
        {
            assert_eq!(installer_asset_name("0.3.0"), None);
        }
    }

    #[cfg(windows)]
    #[test]
    fn installer_asset_prefers_the_exact_name() {
        let release = release_with_assets(
            "0.3.0",
            &[
                "ninfer-monitor_0.3.0_x64-setup.exe",
                "ninfer-monitor_0.3.0_x64-setup.exe.old",
            ],
        );
        let asset = installer_asset(&release);
        assert_eq!(
            asset.map(|a| a.name.as_str()),
            Some("ninfer-monitor_0.3.0_x64-setup.exe")
        );
    }

    #[cfg(windows)]
    #[test]
    fn installer_asset_falls_back_to_version_suffix() {
        // A same-version `*-setup.exe` asset matches when the exact name
        // differs (naming drift); a different version does not.
        let release = release_with_assets(
            "0.3.0",
            &[
                "ninfer-monitor_0.3.0_custom-setup.exe",
                "ninfer-monitor_0.4.0_x64-setup.exe",
                "source.zip",
            ],
        );
        let asset = installer_asset(&release);
        assert_eq!(
            asset.map(|a| a.name.as_str()),
            Some("ninfer-monitor_0.3.0_custom-setup.exe")
        );
    }

    #[test]
    fn installer_asset_is_none_without_a_matching_asset() {
        let release = release_with_assets("0.3.0", &["source.zip"]);
        assert!(installer_asset(&release).is_none());
        #[cfg(not(windows))]
        {
            // No installer is built for non-Windows platforms, even if the
            // release carries the Windows asset.
            let release = release_with_assets("0.3.0", &["ninfer-monitor_0.3.0_x64-setup.exe"]);
            assert!(installer_asset(&release).is_none());
        }
    }

    #[test]
    fn download_installer_without_a_matching_asset_fails_offline() {
        // The asset lookup fails before any network access.
        let release = release_with_assets("0.3.0", &[]);
        assert!(matches!(
            download_installer(&release),
            Err(InstallError::NoAsset(_))
        ));
    }
}
