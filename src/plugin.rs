use openaction::global_events::{
    DidReceiveGlobalSettingsEvent, GlobalEventHandler, set_global_event_handler,
};
use openaction::*;

use serde::{Deserialize, Serialize};

use crate::{
    app_column::ApplicationVolumeColumnAction,
    audio::{self, pulse::pulse_monitor::refresh_audio_applications, *},
    device_dial::AudioDeviceVolumeDialAction,
    dial::ApplicationVolumeDialAction,
    dynamic::{ApplicationSelectorButtonAction, DynamicApplicationVolumeDialAction},
    gfx::{self},
    mixer,
    utils::{self, ButtonPressControl},
};
use std::{collections::HashMap, sync::LazyLock};
use tokio::sync::Mutex;

const VOLUME_INCREMENT: f64 = 0.1;

pub static COLUMN_TO_CHANNEL_MAP: LazyLock<Mutex<HashMap<u8, u8>>> =
    LazyLock::new(|| Mutex::const_new(HashMap::new()));

pub static BUTTON_PRESS_CONTROL: LazyLock<Mutex<ButtonPressControl>> =
    LazyLock::new(|| Mutex::const_new(ButtonPressControl::new()));

pub static SHARED_SETTINGS: LazyLock<Mutex<VolumeControllerSettings>> =
    LazyLock::new(|| Mutex::const_new(VolumeControllerSettings::default()));

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct VolumeControllerSettings {
    pub show_sys_mixer: bool,
    pub ignored_apps_list: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct GlobalPluginSettings {
    pub ignored_apps_list: Vec<String>,
    pub application_columns: Vec<crate::app_column::PersistedColumn>,
    pub dynamic_focus: Vec<crate::dynamic::PersistedFocus>,
}

pub struct GlobalHandler;

#[async_trait]
impl GlobalEventHandler for GlobalHandler {
    async fn plugin_ready(&self) -> OpenActionResult<()> {
        // Request global settings on startup so ignored_apps_list is loaded
        let _ = get_global_settings().await;
        Ok(())
    }

    async fn did_receive_global_settings(
        &self,
        event: DidReceiveGlobalSettingsEvent,
    ) -> OpenActionResult<()> {
        let global: GlobalPluginSettings =
            serde_json::from_value(event.payload.settings).unwrap_or_default();
        crate::app_column::load_persisted_columns(global.application_columns.clone()).await;
        crate::dynamic::load_persisted_focus(global.dynamic_focus.clone()).await;

        println!(
            "did_receive_global_settings: {} ignored apps",
            global.ignored_apps_list.len()
        );

        let mut shared = SHARED_SETTINGS.lock().await;
        if shared.ignored_apps_list != global.ignored_apps_list {
            shared.ignored_apps_list = global.ignored_apps_list.clone();
            drop(shared);

            // Sync ignored_apps_list into all instance settings
            let current = SHARED_SETTINGS.lock().await.clone();
            for inst in visible_instances(VolumeControllerAction::UUID).await {
                let _ = inst.set_settings(&current).await;
            }

            let _ = refresh_audio_applications().await;
        }

        Ok(())
    }
}

pub struct VolumeControllerAction;

#[async_trait]
impl Action for VolumeControllerAction {
    const UUID: ActionUuid = "com.victormarin.volume-controller.volctrl";
    type Settings = VolumeControllerSettings;

    async fn will_disappear(
        &self,
        instance: &Instance,
        _: &Self::Settings,
    ) -> OpenActionResult<()> {
        utils::cleanup_sd_column(instance).await;

        let Some(coords) = instance.coordinates else {
            println!(
                "Warning: Instance {} has no coordinates",
                instance.instance_id
            );
            return Ok(());
        };

        let mut column_map = COLUMN_TO_CHANNEL_MAP.lock().await;
        column_map.remove(&coords.column);

        Ok(())
    }

    async fn did_receive_settings(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        println!(
            "did_receive_settings for instance {}: show_sys_mixer={}",
            instance.instance_id, settings.show_sys_mixer
        );

        // Check if show_sys_mixer changed to avoid infinite loops
        let mut cached = SHARED_SETTINGS.lock().await;
        let settings_changed = cached.show_sys_mixer != settings.show_sys_mixer;

        if settings_changed {
            println!("Settings changed, broadcasting to all instances");
            cached.show_sys_mixer = settings.show_sys_mixer;
            drop(cached);

            // Broadcast show_sys_mixer to all other instances
            for inst in visible_instances(Self::UUID).await {
                if inst.instance_id != instance.instance_id {
                    println!("Broadcasting to instance {}", inst.instance_id);
                    let _ = inst.set_settings(settings).await;
                }
            }

            // Apply show_sys_mixer setting
            utils::set_show_system_mixer(settings.show_sys_mixer);
            let _ = refresh_audio_applications().await;
        } else {
            drop(cached);
            println!("Settings unchanged, skipping broadcast");
        }

        Ok(())
    }

    async fn will_appear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
        // Sync with shared settings when appearing
        let shared = SHARED_SETTINGS.lock().await.clone();
        let _ = instance.set_settings(&shared).await;

        let Some(coords) = instance.coordinates else {
            println!(
                "Warning: Instance {} has no coordinates",
                instance.instance_id
            );
            return Ok(());
        };

        let sd_column = coords.column;
        let channel_index = {
            let mut column_map = COLUMN_TO_CHANNEL_MAP.lock().await;
            let next_index = column_map.len() as u8;
            *column_map.entry(sd_column).or_insert(next_index)
        };
        let channel = match mixer::MIXER_CHANNELS
            .lock()
            .await
            .get(&channel_index)
            .cloned()
        {
            Some(channel) => channel,
            None => {
                utils::cleanup_sd_column(instance).await;
                return Ok(());
            }
        };

        match coords.row {
            0 => {
                utils::update_header(instance, &channel).await;
            }
            1 | 2 => {
                if let Ok((upper_img, lower_img)) =
                    gfx::get_volume_bar_data_uri_split(channel.vol_percent)
                {
                    let img = if coords.row == 1 {
                        upper_img
                    } else {
                        lower_img
                    };
                    instance.set_image(Some(img), None).await?;
                }
            }
            _ => {} // Ignore other rows
        }

        Ok(())
    }

    async fn key_up(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        let mut press_control = BUTTON_PRESS_CONTROL.lock().await;

        // Validate this is the correct button press
        if let Some(action_id) = press_control.action_id.as_ref()
            && action_id != &instance.instance_id
        {
            drop(press_control);
            return Ok(());
        }

        if let Some(duration_ms) = press_control.get_release_time() {
            println!(
                "Button {} held for {} ms",
                instance.instance_id, duration_ms
            );
            drop(press_control);

            let Some(coords) = instance.coordinates else {
                println!(
                    "Warning: Instance {} has no coordinates",
                    instance.instance_id
                );
                return Ok(());
            };
            let sd_column = coords.column;

            if duration_ms > 1000 && coords.row == 0 {
                let column_map = COLUMN_TO_CHANNEL_MAP.lock().await;
                let mut channels = mixer::MIXER_CHANNELS.lock().await;

                // Look up the channel index for this SD column
                let Some(&channel_index) = column_map.get(&sd_column) else {
                    return Ok(());
                };

                if let Some(channel) = channels.get_mut(&channel_index) {
                    let app_name = channel.app_name.clone();
                    let uid = channel.uid;
                    let is_device = channel.is_device;

                    channel.mute = false;

                    // Drop locks before potentially blocking operations
                    drop(channels);
                    drop(column_map);

                    audio::set_application_group_mute(vec![(uid, is_device)], false);

                    // Read cached shared settings, append app, and save back
                    let updated_settings = {
                        let mut shared_settings = SHARED_SETTINGS.lock().await;
                        if !shared_settings.ignored_apps_list.contains(&app_name) {
                            shared_settings.ignored_apps_list.push(app_name.clone());
                        }
                        shared_settings.clone()
                    };

                    // Save ignored apps to global settings
                    let global = GlobalPluginSettings {
                        ignored_apps_list: updated_settings.ignored_apps_list.clone(),
                        application_columns: crate::app_column::persisted_columns().await,
                        dynamic_focus: crate::dynamic::persisted_focus().await,
                    };
                    let _ = set_global_settings(global).await;

                    // Broadcast to ALL instances (including this one)
                    for inst in visible_instances(Self::UUID).await {
                        let _ = inst.set_settings(&updated_settings).await;
                    }

                    println!(
                        "Added {} to ignored apps list and broadcast to all instances",
                        app_name
                    );
                }
            }
        }

        Ok(())
    }

    async fn key_down(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
        let mut press_control = BUTTON_PRESS_CONTROL.lock().await;
        press_control.set_press_time(instance.instance_id.clone());
        drop(press_control); // Release lock early

        let Some(coords) = instance.coordinates else {
            println!(
                "Warning: Instance {} has no coordinates",
                instance.instance_id
            );
            return Ok(());
        };

        let sd_column = coords.column;
        let Some(channel_index) = COLUMN_TO_CHANNEL_MAP.lock().await.get(&sd_column).copied()
        else {
            return Ok(());
        };
        let command = {
            let mut channels = mixer::MIXER_CHANNELS.lock().await;
            let Some(channel) = channels.get_mut(&channel_index) else {
                return Ok(());
            };
            match coords.row {
                0 => {
                    channel.mute = !channel.mute;
                    Some((channel.uid, channel.is_device, None, Some(channel.mute)))
                }
                1 => {
                    let target =
                        (f64::from(channel.vol_percent) + VOLUME_INCREMENT).clamp(0.0, 100.0);
                    channel.vol_percent = target as f32;
                    Some((channel.uid, channel.is_device, Some(target), None))
                }
                2 => {
                    let target =
                        (f64::from(channel.vol_percent) - VOLUME_INCREMENT).clamp(0.0, 100.0);
                    channel.vol_percent = target as f32;
                    Some((channel.uid, channel.is_device, Some(target), None))
                }
                _ => None,
            }
        };
        if let Some((uid, is_device, volume, mute)) = command {
            if let Some(volume) = volume {
                audio::set_application_group_volume(vec![(uid, is_device)], volume);
            }
            if let Some(mute) = mute {
                audio::set_application_group_mute(vec![(uid, is_device)], mute);
            }
        }

        Ok(())
    }
}

