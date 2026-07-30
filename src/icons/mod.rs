mod cache;
mod desktop_entries;
mod icon_theme;

use std::{
    io::Cursor,
    path::{Path, PathBuf},
    sync::{LazyLock, RwLock},
    time::UNIX_EPOCH,
};

macro_rules! icon_debug {
    ($($argument:tt)*) => {
        if cfg!(debug_assertions) {
            eprintln!($($argument)*);
        }
    };
}

use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{DynamicImage, GenericImageView, ImageFormat, RgbaImage, imageops::FilterType};
use resvg::{tiny_skia, usvg};

use crate::audio::AppInfo;
use cache::{IconCache, cache_key};
use desktop_entries::{
    AppMetadata, DesktopEntry, application_dirs, load_entries, select_entry, select_steam_entry,
};
use icon_theme::{TARGET_SIZE, active_theme, icon_roots, resolve_icon};

static DESKTOP_ENTRIES: LazyLock<Vec<DesktopEntry>> =
    LazyLock::new(|| load_entries(&application_dirs()));
static CACHE: LazyLock<IconCache> = LazyLock::new(IconCache::default);
static SOURCE_CACHE: LazyLock<RwLock<std::collections::HashMap<String, Option<PathBuf>>>> =
    LazyLock::new(|| RwLock::new(std::collections::HashMap::new()));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconSourceKind {
    DesktopEntry,
    ApplicationMetadata,
    MediaMetadata,
    Fallback,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct ResolvedAppIcon {
    pub desktop_file_id: Option<String>,
    pub desktop_file_path: Option<PathBuf>,
    pub icon_key: Option<String>,
    pub source_path: Option<PathBuf>,
    pub rendered_data_uri: String,
    pub source_kind: IconSourceKind,
}

pub fn steam_app_id(app: &AppInfo) -> Option<String> {
    for key in [
        "steam.app.id",
        "SteamAppId",
        "SteamGameId",
        "SteamOverlayGameId",
        "STEAM_COMPAT_APP_ID",
    ] {
        if let Some(id) = app
            .metadata
            .get(key)
            .and_then(|value| parse_steam_id(value))
        {
            return Some(id);
        }
    }
    let pid = app
        .metadata
        .get("application.process.id")
        .and_then(|value| value.parse::<u32>().ok())?;
    steam_id_from_process_chain(pid, 8).or_else(|| {
        let executable = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
        steam_id_from_executable(&executable, &steam_library_paths())
    })
}

fn steam_id_from_process_chain(mut pid: u32, limit: usize) -> Option<String> {
    for _ in 0..limit {
        if let Some(id) = std::fs::read(format!("/proc/{pid}/environ"))
            .ok()
            .and_then(|environment| steam_id_from_environment(&environment))
        {
            return Some(id);
        }
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        pid = status
            .lines()
            .find_map(|line| line.strip_prefix("PPid:"))
            .and_then(|value| value.trim().parse().ok())?;
        if pid == 0 {
            break;
        }
    }
    None
}

fn parse_steam_id(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value.chars().all(|character| character.is_ascii_digit()))
        .then(|| value.to_owned())
}

fn steam_id_from_environment(environment: &[u8]) -> Option<String> {
    for entry in environment.split(|byte| *byte == 0) {
        let Ok(entry) = std::str::from_utf8(entry) else {
            continue;
        };
        let Some((key, value)) = entry.split_once('=') else {
            continue;
        };
        if matches!(
            key,
            "SteamAppId"
                | "SteamGameId"
                | "SteamOverlayGameId"
                | "STEAM_COMPAT_APP_ID"
                | "SteamCompatAppId"
        ) && let Some(id) = parse_steam_id(value)
        {
            return Some(id);
        }
    }
    None
}

fn steam_library_paths() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let mut libraries = vec![
        home.join(".local/share/Steam"),
        home.join(".steam/steam"),
        home.join(".var/app/com.valvesoftware.Steam/data/Steam"),
    ];
    for config in [
        home.join(".local/share/Steam/steamapps/libraryfolders.vdf"),
        home.join(".local/share/Steam/config/libraryfolders.vdf"),
    ] {
        let Ok(contents) = std::fs::read_to_string(config) else {
            continue;
        };
        for line in contents.lines() {
            if let Some((_, value)) = line.split_once("\"path\"")
                && let Some(path) = value.split('"').nth(1)
            {
                libraries.push(PathBuf::from(path.replace("\\\\", "\\")));
            }
        }
    }
    libraries.sort();
    libraries.dedup();
    libraries
}

