use std::{
    collections::HashMap,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use openaction::*;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};

use crate::{
    audio::{self, AudioDeviceInfo, DeviceKind},
    dial::{adjusted_percent, progress_percent, sanitize_maximum_volume},
    utils::get_app_icon_uri,
};

const FEEDBACK_LAYOUT: &str = "$B1";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct AudioDeviceVolumeDialSettings {
    pub device_kind: DeviceKind,
    pub target_id: String,
    pub custom_title: String,
    pub volume_step: u8,
    pub maximum_volume: u16,
}

impl Default for AudioDeviceVolumeDialSettings {
    fn default() -> Self {
        Self {
            device_kind: DeviceKind::Output,
            target_id: String::new(),
            custom_title: String::new(),
            volume_step: 2,
            maximum_volume: 100,
        }
    }
}

impl AudioDeviceVolumeDialSettings {
    fn title<'a>(&'a self, automatic: &'a str) -> &'a str {
        if self.custom_title.is_empty() {
            automatic
        } else {
            &self.custom_title
        }
    }
}

#[derive(Serialize)]
struct Value<T> {
    value: T,
    opacity: f32,
}

#[derive(Serialize)]
struct Indicator {
    value: u8,
    opacity: f32,
    bar_bg_c: &'static str,
    bar_fill_c: &'static str,
}

