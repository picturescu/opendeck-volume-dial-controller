use crate::audio::{AppInfo, AudioDeviceInfo, AudioSystem, DeviceKind, is_monitor_source};
use libpulse_binding::volume::{ChannelVolumes, Volume};
use std::collections::BTreeMap;
use std::error::Error;

use super::client::PulseClient;

pub fn pulse_raw_to_percent(raw: u32) -> f64 {
    f64::from(raw) / f64::from(Volume::NORMAL.0) * 100.0
}

pub fn percent_to_pulse_raw(percent: f64) -> u32 {
    if !percent.is_finite() {
        return Volume::MUTED.0;
    }
    (percent.max(0.0) / 100.0 * f64::from(Volume::NORMAL.0)).round() as u32
}

pub struct PulseAudioSystem {
    client: PulseClient,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeviceMuteApi {
    Sink,
    Source,
}

fn device_mute_api(kind: DeviceKind) -> DeviceMuteApi {
    match kind {
        DeviceKind::Output => DeviceMuteApi::Sink,
        DeviceKind::Input => DeviceMuteApi::Source,
    }
}

impl PulseAudioSystem {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            client: PulseClient::connect("OpenDeck Volume Controller")?,
        })
    }
}

impl AudioSystem for PulseAudioSystem {
    fn list_devices(&mut self, kind: DeviceKind) -> Result<Vec<AudioDeviceInfo>, Box<dyn Error>> {
        let devices = match kind {
            DeviceKind::Output => self
                .client
                .list_sinks()?
                .into_iter()
                .map(|device| AudioDeviceInfo {
                    stable_name: device.name,
                    description: device
                        .description
                        .unwrap_or_else(|| "Audio output".to_owned()),
                    mute: device.mute,
                    vol_percent: get_pulse_app_volume_percentage(&device.volume),
                    icon_name: device.icon_name,
                    kind,
                })
                .collect(),
            DeviceKind::Input => self
                .client
                .list_sources()?
                .into_iter()
                .filter(|device| {
                    device.monitor_of_sink.is_none()
                        && !is_monitor_source(
                            &device.name,
                            device.description.as_deref().unwrap_or_default(),
                        )
                })
                .map(|device| AudioDeviceInfo {
                    stable_name: device.name,
                    description: device
                        .description
                        .unwrap_or_else(|| "Audio input".to_owned()),
                    mute: device.mute,
                    vol_percent: get_pulse_app_volume_percentage(&device.volume),
                    icon_name: device.icon_name,
                    kind,
                })
                .collect(),
        };
        Ok(devices)
    }

    fn list_applications(&mut self) -> Result<Vec<AppInfo>, Box<dyn Error>> {
        let mut res: Vec<AppInfo> = Vec::new();

        // Add individual applications first to collect all app names
        let apps = self.client.list_playback_streams()?;
        let client_metadata = self.client.client_metadata().unwrap_or_default();

        // Collect all app names including system mixer if present
        let mut app_names: Vec<String> = apps
            .iter()
            .map(|app| {
                app.properties
                    .get("application.name")
                    .cloned()
                    .unwrap_or("app_stream".to_string())
                    .to_lowercase()
            })
            .collect();

        // Add the default system sink (main PC audio) only if the global flag is set
        if crate::utils::should_show_system_mixer()
            && let Ok(default_sink) = self.client.default_sink()
        {
            let system_name = default_sink
                .description
                .clone()
                .unwrap_or("System Audio".to_string());

            // Add system mixer name to app_names for duplicate detection
            app_names.push(system_name.clone());

            res.push(AppInfo {
                uid: default_sink.index,
                app_name: system_name,
                sink_name: Some("System Audio".to_string()),
                mute: default_sink.mute,
                vol_percent: get_pulse_app_volume_percentage(&default_sink.volume),
                icon_name: Some("audio-card".to_string()),
                is_device: true,
                is_multi_sink_app: false,
                metadata: BTreeMap::new(),
            });
        }

        res.extend(apps.into_iter().map(|app| {
            let app_name = app
                .properties
                .get("application.name")
                .cloned()
                .unwrap_or("app_stream".to_string())
                .to_lowercase();

            let name_count = app_names.iter().filter(|&name| name == &app_name).count();

            let mut metadata = app.properties.clone();
            if let Some(client) = app.client.and_then(|index| client_metadata.get(&index)) {
                for (key, value) in client {
                    metadata.entry(key.clone()).or_insert_with(|| value.clone());
                }
                if !metadata.contains_key("application.process.id")
                    && let Some(pid) = metadata.get("pipewire.sec.pid").cloned()
                {
                    metadata.insert("application.process.id".into(), pid);
                }
            }

            AppInfo {
                uid: app.index,
                app_name,
                sink_name: app.name,
                mute: app.mute,
                vol_percent: get_pulse_app_volume_percentage(&app.volume),
                icon_name: app.properties.get("application.icon_name").cloned(),
                is_device: false,
                is_multi_sink_app: name_count > 1,
                metadata,
            }
        }));

        Ok(res)
    }