fn steam_id_from_executable(executable: &Path, libraries: &[PathBuf]) -> Option<String> {
    for library in libraries {
        let Ok(manifests) = std::fs::read_dir(library.join("steamapps")) else {
            continue;
        };
        for manifest in manifests.flatten() {
            let path = manifest.path();
            let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            let Some(app_id) = name
                .strip_prefix("appmanifest_")
                .and_then(|value| value.strip_suffix(".acf"))
            else {
                continue;
            };
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some(install_dir) = contents.lines().find_map(|line| {
                let (key, rest) = line.trim().split_once(char::is_whitespace)?;
                key.trim_matches('"')
                    .eq_ignore_ascii_case("installdir")
                    .then(|| rest.trim().trim_matches('"'))
            }) else {
                continue;
            };
            if executable.starts_with(library.join("steamapps/common").join(install_dir)) {
                return parse_steam_id(app_id);
            }
        }
    }
    None
}

pub fn stable_application_id(app: &AppInfo) -> String {
    steam_app_id(app).map_or_else(
        || format!("application\u{1f}{}", app.app_name),
        |id| format!("steam-app:{id}"),
    )
}

fn metadata(app: &AppInfo) -> AppMetadata {
    let get = |key: &str| app.metadata.get(key).cloned();
    AppMetadata {
        desktop: get("application.desktop"),
        application_id: get("application.id"),
        application_name: app.app_name.clone(),
        process_binary: get("application.process.binary"),
        process_id: get("application.process.id").and_then(|value| value.parse().ok()),
        startup_wm_class: get("window.x11.class")
            .or_else(|| get("window.x11.instance"))
            .or_else(|| get("application.wmclass")),
        flatpak_id: get("flatpak.app-id").or_else(|| get("application.flatpak.id")),
    }
}

