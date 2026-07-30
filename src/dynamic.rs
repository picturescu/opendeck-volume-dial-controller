use std::{
    collections::{HashMap, HashSet},
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
    application_targets::{ApplicationTargetOption, application_target_inventory},
    audio::{self, AppInfo},
    dial::{adjusted_percent, arbitrate_active, progress_percent, sanitize_maximum_volume},
    icons::{resolve_app_icon, stable_application_id},
    utils::get_app_icon_uri,
};

const FEEDBACK_LAYOUT: &str = "$B1";
static DYNAMIC_ROTATION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

macro_rules! dynamic_debug {
    ($($argument:tt)*) => {
        if cfg!(debug_assertions) {
            eprintln!($($argument)*);
        }
    };
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct FocusIdentity {
    device_id: String,
    focus_group: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PersistedFocus {
    identity: FocusIdentity,
    selector_id: String,
    target_ids: Vec<String>,
    custom_title: String,
}

#[derive(Clone, Debug, Default)]
struct FocusState {
    selector_id: String,
    target_ids: Vec<String>,
    custom_title: String,
    active_target_id: Option<String>,
    last_name: Option<String>,
    last_icon: Option<String>,
    icon_target_id: Option<String>,
    generation: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ApplicationSelectorButtonSettings {
    target_ids: Vec<String>,
    target_id: String,
    target_name: String,
    custom_title: String,
    focus_group: String,
}

impl ApplicationSelectorButtonSettings {
    fn target_ids(&self) -> Vec<String> {
        let source = if self.target_ids.is_empty() && !self.target_id.is_empty() {
            vec![self.target_id.clone()]
        } else {
            self.target_ids.clone()
        };
        let mut seen = HashSet::new();
        source
            .into_iter()
            .filter(|id| !id.is_empty() && seen.insert(id.clone()))
            .collect()
    }

    fn group(&self) -> String {
        let value = self.focus_group.trim();
        if value.is_empty() {
            "main".to_owned()
        } else {
            value.to_owned()
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct DynamicApplicationDialSettings {
    focus_group: String,
    custom_title: String,
    volume_step: u8,
    maximum_volume: u16,
}

impl Default for DynamicApplicationDialSettings {
    fn default() -> Self {
        Self {
            focus_group: "main".into(),
            custom_title: String::new(),
            volume_step: 2,
            maximum_volume: 100,
        }
    }
}

impl DynamicApplicationDialSettings {
    fn group(&self) -> String {
        let value = self.focus_group.trim();
        if value.is_empty() {
            "main".to_owned()
        } else {
            value.to_owned()
        }
    }
}

#[derive(Default)]
struct SelectorRuntime {
    active_target_id: Option<String>,
    last_name: Option<String>,
    last_icon: Option<String>,
    icon_target_id: Option<String>,
    last_frame: Option<String>,
}

#[derive(Default)]
struct DialRuntime {
    render_lock: Mutex<()>,
    render_generation: AtomicU64,
    layout_initialized: Mutex<bool>,
    last_feedback: Mutex<Option<String>>,
    optimistic_volume: Mutex<Option<(f64, Instant, u64)>>,
    optimistic_mute: Mutex<Option<(bool, Instant, u64)>>,
    command_sequence: AtomicU64,
}

#[derive(Clone)]
struct GroupSnapshot {
    configured: bool,
    available: bool,
    target_id: Option<String>,
    title: String,
    icon: String,
    volume: f64,
    muted: bool,
    focus_generation: u64,
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SelectorTargetList {
    event: &'static str,
    targets: Vec<ApplicationTargetOption>,
    active_target_id: Option<String>,
    focused: bool,
    error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DynamicStatus {
    event: &'static str,
    selector_id: Option<String>,
    active_target_id: Option<String>,
    available: bool,
}

static FOCUS: LazyLock<RwLock<HashMap<FocusIdentity, FocusState>>> =
    LazyLock::new(|| RwLock::const_new(HashMap::new()));
static SELECTOR_SETTINGS: LazyLock<RwLock<HashMap<String, ApplicationSelectorButtonSettings>>> =
    LazyLock::new(|| RwLock::const_new(HashMap::new()));
static SELECTOR_RUNTIME: LazyLock<RwLock<HashMap<String, SelectorRuntime>>> =
    LazyLock::new(|| RwLock::const_new(HashMap::new()));
static DIAL_SETTINGS: LazyLock<RwLock<HashMap<String, DynamicApplicationDialSettings>>> =
    LazyLock::new(|| RwLock::const_new(HashMap::new()));
static DIAL_RUNTIME: LazyLock<RwLock<HashMap<String, Arc<DialRuntime>>>> =
    LazyLock::new(|| RwLock::const_new(HashMap::new()));

pub struct ApplicationSelectorButtonAction;
pub struct DynamicApplicationVolumeDialAction;

fn focus_identity(device_id: &str, group: &str) -> FocusIdentity {
    FocusIdentity {
        device_id: device_id.to_owned(),
        focus_group: if group.trim().is_empty() {
            "main".to_owned()
        } else {
            group.trim().to_owned()
        },
    }
}

fn next_focus_generation(previous: u64, same_selection: bool) -> u64 {
    if same_selection {
        previous
    } else {
        previous + 1
    }
}

fn dynamic_command_is_current(
    current_sequence: u64,
    command_sequence: u64,
    current_focus_generation: u64,
    command_focus_generation: u64,
) -> bool {
    current_sequence == command_sequence && current_focus_generation == command_focus_generation
}

fn applications() -> Vec<AppInfo> {
    audio::registry_applications().unwrap_or_default()
}

fn active_members(target_id: Option<&str>) -> Vec<AppInfo> {
    let Some(target_id) = target_id else {
        return Vec::new();
    };
    applications()
        .into_iter()
        .filter(|app| stable_application_id(app) == target_id)
        .collect()
}

fn choose_active(
    target_ids: &[String],
    current: Option<&str>,
    applications: &[AppInfo],
) -> Option<String> {
    let available = applications
        .iter()
        .map(stable_application_id)
        .collect::<HashSet<_>>();
    arbitrate_active(target_ids, &available, current)
}

fn selector_title(
    settings: &ApplicationSelectorButtonSettings,
    automatic_name: Option<&str>,
) -> String {
    if !settings.custom_title.is_empty() {
        settings.custom_title.clone()
    } else if let Some(name) = automatic_name {
        name.to_owned()
    } else if settings.target_ids().is_empty() {
        "Select apps".to_owned()
    } else {
        "Unavailable".to_owned()
    }
}

async fn selector_snapshot(
    instance_id: &str,
    settings: &ApplicationSelectorButtonSettings,
    device_id: &str,
) -> GroupSnapshot {
    let apps = applications();
    let target_ids = settings.target_ids();
    let previous = SELECTOR_RUNTIME
        .read()
        .await
        .get(instance_id)
        .map(|runtime| {
            (
                runtime.active_target_id.clone(),
                runtime.last_name.clone(),
                runtime.last_icon.clone(),
                runtime.icon_target_id.clone(),
            )
        })
        .unwrap_or_default();
    let active_target_id = choose_active(&target_ids, previous.0.as_deref(), &apps);
    let members = apps
        .iter()
        .filter(|app| {
            active_target_id
                .as_ref()
                .is_some_and(|id| stable_application_id(app) == *id)
        })
        .collect::<Vec<_>>();
    let mut last_name = previous.1;
    let mut last_icon = previous.2;
    let mut icon_target_id = previous.3;
    if let Some(app) = members.first() {
        let id = stable_application_id(app);
        last_name = Some(app.app_name.clone());
        if icon_target_id.as_deref() != Some(&id) {
            last_icon = Some(resolve_app_icon(app).rendered_data_uri);
            icon_target_id = Some(id);
        }
    }
    {
        let mut runtimes = SELECTOR_RUNTIME.write().await;
        let runtime = runtimes.entry(instance_id.to_owned()).or_default();
        runtime.active_target_id = active_target_id.clone();
        runtime.last_name = last_name.clone();
        runtime.last_icon = last_icon.clone();
        runtime.icon_target_id = icon_target_id;
    }
    let focused = FOCUS
        .read()
        .await
        .get(&focus_identity(device_id, &settings.group()))
        .is_some_and(|focus| focus.selector_id == instance_id);
    let automatic_name = members
        .first()
        .map(|app| app.app_name.as_str())
        .or(last_name.as_deref());
    let title = selector_title(settings, automatic_name);
    GroupSnapshot {
        configured: !target_ids.is_empty(),
        available: !members.is_empty(),
        target_id: active_target_id,
        title,
        icon: last_icon.unwrap_or_else(|| get_app_icon_uri(None, "audio-x-generic".to_owned()).0),
        volume: if members.is_empty() {
            0.0
        } else {
            members
                .iter()
                .map(|app| f64::from(app.vol_percent))
                .sum::<f64>()
                / members.len() as f64
        },
        muted: !members.is_empty() && members.iter().all(|app| app.mute),
        focus_generation: u64::from(focused),
    }
}

async fn focus_snapshot(identity: &FocusIdentity) -> GroupSnapshot {
    let apps = applications();
    dynamic_debug!("[dynamic-dial] focus-lock-wait");
    let focus = FOCUS.read().await.get(identity).cloned();
    dynamic_debug!("[dynamic-dial] focus-lock-acquired");
    let Some(focus) = focus else {
        dynamic_debug!("[dynamic-dial] focus-lock-released");
        return GroupSnapshot {
            configured: false,
            available: false,
            target_id: None,
            title: "Select app".into(),
            icon: get_app_icon_uri(None, "audio-x-generic".to_owned()).0,
            volume: 0.0,
            muted: false,
            focus_generation: 0,
        };
    };
    dynamic_debug!("[dynamic-dial] focus-lock-released");
    let active_target_id =
        choose_active(&focus.target_ids, focus.active_target_id.as_deref(), &apps);
    let members = apps
        .iter()
        .filter(|app| {
            active_target_id
                .as_ref()
                .is_some_and(|id| stable_application_id(app) == *id)
        })
        .collect::<Vec<_>>();
    let mut last_name = focus.last_name.clone();
    let mut last_icon = focus.last_icon.clone();
    let mut icon_target_id = focus.icon_target_id.clone();
    if let Some(app) = members.first() {
        let id = stable_application_id(app);
        last_name = Some(app.app_name.clone());
        if icon_target_id.as_deref() != Some(&id) {
            last_icon = Some(resolve_app_icon(app).rendered_data_uri);
            icon_target_id = Some(id);
        }
    }
    let focus_metadata_changed = focus.active_target_id != active_target_id
        || focus.last_name != last_name
        || focus.last_icon != last_icon
        || focus.icon_target_id != icon_target_id;
    if focus_metadata_changed {
        dynamic_debug!("[dynamic-dial] focus-update-lock-wait");
        let mut focus_map = FOCUS.write().await;
        dynamic_debug!("[dynamic-dial] focus-update-lock-acquired");
        if let Some(current) = focus_map.get_mut(identity)
            && current.generation == focus.generation
            && current.selector_id == focus.selector_id
        {
            current.active_target_id = active_target_id.clone();
            current.last_name = last_name.clone();
            current.last_icon = last_icon.clone();
            current.icon_target_id = icon_target_id;
        }
        dynamic_debug!("[dynamic-dial] focus-update-lock-released");
    }
    GroupSnapshot {
        configured: !focus.target_ids.is_empty(),
        available: !members.is_empty(),
        target_id: active_target_id,
        title: if focus.custom_title.is_empty() {
            members
                .first()
                .map(|app| app.app_name.clone())
                .or_else(|| last_name.clone())
                .unwrap_or_else(|| "Unavailable".into())
        } else {
            focus.custom_title.clone()
        },
        icon: last_icon.unwrap_or_else(|| get_app_icon_uri(None, "audio-x-generic".to_owned()).0),
        volume: if members.is_empty() {
            0.0
        } else {
            members
                .iter()
                .map(|app| f64::from(app.vol_percent))
                .sum::<f64>()
                / members.len() as f64
        },
        muted: !members.is_empty() && members.iter().all(|app| app.mute),
        focus_generation: focus.generation,
    }
}

async fn render_selector(instance: &Instance, settings: &ApplicationSelectorButtonSettings) {
    let snapshot = selector_snapshot(&instance.instance_id, settings, &instance.device_id).await;
    let focused = snapshot.focus_generation == 1;
    let opacity = if snapshot.available { 1.0 } else { 0.4 };
    let key = format!(
        "{}:{}:{}:{}",
        snapshot.icon, snapshot.title, opacity, focused
    );
    if SELECTOR_RUNTIME
        .read()
        .await
        .get(&instance.instance_id)
        .and_then(|runtime| runtime.last_frame.as_ref())
        == Some(&key)
    {
        return;
    }
    let icon = if snapshot.available {
        snapshot.icon
    } else {
        crate::gfx::dim_data_uri(&snapshot.icon).unwrap_or(snapshot.icon)
    };
    let _ = instance.set_image(Some(icon), None).await;
    let title = if focused {
        format!("● {}", snapshot.title)
    } else {
        snapshot.title
    };
    let _ = instance.set_title(Some(title), None).await;
    SELECTOR_RUNTIME
        .write()
        .await
        .entry(instance.instance_id.clone())
        .or_default()
        .last_frame = Some(key);
}

async fn dial_runtime(instance_id: &str) -> Arc<DialRuntime> {
    if let Some(runtime) = DIAL_RUNTIME.read().await.get(instance_id).cloned() {
        return runtime;
    }
    DIAL_RUNTIME
        .write()
        .await
        .entry(instance_id.to_owned())
        .or_insert_with(|| Arc::new(DialRuntime::default()))
        .clone()
}

async fn reconcile_optimistic_volume(
    runtime: &DialRuntime,
    confirmed: f64,
    focus_generation: u64,
) -> f64 {
    let mut optimistic = runtime.optimistic_volume.lock().await;
    let Some((predicted, issued, predicted_generation)) = *optimistic else {
        return confirmed;
    };
    if predicted_generation != focus_generation
        || (confirmed - predicted).abs() <= 0.75
        || issued.elapsed() >= Duration::from_millis(75)
    {
        *optimistic = None;
        confirmed
    } else {
        predicted
    }
}

async fn reconcile_optimistic_mute(
    runtime: &DialRuntime,
    confirmed: bool,
    focus_generation: u64,
) -> bool {
    let mut optimistic = runtime.optimistic_mute.lock().await;
    let Some((predicted, issued, predicted_generation)) = *optimistic else {
        return confirmed;
    };
    if predicted_generation != focus_generation
        || confirmed == predicted
        || issued.elapsed() >= Duration::from_millis(75)
    {
        *optimistic = None;
        confirmed
    } else {
        predicted
    }
}

async fn render_dynamic(instance: &Instance, settings: &DynamicApplicationDialSettings) {
    let runtime = dial_runtime(&instance.instance_id).await;
    let generation = runtime.render_generation.fetch_add(1, Ordering::AcqRel) + 1;
    let identity = focus_identity(&instance.device_id, &settings.group());
    let mut snapshot = focus_snapshot(&identity).await;
    if !settings.custom_title.is_empty() {
        snapshot.title = settings.custom_title.clone();
    }
    snapshot.volume =
        reconcile_optimistic_volume(&runtime, snapshot.volume, snapshot.focus_generation).await;
    snapshot.muted =
        reconcile_optimistic_mute(&runtime, snapshot.muted, snapshot.focus_generation).await;
    let opacity = if snapshot.available && !snapshot.muted {
        1.0
    } else {
        0.4
    };
    let value = if !snapshot.configured {
        "Select app".to_owned()
    } else if !snapshot.available {
        "Unavailable".to_owned()
    } else if snapshot.muted {
        "Muted".to_owned()
    } else {
        format!("{:.0}%", snapshot.volume)
    };
    let feedback = DialFeedback {
        icon: FeedbackValue {
            value: snapshot.icon,
            opacity,
        },
        title: FeedbackValue {
            value: snapshot.title,
            opacity,
        },
        value: FeedbackValue { value, opacity },
        indicator: IndicatorFeedback {
            value: progress_percent(snapshot.volume, settings.maximum_volume),
            opacity,
            bar_bg_c: "#303030",
            bar_fill_c: "#ffffff",
        },
    };
    let key = serde_json::to_string(&feedback).unwrap_or_default();
    let _guard = runtime.render_lock.lock().await;
    if generation != runtime.render_generation.load(Ordering::Acquire) {
        return;
    }
    let mut initialized = runtime.layout_initialized.lock().await;
    if !*initialized {
        if !matches!(
            tokio::time::timeout(
                Duration::from_secs(1),
                instance.set_feedback_layout(FEEDBACK_LAYOUT.to_owned()),
            )
            .await,
            Ok(Ok(()))
        ) {
            eprintln!(
                "[volume-worker] feedback-timeout context={}",
                instance.instance_id
            );
            return;
        }
        *initialized = true;
    }
    drop(initialized);
    if runtime.last_feedback.lock().await.as_ref() == Some(&key) {
        return;
    }
    if tokio::time::timeout(Duration::from_secs(1), instance.set_feedback(&feedback))
        .await
        .is_ok_and(|result| result.is_ok())
    {
        *runtime.last_feedback.lock().await = Some(key);
    } else {
        eprintln!(
            "[volume-worker] feedback-timeout context={}",
            instance.instance_id
        );
    }
}

async fn focus_selector(instance: &Instance, settings: &ApplicationSelectorButtonSettings) {
    eprintln!(
        "[app-selector] focus action context={} group={}",
        instance.instance_id,
        settings.group()
    );
    if settings.target_ids().is_empty() {
        eprintln!(
            "[app-selector] focus ignored for unconfigured context={}",
            instance.instance_id
        );
        return;
    }
    let identity = focus_identity(&instance.device_id, &settings.group());
    let selector = selector_snapshot(&instance.instance_id, settings, &instance.device_id).await;
    let mut focus = FOCUS.write().await;
    let previous = focus.remove(&identity);
    let target_ids = settings.target_ids();
    let same_selector = previous.as_ref().is_some_and(|state| {
        state.selector_id == instance.instance_id
            && state.target_ids == target_ids
            && state.custom_title == settings.custom_title
    });
    let previous_generation = previous.as_ref().map_or(0, |state| state.generation);
    focus.insert(
        identity.clone(),
        FocusState {
            selector_id: instance.instance_id.clone(),
            target_ids,
            custom_title: settings.custom_title.clone(),
            active_target_id: selector.target_id.or_else(|| {
                previous
                    .as_ref()
                    .and_then(|state| state.active_target_id.clone())
            }),
            last_name: previous.as_ref().and_then(|state| state.last_name.clone()),
            last_icon: Some(selector.icon),
            icon_target_id: previous
                .as_ref()
                .and_then(|state| state.icon_target_id.clone()),
            generation: next_focus_generation(previous_generation, same_selector),
        },
    );
    drop(focus);
    persist_focus().await;
    refresh_focus(&identity).await;
}

async fn refresh_focus(identity: &FocusIdentity) {
    for selector in visible_instances(ApplicationSelectorButtonAction::UUID).await {
        if selector.device_id == identity.device_id
            && let Some(settings) = SELECTOR_SETTINGS
                .read()
                .await
                .get(&selector.instance_id)
                .cloned()
            && settings.group() == identity.focus_group
        {
            render_selector(&selector, &settings).await;
        }
    }
    for dial in visible_instances(DynamicApplicationVolumeDialAction::UUID).await {
        if dial.device_id == identity.device_id
            && let Some(settings) = DIAL_SETTINGS.read().await.get(&dial.instance_id).cloned()
            && settings.group() == identity.focus_group
        {
            render_dynamic(&dial, &settings).await;
        }
    }
}

pub async fn refresh_for_audio_changes(changed: &HashSet<String>) {
    let selector_settings = SELECTOR_SETTINGS.read().await.clone();
    for selector in visible_instances(ApplicationSelectorButtonAction::UUID).await {
        if let Some(settings) = selector_settings.get(&selector.instance_id)
            && settings.target_ids().iter().any(|id| changed.contains(id))
        {
            render_selector(&selector, settings).await;
        }
    }
    let focused = FOCUS.read().await.clone();
    for (identity, state) in focused {
        if state.target_ids.iter().any(|id| changed.contains(id)) {
            dynamic_debug!(
                "[dynamic-dial] confirmation target={:?} focus-generation=current generation={}",
                state.active_target_id,
                state.generation
            );
            refresh_focus(&identity).await;
            dynamic_debug!("[dynamic-dial] confirmation finished");
        }
    }
}

pub async fn persisted_focus() -> Vec<PersistedFocus> {
    FOCUS
        .read()
        .await
        .iter()
        .map(|(identity, state)| PersistedFocus {
            identity: identity.clone(),
            selector_id: state.selector_id.clone(),
            target_ids: state.target_ids.clone(),
            custom_title: state.custom_title.clone(),
        })
        .collect()
}

pub async fn load_persisted_focus(values: Vec<PersistedFocus>) {
    let mut focus = FOCUS.write().await;
    for value in values {
        focus.insert(
            value.identity,
            FocusState {
                selector_id: value.selector_id,
                target_ids: value.target_ids,
                custom_title: value.custom_title,
                ..FocusState::default()
            },
        );
    }
}

async fn persist_focus() {
    let ignored_apps_list = crate::plugin::SHARED_SETTINGS
        .lock()
        .await
        .ignored_apps_list
        .clone();
    let _ = set_global_settings(crate::plugin::GlobalPluginSettings {
        ignored_apps_list,
        application_columns: crate::app_column::persisted_columns().await,
        dynamic_focus: persisted_focus().await,
    })
    .await;
}

async fn send_selector_targets(instance: &Instance, settings: &ApplicationSelectorButtonSettings) {
    let (targets, error) = application_target_inventory();
    let active_target_id = {
        SELECTOR_RUNTIME
            .read()
            .await
            .get(&instance.instance_id)
            .and_then(|state| state.active_target_id.clone())
    };
    let focused = FOCUS
        .read()
        .await
        .get(&focus_identity(&instance.device_id, &settings.group()))
        .is_some_and(|state| state.selector_id == instance.instance_id);
    match instance
        .send_to_property_inspector(SelectorTargetList {
            event: "audioTargetList",
            targets,
            active_target_id,
            focused,
            error,
        })
        .await
    {
        Ok(()) => eprintln!(
            "[app-selector] target-list response sent context={}",
            instance.instance_id
        ),
        Err(error) => eprintln!(
            "[app-selector] target-list response failed context={}: {error}",
            instance.instance_id
        ),
    }
}

async fn send_dynamic_status(instance: &Instance, settings: &DynamicApplicationDialSettings) {
    let identity = focus_identity(&instance.device_id, &settings.group());
    let snapshot = focus_snapshot(&identity).await;
    let selector_id = FOCUS
        .read()
        .await
        .get(&identity)
        .map(|state| state.selector_id.clone());
    let _ = instance
        .send_to_property_inspector(DynamicStatus {
            event: "dynamicDialStatus",
            selector_id,
            active_target_id: snapshot.target_id,
            available: snapshot.available,
        })
        .await;
}

#[async_trait]
impl Action for ApplicationSelectorButtonAction {
    const UUID: ActionUuid = "com.victormarin.volume-controller.appselector";
    type Settings = ApplicationSelectorButtonSettings;

    async fn will_appear(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        SELECTOR_SETTINGS
            .write()
            .await
            .insert(instance.instance_id.clone(), settings.clone());
        render_selector(instance, settings).await;
        Ok(())
    }
    async fn will_disappear(
        &self,
        instance: &Instance,
        _: &Self::Settings,
    ) -> OpenActionResult<()> {
        SELECTOR_SETTINGS
            .write()
            .await
            .remove(&instance.instance_id);
        SELECTOR_RUNTIME.write().await.remove(&instance.instance_id);
        Ok(())
    }
    async fn did_receive_settings(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        eprintln!(
            "[app-selector] settings received context={} targets={} group={}",
            instance.instance_id,
            settings.target_ids().len(),
            settings.group()
        );
        SELECTOR_SETTINGS
            .write()
            .await
            .insert(instance.instance_id.clone(), settings.clone());
        let identity = focus_identity(&instance.device_id, &settings.group());
        let is_focused = FOCUS
            .read()
            .await
            .get(&identity)
            .is_some_and(|focus| focus.selector_id == instance.instance_id);
        if is_focused {
            focus_selector(instance, settings).await;
        }
        render_selector(instance, settings).await;
        Ok(())
    }
    async fn property_inspector_did_appear(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        eprintln!(
            "[app-selector] property_inspector_did_appear context={}",
            instance.instance_id
        );
        send_selector_targets(instance, settings).await;
        Ok(())
    }
    async fn send_to_plugin(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
        payload: &serde_json::Value,
    ) -> OpenActionResult<()> {
        if payload.get("event").and_then(serde_json::Value::as_str) == Some("requestAudioTargets") {
            eprintln!(
                "[app-selector] requestAudioTargets received context={}",
                instance.instance_id
            );
            send_selector_targets(instance, settings).await;
        }
        Ok(())
    }
    async fn key_down(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        focus_selector(instance, settings).await;
        Ok(())
    }
}

#[async_trait]
impl Action for DynamicApplicationVolumeDialAction {
    const UUID: ActionUuid = "com.victormarin.volume-controller.dynamicappdial";
    type Settings = DynamicApplicationDialSettings;

    async fn will_appear(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        DIAL_SETTINGS
            .write()
            .await
            .insert(instance.instance_id.clone(), settings.clone());
        render_dynamic(instance, settings).await;
        Ok(())
    }
    async fn will_disappear(
        &self,
        instance: &Instance,
        _: &Self::Settings,
    ) -> OpenActionResult<()> {
        DIAL_SETTINGS.write().await.remove(&instance.instance_id);
        DIAL_RUNTIME.write().await.remove(&instance.instance_id);
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
        render_dynamic(instance, settings).await;
        send_dynamic_status(instance, settings).await;
        Ok(())
    }
    async fn property_inspector_did_appear(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        send_dynamic_status(instance, settings).await;
        Ok(())
    }
    async fn dial_rotate(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
        ticks: i16,
        _: bool,
    ) -> OpenActionResult<()> {
        let diagnostic_sequence = DYNAMIC_ROTATION_SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1;
        dynamic_debug!(
            "[dynamic-dial] seq={} rotate-enter context={}",
            diagnostic_sequence,
            instance.instance_id
        );
        let identity = focus_identity(&instance.device_id, &settings.group());
        dynamic_debug!("[dynamic-dial] seq={diagnostic_sequence} focus-lock-wait");
        let snapshot = focus_snapshot(&identity).await;
        dynamic_debug!(
            "[dynamic-dial] seq={} focus-snapshot-built target={:?}",
            diagnostic_sequence,
            snapshot.target_id
        );
        if !snapshot.available || ticks == 0 {
            dynamic_debug!("[dynamic-dial] seq={diagnostic_sequence} rotate-finished");
            return Ok(());
        }
        let runtime = dial_runtime(&instance.instance_id).await;
        dynamic_debug!("[dynamic-dial] seq={diagnostic_sequence} runtime-lock-wait");
        let current = {
            let optimistic = runtime.optimistic_volume.lock().await;
            dynamic_debug!("[dynamic-dial] seq={diagnostic_sequence} runtime-lock-acquired");
            optimistic.map_or(snapshot.volume, |value| value.0)
        };
        let target = adjusted_percent(
            current,
            ticks,
            f64::from(settings.volume_step.clamp(1, 10)),
            sanitize_maximum_volume(settings.maximum_volume),
        );
        let sequence = runtime.command_sequence.fetch_add(1, Ordering::AcqRel) + 1;
        {
            let mut optimistic = runtime.optimistic_volume.lock().await;
            *optimistic = Some((target, Instant::now(), snapshot.focus_generation));
        }
        dynamic_debug!(
            "[dynamic-dial] seq={} optimistic-target={} runtime-lock-released",
            diagnostic_sequence,
            target
        );
        dynamic_debug!("[dynamic-dial] seq={diagnostic_sequence} render-requested");
        render_dynamic(instance, settings).await;
        let target_id = snapshot.target_id;
        let focus_generation = snapshot.focus_generation;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let current_generation = FOCUS
                .read()
                .await
                .get(&identity)
                .map_or(0, |state| state.generation);
            if !dynamic_command_is_current(
                runtime.command_sequence.load(Ordering::Acquire),
                sequence,
                current_generation,
                focus_generation,
            ) {
                return;
            }
            let members = active_members(target_id.as_deref());
            dynamic_debug!("[dynamic-dial] seq={diagnostic_sequence} command-submit-start");
            audio::set_application_group_volume(
                members
                    .into_iter()
                    .map(|app| (app.uid, app.is_device))
                    .collect(),
                target,
            );
            dynamic_debug!("[dynamic-dial] seq={diagnostic_sequence} command-submit-complete");
        });
        dynamic_debug!("[dynamic-dial] seq={diagnostic_sequence} rotate-finished");
        Ok(())
    }
    async fn dial_up(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        toggle_dynamic_mute(instance, settings).await;
        Ok(())
    }
    async fn touch_tap(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
        _: (u16, u16),
        hold: bool,
    ) -> OpenActionResult<()> {
        if !hold {
            toggle_dynamic_mute(instance, settings).await;
        }
        Ok(())
    }
}

async fn toggle_dynamic_mute(instance: &Instance, settings: &DynamicApplicationDialSettings) {
    let identity = focus_identity(&instance.device_id, &settings.group());
    let snapshot = focus_snapshot(&identity).await;
    if !snapshot.available {
        return;
    }
    let members = active_members(snapshot.target_id.as_deref());
    let mute = !members.iter().all(|app| app.mute);
    let runtime = dial_runtime(&instance.instance_id).await;
    *runtime.optimistic_mute.lock().await = Some((mute, Instant::now(), snapshot.focus_generation));
    render_dynamic(instance, settings).await;
    audio::set_application_group_mute(
        members
            .into_iter()
            .map(|app| (app.uid, app.is_device))
            .collect(),
        mute,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_is_isolated_by_device_and_group() {
        assert_ne!(
            focus_identity("deck-a", "main"),
            focus_identity("deck-b", "main")
        );
        assert_ne!(
            focus_identity("deck-a", "main"),
            focus_identity("deck-a", "games")
        );
    }

    #[test]
    fn selector_defaults_and_migration_are_stable() {
        let default = ApplicationSelectorButtonSettings::default();
        assert_eq!(default.group(), "main");
        let old = ApplicationSelectorButtonSettings {
            target_id: "firefox".into(),
            ..Default::default()
        };
        assert_eq!(old.target_ids(), ["firefox"]);
    }

    #[test]
    fn selector_settings_preserve_order_and_other_fields() {
        let settings = ApplicationSelectorButtonSettings {
            target_ids: vec!["firefox".into(), "chromium".into(), "firefox".into()],
            custom_title: "Browser".into(),
            focus_group: "main".into(),
            ..Default::default()
        };
        assert_eq!(settings.target_ids(), ["firefox", "chromium"]);
        assert_eq!(settings.custom_title, "Browser");
        assert_eq!(settings.group(), "main");
    }

    #[test]
    fn configured_selector_never_uses_select_apps_title() {
        let configured = ApplicationSelectorButtonSettings {
            target_ids: vec!["application\u{1f}firefox".into()],
            ..Default::default()
        };
        assert_eq!(selector_title(&configured, None), "Unavailable");
        assert_eq!(selector_title(&configured, Some("Firefox")), "Firefox");
        assert_eq!(
            selector_title(&ApplicationSelectorButtonSettings::default(), None),
            "Select apps"
        );
    }

    #[test]
    fn title_priority_prefers_dynamic_then_selector_then_application() {
        fn title(dynamic: &str, selector: &str, application: &str) -> String {
            [dynamic, selector, application]
                .into_iter()
                .find(|value| !value.is_empty())
                .unwrap()
                .to_owned()
        }
        assert_eq!(title("Dial", "Browser", "Firefox"), "Dial");
        assert_eq!(title("", "Browser", "Firefox"), "Browser");
        assert_eq!(title("", "", "Firefox"), "Firefox");
    }

    #[test]
    fn focus_generation_rejects_old_commands() {
        let old = 4_u64;
        let current = next_focus_generation(old, false);
        assert_ne!(old, current);
        assert_eq!(next_focus_generation(current, true), current);
        assert!(!dynamic_command_is_current(9, 9, current, old));
        assert!(!dynamic_command_is_current(10, 9, current, current));
        assert!(dynamic_command_is_current(9, 9, current, current));
    }

    #[tokio::test]
    async fn optimistic_reconciliation_does_not_reenter_runtime_lock() {
        let runtime = DialRuntime::default();
        *runtime.optimistic_volume.lock().await = Some((80.0, Instant::now(), 7));
        let result = tokio::time::timeout(
            Duration::from_millis(100),
            reconcile_optimistic_volume(&runtime, 80.0, 7),
        )
        .await
        .expect("reconciliation self-deadlocked");
        assert_eq!(result, 80.0);
        assert!(runtime.optimistic_volume.lock().await.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dynamic_dial_concurrency_stress() {
        tokio::time::timeout(Duration::from_secs(2), async {
            let runtime = Arc::new(DialRuntime::default());
            let focus_generation = Arc::new(AtomicU64::new(1));
            let mut tasks = Vec::new();
            for step in 0..1_000_u64 {
                let runtime = Arc::clone(&runtime);
                let focus_generation = Arc::clone(&focus_generation);
                tasks.push(tokio::spawn(async move {
                    if step % 17 == 0 {
                        focus_generation.fetch_add(1, Ordering::AcqRel);
                    }
                    let generation = focus_generation.load(Ordering::Acquire);
                    let sequence = runtime.command_sequence.fetch_add(1, Ordering::AcqRel) + 1;
                    {
                        let mut optimistic = runtime.optimistic_volume.lock().await;
                        *optimistic =
                            Some((f64::from((step % 101) as u32), Instant::now(), generation));
                    }
                    let _ = reconcile_optimistic_volume(&runtime, 50.0, generation).await;
                    dynamic_command_is_current(
                        runtime.command_sequence.load(Ordering::Acquire),
                        sequence,
                        focus_generation.load(Ordering::Acquire),
                        generation,
                    )
                }));
            }
            for task in tasks {
                let _ = task.await.unwrap();
            }
            assert_eq!(runtime.command_sequence.load(Ordering::Acquire), 1_000);
        })
        .await
        .expect("dynamic concurrency stress timed out");
    }

    #[test]
    fn sticky_selection_uses_shared_arbitration() {
        let configured = vec!["firefox".into(), "chromium".into()];
        let available = ["firefox".into(), "chromium".into()].into_iter().collect();
        assert_eq!(
            arbitrate_active(&configured, &available, Some("chromium")).as_deref(),
            Some("chromium")
        );
    }

    #[test]
    fn dynamic_feedback_is_complete_and_has_no_raw_placeholder() {
        let feedback = DialFeedback {
            icon: FeedbackValue {
                value: "data:image/png;base64,abc".into(),
                opacity: 1.0,
            },
            title: FeedbackValue {
                value: "Browser".into(),
                opacity: 1.0,
            },
            value: FeedbackValue {
                value: "80%".into(),
                opacity: 1.0,
            },
            indicator: IndicatorFeedback {
                value: 80,
                opacity: 1.0,
                bar_bg_c: "#303030",
                bar_fill_c: "#ffffff",
            },
        };
        let serialized = serde_json::to_value(feedback).unwrap();
        for key in ["icon", "title", "value", "indicator"] {
            assert!(serialized.get(key).is_some());
        }
        assert!(!serialized.to_string().contains("{{value}}"));
    }

    #[test]
    fn focused_group_persistence_round_trips_without_stream_indexes() {
        let persisted = PersistedFocus {
            identity: focus_identity("deck", "main"),
            selector_id: "selector-context".into(),
            target_ids: vec!["application\u{1f}firefox".into()],
            custom_title: "Browser".into(),
        };
        let value = serde_json::to_value(&persisted).unwrap();
        let restored: PersistedFocus = serde_json::from_value(value).unwrap();
        assert_eq!(restored.identity, persisted.identity);
        assert_eq!(restored.target_ids, persisted.target_ids);
    }
}
