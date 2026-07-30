use openaction::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, RwLock};

use crate::application_targets::{ApplicationTargetOption, application_target_inventory};
use crate::audio::{self, AppInfo};
use crate::icons::{resolve_app_icon, stable_application_id};
use crate::utils::get_app_icon_uri;

const FEEDBACK_LAYOUT: &str = "$B1";
static DIAL_SETTINGS: LazyLock<RwLock<HashMap<String, ApplicationVolumeDialSettings>>> =
    LazyLock::new(|| RwLock::const_new(HashMap::new()));
static DIAL_RUNTIME: LazyLock<RwLock<HashMap<String, ApplicationDialRuntime>>> =
    LazyLock::new(|| RwLock::const_new(HashMap::new()));

#[derive(Clone, Debug, Default)]
struct ApplicationDialRuntime {
    active_target_id: Option<String>,
    last_active_target_id: Option<String>,
    last_display_name: Option<String>,
    last_icon: Option<String>,
    icon_target_id: Option<String>,
    layout_initialized: bool,
    last_feedback: Option<String>,
    optimistic_volume: Option<f64>,
    optimistic_at: Option<Instant>,
    command_sequence: u64,
}

#[derive(Default)]
struct RenderRuntime {
    lock: Mutex<()>,
    generation: AtomicU64,
}

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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct ApplicationVolumeDialSettings {
    pub target_ids: Vec<String>,
    pub custom_title: String,
    pub target_id: String,
    pub target_name: String,
    pub volume_step: u8,
    pub maximum_volume: u16,
}

impl Default for ApplicationVolumeDialSettings {
    fn default() -> Self {
        Self {
            target_ids: Vec::new(),
            custom_title: String::new(),
            target_id: String::new(),
            target_name: String::new(),
            volume_step: 2,
            maximum_volume: 100,
        }
    }
}

impl ApplicationVolumeDialSettings {
    fn step(&self) -> f64 {
        f64::from(self.volume_step.clamp(1, 10))
    }

    fn maximum(&self) -> u16 {
        sanitize_maximum_volume(self.maximum_volume)
    }

    pub(crate) fn normalized_target_ids(&self) -> Vec<String> {
        let values = if self.target_ids.is_empty() && !self.target_id.is_empty() {
            vec![self.target_id.clone()]
        } else {
            self.target_ids.clone()
        };
        let mut seen = std::collections::HashSet::new();
        values
            .into_iter()
            .filter(|value| !value.is_empty() && seen.insert(value.clone()))
            .collect()
    }

    fn title<'a>(&'a self, automatic: &'a str) -> &'a str {
        if self.custom_title.is_empty() {
            automatic
        } else {
            &self.custom_title
        }
    }
}

pub fn sanitize_maximum_volume(value: u16) -> u16 {
    if value == 150 { 150 } else { 100 }
}

pub fn clamp_target_percent(percent: f64, maximum: u16) -> f64 {
    if !percent.is_finite() {
        return 0.0;
    }
    percent.clamp(0.0, f64::from(sanitize_maximum_volume(maximum)))
}

pub fn adjusted_percent(current: f64, ticks: i16, step: f64, maximum: u16) -> f64 {
    clamp_target_percent(current + f64::from(ticks) * step.clamp(1.0, 10.0), maximum)
}

pub fn progress_percent(actual: f64, maximum: u16) -> u8 {
    ((actual / f64::from(sanitize_maximum_volume(maximum))) * 100.0)
        .clamp(0.0, 100.0)
        .round() as u8
}

pub fn arbitrate_active(
    configured: &[String],
    available: &std::collections::HashSet<String>,
    current: Option<&str>,
) -> Option<String> {
    if let Some(current) = current
        && configured.iter().any(|target| target == current)
        && available.contains(current)
    {
        return Some(current.to_owned());
    }
    configured
        .iter()
        .find(|target| available.contains(*target))
        .cloned()
}

#[derive(Serialize)]
struct FeedbackValue<T> {
    value: T,
    opacity: f32,
}

#[derive(Serialize)]
struct IndicatorFeedback {
    value: u8,
    opacity: f32,
    bar_bg_c: &'static str,
    bar_fill_c: &'static str,
}

#[derive(Serialize)]
struct DialFeedback {
    icon: FeedbackValue<String>,
    title: FeedbackValue<String>,
    value: FeedbackValue<String>,
    indicator: IndicatorFeedback,
}

