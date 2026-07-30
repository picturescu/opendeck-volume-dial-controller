use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
};

pub const TARGET_SIZE: u32 = 128;

pub fn active_theme() -> String {
    config_files()
        .into_iter()
        .find_map(|path| parse_kde_theme(&path))
        .unwrap_or_else(|| "breeze".to_owned())
}

fn config_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Some(config) = env::var_os("XDG_CONFIG_HOME") {
        files.push(PathBuf::from(config).join("kdeglobals"));
    }
    if let Some(home) = env::var_os("HOME") {
        files.push(PathBuf::from(home).join(".config/kdeglobals"));
    }
    files
}

pub fn parse_kde_theme(path: &Path) -> Option<String> {
    let contents = fs::read_to_string(path).ok()?;
    let mut icons = false;
    for line in contents.lines().map(str::trim) {
        if line.starts_with('[') {
            icons = line == "[Icons]";
        } else if icons
            && let Some(value) = line.strip_prefix("Theme=")
            && !value.trim().is_empty()
        {
            return Some(value.trim().to_owned());
        }
    }
    None
}

pub fn icon_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = env::var_os("HOME") {
        roots.push(PathBuf::from(&home).join(".icons"));
        roots.push(PathBuf::from(&home).join(".local/share/icons"));
        roots.push(PathBuf::from(home).join(".local/share/flatpak/exports/share/icons"));
    }
    if let Some(data_home) = env::var_os("XDG_DATA_HOME") {
        roots.push(PathBuf::from(data_home).join("icons"));
    }
    let data_dirs =
        env::var_os("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    roots.extend(env::split_paths(&data_dirs).map(|path| path.join("icons")));
    roots.extend([
        PathBuf::from("/usr/local/share/icons"),
        PathBuf::from("/usr/share/icons"),
        PathBuf::from("/var/lib/flatpak/exports/share/icons"),
    ]);
    let mut seen = HashSet::new();
    roots
        .into_iter()
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

pub fn resolve_icon(icon: &str, theme: &str, roots: &[PathBuf]) -> Option<PathBuf> {
    let path = PathBuf::from(icon);
    if path.is_absolute() {
        return path.is_file().then_some(path);
    }
    let name = icon.trim_end_matches(".png").trim_end_matches(".svg");
    let mut themes = Vec::new();
    collect_themes(theme, roots, &mut themes, &mut HashSet::new());
    collect_themes("breeze", roots, &mut themes, &mut HashSet::new());
    collect_themes("hicolor", roots, &mut themes, &mut HashSet::new());
    themes.dedup();
    for theme in themes {
        let mut candidates = Vec::new();
        for root in roots {
            collect_candidates(&root.join(&theme), name, &mut candidates);
        }
        if let Some(best) = candidates
            .into_iter()
            .max_by_key(|path| candidate_rank(path))
        {
            return Some(best);
        }
    }
    None
}

fn collect_themes(
    theme: &str,
    roots: &[PathBuf],
    result: &mut Vec<String>,
    visited: &mut HashSet<String>,
) {
    if theme.is_empty() || !visited.insert(theme.to_owned()) {
        return;
    }
    result.push(theme.to_owned());
    for root in roots {
        if let Some(inherits) = parse_inherits(&root.join(theme).join("index.theme")) {
            for inherited in inherits {
                collect_themes(&inherited, roots, result, visited);
            }
            break;
        }
    }
}

fn parse_inherits(path: &Path) -> Option<Vec<String>> {
    let contents = fs::read_to_string(path).ok()?;
    contents.lines().find_map(|line| {
        line.trim().strip_prefix("Inherits=").map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect()
        })
    })
}

fn collect_candidates(directory: &Path, name: &str, result: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_candidates(&path, name, result);
        } else if path
            .file_stem()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value == name)
            && matches!(
                path.extension().and_then(|value| value.to_str()),
                Some("png" | "svg")
            )
        {
            result.push(path);
        }
    }
}

fn candidate_rank(path: &Path) -> (u8, u32, u8, String) {
    let extension = path.extension().and_then(|value| value.to_str());
    if extension == Some("svg") {
        return (4, u32::MAX, 1, path.display().to_string());
    }
    let size = source_size(path).unwrap_or(0);
    let class = if size == TARGET_SIZE {
        3
    } else if size > TARGET_SIZE {
        2
    } else {
        1
    };
    let closeness = if size >= TARGET_SIZE {
        u32::MAX - size.saturating_sub(TARGET_SIZE)
    } else {
        size
    };
    (class, closeness, 0, path.display().to_string())
}

pub fn source_size(path: &Path) -> Option<u32> {
    let parent = path.parent()?.parent()?.file_name()?.to_str()?;
    let value = parent.split('x').next()?.parse::<u32>().ok();
    value.or_else(|| {
        image::image_dimensions(path)
            .ok()
            .map(|size| size.0.max(size.1))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn icon(root: &Path, theme: &str, dir: &str, name: &str) -> PathBuf {
        let path = root.join(theme).join(dir).join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            if name.ends_with(".svg") {
                b"<svg/>".as_slice()
            } else {
                b"x"
            },
        )
        .unwrap();
        path
    }

    #[test]
    fn active_theme_parser() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("kdeglobals");
        fs::write(&path, "[Icons]\nTheme=MyTheme\n").unwrap();
        assert_eq!(parse_kde_theme(&path).as_deref(), Some("MyTheme"));
    }

    #[test]
    fn inheritance_and_hicolor_fallback() {
        let temp = tempdir().unwrap();
        fs::create_dir_all(temp.path().join("child")).unwrap();
        fs::write(
            temp.path().join("child/index.theme"),
            "[Icon Theme]\nInherits=parent\n",
        )
        .unwrap();
        let inherited = icon(temp.path(), "parent", "128x128/apps", "app.png");
        assert_eq!(
            resolve_icon("app", "child", &[temp.path().into()]),
            Some(inherited)
        );
        let fallback = icon(temp.path(), "hicolor", "128x128/apps", "other.png");
        assert_eq!(
            resolve_icon("other", "missing", &[temp.path().into()]),
            Some(fallback)
        );
    }

    #[test]
    fn scalable_and_large_sources_win() {
        let temp = tempdir().unwrap();
        icon(temp.path(), "theme", "32x32/apps", "app.png");
        let large = icon(temp.path(), "theme", "256x256/apps", "app.png");
        assert_eq!(
            resolve_icon("app", "theme", &[temp.path().into()]),
            Some(large)
        );
        let svg = icon(temp.path(), "theme", "scalable/apps", "app.svg");
        assert_eq!(
            resolve_icon("app", "theme", &[temp.path().into()]),
            Some(svg)
        );
    }

    #[test]
    fn absolute_path_and_largest_png() {
        let temp = tempdir().unwrap();
        let direct = icon(temp.path(), "theme", "64x64/apps", "direct.png");
        assert_eq!(
            resolve_icon(direct.to_str().unwrap(), "theme", &[]),
            Some(direct)
        );
        icon(temp.path(), "theme", "24x24/apps", "small.png");
        let biggest = icon(temp.path(), "theme", "96x96/apps", "small.png");
        assert_eq!(
            resolve_icon("small", "theme", &[temp.path().into()]),
            Some(biggest)
        );
    }
}
