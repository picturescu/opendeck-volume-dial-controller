use std::{
    collections::{BTreeMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default)]
pub struct DesktopEntry {
    pub id: String,
    pub path: PathBuf,
    pub name: String,
    pub icon: String,
    pub exec: String,
    pub try_exec: String,
    pub startup_wm_class: String,
    pub flatpak_id: String,
    pub hidden: bool,
    #[allow(dead_code)]
    pub no_display: bool,
}

#[derive(Clone, Debug, Default)]
pub struct AppMetadata {
    pub desktop: Option<String>,
    pub application_id: Option<String>,
    pub application_name: String,
    pub process_binary: Option<String>,
    pub process_id: Option<u32>,
    pub startup_wm_class: Option<String>,
    pub flatpak_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct DesktopMatch {
    pub entry: DesktopEntry,
    pub score: u16,
    pub reason: &'static str,
}

pub fn normalize_desktop_id(value: &str) -> String {
    let value = value.trim();
    if value.to_ascii_lowercase().ends_with(".desktop") {
        value.to_ascii_lowercase()
    } else {
        format!("{}.desktop", value.to_ascii_lowercase())
    }
}

pub fn application_dirs() -> Vec<PathBuf> {
    let mut result = Vec::new();
    if let Some(value) = env::var_os("XDG_DATA_HOME") {
        result.push(PathBuf::from(value).join("applications"));
    }
    let dirs = env::var_os("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    result.extend(env::split_paths(&dirs).map(|path| path.join("applications")));
    if let Some(home) = env::var_os("HOME") {
        let home = PathBuf::from(home);
        result.push(home.join(".local/share/applications"));
        result.push(home.join(".local/share/flatpak/exports/share/applications"));
    }
    result.extend([
        PathBuf::from("/usr/local/share/applications"),
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
    ]);
    deduplicate(result)
}

fn deduplicate(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

pub fn load_entries(dirs: &[PathBuf]) -> Vec<DesktopEntry> {
    let mut entries = Vec::new();
    let mut ids = HashSet::new();
    for directory in dirs {
        let Ok(files) = fs::read_dir(directory) else {
            continue;
        };
        let mut files = files.flatten().map(|item| item.path()).collect::<Vec<_>>();
        files.sort();
        for path in files {
            let Some(id) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            if path.extension().and_then(|value| value.to_str()) != Some("desktop")
                || !ids.insert(id.to_ascii_lowercase())
            {
                continue;
            }
            if let Some(entry) = parse_entry(&path) {
                entries.push(entry);
            }
        }
    }
    entries
}

pub fn parse_entry(path: &Path) -> Option<DesktopEntry> {
    let contents = fs::read_to_string(path).ok()?;
    let mut values = BTreeMap::new();
    let mut in_entry = false;
    for raw in contents.lines() {
        let line = raw.trim();
        if line == "[Desktop Entry]" {
            in_entry = true;
            continue;
        }
        if line.starts_with('[') {
            in_entry = false;
            continue;
        }
        if !in_entry || line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && !key.contains('[')
        {
            values
                .entry(key.to_owned())
                .or_insert(value.trim().to_owned());
        }
    }
    if values.get("Type").map(String::as_str) != Some("Application") {
        return None;
    }
    Some(DesktopEntry {
        id: path.file_name()?.to_str()?.to_owned(),
        path: path.to_owned(),
        name: values.remove("Name").unwrap_or_default(),
        icon: values.remove("Icon").unwrap_or_default(),
        exec: values.remove("Exec").unwrap_or_default(),
        try_exec: values.remove("TryExec").unwrap_or_default(),
        startup_wm_class: values.remove("StartupWMClass").unwrap_or_default(),
        flatpak_id: values.remove("X-Flatpak").unwrap_or_default(),
        hidden: parse_bool(values.get("Hidden")),
        no_display: parse_bool(values.get("NoDisplay")),
    })
}

fn parse_bool(value: Option<&String>) -> bool {
    value.is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

pub fn executable_basename(command: &str) -> Option<String> {
    command
        .split_whitespace()
        .find(|part| !part.starts_with('%') && !part.contains('='))
        .and_then(|part| Path::new(part).file_name())
        .and_then(|part| part.to_str())
        .map(|part| part.to_ascii_lowercase())
}

fn normalized(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

pub fn select_entry(entries: &[DesktopEntry], metadata: &AppMetadata) -> Option<DesktopMatch> {
    let requested_desktop = metadata.desktop.as_deref().map(normalize_desktop_id);
    let process_exe = metadata
        .process_id
        .and_then(|pid| fs::read_link(format!("/proc/{pid}/exe")).ok())
        .and_then(|path| path.file_name().map(|value| value.to_owned()))
        .and_then(|value| value.to_str().map(str::to_ascii_lowercase));
    entries
        .iter()
        .filter_map(|entry| {
            let id = normalize_desktop_id(&entry.id);
            let exact_desktop = requested_desktop.as_ref().is_some_and(|value| value == &id);
            if entry.hidden && !exact_desktop {
                return None;
            }
            let app_id = metadata.application_id.as_deref().map(normalize_desktop_id);
            let flatpak = metadata.flatpak_id.as_deref().map(str::to_ascii_lowercase);
            let entry_executables = [
                executable_basename(&entry.exec),
                executable_basename(&entry.try_exec),
            ];
            let candidate = if exact_desktop {
                (1000, "application.desktop")
            } else if app_id.as_ref().is_some_and(|value| value == &id)
                || metadata
                    .application_id
                    .as_ref()
                    .is_some_and(|value| value.eq_ignore_ascii_case(&entry.flatpak_id))
            {
                (900, "application-id")
            } else if flatpak
                .as_ref()
                .is_some_and(|value| value.eq_ignore_ascii_case(&entry.flatpak_id))
            {
                (850, "flatpak-id")
            } else if metadata.process_binary.as_ref().is_some_and(|value| {
                entry_executables
                    .iter()
                    .flatten()
                    .any(|candidate| candidate.eq_ignore_ascii_case(value))
            }) {
                (800, "process-binary")
            } else if process_exe.as_ref().is_some_and(|value| {
                entry_executables
                    .iter()
                    .flatten()
                    .any(|candidate| candidate == value)
            }) {
                (750, "process-exe")
            } else if metadata
                .startup_wm_class
                .as_ref()
                .is_some_and(|value| value.eq_ignore_ascii_case(&entry.startup_wm_class))
            {
                (700, "startup-wm-class")
            } else if !entry.startup_wm_class.is_empty()
                && metadata
                    .application_name
                    .eq_ignore_ascii_case(&entry.startup_wm_class)
            {
                (650, "startup-wm-class")
            } else if !metadata.application_name.is_empty()
                && normalized(&metadata.application_name) == normalized(&entry.name)
            {
                (100, "application-name")
            } else {
                return None;
            };
            Some(DesktopMatch {
                entry: entry.clone(),
                score: candidate.0,
                reason: candidate.1,
            })
        })
        .max_by(|left, right| {
            left.score
                .cmp(&right.score)
                .then_with(|| right.entry.id.cmp(&left.entry.id))
        })
}

pub fn select_steam_entry(entries: &[DesktopEntry], app_id: &str) -> Option<DesktopMatch> {
    let exact_id = normalize_desktop_id(&format!("steam_app_{app_id}"));
    let run_uri = format!("steam://rungameid/{app_id}");
    entries
        .iter()
        .filter_map(|entry| {
            let (score, reason) = if normalize_desktop_id(&entry.id) == exact_id {
                (1200, "steam-desktop-id")
            } else if entry
                .exec
                .split_whitespace()
                .any(|argument| argument.trim_matches(['\'', '"']) == run_uri)
            {
                (1150, "steam-rungameid")
            } else {
                return None;
            };
            Some(DesktopMatch {
                entry: entry.clone(),
                score,
                reason,
            })
        })
        .max_by(|left, right| left.score.cmp(&right.score))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, name: &str, exec: &str) -> DesktopEntry {
        DesktopEntry {
            id: id.into(),
            name: name.into(),
            exec: exec.into(),
            ..DesktopEntry::default()
        }
    }

    #[test]
    fn desktop_suffix_normalization() {
        assert_eq!(normalize_desktop_id("Firefox"), "firefox.desktop");
        assert_eq!(normalize_desktop_id("Firefox.desktop"), "firefox.desktop");
    }

    #[test]
    fn exact_desktop_flatpak_binary_and_wmclass_matching() {
        let mut flatpak = entry("org.test.App.desktop", "Test", "/app/bin/test %U");
        flatpak.flatpak_id = "org.test.App".into();
        flatpak.startup_wm_class = "test-window".into();
        let entries = [flatpak];
        for (metadata, reason) in [
            (
                AppMetadata {
                    desktop: Some("org.test.App".into()),
                    ..Default::default()
                },
                "application.desktop",
            ),
            (
                AppMetadata {
                    flatpak_id: Some("org.test.App".into()),
                    ..Default::default()
                },
                "flatpak-id",
            ),
            (
                AppMetadata {
                    process_binary: Some("test".into()),
                    ..Default::default()
                },
                "process-binary",
            ),
            (
                AppMetadata {
                    startup_wm_class: Some("test-window".into()),
                    ..Default::default()
                },
                "startup-wm-class",
            ),
        ] {
            assert_eq!(select_entry(&entries, &metadata).unwrap().reason, reason);
        }
    }

    #[test]
    fn exec_field_codes_are_ignored() {
        for code in ["%U", "%u", "%F", "%f"] {
            assert_eq!(
                executable_basename(&format!("/usr/bin/browser {code}")).as_deref(),
                Some("browser")
            );
        }
    }

    #[test]
    fn substring_is_not_a_match() {
        let entries = [entry("music.desktop", "Super Spotify Player", "music")];
        assert!(
            select_entry(
                &entries,
                &AppMetadata {
                    application_name: "Spotify".into(),
                    ..Default::default()
                }
            )
            .is_none()
        );
    }

    #[test]
    fn exact_steam_desktop_id_is_preferred() {
        let entries = [
            entry("applications-games.desktop", "Games", "games"),
            entry("steam_app_730.desktop", "Example Game", "game"),
        ];
        let matched = select_entry(
            &entries,
            &AppMetadata {
                desktop: Some("steam_app_730".into()),
                application_name: "game".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(matched.entry.id, "steam_app_730.desktop");
        assert_eq!(matched.reason, "application.desktop");
    }

    #[test]
    fn steam_rungameid_shortcut_is_matched_exactly() {
        let entries = [
            entry("shortcut.desktop", "Example", "steam steam://rungameid/730"),
            entry("other.desktop", "Other", "steam steam://rungameid/7300"),
        ];
        let matched = select_steam_entry(&entries, "730").unwrap();
        assert_eq!(matched.entry.id, "shortcut.desktop");
        assert_eq!(matched.reason, "steam-rungameid");
    }
}