pub async fn init() -> OpenActionResult<()> {
    println!("Stream Deck connected - starting PulseAudio monitoring");

    // start listening to changes
    audio::pulse::start_pulse_monitoring();

    // create initial map (ignored apps will be loaded via did_receive_global_settings)
    let (applications, outputs, inputs) = {
        let mut audio_system = create();
        let applications = audio_system
            .list_applications()
            .expect("Error fetching applications from SinkController");
        let outputs = audio_system
            .list_devices(crate::audio::DeviceKind::Output)
            .unwrap_or_default();
        let inputs = audio_system
            .list_devices(crate::audio::DeviceKind::Input)
            .unwrap_or_default();
        (applications, outputs, inputs)
    };
    audio::update_registry(applications.clone(), outputs, inputs);

    let ignored_apps = SHARED_SETTINGS.lock().await.ignored_apps_list.clone();
    mixer::create_mixer_channels(applications, &ignored_apps).await;

    // Register global event handler and action
    set_global_event_handler(&GlobalHandler);
    register_action(VolumeControllerAction).await;
    register_action(ApplicationVolumeColumnAction).await;
    register_action(ApplicationVolumeDialAction).await;
    register_action(AudioDeviceVolumeDialAction).await;
    register_action(DynamicApplicationVolumeDialAction).await;
    register_action(ApplicationSelectorButtonAction).await;

    run(std::env::args().collect()).await
}