fn build_feedback(
    icon: String,
    title: String,
    text: String,
    volume: f64,
    maximum: u16,
    opacity: f32,
) -> DialFeedback {
    DialFeedback {
        icon: FeedbackValue {
            value: icon,
            opacity,
        },
        title: FeedbackValue {
            value: title,
            opacity,
        },
        value: FeedbackValue {
            value: text,
            opacity,
        },
        indicator: IndicatorFeedback {
            value: progress_percent(volume, maximum),
            opacity,
            bar_bg_c: "#303030",
            bar_fill_c: "#ffffff",
        },
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TargetList {
    event: &'static str,
    targets: Vec<ApplicationTargetOption>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    active_target_id: Option<String>,
}

pub struct ApplicationVolumeDialAction;

pub async fn refresh_visible_dials_for(changed: Option<&std::collections::HashSet<String>>) {
    let settings = DIAL_SETTINGS.read().await.clone();
    for instance in visible_instances(ApplicationVolumeDialAction::UUID).await {
        if let Some(settings) = settings.get(&instance.instance_id) {
            if let Some(changed) = changed {
                let configured = settings.normalized_target_ids();
                let active = DIAL_RUNTIME
                    .read()
                    .await
                    .get(&instance.instance_id)
                    .and_then(|state| state.active_target_id.clone());
                if !should_refresh_for(&configured, active.as_deref(), changed) {
                    continue;
                }
            }
            let _ = ApplicationVolumeDialAction::render(&instance, settings).await;
        }
    }
}

fn should_refresh_for(
    configured: &[String],
    active: Option<&str>,
    changed: &std::collections::HashSet<String>,
) -> bool {
    configured.iter().any(|id| changed.contains(id))
        || active.is_some_and(|id| changed.contains(id))
}

impl ApplicationVolumeDialAction {
    fn target_id(app: &AppInfo) -> String {
        stable_application_id(app)
    }

    fn applications() -> Result<Vec<AppInfo>, String> {
        audio::registry_applications().ok_or_else(|| "Audio registry is not initialized".to_owned())
    }

    async fn selected(
        instance: &Instance,
        settings: &ApplicationVolumeDialSettings,
    ) -> Result<Vec<AppInfo>, String> {
        let applications = Self::applications()?;
        let available = applications
            .iter()
            .map(Self::target_id)
            .collect::<std::collections::HashSet<_>>();
        let configured = settings.normalized_target_ids();
        let mut runtime = DIAL_RUNTIME.write().await;
        let state = runtime.entry(instance.instance_id.clone()).or_default();
        let active = arbitrate_active(&configured, &available, state.active_target_id.as_deref());
        state.active_target_id = active.clone();
        if active.is_some() {
            state.last_active_target_id = active.clone();
        }
        Ok(applications
            .into_iter()
            .filter(|app| {
                active
                    .as_ref()
                    .is_some_and(|id| Self::target_id(app) == *id)
            })
            .collect())
    }

    async fn send_targets(instance: &Instance) -> OpenActionResult<()> {
        let (targets, enumeration_error) = application_target_inventory();
        let message = TargetList {
            event: "audioTargetList",
            targets,
            error: enumeration_error,
            active_target_id: DIAL_RUNTIME
                .read()
                .await
                .get(&instance.instance_id)
                .and_then(|state| state.active_target_id.clone()),
        };
        match instance.send_to_property_inspector(message).await {
            Ok(()) => {
                eprintln!("[volume-dial] send_to_property_inspector success");
                Ok(())
            }
            Err(error) => {
                eprintln!("[volume-dial] send_to_property_inspector failure: {error}");
                Err(error)
            }
        }
    }

    async fn render(
        instance: &Instance,
        settings: &ApplicationVolumeDialSettings,
    ) -> OpenActionResult<()> {
        let render_runtime = render_runtime(&instance.instance_id).await;
        let generation = render_runtime.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let targets = Self::selected(instance, settings).await.unwrap_or_default();
        let target = targets.first();
        let available = target.is_some();
        let muted = available && targets.iter().all(|app| app.mute);
        let opacity = if available && !muted { 1.0 } else { 0.4 };
        let automatic_title = target
            .as_ref()
            .map(|app| app.app_name.clone())
            .filter(|name| !name.is_empty())
            .or_else(|| {
                DIAL_RUNTIME.try_read().ok().and_then(|runtime| {
                    runtime
                        .get(&instance.instance_id)?
                        .last_display_name
                        .clone()
                })
            })
            .unwrap_or_else(|| "No active app".to_owned());
        let title = settings.title(&automatic_title).to_owned();
        let actual_volume = if targets.is_empty() {
            0.0
        } else {
            f64::from(targets.iter().map(|app| app.vol_percent).sum::<f32>() / targets.len() as f32)
        };
        let icon = if let Some(app) = target {
            let target_id = Self::target_id(app);
            let cached = DIAL_RUNTIME
                .read()
                .await
                .get(&instance.instance_id)
                .filter(|state| state.icon_target_id.as_deref() == Some(&target_id))
                .and_then(|state| state.last_icon.clone());
            if let Some(icon) = cached {
                icon
            } else {
                let icon = resolve_app_icon(app).rendered_data_uri;
                if let Some(state) = DIAL_RUNTIME.write().await.get_mut(&instance.instance_id) {
                    state.last_display_name = Some(app.app_name.clone());
                    state.last_icon = Some(icon.clone());
                    state.icon_target_id = Some(target_id);
                }
                icon
            }
        } else {
            DIAL_RUNTIME
                .read()
                .await
                .get(&instance.instance_id)
                .and_then(|state| state.last_icon.clone())
                .unwrap_or_else(|| get_app_icon_uri(None, title.clone()).0)
        };
        let shown_volume = {
            let mut runtime = DIAL_RUNTIME.write().await;
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
        let value = if !available {
            "Unavailable".to_owned()
        } else if muted {
            "Muted".to_owned()
        } else {
            format!("{shown_volume:.0}%")
        };
        let feedback = build_feedback(
            icon,
            title,
            value,
            shown_volume,
            settings.maximum(),
            opacity,
        );
        let feedback_key = serde_json::to_string(&feedback)?;
        let _guard = render_runtime.lock.lock().await;
        if generation != render_runtime.generation.load(Ordering::Acquire) {
            return Ok(());
        }
        let needs_layout = DIAL_RUNTIME
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
            DIAL_RUNTIME
                .write()
                .await
                .entry(instance.instance_id.clone())
                .or_default()
                .layout_initialized = true;
        }
        if DIAL_RUNTIME
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
                "[volume-dial] feedback failed context={} generation={generation}: {error}",
                instance.instance_id
            );
            return Err(error);
        }
        DIAL_RUNTIME
            .write()
            .await
            .entry(instance.instance_id.clone())
            .or_default()
            .last_feedback = Some(feedback_key);
        Ok(())
    }

    async fn adjust(
        instance: &Instance,
        settings: &ApplicationVolumeDialSettings,
        ticks: i16,
    ) -> OpenActionResult<()> {
        let apps = Self::selected(instance, settings).await.unwrap_or_default();
        if apps.is_empty() {
            instance.show_alert().await?;
            return Ok(());
        }
        if ticks != 0 {
            let confirmed = apps
                .iter()
                .map(|app| f64::from(app.vol_percent))
                .sum::<f64>()
                / apps.len() as f64;
            let mut runtime = DIAL_RUNTIME.write().await;
            let state = runtime.entry(instance.instance_id.clone()).or_default();
            let current = state.optimistic_volume.unwrap_or(confirmed);
            let target = adjusted_percent(current, ticks, settings.step(), settings.maximum());
            state.optimistic_volume = Some(target);
            state.optimistic_at = Some(Instant::now());
            state.command_sequence += 1;
            let sequence = state.command_sequence;
            drop(runtime);
            Self::render(instance, settings).await?;

            let instance_id = instance.instance_id.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                let current_sequence = DIAL_RUNTIME
                    .read()
                    .await
                    .get(&instance_id)
                    .map_or(0, |state| state.command_sequence);
                if current_sequence != sequence {
                    return;
                }
                let settings = DIAL_SETTINGS.read().await.get(&instance_id).cloned();
                let Some(settings) = settings else { return };
                let Some(instance) = visible_instances(ApplicationVolumeDialAction::UUID)
                    .await
                    .into_iter()
                    .find(|candidate| candidate.instance_id == instance_id)
                else {
                    return;
                };
                let apps = ApplicationVolumeDialAction::selected(&instance, &settings)
                    .await
                    .unwrap_or_default();
                let target = DIAL_RUNTIME
                    .read()
                    .await
                    .get(&instance_id)
                    .and_then(|state| state.optimistic_volume);
                let Some(target) = target else { return };
                audio::set_application_group_volume(
                    apps.iter().map(|app| (app.uid, app.is_device)).collect(),
                    target,
                );
            });
            return Ok(());
        }
        Self::render(instance, settings).await
    }

    async fn toggle(
        instance: &Instance,
        settings: &ApplicationVolumeDialSettings,
    ) -> OpenActionResult<()> {
        let apps = Self::selected(instance, settings).await.unwrap_or_default();
        if apps.is_empty() {
            instance.show_alert().await?;
            return Ok(());
        }
        let mute = !apps.iter().all(|app| app.mute);
        audio::set_application_group_mute(
            apps.iter().map(|app| (app.uid, app.is_device)).collect(),
            mute,
        );
        Self::render(instance, settings).await
    }
}