pub fn resolve_app_icon(app: &AppInfo) -> ResolvedAppIcon {
    let theme = active_theme();
    let mut metadata = metadata(app);
    let steam_id = steam_app_id(app);
    if let Some(id) = steam_id.as_ref() {
        metadata.desktop = Some(format!("steam_app_{id}.desktop"));
        icon_debug!(
            "[volume-icon] process={} steam_app_id={id}",
            metadata.process_binary.as_deref().unwrap_or(&app.app_name)
        );
    }
    let desktop_match = steam_id
        .as_deref()
        .and_then(|id| select_steam_entry(&DESKTOP_ENTRIES, id))
        .or_else(|| select_entry(&DESKTOP_ENTRIES, &metadata));
    icon_debug!("[volume-icon] app={}", app.app_name);
    icon_debug!(
        "[volume-icon] process={} pid={}",
        metadata.process_binary.as_deref().unwrap_or("unknown"),
        metadata
            .process_id
            .map_or_else(|| "unknown".to_owned(), |pid| pid.to_string())
    );
    icon_debug!(
        "[volume-icon] steam_app_id={}",
        steam_id.as_deref().unwrap_or("none")
    );
    icon_debug!(
        "[volume-icon] stable_identity={}",
        stable_application_id(app)
    );
    icon_debug!(
        "[volume-icon] audio_icon={}",
        app.icon_name.as_deref().unwrap_or("none")
    );
    if let Some(found) = desktop_match.as_ref() {
        icon_debug!(
            "[volume-icon] desktop_id={} desktop_path={} desktop_icon={} match={} score={}",
            found.entry.id,
            found.entry.path.display(),
            found.entry.icon,
            found.reason,
            found.score
        );
    } else {
        icon_debug!(
            "[volume-icon] app={} metadata desktop={:?} id={:?} binary={:?} flatpak={:?}",
            app.app_name,
            metadata.desktop,
            metadata.application_id,
            metadata.process_binary,
            metadata.flatpak_id
        );
    }

    if let Some(icon_key) = desktop_match
        .as_ref()
        .map(|found| found.entry.icon.as_str())
        .filter(|icon| !icon.trim().is_empty())
    {
        icon_debug!("[volume-icon] icon_key={icon_key} theme={theme}");
        if let Some(path) = resolve_source(icon_key, &theme)
            && let Ok(rendered_data_uri) = cached_render(&app.app_name, &theme, icon_key, &path)
        {
            log_selection(&path, "exact-steam-desktop-entry");
            return ResolvedAppIcon {
                desktop_file_id: desktop_match.as_ref().map(|found| found.entry.id.clone()),
                desktop_file_path: desktop_match.as_ref().map(|found| found.entry.path.clone()),
                icon_key: Some(icon_key.to_owned()),
                source_path: Some(path),
                rendered_data_uri,
                source_kind: IconSourceKind::DesktopEntry,
            };
        }
    }
    if let Some(id) = steam_id.as_deref()
        && let Some(path) = find_steam_cache_icon(id)
        && let Ok(rendered_data_uri) = cached_render(&app.app_name, &theme, id, &path)
    {
        log_selection(&path, "exact-steam-cache");
        return ResolvedAppIcon {
            desktop_file_id: desktop_match.as_ref().map(|found| found.entry.id.clone()),
            desktop_file_path: desktop_match.as_ref().map(|found| found.entry.path.clone()),
            icon_key: Some(format!("steam-app:{id}")),
            source_path: Some(path),
            rendered_data_uri,
            source_kind: IconSourceKind::DesktopEntry,
        };
    }
    let candidates = [
        app.metadata
            .get("application.icon_name")
            .map(|value| (value.as_str(), IconSourceKind::ApplicationMetadata)),
        app.metadata
            .get("media.icon_name")
            .map(|value| (value.as_str(), IconSourceKind::MediaMetadata)),
        app.icon_name
            .as_deref()
            .map(|value| (value, IconSourceKind::ApplicationMetadata)),
    ];
    for (icon_key, source_kind) in candidates.into_iter().flatten() {
        if icon_key.trim().is_empty() || is_generic_game_icon(icon_key) {
            continue;
        }
        icon_debug!("[volume-icon] icon_key={icon_key} theme={theme}");
        if let Some(path) = resolve_source(icon_key, &theme)
            && let Ok(rendered_data_uri) = cached_render(&app.app_name, &theme, icon_key, &path)
        {
            let dimensions = image::image_dimensions(&path)
                .map(|(width, height)| format!("{width}x{height}"))
                .unwrap_or_else(|_| "scalable".to_owned());
            log_selection(&path, "specific-audio-metadata");
            icon_debug!(
                "[volume-icon] source_size={dimensions} rendered_size={TARGET_SIZE}x{TARGET_SIZE}"
            );
            return ResolvedAppIcon {
                desktop_file_id: desktop_match.as_ref().map(|found| found.entry.id.clone()),
                desktop_file_path: desktop_match.as_ref().map(|found| found.entry.path.clone()),
                icon_key: Some(icon_key.to_owned()),
                source_path: Some(path),
                rendered_data_uri,
                source_kind,
            };
        }
    }
    for icon_key in candidates
        .into_iter()
        .flatten()
        .map(|(icon, _)| icon)
        .filter(|icon| is_generic_game_icon(icon))
    {
        if let Some(path) = resolve_source(icon_key, &theme)
            && let Ok(rendered_data_uri) = cached_render(&app.app_name, &theme, icon_key, &path)
        {
            log_selection(&path, "generic-game-fallback");
            return ResolvedAppIcon {
                desktop_file_id: desktop_match.as_ref().map(|found| found.entry.id.clone()),
                desktop_file_path: desktop_match.as_ref().map(|found| found.entry.path.clone()),
                icon_key: Some(icon_key.to_owned()),
                source_path: Some(path),
                rendered_data_uri,
                source_kind: IconSourceKind::Fallback,
            };
        }
    }
    icon_debug!("[volume-icon] app={} fallback=generic-audio", app.app_name);
    ResolvedAppIcon {
        desktop_file_id: desktop_match.as_ref().map(|found| found.entry.id.clone()),
        desktop_file_path: desktop_match.map(|found| found.entry.path),
        icon_key: None,
        source_path: None,
        rendered_data_uri: fallback_data_uri(),
        source_kind: IconSourceKind::Fallback,
    }
}

fn normalize_icon_name(value: &str) -> String {
    let stem = Path::new(value)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(value);
    let normalized = stem.to_ascii_lowercase().replace('_', "-");
    normalized
        .strip_suffix("-symbolic")
        .unwrap_or(&normalized)
        .to_owned()
}

fn is_generic_game_icon(value: &str) -> bool {
    matches!(
        normalize_icon_name(value).as_str(),
        "applications-games"
            | "input-gaming"
            | "game"
            | "games"
            | "gaming"
            | "controller"
            | "gamepad"
            | "preferences-desktop-gaming"
    )
}