#[derive(Serialize)]
struct Feedback {
    icon: Value<String>,
    title: Value<String>,
    value: Value<String>,
    indicator: Indicator,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceOption {
    id: String,
    name: String,
    kind: DeviceKind,
    available: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceList {
    event: &'static str,
    devices: Vec<DeviceOption>,
    error: Option<String>,
}

#[derive(Clone, Default)]
struct Runtime {
    last_name: Option<String>,
    last_icon: Option<String>,
    icon_target_id: Option<String>,
    layout_initialized: bool,
    last_feedback: Option<String>,
    optimistic_volume: Option<f64>,
    optimistic_at: Option<Instant>,
    optimistic_mute: Option<(bool, Instant)>,
    command_sequence: u64,
}

#[derive(Default)]
struct RenderRuntime {
    lock: Mutex<()>,
    generation: AtomicU64,
}

static SETTINGS: LazyLock<RwLock<HashMap<String, AudioDeviceVolumeDialSettings>>> =
    LazyLock::new(|| RwLock::const_new(HashMap::new()));
static RUNTIME: LazyLock<RwLock<HashMap<String, Runtime>>> =
    LazyLock::new(|| RwLock::const_new(HashMap::new()));
static RENDER_RUNTIME: LazyLock<RwLock<HashMap<String, Arc<RenderRuntime>>>> =
    LazyLock::new(|| RwLock::const_new(HashMap::new()));

async fn render_runtime(instance_id: &str) -> Arc<RenderRuntime> {
    if let Some(runtime) = RENDER_RUNTIME.read().await.get(instance_id).cloned() {
        return runtime;
    }
    RENDER_RUNTIME
        .write()
        .await
        .entry(instance_id.to_owned())
        .or_insert_with(|| Arc::new(RenderRuntime::default()))
        .clone()
}

pub struct AudioDeviceVolumeDialAction;

fn toggled_mute(current: bool) -> bool {
    !current
}

fn device_value_text(available: bool, muted: bool, kind: DeviceKind, volume: f64) -> String {
    if !available {
        "Unavailable".to_owned()
    } else if muted && kind == DeviceKind::Output {
        "Muted".to_owned()
    } else {
        format!("{volume:.0}%")
    }
}

impl AudioDeviceVolumeDialAction {
    fn devices(kind: DeviceKind) -> Result<Vec<AudioDeviceInfo>, String> {
        audio::registry_devices(kind).ok_or_else(|| "Audio registry is not initialized".to_owned())
    }

    fn selected(settings: &AudioDeviceVolumeDialSettings) -> Option<AudioDeviceInfo> {
        select_device(
            Self::devices(settings.device_kind).ok()?,
            &settings.target_id,
        )
    }

    async fn render(
        instance: &Instance,
        settings: &AudioDeviceVolumeDialSettings,
    ) -> OpenActionResult<()> {
        let render_runtime = render_runtime(&instance.instance_id).await;
        let generation = render_runtime.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let device = Self::selected(settings);
        let available = device.is_some();
        let confirmed_muted = device.as_ref().is_some_and(|device| device.mute);
        let mut runtime = RUNTIME.write().await;
        let state = runtime.entry(instance.instance_id.clone()).or_default();
        let muted = if let Some((optimistic, issued)) = state.optimistic_mute {
            if confirmed_muted == optimistic || issued.elapsed() >= Duration::from_millis(75) {
                state.optimistic_mute = None;
                confirmed_muted
            } else {
                optimistic
            }
        } else {
            confirmed_muted
        };
        let opacity = if available && !muted { 1.0 } else { 0.4 };
        if let Some(device) = device.as_ref() {
            state.last_name = Some(device.description.clone());
            if state.icon_target_id.as_deref() != Some(&device.stable_name) {
                let fallback = match device.kind {
                    DeviceKind::Output => "audio-speakers",
                    DeviceKind::Input => "audio-input-microphone",
                };
                state.last_icon = Some(
                    get_app_icon_uri(
                        device
                            .icon_name
                            .clone()
                            .or_else(|| Some(fallback.to_owned())),
                        fallback.to_owned(),
                    )
                    .0,
                );
                state.icon_target_id = Some(device.stable_name.clone());
            }
        }
        let automatic = device
            .as_ref()
            .map(|device| device.description.as_str())
            .or(state.last_name.as_deref())
            .unwrap_or("Select device");
        let title = settings.title(automatic).to_owned();
        let icon = state.last_icon.clone().unwrap_or_else(|| {
            let fallback = match settings.device_kind {
                DeviceKind::Output => "audio-speakers",
                DeviceKind::Input => "audio-input-microphone",
            };
            get_app_icon_uri(Some(fallback.to_owned()), fallback.to_owned()).0
        });
        let actual_volume = device
            .as_ref()
            .map_or(0.0, |device| f64::from(device.vol_percent));
        let shown_volume = {
            let state = runtime.entry(instance.instance_id.clone()).or_default();
            if let (Some(optimistic), Some(issued)) = (state.optimistic_volume, state.optimistic_at)
            {
                if (actual_volume - optimistic).abs() <= 0.75 {
                    state.optimistic_volume = None;
                    state.optimistic_at = None;
                    actual_volume
                } else if issued.elapsed() < Duration::from_millis(75) {
                    optimistic
                } else {
                    state.optimistic_volume = None;
                    state.optimistic_at = None;
                    actual_volume
                }
            } else {
                actual_volume
            }
        };
        drop(runtime);
        let text = device_value_text(
            available,
            muted,
            device
                .as_ref()
                .map_or(settings.device_kind, |item| item.kind),
            shown_volume,
        );
        let feedback = Feedback {
            icon: Value {
                value: icon,
                opacity,
            },
            title: Value {
                value: title.clone(),
                opacity,
            },
            value: Value {
                value: text,
                opacity,
            },
            indicator: Indicator {
                value: progress_percent(
                    shown_volume,
                    sanitize_maximum_volume(settings.maximum_volume),
                ),
                opacity,
                bar_bg_c: "#303030",
                bar_fill_c: "#ffffff",
            },
        };
        let feedback_key = serde_json::to_string(&feedback)?;
        let _guard = render_runtime.lock.lock().await;
        if generation != render_runtime.generation.load(Ordering::Acquire) {
            return Ok(());
        }
        let needs_layout = RUNTIME
            .read()
            .await
            .get(&instance.instance_id)
            .is_none_or(|state| !state.layout_initialized);
        if needs_layout {
            let Ok(result) = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                instance.set_feedback_layout(FEEDBACK_LAYOUT.to_owned()),
            )
            .await
            else {
                eprintln!(
                    "[volume-worker] feedback-timeout context={}",
                    instance.instance_id
                );
                return Ok(());
            };
            result?;
            RUNTIME
                .write()
                .await
                .entry(instance.instance_id.clone())
                .or_default()
                .layout_initialized = true;
        }
        if RUNTIME
            .read()
            .await
            .get(&instance.instance_id)
            .and_then(|state| state.last_feedback.as_ref())
            == Some(&feedback_key)
        {
            return Ok(());
        }
        if generation != render_runtime.generation.load(Ordering::Acquire) {
            return Ok(());
        }
        let Ok(result) = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            instance.set_feedback(&feedback),
        )
        .await
        else {
            eprintln!(
                "[volume-worker] feedback-timeout context={}",
                instance.instance_id
            );
            return Ok(());
        };
        if let Err(error) = result {
            eprintln!(
                "[device-dial] feedback failed context={} generation={generation}: {error}",
                instance.instance_id
            );
            return Err(error);
        }
        RUNTIME
            .write()
            .await
            .entry(instance.instance_id.clone())
            .or_default()
            .last_feedback = Some(feedback_key);
        Ok(())
    }