#[async_trait]
impl Action for ApplicationVolumeDialAction {
    const UUID: ActionUuid = "com.victormarin.volume-controller.appdial";
    type Settings = ApplicationVolumeDialSettings;

    async fn will_appear(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        DIAL_SETTINGS
            .write()
            .await
            .insert(instance.instance_id.clone(), settings.clone());
        Self::render(instance, settings).await
    }

    async fn will_disappear(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        DIAL_SETTINGS.write().await.remove(&instance.instance_id);
        DIAL_RUNTIME.write().await.remove(&instance.instance_id);
        RENDER_RUNTIME.write().await.remove(&instance.instance_id);
        Ok(())
    }

    async fn did_receive_settings(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        DIAL_SETTINGS
            .write()
            .await
            .insert(instance.instance_id.clone(), settings.clone());
        Self::render(instance, settings).await
    }

    async fn property_inspector_did_appear(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        eprintln!("[volume-dial] property_inspector_did_appear called");
        Self::send_targets(instance).await
    }

    async fn send_to_plugin(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
        payload: &serde_json::Value,
    ) -> OpenActionResult<()> {
        eprintln!("[volume-dial] send_to_plugin payload received: {payload}");
        if payload.get("event").and_then(serde_json::Value::as_str) == Some("requestAudioTargets") {
            eprintln!("[volume-dial] requestAudioTargets recognized");
            Self::send_targets(instance).await?;
        }
        Ok(())
    }