fn log_selection(path: &Path, reason: &str) {
    let dimensions = image::image_dimensions(path)
        .map(|(width, height)| format!("{width}x{height}"))
        .unwrap_or_else(|_| "scalable".to_owned());
    icon_debug!("[volume-icon] selected_source={}", path.display());
    icon_debug!("[volume-icon] selected_reason={reason}");
    icon_debug!("[volume-icon] source_size={dimensions} rendered_size={TARGET_SIZE}x{TARGET_SIZE}");
}

fn find_steam_cache_icon(app_id: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let roots = [
        home.join(".local/share/Steam/appcache/librarycache")
            .join(app_id),
        home.join(".steam/steam/appcache/librarycache").join(app_id),
        home.join(".var/app/com.valvesoftware.Steam/data/Steam/appcache/librarycache")
            .join(app_id),
    ];
    best_steam_cache_icon(&roots)
}

fn best_steam_cache_icon(roots: &[PathBuf]) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    for root in roots {
        collect_images(root, &mut candidates);
    }
    candidates
        .into_iter()
        .filter_map(|path| classify_steam_candidate(path, roots))
        .max_by_key(|candidate| {
            let score = candidate.score();
            icon_debug!("[volume-icon] candidate={}", candidate.path.display());
            icon_debug!(
                "[volume-icon] dimensions={}x{}",
                candidate.width,
                candidate.height
            );
            icon_debug!("[volume-icon] aspect_ratio={:.3}", candidate.aspect_ratio);
            icon_debug!(
                "[volume-icon] exact_app_directory={}",
                candidate.exact_app_directory
            );
            icon_debug!("[volume-icon] role={}", candidate.role.as_str());
            icon_debug!("[volume-icon] candidate_score={score:?}");
            score
        })
        .map(|candidate| {
            icon_debug!("[volume-icon] selected_source={}", candidate.path.display());
            icon_debug!("[volume-icon] selected_role={}", candidate.role.as_str());
            icon_debug!(
                "[volume-icon] selected_reason={}",
                if candidate.role == SteamArtworkRole::AppIcon {
                    "exact-square-steam-app-cache"
                } else {
                    "exact-steam-artwork-fallback"
                }
            );
            candidate.path
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SteamArtworkRole {
    AppIcon,
    SquareArtwork,
    Logo,
    Header,
    Hero,
    Unknown,
}

impl SteamArtworkRole {
    fn rank(self) -> u8 {
        match self {
            Self::AppIcon => 6,
            Self::SquareArtwork => 5,
            Self::Logo => 3,
            Self::Header => 2,
            Self::Hero => 1,
            Self::Unknown => 0,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::AppIcon => "app-icon",
            Self::SquareArtwork => "square-artwork",
            Self::Logo => "logo",
            Self::Header => "header",
            Self::Hero => "hero",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug)]
struct SteamCacheCandidate {
    path: PathBuf,
    width: u32,
    height: u32,
    aspect_ratio: f64,
    exact_app_directory: bool,
    role: SteamArtworkRole,
}

impl SteamCacheCandidate {
    fn score(&self) -> (u8, u8, u64) {
        let format = match self
            .path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "svg" => 4,
            "png" => 3,
            "jpg" | "jpeg" => 1,
            _ => 0,
        };
        (
            self.role.rank(),
            format,
            u64::from(self.width) * u64::from(self.height),
        )
    }
}

fn classify_steam_candidate(path: PathBuf, exact_roots: &[PathBuf]) -> Option<SteamCacheCandidate> {
    let (width, height) = image::image_dimensions(&path).ok()?;
    if height == 0 {
        return None;
    }
    let aspect_ratio = f64::from(width) / f64::from(height);
    let square = (0.80..=1.25).contains(&aspect_ratio);
    let exact_app_directory = exact_roots.iter().any(|root| path.starts_with(root));
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let opaque_hash =
        stem.len() >= 16 && stem.chars().all(|character| character.is_ascii_hexdigit());
    let role = if exact_app_directory && square && opaque_hash {
        SteamArtworkRole::AppIcon
    } else if stem == "logo" {
        SteamArtworkRole::Logo
    } else if stem.contains("header") || stem.contains("capsule") {
        SteamArtworkRole::Header
    } else if stem.contains("hero") {
        SteamArtworkRole::Hero
    } else if exact_app_directory && square {
        SteamArtworkRole::SquareArtwork
    } else {
        SteamArtworkRole::Unknown
    };
    Some(SteamCacheCandidate {
        path,
        width,
        height,
        aspect_ratio,
        exact_app_directory,
        role,
    })
}

fn collect_images(directory: &Path, result: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_images(&path, result);
        } else if matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("svg" | "png" | "jpg" | "jpeg")
        ) {
            result.push(path);
        }
    }
}

