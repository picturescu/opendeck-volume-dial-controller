use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

const ICON_CACHE_SCHEMA: &str = "steam-semantic-v3";

#[derive(Clone, Default)]
pub struct IconCache(Arc<RwLock<HashMap<String, String>>>);

impl IconCache {
    pub fn get(&self, key: &str) -> Option<String> {
        self.0.read().ok()?.get(key).cloned()
    }

    pub fn insert(&self, key: String, value: String) {
        if let Ok(mut entries) = self.0.write() {
            entries.insert(key, value);
        }
    }
}

pub fn cache_key(identity: &str, theme: &str, icon: &str, size: u32, modified: u64) -> String {
    format!("{ICON_CACHE_SCHEMA}:{identity}:{theme}:{icon}:{size}:{modified}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_changes_with_theme() {
        assert_ne!(
            cache_key("app", "breeze", "icon", 128, 0),
            cache_key("app", "hicolor", "icon", 128, 0)
        );
    }
}
