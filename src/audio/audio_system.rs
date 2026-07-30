use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::error::Error;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    #[default]
    Output,
    Input,
}

#[derive(Clone, Debug)]
pub struct AudioDeviceInfo {
    pub stable_name: String,
    pub description: String,
    pub mute: bool,
    pub vol_percent: f32,
    pub icon_name: Option<String>,
    pub kind: DeviceKind,
}

pub fn is_monitor_source(name: &str, description: &str) -> bool {
    name.to_ascii_lowercase().contains(".monitor")
        || description.to_ascii_lowercase().contains("monitor of")
}

#[derive(Clone, Debug)]
pub struct AppInfo {
    pub uid: u32,
    pub app_name: String,
    pub sink_name: Option<String>,
    pub mute: bool,
    pub vol_percent: f32,
    pub icon_name: Option<String>,
    pub is_device: bool,
    pub is_multi_sink_app: bool,
    pub metadata: BTreeMap<String, String>,
}

pub trait AudioSystem {
    fn list_applications(&mut self) -> Result<Vec<AppInfo>, Box<dyn Error>>;
    fn list_devices(&mut self, kind: DeviceKind) -> Result<Vec<AudioDeviceInfo>, Box<dyn Error>>;
    fn set_volume_percent(
        &mut self,
        app_index: u32,
        percent: f64,
        is_device: bool,
    ) -> Result<(), Box<dyn Error>>;
    fn mute_volume(
        &mut self,
        app_index: u32,
        mute: bool,
        is_device: bool,
    ) -> Result<(), Box<dyn Error>>;
    fn set_device_target(
        &mut self,
        stable_name: &str,
        kind: DeviceKind,
        percent: f64,
    ) -> Result<(), Box<dyn Error>>;
    fn mute_device(
        &mut self,
        stable_name: &str,
        kind: DeviceKind,
        mute: bool,
    ) -> Result<(), Box<dyn Error>>;
    fn query_application_state(&mut self, index: u32) -> Result<(f32, bool), Box<dyn Error>>;
    fn query_device_state(
        &mut self,
        index: u32,
        kind: DeviceKind,
    ) -> Result<(String, f32, bool), Box<dyn Error>>;
}