fn resolve_source(icon_key: &str, theme: &str) -> Option<PathBuf> {
    let key = format!("{theme}:{icon_key}");
    if let Some(path) = SOURCE_CACHE.read().ok()?.get(&key) {
        return path.clone();
    }
    let path = resolve_icon(icon_key, theme, &icon_roots());
    if let Ok(mut cache) = SOURCE_CACHE.write() {
        cache.insert(key, path.clone());
    }
    path
}

fn cached_render(identity: &str, theme: &str, icon: &str, path: &Path) -> Result<String, String> {
    let modified = path
        .metadata()
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |value| value.as_secs());
    let source_specific_icon = format!("{icon}:{}", path.display());
    let key = cache_key(
        identity,
        theme,
        &source_specific_icon,
        TARGET_SIZE,
        modified,
    );
    if let Some(value) = CACHE.get(&key) {
        return Ok(value);
    }
    let bytes = render(path)?;
    let value = png_data_uri(&bytes);
    CACHE.insert(key, value.clone());
    Ok(value)
}

fn render(path: &Path) -> Result<Vec<u8>, String> {
    match path.extension().and_then(|value| value.to_str()) {
        Some("svg") => {
            let data = std::fs::read(path).map_err(|error| error.to_string())?;
            let tree = usvg::Tree::from_data(&data, &usvg::Options::default())
                .map_err(|error| error.to_string())?;
            let size = tree.size();
            let scale = (TARGET_SIZE as f32 / size.width()).min(TARGET_SIZE as f32 / size.height());
            let x = (TARGET_SIZE as f32 - size.width() * scale) / 2.0;
            let y = (TARGET_SIZE as f32 - size.height() * scale) / 2.0;
            let mut pixmap = tiny_skia::Pixmap::new(TARGET_SIZE, TARGET_SIZE)
                .ok_or_else(|| "invalid icon canvas".to_owned())?;
            resvg::render(
                &tree,
                tiny_skia::Transform::from_scale(scale, scale).post_translate(x, y),
                &mut pixmap.as_mut(),
            );
            pixmap.encode_png().map_err(|error| error.to_string())
        }
        Some("png" | "jpg" | "jpeg") => {
            let image = image::open(path).map_err(|error| error.to_string())?;
            let (width, height) = image.dimensions();
            let scale = (TARGET_SIZE as f32 / width as f32).min(TARGET_SIZE as f32 / height as f32);
            let resized = image.resize(
                (width as f32 * scale).round().max(1.0) as u32,
                (height as f32 * scale).round().max(1.0) as u32,
                FilterType::Lanczos3,
            );
            let mut canvas = RgbaImage::new(TARGET_SIZE, TARGET_SIZE);
            image::imageops::overlay(
                &mut canvas,
                &resized.to_rgba8(),
                i64::from((TARGET_SIZE - resized.width()) / 2),
                i64::from((TARGET_SIZE - resized.height()) / 2),
            );
            let mut output = Cursor::new(Vec::new());
            DynamicImage::ImageRgba8(canvas)
                .write_to(&mut output, ImageFormat::Png)
                .map_err(|error| error.to_string())?;
            Ok(output.into_inner())
        }
        _ => Err("unsupported icon format".to_owned()),
    }
}

fn fallback_data_uri() -> String {
    png_data_uri(include_bytes!("../../img/wave-sound.png"))
}