    async fn dial_rotate(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
        ticks: i16,
        _pressed: bool,
    ) -> OpenActionResult<()> {
        Self::adjust(instance, settings, ticks).await
    }

    async fn dial_up(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        Self::toggle(instance, settings).await
    }

    async fn touch_tap(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
        _position: (u16, u16),
        hold: bool,
    ) -> OpenActionResult<()> {
        if !hold {
            Self::toggle(instance, settings).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str, sink: &str) -> AppInfo {
        AppInfo {
            uid: 1,
            app_name: name.to_owned(),
            sink_name: Some(sink.to_owned()),
            mute: false,
            vol_percent: 50.0,
            icon_name: None,
            is_device: false,
            is_multi_sink_app: false,
            metadata: Default::default(),
        }
    }

    #[test]
    fn application_identity_is_stable_across_streams() {
        assert_eq!(
            ApplicationVolumeDialAction::target_id(&app("spotify", "stream-a")),
            ApplicationVolumeDialAction::target_id(&app("spotify", "stream-b"))
        );
    }

    #[test]
    fn feedback_is_complete_and_contains_no_template_placeholders() {
        let feedback = build_feedback(
            "data:image/png;base64,abc".into(),
            "Browser".into(),
            "80%".into(),
            80.0,
            100,
            1.0,
        );
        let value = serde_json::to_value(feedback).unwrap();
        for key in ["icon", "title", "value", "indicator"] {
            assert!(value.get(key).is_some(), "missing {key}");
        }
        let indicator = &value["indicator"];
        for key in ["value", "opacity", "bar_bg_c", "bar_fill_c"] {
            assert!(indicator.get(key).is_some(), "missing indicator.{key}");
        }
        assert!(!value.to_string().contains("{{value}}"));
    }

    #[test]
    fn rapid_ticks_are_one_final_calculation() {
        assert_eq!(adjusted_percent(80.0, 8, 2.0, 100), 96.0);
        assert_eq!(
            adjusted_percent(adjusted_percent(80.0, 3, 2.0, 100), 5, 2.0, 100),
            adjusted_percent(80.0, 8, 2.0, 100)
        );
    }

    #[test]
    fn unrelated_audio_identity_is_not_routed_to_dial() {
        let configured = vec!["application\u{1f}firefox".to_owned()];
        let changed = ["application\u{1f}spotify".to_owned()]
            .into_iter()
            .collect();
        assert!(!should_refresh_for(
            &configured,
            Some("application\u{1f}firefox"),
            &changed
        ));
    }

    #[test]
    fn stale_generation_is_detected() {
        let runtime = RenderRuntime::default();
        let older = runtime.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let newer = runtime.generation.fetch_add(1, Ordering::AcqRel) + 1;
        assert_ne!(older, runtime.generation.load(Ordering::Acquire));
        assert_eq!(newer, runtime.generation.load(Ordering::Acquire));
    }

    #[test]
    fn volume_step_defaults_and_clamps() {
        assert_eq!(ApplicationVolumeDialSettings::default().step(), 2.0);
        assert_eq!(
            ApplicationVolumeDialSettings {
                volume_step: 99,
                ..ApplicationVolumeDialSettings::default()
            }
            .step(),
            10.0
        );
    }

    #[test]
    fn maximum_defaults_and_invalid_values_fall_back() {
        assert_eq!(ApplicationVolumeDialSettings::default().maximum(), 100);
        assert_eq!(sanitize_maximum_volume(0), 100);
        assert_eq!(sanitize_maximum_volume(125), 100);
        assert_eq!(sanitize_maximum_volume(150), 150);
    }

    #[test]
    fn encoder_targets_clamp_to_configured_maximum() {
        assert_eq!(adjusted_percent(98.0, 1, 2.0, 100), 100.0);
        assert_eq!(adjusted_percent(100.0, 1, 2.0, 100), 100.0);
        assert_eq!(adjusted_percent(100.0, 1, 2.0, 150), 102.0);
        assert_eq!(adjusted_percent(148.0, 2, 2.0, 150), 150.0);
        assert_eq!(adjusted_percent(4.0, -3, 2.0, 150), 0.0);
    }

    #[test]
    fn progress_uses_maximum_without_changing_actual_value() {
        assert_eq!(progress_percent(100.0, 100), 100);
        assert_eq!(progress_percent(100.0, 150), 67);
        assert_eq!(progress_percent(150.0, 150), 100);
    }

    #[test]
    fn old_single_target_migrates_without_losing_limits() {
        let settings: ApplicationVolumeDialSettings = serde_json::from_value(serde_json::json!({
            "target_id": "application\u{1f}firefox",
            "volume_step": 7,
            "maximum_volume": 150
        }))
        .unwrap();
        assert_eq!(
            settings.normalized_target_ids(),
            ["application\u{1f}firefox"]
        );
        assert_eq!(settings.step(), 7.0);
        assert_eq!(settings.maximum(), 150);
        assert!(
            ApplicationVolumeDialSettings::default()
                .normalized_target_ids()
                .is_empty()
        );
    }

    #[test]
    fn duplicate_targets_are_deduplicated_in_order() {
        let settings = ApplicationVolumeDialSettings {
            target_ids: vec!["a".into(), "b".into(), "a".into(), "c".into()],
            ..Default::default()
        };
        assert_eq!(settings.normalized_target_ids(), ["a", "b", "c"]);
    }

    #[test]
    fn sticky_arbitration_does_not_preempt_current_target() {
        let configured = vec!["firefox".to_owned(), "chromium".to_owned()];
        let available = ["firefox".to_owned()].into_iter().collect();
        assert_eq!(
            arbitrate_active(&configured, &available, None).as_deref(),
            Some("firefox")
        );
        let available = ["firefox".to_owned(), "chromium".to_owned()]
            .into_iter()
            .collect();
        assert_eq!(
            arbitrate_active(&configured, &available, Some("firefox")).as_deref(),
            Some("firefox")
        );
        let available = ["chromium".to_owned()].into_iter().collect();
        assert_eq!(
            arbitrate_active(&configured, &available, Some("firefox")).as_deref(),
            Some("chromium")
        );
        let available = ["firefox".to_owned(), "chromium".to_owned()]
            .into_iter()
            .collect();
        assert_eq!(
            arbitrate_active(&configured, &available, Some("chromium")).as_deref(),
            Some("chromium")
        );
        assert!(arbitrate_active(&configured, &Default::default(), None).is_none());
        assert_eq!(
            arbitrate_active(&["firefox".into()], &available, Some("chromium")).as_deref(),
            Some("firefox")
        );
    }

    #[test]
    fn custom_title_is_constant_and_empty_title_is_dynamic() {
        let settings = ApplicationVolumeDialSettings::default();
        assert_eq!(settings.title("Firefox"), "Firefox");
        assert_eq!(settings.title("Chromium"), "Chromium");
        let settings = ApplicationVolumeDialSettings {
            custom_title: "Browser".into(),
            ..Default::default()
        };
        assert_eq!(settings.title("Firefox"), "Browser");
        assert_eq!(settings.title("Chromium"), "Browser");
    }
}