    fn set_volume_percent(
        &mut self,
        app_index: u32,
        percent: f64,
        is_device: bool,
    ) -> Result<(), Box<dyn Error>> {
        let volume = Volume(percent_to_pulse_raw(percent));
        if is_device {
            let mut device = self.client.sink_by_index(app_index)?;
            let channels = device.volume.len();
            device.volume.set(channels, volume);
            self.client.set_sink_volume(&device.name, &device.volume)?;
        } else {
            let mut app = self.client.playback_stream(app_index)?;
            let channels = app.volume.len();
            app.volume.set(channels, volume);
            self.client.set_playback_volume(app_index, &app.volume)?;
        }
        Ok(())
    }

    fn mute_volume(
        &mut self,
        app_index: u32,
        mute: bool,
        is_device: bool,
    ) -> Result<(), Box<dyn Error>> {
        if is_device {
            let device = self.client.sink_by_index(app_index)?;
            self.client.set_sink_mute(&device.name, mute)?;
        } else {
            self.client.set_playback_mute(app_index, mute)?;
        }
        Ok(())
    }

    fn set_device_target(
        &mut self,
        stable_name: &str,
        kind: DeviceKind,
        percent: f64,
    ) -> Result<(), Box<dyn Error>> {
        let volume = Volume(percent_to_pulse_raw(percent));
        match kind {
            DeviceKind::Output => {
                let mut device = self.client.sink_by_name(stable_name)?;
                let channels = device.volume.len();
                device.volume.set(channels, volume);
                self.client.set_sink_volume(stable_name, &device.volume)?;
            }
            DeviceKind::Input => {
                let mut device = self.client.source_by_name(stable_name)?;
                let channels = device.volume.len();
                device.volume.set(channels, volume);
                self.client.set_source_volume(stable_name, &device.volume)?;
            }
        }
        Ok(())
    }

    fn mute_device(
        &mut self,
        stable_name: &str,
        kind: DeviceKind,
        mute: bool,
    ) -> Result<(), Box<dyn Error>> {
        match device_mute_api(kind) {
            DeviceMuteApi::Sink => self.client.set_sink_mute(stable_name, mute)?,
            DeviceMuteApi::Source => {
                let source = self.client.source_by_name(stable_name)?;
                eprintln!(
                    "[device-dial] kind=input stable_source={stable_name} runtime_index={} current_muted={} requested_muted={mute}",
                    source.index, source.mute
                );
                match self.client.set_source_mute(stable_name, mute) {
                    Ok(()) => eprintln!("[device-dial] source mute result=success"),
                    Err(error) => {
                        eprintln!("[device-dial] source mute result=error: {error}");
                        return Err(error);
                    }
                }
            }
        }
        Ok(())
    }

    fn query_application_state(&mut self, index: u32) -> Result<(f32, bool), Box<dyn Error>> {
        let app = self.client.playback_stream(index)?;
        Ok((get_pulse_app_volume_percentage(&app.volume), app.mute))
    }

    fn query_device_state(
        &mut self,
        index: u32,
        kind: DeviceKind,
    ) -> Result<(String, f32, bool), Box<dyn Error>> {
        let device = match kind {
            DeviceKind::Output => self.client.sink_by_index(index)?,
            DeviceKind::Input => self.client.source_by_index(index)?,
        };
        Ok((
            device.name,
            get_pulse_app_volume_percentage(&device.volume),
            device.mute,
        ))
    }
}

fn get_pulse_app_volume_percentage(channel_volumes: &ChannelVolumes) -> f32 {
    let channel_count = channel_volumes.len();
    if channel_count == 0 {
        return 0.0;
    }

    // Get average of all channels
    let total_volume: u64 = (0..channel_count)
        .map(|i| u64::from(channel_volumes.get()[i as usize].0))
        .sum();

    let avg_volume = total_volume as f64 / f64::from(channel_count);
    pulse_raw_to_percent(avg_volume.round() as u32) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulse_raw_converts_against_normal_volume() {
        assert_eq!(pulse_raw_to_percent(32_768), 50.0);
        assert_eq!(pulse_raw_to_percent(65_536), 100.0);
        assert_eq!(pulse_raw_to_percent(98_304), 150.0);
    }

    #[test]
    fn percent_converts_to_pulse_raw() {
        assert_eq!(percent_to_pulse_raw(50.0), 32_768);
        assert_eq!(percent_to_pulse_raw(100.0), 65_536);
        assert_eq!(percent_to_pulse_raw(150.0), 98_304);
    }

    #[test]
    fn device_mute_dispatches_outputs_to_sinks_and_inputs_to_sources() {
        assert_eq!(device_mute_api(DeviceKind::Output), DeviceMuteApi::Sink);
        assert_eq!(device_mute_api(DeviceKind::Input), DeviceMuteApi::Source);
        assert_ne!(device_mute_api(DeviceKind::Input), DeviceMuteApi::Sink);
    }
}