fn png_data_uri(bytes: &[u8]) -> String {
    format!("data:image/png;base64,{}", STANDARD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_icon_fallback_is_png() {
        assert!(fallback_data_uri().starts_with("data:image/png;base64,"));
    }

    #[test]
    fn steam_environment_is_filtered_and_stable() {
        let environment = b"SECRET=nope\0SteamAppId=730\0OTHER=value\0";
        assert_eq!(
            steam_id_from_environment(environment).as_deref(),
            Some("730")
        );
        let mut app = AppInfo {
            uid: 1,
            app_name: "game".into(),
            sink_name: None,
            mute: false,
            vol_percent: 50.0,
            icon_name: Some("applications-games".into()),
            is_device: false,
            is_multi_sink_app: false,
            metadata: Default::default(),
        };
        app.metadata.insert("SteamAppId".into(), "730".into());
        assert_eq!(stable_application_id(&app), "steam-app:730");
    }

    #[test]
    fn steam_manifest_matches_executable_install_directory() {
        let temp = tempfile::tempdir().unwrap();
        let steamapps = temp.path().join("steamapps");
        std::fs::create_dir_all(&steamapps).unwrap();
        std::fs::write(
            steamapps.join("appmanifest_730.acf"),
            "\"AppState\"\n{\n\"appid\" \"730\"\n\"installdir\" \"Example Game\"\n}\n",
        )
        .unwrap();
        let executable = temp.path().join("steamapps/common/Example Game/bin/game");
        assert_eq!(
            steam_id_from_executable(&executable, &[temp.path().to_owned()]).as_deref(),
            Some("730")
        );
    }

    #[test]
    fn high_resolution_steam_cache_icon_wins() {
        let temp = tempfile::tempdir().unwrap();
        let small = image::RgbaImage::new(32, 32);
        small
            .save(temp.path().join("aaaaaaaaaaaaaaaa.png"))
            .unwrap();
        let large = image::RgbaImage::new(256, 256);
        let large_path = temp.path().join("bbbbbbbbbbbbbbbb.png");
        large.save(&large_path).unwrap();
        assert_eq!(
            best_steam_cache_icon(&[temp.path().to_owned()]),
            Some(large_path)
        );
    }

    #[test]
    fn generic_game_icons_are_normalized() {
        for icon in [
            "applications-games",
            "input_gaming-symbolic.svg",
            "GAMEPAD.PNG",
            "preferences-desktop-gaming",
        ] {
            assert!(is_generic_game_icon(icon), "{icon}");
        }
        assert!(!is_generic_game_icon("steam-game-specific"));
    }

    #[test]
    fn square_steam_icon_beats_wide_banner() {
        let temp = tempfile::tempdir().unwrap();
        image::RgbaImage::new(639, 360)
            .save(temp.path().join("logo.png"))
            .unwrap();
        let square = temp
            .path()
            .join("8dbc71957312bbd3baea65848b545be9eae2a355.jpg");
        image::RgbImage::new(32, 32).save(&square).unwrap();
        assert_eq!(
            best_steam_cache_icon(&[temp.path().to_owned()]),
            Some(square)
        );
    }

    #[test]
    fn steam_cache_roles_use_semantics_and_exact_directory() {
        let exact = tempfile::tempdir().unwrap();
        let unrelated = tempfile::tempdir().unwrap();
        let hash = exact
            .path()
            .join("1234567890abcdef1234567890abcdef12345678.jpg");
        image::RgbImage::new(32, 32).save(&hash).unwrap();
        let logo = exact.path().join("logo.png");
        image::RgbaImage::new(639, 360).save(&logo).unwrap();
        assert_eq!(
            classify_steam_candidate(hash, &[exact.path().to_owned()])
                .unwrap()
                .role,
            SteamArtworkRole::AppIcon
        );
        assert_eq!(
            classify_steam_candidate(logo, &[exact.path().to_owned()])
                .unwrap()
                .role,
            SteamArtworkRole::Logo
        );

        let outside_hash = unrelated
            .path()
            .join("1234567890abcdef1234567890abcdef12345678.png");
        image::RgbaImage::new(256, 256).save(&outside_hash).unwrap();
        assert_ne!(
            classify_steam_candidate(outside_hash, &[exact.path().to_owned()])
                .unwrap()
                .role,
            SteamArtworkRole::AppIcon
        );
    }

    #[test]
    fn newly_available_app_icon_replaces_logo_selection() {
        let exact = tempfile::tempdir().unwrap();
        let logo = exact.path().join("logo.png");
        image::RgbaImage::new(639, 360).save(&logo).unwrap();
        assert_eq!(
            best_steam_cache_icon(&[exact.path().to_owned()]),
            Some(logo)
        );

        let app_icon = exact
            .path()
            .join("abcdefabcdefabcdefabcdefabcdefabcdefabcd.jpg");
        image::RgbImage::new(32, 32).save(&app_icon).unwrap();
        assert_eq!(
            best_steam_cache_icon(&[exact.path().to_owned()]),
            Some(app_icon)
        );
    }
}
