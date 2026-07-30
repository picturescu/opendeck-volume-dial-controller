use crate::utils::get_app_icon_uri;
use std::collections::HashMap;
use std::sync::LazyLock;
use tokio::sync::Mutex;

#[derive(Clone, Debug)]
pub struct MixerChannel {
    pub header_id: Option<String>,
    pub upper_vol_btn_id: Option<String>,
    pub lower_vol_btn_id: Option<String>,
    pub uid: u32,
    pub app_name: String,
    pub sink_name: Option<String>,
    pub mute: bool,
    pub vol_percent: f32,
    pub icon_uri: String,
    pub icon_uri_mute: String,
    pub uses_default_icon: bool,
    pub is_device: bool,
    pub is_multi_sink_app: bool,
}

pub static MIXER_CHANNELS: LazyLock<Mutex<HashMap<u8, MixerChannel>>> =
    LazyLock::new(|| Mutex::const_new(HashMap::new()));

pub fn update_channel_state(uid: u32, volume: f32, mute: bool) {
    let Ok(mut channels) = MIXER_CHANNELS.try_lock() else {
        return;
    };
    for channel in channels.values_mut().filter(|channel| channel.uid == uid) {
        channel.vol_percent = volume;
        channel.mute = mute;
    }
}

pub async fn create_mixer_channels(
    applications: Vec<crate::audio::audio_system::AppInfo>,
    ignored_apps: &[String],
) {
    update_mixer_channels(applications, ignored_apps).await;
}

pub async fn update_mixer_channels(
    applications: Vec<crate::audio::audio_system::AppInfo>,
    ignored_apps: &[String],
) {
    // Clone the small presentation registry first. Icon/file-system work below
    // must never run while the shared mixer lock is held.
    let previous = MIXER_CHANNELS.lock().await.clone();
    let mut replacement = HashMap::new();
    let mut col_key = 0;
    for app in applications {
        if ignored_apps.contains(&app.app_name) {
            println!("Skipping ignored app: {}", app.app_name);
            continue;
        }

        let (icon_uri, icon_uri_mute, uses_default_icon) =
            if let Some(channel) = previous.get(&col_key).filter(|old| old.uid == app.uid) {
                (
                    channel.icon_uri.clone(),
                    channel.icon_uri_mute.clone(),
                    channel.uses_default_icon,
                )
            } else {
                get_app_icon_uri(app.icon_name, app.app_name.clone())
            };
        let (header_id, upper_vol_btn_id, lower_vol_btn_id) =
            if let Some(channel) = previous.get(&col_key) {
                (
                    channel.header_id.clone(),
                    channel.upper_vol_btn_id.clone(),
                    channel.lower_vol_btn_id.clone(),
                )
            } else {
                (None, None, None)
            };
        replacement.insert(
            col_key,
            MixerChannel {
                header_id,
                upper_vol_btn_id,
                lower_vol_btn_id,
                uid: app.uid,
                app_name: app.app_name,
                sink_name: app.sink_name,
                mute: app.mute,
                vol_percent: app.vol_percent,
                icon_uri,
                icon_uri_mute,
                uses_default_icon,
                is_device: app.is_device,
                is_multi_sink_app: app.is_multi_sink_app,
            },
        );
        col_key += 1;
    }

    {
        let mut channels = MIXER_CHANNELS.lock().await;
        *channels = replacement;
    }

    println!(
        "Updated mixer channels (filtered {} ignored apps)",
        ignored_apps.len()
    );
}
