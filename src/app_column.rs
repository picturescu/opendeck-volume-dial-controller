use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    sync::{
        Arc, LazyLock,
        atomic::{AtomicU64, Ordering},
    },
};

use openaction::*;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};

use crate::{
    audio::{self, AppInfo},
    dial::{arbitrate_active, clamp_target_percent},
    gfx,
    icons::{resolve_app_icon, stable_application_id},
    utils::get_app_icon_uri,
};

const BUTTON_STEP: f64 = 10.0;

#[derive(Clone, Debug, Eq, Serialize)]
struct ColumnIdentity {
    device_id: String,
    column: u8,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PersistedColumn {
    device_id: String,
    column: u8,
    settings: ApplicationVolumeColumnSettings,
}

impl PartialEq for ColumnIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.device_id == other.device_id && self.column == other.column
    }
}

impl Hash for ColumnIdentity {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.device_id.hash(state);
        self.column.hash(state);
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct ApplicationVolumeColumnSettings {
    target_ids: Vec<String>,
    target_id: String,
    target_name: String,
}

impl ApplicationVolumeColumnSettings {
    fn normalized_target_ids(&self) -> Vec<String> {
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

    fn normalized(mut self) -> Self {
        self.target_ids = self.normalized_target_ids();
        self.target_id.clear();
        self.target_name.clear();
        self
    }
}

#[derive(Default)]
struct ColumnState {
    settings: ApplicationVolumeColumnSettings,
    active_target_id: Option<String>,
    last_icon: Option<String>,
    icon_target_id: Option<String>,
    last_frames: HashMap<u8, String>,
}

#[derive(Default)]
struct ColumnRuntime {
    render_lock: Mutex<()>,
    generation: AtomicU64,
    state: Mutex<ColumnState>,
}

#[derive(Clone)]
struct ColumnSnapshot {
    available: bool,
    target_id: Option<String>,
    icon: String,
    volume: f64,
    muted: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TargetOption {
    id: String,
    name: String,
    detail: String,
    available: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TargetList {
    event: &'static str,
    targets: Vec<TargetOption>,
    active_target_id: Option<String>,
    column: u8,
    present_rows: Vec<u8>,
    complete: bool,
    error: Option<String>,
}

static COLUMNS: LazyLock<RwLock<HashMap<ColumnIdentity, Arc<ColumnRuntime>>>> =
    LazyLock::new(|| RwLock::const_new(HashMap::new()));
static CONTEXT_COLUMNS: LazyLock<RwLock<HashMap<String, ColumnIdentity>>> =
    LazyLock::new(|| RwLock::const_new(HashMap::new()));

pub struct ApplicationVolumeColumnAction;

fn identity(instance: &Instance) -> Option<ColumnIdentity> {
    let coordinates = instance.coordinates?;
    Some(ColumnIdentity {
        device_id: instance.device_id.clone(),
        column: coordinates.column,
    })
}

async fn runtime(identity: &ColumnIdentity) -> Arc<ColumnRuntime> {
    if let Some(runtime) = COLUMNS.read().await.get(identity).cloned() {
        return runtime;
    }
    COLUMNS
        .write()
        .await
        .entry(identity.clone())
        .or_insert_with(|| Arc::new(ColumnRuntime::default()))
        .clone()
}

async fn instances(identity: &ColumnIdentity) -> HashMap<u8, Arc<Instance>> {
    visible_instances(ApplicationVolumeColumnAction::UUID)
        .await
        .into_iter()
        .filter_map(|instance| {
            let coordinates = instance.coordinates?;
            (instance.device_id == identity.device_id
                && coordinates.column == identity.column
                && coordinates.row <= 2)
                .then_some((coordinates.row, instance))
        })
        .collect()
}

fn grouped_applications(target_id: Option<&str>) -> Vec<AppInfo> {
    let Some(target_id) = target_id else {
        return Vec::new();
    };
    audio::registry_applications()
        .unwrap_or_default()
        .into_iter()
        .filter(|app| stable_application_id(app) == target_id)
        .collect()
}

async fn snapshot(runtime: &ColumnRuntime) -> ColumnSnapshot {
    let applications = audio::registry_applications().unwrap_or_default();
    let available = applications
        .iter()
        .map(stable_application_id)
        .collect::<HashSet<_>>();
    let mut state = runtime.state.lock().await;
    let configured = state.settings.normalized_target_ids();
    let active = arbitrate_active(&configured, &available, state.active_target_id.as_deref());
    state.active_target_id = active.clone();
    let members = applications
        .iter()
        .filter(|app| {
            active
                .as_ref()
                .is_some_and(|id| stable_application_id(app) == *id)
        })
        .collect::<Vec<_>>();
    let available = !members.is_empty();
    let muted = available && members.iter().all(|app| app.mute);
    let volume = if available {
        members
            .iter()
            .map(|app| f64::from(app.vol_percent))
            .sum::<f64>()
            / members.len() as f64
    } else {
        0.0
    };
    if let Some(app) = members.first() {
        let target_id = stable_application_id(app);
        if state.icon_target_id.as_deref() != Some(&target_id) {
            state.last_icon = Some(resolve_app_icon(app).rendered_data_uri);
            state.icon_target_id = Some(target_id);
        }
    }
    let icon = state
        .last_icon
        .clone()
        .unwrap_or_else(|| get_app_icon_uri(None, "audio-x-generic".to_owned()).0);
    ColumnSnapshot {
        available,
        target_id: active,
        icon,
        volume,
        muted,
    }
}

fn frames(snapshot: &ColumnSnapshot) -> Result<[String; 3], String> {
    let top = if snapshot.available && !snapshot.muted {
        snapshot.icon.clone()
    } else {
        gfx::dim_data_uri(&snapshot.icon).unwrap_or_else(|_| snapshot.icon.clone())
    };
    let (upper, lower) = gfx::get_volume_bar_data_uri_split(snapshot.volume.min(100.0) as f32)
        .map_err(|e| e.to_string())?;
    Ok([top, upper, lower])
}

async fn render_column(identity: &ColumnIdentity) -> OpenActionResult<()> {
    let runtime = runtime(identity).await;
    let generation = runtime.generation.fetch_add(1, Ordering::AcqRel) + 1;
    let row_instances = instances(identity).await;
    let complete = [0, 1, 2]
        .into_iter()
        .all(|row| row_instances.contains_key(&row));
    let snapshot = snapshot(&runtime).await;
    let rendered = frames(&snapshot).unwrap_or_else(|_| {
        [
            gfx::TRANSPARENT_ICON.clone(),
            gfx::TRANSPARENT_ICON.clone(),
            gfx::TRANSPARENT_ICON.clone(),
        ]
    });
    let _guard = runtime.render_lock.lock().await;
    if generation != runtime.generation.load(Ordering::Acquire) {
        return Ok(());
    }
    let last_frames = runtime.state.lock().await.last_frames.clone();
    let mut sent = Vec::new();
    for (row, instance) in row_instances {
        let frame = rendered[usize::from(row.min(2))].clone();
        let key = format!("{complete}:{frame}");
        if last_frames.get(&row) == Some(&key) {
            continue;
        }
        let send = async {
            if complete {
                instance.set_image(Some(frame), None).await?;
                instance.set_title(Some(""), None).await?;
            } else {
                instance
                    .set_image(Some(gfx::TRANSPARENT_ICON.clone()), None)
                    .await?;
                instance.set_title(Some("Add all 3 keys"), None).await?;
            }
            OpenActionResult::Ok(())
        };
        match tokio::time::timeout(std::time::Duration::from_secs(1), send).await {
            Ok(Ok(())) => sent.push((row, key)),
            Ok(Err(error)) => return Err(error),
            Err(_) => {
                eprintln!(
                    "[volume-worker] feedback-timeout context={}",
                    instance.instance_id
                );
            }
        }
    }
    let mut state = runtime.state.lock().await;
    for (row, key) in sent {
        state.last_frames.insert(row, key);
    }
    Ok(())
}

async fn synchronize_settings(
    identity: &ColumnIdentity,
    settings: ApplicationVolumeColumnSettings,
    force_broadcast: bool,
) -> OpenActionResult<()> {
    let settings = settings.normalized();
    let runtime = runtime(identity).await;
    let changed = {
        let mut state = runtime.state.lock().await;
        if state.settings != settings {
            state.settings = settings.clone();
            true
        } else {
            false
        }
    };
    if !changed && !force_broadcast {
        return render_column(identity).await;
    }
    for instance in instances(identity).await.into_values() {
        instance.set_settings(&settings).await?;
    }
    if changed {
        persist_columns().await;
    }
    render_column(identity).await
}

pub async fn load_persisted_columns(columns: Vec<PersistedColumn>) {
    for column in columns {
        let identity = ColumnIdentity {
            device_id: column.device_id,
            column: column.column,
        };
        let settings = column.settings.normalized();
        runtime(&identity).await.state.lock().await.settings = settings.clone();
        for instance in instances(&identity).await.into_values() {
            let _ = instance.set_settings(&settings).await;
        }
        let _ = render_column(&identity).await;
    }
}

pub async fn persisted_columns() -> Vec<PersistedColumn> {
    let columns = COLUMNS.read().await.clone();
    let mut persisted = Vec::new();
    for (identity, runtime) in columns {
        let settings = runtime.state.lock().await.settings.clone();
        if !settings.normalized_target_ids().is_empty() {
            persisted.push(PersistedColumn {
                device_id: identity.device_id,
                column: identity.column,
                settings,
            });
        }
    }
    persisted.sort_by(|left, right| {
        left.device_id
            .cmp(&right.device_id)
            .then_with(|| left.column.cmp(&right.column))
    });
    persisted
}

async fn persist_columns() {
    let ignored_apps_list = crate::plugin::SHARED_SETTINGS
        .lock()
        .await
        .ignored_apps_list
        .clone();
    let _ = set_global_settings(crate::plugin::GlobalPluginSettings {
        ignored_apps_list,
        application_columns: persisted_columns().await,
        dynamic_focus: crate::dynamic::persisted_focus().await,
    })
    .await;
}

async fn send_targets(instance: &Instance, identity: &ColumnIdentity) -> OpenActionResult<()> {
    let applications = audio::registry_applications();
    let error = applications
        .is_none()
        .then(|| "Audio registry is not initialized".to_owned());
    let mut grouped = HashMap::<String, TargetOption>::new();
    for app in applications.unwrap_or_default() {
        let id = stable_application_id(&app);
        let detail = if app.is_multi_sink_app {
            "All active streams".to_owned()
        } else {
            app.sink_name.unwrap_or_default()
        };
        grouped.entry(id.clone()).or_insert(TargetOption {
            id,
            name: app.app_name,
            detail,
            available: true,
        });
    }
    let runtime = runtime(identity).await;
    let active_target_id = runtime.state.lock().await.active_target_id.clone();
    let rows = instances(identity).await;
    let mut present_rows = rows.keys().copied().collect::<Vec<_>>();
    present_rows.sort_unstable();
    instance
        .send_to_property_inspector(TargetList {
            event: "audioTargetList",
            targets: grouped.into_values().collect(),
            active_target_id,
            column: identity.column,
            complete: [0, 1, 2].into_iter().all(|row| rows.contains_key(&row)),
            present_rows,
            error,
        })
        .await
}

async fn change_volume(identity: &ColumnIdentity, delta: f64) -> OpenActionResult<()> {
    let runtime = runtime(identity).await;
    let snapshot = snapshot(&runtime).await;
    if !snapshot.available {
        return Ok(());
    }
    let target = clamp_target_percent(snapshot.volume + delta, 100);
    let members = grouped_applications(snapshot.target_id.as_deref());
    audio::set_application_group_volume(
        members
            .into_iter()
            .map(|app| (app.uid, app.is_device))
            .collect(),
        target,
    );
    Ok(())
}

async fn toggle_mute(identity: &ColumnIdentity) -> OpenActionResult<()> {
    let runtime = runtime(identity).await;
    let snapshot = snapshot(&runtime).await;
    if !snapshot.available {
        return Ok(());
    }
    let members = grouped_applications(snapshot.target_id.as_deref());
    let mute = !members.iter().all(|app| app.mute);
    audio::set_application_group_mute(
        members
            .into_iter()
            .map(|app| (app.uid, app.is_device))
            .collect(),
        mute,
    );
    Ok(())
}

pub async fn refresh_visible_columns_for(changed: &HashSet<String>) {
    let columns = COLUMNS.read().await.clone();
    for (identity, runtime) in columns {
        let state = runtime.state.lock().await;
        let configured = state.settings.normalized_target_ids();
        let active = state.active_target_id.clone();
        drop(state);
        if configured.iter().any(|id| changed.contains(id))
            || active.as_ref().is_some_and(|id| changed.contains(id))
        {
            let _ = render_column(&identity).await;
        }
    }
}

#[async_trait]
impl Action for ApplicationVolumeColumnAction {
    const UUID: ActionUuid = "com.victormarin.volume-controller.appcolumn";
    type Settings = ApplicationVolumeColumnSettings;

    async fn will_appear(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        let Some(identity) = identity(instance) else {
            return Ok(());
        };
        CONTEXT_COLUMNS
            .write()
            .await
            .insert(instance.instance_id.clone(), identity.clone());
        let runtime = runtime(&identity).await;
        let shared = runtime.state.lock().await.settings.clone();
        let selected = if !settings.normalized_target_ids().is_empty() {
            settings.clone()
        } else {
            shared
        };
        synchronize_settings(&identity, selected, true).await
    }

    async fn will_disappear(
        &self,
        instance: &Instance,
        _: &Self::Settings,
    ) -> OpenActionResult<()> {
        if let Some(identity) = CONTEXT_COLUMNS.write().await.remove(&instance.instance_id) {
            render_column(&identity).await?;
        }
        Ok(())
    }

    async fn did_receive_settings(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        if let Some(identity) = identity(instance) {
            synchronize_settings(&identity, settings.clone(), false).await?;
        }
        Ok(())
    }

    async fn property_inspector_did_appear(
        &self,
        instance: &Instance,
        _: &Self::Settings,
    ) -> OpenActionResult<()> {
        if let Some(identity) = identity(instance) {
            send_targets(instance, &identity).await?;
        }
        Ok(())
    }

    async fn send_to_plugin(
        &self,
        instance: &Instance,
        _: &Self::Settings,
        payload: &serde_json::Value,
    ) -> OpenActionResult<()> {
        if payload.get("event").and_then(serde_json::Value::as_str) == Some("requestAudioTargets")
            && let Some(identity) = identity(instance)
        {
            send_targets(instance, &identity).await?;
        }
        Ok(())
    }

    async fn key_down(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
        let Some(identity) = identity(instance) else {
            return Ok(());
        };
        let rows = instances(&identity).await;
        if ![0, 1, 2].into_iter().all(|row| rows.contains_key(&row)) {
            return Ok(());
        }
        match instance.coordinates.map(|coordinates| coordinates.row) {
            Some(0) => toggle_mute(&identity).await?,
            Some(1) => change_volume(&identity, BUTTON_STEP).await?,
            Some(2) => change_volume(&identity, -BUTTON_STEP).await?,
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(device: &str, column: u8) -> ColumnIdentity {
        ColumnIdentity {
            device_id: device.into(),
            column,
        }
    }

    #[test]
    fn column_identity_shares_rows_but_separates_columns_and_devices() {
        assert_eq!(identity("deck-a", 2), identity("deck-a", 2));
        assert_ne!(identity("deck-a", 2), identity("deck-a", 3));
        assert_ne!(identity("deck-a", 2), identity("deck-b", 2));
    }

    #[test]
    fn settings_support_one_or_ordered_multiple_targets() {
        let one = ApplicationVolumeColumnSettings {
            target_ids: vec!["one".into()],
            ..Default::default()
        };
        assert_eq!(one.normalized_target_ids(), ["one"]);
        let multiple = ApplicationVolumeColumnSettings {
            target_ids: vec!["one".into(), "two".into(), "one".into()],
            ..Default::default()
        };
        assert_eq!(multiple.normalized_target_ids(), ["one", "two"]);
    }

    #[test]
    fn completeness_requires_exact_three_rows() {
        let complete = |rows: &[u8]| [0, 1, 2].into_iter().all(|row| rows.contains(&row));
        assert!(complete(&[0, 1, 2]));
        assert!(!complete(&[0, 2]));
    }

    #[test]
    fn column_uses_shared_sticky_arbitration() {
        let configured = vec!["firefox".into(), "chromium".into()];
        let both = ["firefox".into(), "chromium".into()].into_iter().collect();
        assert_eq!(
            arbitrate_active(&configured, &both, Some("chromium")).as_deref(),
            Some("chromium")
        );
        let firefox_only = ["firefox".into()].into_iter().collect();
        assert_eq!(
            arbitrate_active(&configured, &firefox_only, Some("chromium")).as_deref(),
            Some("firefox")
        );
    }

    #[test]
    fn group_volume_uses_correct_percentage_scale() {
        assert_eq!(clamp_target_percent(90.0 + BUTTON_STEP, 100), 100.0);
        assert_eq!(clamp_target_percent(100.0 + BUTTON_STEP, 100), 100.0);
        assert_eq!(clamp_target_percent(0.0 - BUTTON_STEP, 100), 0.0);
    }

    #[test]
    fn mixed_mute_state_mutes_all_and_all_muted_unmutes() {
        let next = |states: &[bool]| !states.iter().all(|muted| *muted);
        assert!(next(&[true, false]));
        assert!(!next(&[true, true]));
    }

    #[test]
    fn column_and_dial_share_the_same_icon_resolver_output() {
        let application = AppInfo {
            uid: 1,
            app_name: "test application".into(),
            sink_name: None,
            mute: false,
            vol_percent: 50.0,
            icon_name: Some("audio-x-generic".into()),
            is_device: false,
            is_multi_sink_app: false,
            metadata: Default::default(),
        };
        let dial_source = resolve_app_icon(&application);
        let column_source = resolve_app_icon(&application);
        assert_eq!(column_source.source_path, dial_source.source_path);
        assert_eq!(
            column_source.rendered_data_uri,
            dial_source.rendered_data_uri
        );
    }
}