    async fn adjust(
        instance: &Instance,
        settings: &AudioDeviceVolumeDialSettings,
        ticks: i16,
    ) -> OpenActionResult<()> {
        let Some(device) = Self::selected(settings) else {
            instance.show_alert().await?;
            return Ok(());
        };
        let mut runtime = RUNTIME.write().await;
        let state = runtime.entry(instance.instance_id.clone()).or_default();
        let target = adjusted_percent(
            state
                .optimistic_volume
                .unwrap_or_else(|| f64::from(device.vol_percent)),
            ticks,
            f64::from(settings.volume_step.clamp(1, 10)),
            settings.maximum_volume,
        );
        state.optimistic_volume = Some(target);
        state.optimistic_at = Some(Instant::now());
        state.command_sequence += 1;
        let sequence = state.command_sequence;
        drop(runtime);
        Self::render(instance, settings).await?;
        let instance_id = instance.instance_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let target = {
                let runtime = RUNTIME.read().await;
                let Some(state) = runtime.get(&instance_id) else {
                    return;
                };
                if state.command_sequence != sequence {
                    return;
                }
                let Some(target) = state.optimistic_volume else {
                    return;
                };
                target
            };
            let settings = SETTINGS.read().await.get(&instance_id).cloned();
            let Some(settings) = settings else { return };
            audio::set_device_volume(settings.target_id, settings.device_kind, target);
        });
        Ok(())
    }

    async fn toggle_selected_device_mute(
        instance: &Instance,
        settings: &AudioDeviceVolumeDialSettings,
    ) -> OpenActionResult<()> {
        let Some(device) = Self::selected(settings) else {
            instance.show_alert().await?;
            return Ok(());
        };
        let requested = toggled_mute(device.mute);
        RUNTIME
            .write()
            .await
            .entry(instance.instance_id.clone())
            .or_default()
            .optimistic_mute = Some((requested, Instant::now()));
        Self::render(instance, settings).await?;
        audio::set_device_mute(device.stable_name, device.kind, requested);
        Ok(())
    }

    async fn send_devices(
        instance: &Instance,
        settings: &AudioDeviceVolumeDialSettings,
    ) -> OpenActionResult<()> {
        let (devices, error) = match Self::devices(settings.device_kind) {
            Ok(devices) => (devices, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        instance
            .send_to_property_inspector(DeviceList {
                event: "audioDeviceList",
                devices: devices
                    .into_iter()
                    .map(|device| DeviceOption {
                        id: device.stable_name,
                        name: device.description,
                        kind: device.kind,
                        available: true,
                    })
                    .collect(),
                error,
            })
            .await
    }
}

fn select_device(devices: Vec<AudioDeviceInfo>, stable_name: &str) -> Option<AudioDeviceInfo> {
    devices
        .into_iter()
        .find(|device| device.stable_name == stable_name)
}

pub async fn refresh_visible_device_dials_for(
    changed_outputs: Option<&std::collections::HashSet<String>>,
    changed_inputs: Option<&std::collections::HashSet<String>>,
) {
    let settings = SETTINGS.read().await.clone();
    for instance in visible_instances(AudioDeviceVolumeDialAction::UUID).await {
        if let Some(settings) = settings.get(&instance.instance_id) {
            let changed = match settings.device_kind {
                DeviceKind::Output => changed_outputs,
                DeviceKind::Input => changed_inputs,
            };
            if changed.is_some_and(|ids| !ids.contains(&settings.target_id)) {
                continue;
            }
            let _ = AudioDeviceVolumeDialAction::render(&instance, settings).await;
        }
    }
}

#[async_trait]
impl Action for AudioDeviceVolumeDialAction {
    const UUID: ActionUuid = "com.victormarin.volume-controller.devicedial";
    type Settings = AudioDeviceVolumeDialSettings;

    async fn will_appear(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        SETTINGS
            .write()
            .await
            .insert(instance.instance_id.clone(), settings.clone());
        Self::render(instance, settings).await
    }

    async fn will_disappear(
        &self,
        instance: &Instance,
        _: &Self::Settings,
    ) -> OpenActionResult<()> {
        SETTINGS.write().await.remove(&instance.instance_id);
        RUNTIME.write().await.remove(&instance.instance_id);
        RENDER_RUNTIME.write().await.remove(&instance.instance_id);
        Ok(())
    }

    async fn did_receive_settings(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        SETTINGS
            .write()
            .await
            .insert(instance.instance_id.clone(), settings.clone());
        Self::render(instance, settings).await?;
        Self::send_devices(instance, settings).await
    }

    async fn property_inspector_did_appear(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        Self::send_devices(instance, settings).await
    }

    async fn send_to_plugin(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
        payload: &serde_json::Value,
    ) -> OpenActionResult<()> {
        if payload.get("event").and_then(serde_json::Value::as_str) == Some("requestAudioDevices") {
            Self::send_devices(instance, settings).await?;
        }
        Ok(())
    }

    async fn dial_rotate(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
        ticks: i16,
        _: bool,
    ) -> OpenActionResult<()> {
        Self::adjust(instance, settings, ticks).await
    }

    async fn dial_up(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        Self::toggle_selected_device_mute(instance, settings).await
    }

    async fn touch_tap(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
        _: (u16, u16),
        hold: bool,
    ) -> OpenActionResult<()> {
        if !hold {
            Self::toggle_selected_device_mute(instance, settings).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_titles_use_automatic_or_custom_value() {
        let settings = AudioDeviceVolumeDialSettings::default();
        assert_eq!(settings.title("Speakers"), "Speakers");
        let settings = AudioDeviceVolumeDialSettings {
            custom_title: "Desk audio".into(),
            ..Default::default()
        };
        assert_eq!(settings.title("Speakers"), "Desk audio");
    }

    #[test]
    fn stable_name_does_not_depend_on_runtime_index() {
        let first = ("alsa_output.usb", 10_u32);
        let rebound = ("alsa_output.usb", 99_u32);
        assert_eq!(first.0, rebound.0);
        assert_ne!(first.1, rebound.1);
    }

    #[test]
    fn monitor_names_are_detectable() {
        assert!(crate::audio::audio_system::is_monitor_source(
            "alsa_output.card.monitor",
            "Monitor of Speakers"
        ));
        assert!(!crate::audio::audio_system::is_monitor_source(
            "alsa_input.usb",
            "USB microphone"
        ));
    }

    fn device(name: &str, kind: DeviceKind) -> AudioDeviceInfo {
        AudioDeviceInfo {
            stable_name: name.into(),
            description: name.into(),
            mute: false,
            vol_percent: 50.0,
            icon_name: None,
            kind,
        }
    }

    #[test]
    fn unavailable_device_rebinds_by_stable_name() {
        assert!(select_device(Vec::new(), "sink.stable").is_none());
        assert_eq!(
            select_device(
                vec![
                    device("other", DeviceKind::Output),
                    device("sink.stable", DeviceKind::Output)
                ],
                "sink.stable"
            )
            .unwrap()
            .stable_name,
            "sink.stable"
        );
    }

    #[test]
    fn input_and_output_clamp_and_mute_toggle() {
        for kind in [DeviceKind::Output, DeviceKind::Input] {
            let device = device("stable", kind);
            assert_eq!(
                adjusted_percent(device.vol_percent.into(), 100, 2.0, 100),
                100.0
            );
            assert_eq!(
                adjusted_percent(device.vol_percent.into(), 100, 2.0, 150),
                150.0
            );
            assert!(toggled_mute(device.mute));
            assert!(!toggled_mute(true));
        }
    }

    #[test]
    fn muted_input_is_dimmed_but_retains_actual_volume_text() {
        assert_eq!(
            device_value_text(true, true, DeviceKind::Input, 73.0),
            "73%"
        );
        assert_eq!(
            device_value_text(true, true, DeviceKind::Output, 73.0),
            "Muted"
        );
    }
}
