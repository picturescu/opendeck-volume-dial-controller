use crate::{
    audio::{self, AudioSystem, DeviceKind},
    mixer, utils,
};
use libpulse_binding::{
    context::{
        Context, FlagSet,
        subscribe::{Facility, InterestMaskSet, Operation},
    },
    mainloop::threaded::Mainloop,
    proplist::Proplist,
};
use std::{
    collections::HashMap,
    error::Error,
    sync::{
        LazyLock, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError},
    },
    time::{Duration, Instant},
};

static MONITOR_STARTED: AtomicBool = AtomicBool::new(false);
static REFRESH_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const REFRESH_TIMEOUT: Duration = Duration::from_secs(2);

macro_rules! refresh_debug {
    ($($argument:tt)*) => {
        if cfg!(debug_assertions) {
            eprintln!($($argument)*);
        }
    };
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum ChangedObject {
    Application(u32),
    Output(u32),
    Input(u32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SubscriptionRoute {
    Ignore,
    Change(ChangedObject),
    Topology,
}

#[derive(Clone, Copy, Debug)]
enum RefreshReason {
    Initial,
    Topology,
    Explicit,
    Reconnect,
}

impl RefreshReason {
    fn label(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Topology => "topology",
            Self::Explicit => "explicit",
            Self::Reconnect => "reconnect",
        }
    }
}

type RefreshChannel = (
    SyncSender<RefreshReason>,
    Mutex<Option<Receiver<RefreshReason>>>,
);
type ChangeChannel = (SyncSender<()>, Mutex<Option<Receiver<()>>>);

static REFRESH_CHANNEL: LazyLock<RefreshChannel> = LazyLock::new(|| {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    (sender, Mutex::new(Some(receiver)))
});
static CHANGE_CHANNEL: LazyLock<ChangeChannel> = LazyLock::new(|| {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    (sender, Mutex::new(Some(receiver)))
});
static PENDING_CHANGES: LazyLock<Mutex<HashMap<ChangedObject, ()>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn start_pulse_monitoring() {
    if MONITOR_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }

    let runtime = tokio::runtime::Handle::current();
    start_refresh_processor(runtime.clone());
    start_change_processor(runtime);
    request_topology_refresh(RefreshReason::Initial);

    std::thread::Builder::new()
        .name("pulse-subscription".into())
        .spawn(run_subscription_monitor)
        .expect("failed to spawn PulseAudio subscription monitor");
}

fn run_subscription_monitor() {
    println!("[volume-worker] worker-started name=pulse-subscription");
    let mut backoff = Duration::from_millis(100);
    loop {
        match monitor_once() {
            Ok(()) => {
                eprintln!(
                    "[volume-worker] worker-stopped name=pulse-subscription reason=terminated"
                );
            }
            Err(error) => {
                eprintln!("[volume-worker] worker-stopped name=pulse-subscription reason={error}");
            }
        }
        eprintln!("[volume-worker] worker-restarting name=pulse-subscription");
        std::thread::sleep(backoff);
        backoff = next_reconnect_backoff(backoff);
        request_topology_refresh(RefreshReason::Reconnect);
    }
}

fn next_reconnect_backoff(current: Duration) -> Duration {
    (current * 2).min(Duration::from_secs(5))
}

fn monitor_once() -> Result<(), Box<dyn Error>> {
    let mut mainloop = Mainloop::new().ok_or("failed to create PulseAudio mainloop")?;
    mainloop.start().map_err(|_| "failed to start mainloop")?;
    let mut proplist = Proplist::new().ok_or("failed to create property list")?;
    proplist
        .set_str("application.name", "Volume Controller")
        .map_err(|_| "failed to set PulseAudio property")?;
    let mut context = Context::new_with_proplist(&mainloop, "VolumeControllerMonitor", &proplist)
        .ok_or("failed to create PulseAudio context")?;

    context.set_subscribe_callback(Some(Box::new(move |facility, operation, index| {
        route_subscription_event(facility, operation, index);
    })));
    context
        .connect(None, FlagSet::NOFLAGS, None)
        .map_err(|_| "failed to connect to PulseAudio")?;

    loop {
        match context.get_state() {
            libpulse_binding::context::State::Ready => break,
            libpulse_binding::context::State::Failed => return Err("connection failed".into()),
            libpulse_binding::context::State::Terminated => {
                return Err("connection terminated".into());
            }
            _ => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    context.subscribe(
        InterestMaskSet::SINK_INPUT | InterestMaskSet::SINK | InterestMaskSet::SOURCE,
        |_| {},
    );
    println!("PulseAudio monitoring started successfully");

    loop {
        match context.get_state() {
            libpulse_binding::context::State::Failed
            | libpulse_binding::context::State::Terminated => {
                return Err("subscription connection lost".into());
            }
            _ => std::thread::sleep(Duration::from_secs(1)),
        }
    }
}

fn route_subscription_event(facility: Option<Facility>, operation: Option<Operation>, index: u32) {
    match classify_subscription_event(facility, operation, index) {
        SubscriptionRoute::Topology => request_topology_refresh(RefreshReason::Topology),
        SubscriptionRoute::Change(changed) => {
            if let Ok(mut pending) = PENDING_CHANGES.lock() {
                pending.insert(changed, ());
            }
            let _ = CHANGE_CHANNEL.0.try_send(());
        }
        SubscriptionRoute::Ignore => {}
    }
}

fn classify_subscription_event(
    facility: Option<Facility>,
    operation: Option<Operation>,
    index: u32,
) -> SubscriptionRoute {
    match (facility, operation) {
        (Some(Facility::SinkInput), Some(Operation::Changed)) => {
            SubscriptionRoute::Change(ChangedObject::Application(index))
        }
        (Some(Facility::Sink), Some(Operation::Changed)) => {
            SubscriptionRoute::Change(ChangedObject::Output(index))
        }
        (Some(Facility::Source), Some(Operation::Changed)) => {
            SubscriptionRoute::Change(ChangedObject::Input(index))
        }
        (
            Some(Facility::SinkInput | Facility::Sink | Facility::Source),
            Some(Operation::New | Operation::Removed),
        ) => SubscriptionRoute::Topology,
        _ => SubscriptionRoute::Ignore,
    }
}

fn request_topology_refresh(reason: RefreshReason) {
    match REFRESH_CHANNEL.0.try_send(reason) {
        Ok(()) | Err(TrySendError::Full(_)) => {}
        Err(TrySendError::Disconnected(_)) => {
            eprintln!("[volume-worker] channel-closed name=topology-refresh");
        }
    }
}

fn start_change_processor(runtime: tokio::runtime::Handle) {
    let receiver = CHANGE_CHANNEL
        .1
        .lock()
        .ok()
        .and_then(|mut slot| slot.take());
    let Some(receiver) = receiver else {
        return;
    };
    std::thread::Builder::new()
        .name("pulse-change".into())
        .spawn(move || {
            println!("[volume-worker] worker-started name=pulse-change");
            let mut backend = audio::create();
            while receiver.recv().is_ok() {
                let changes = PENDING_CHANGES
                    .lock()
                    .map(|mut pending| pending.drain().map(|(key, ())| key).collect::<Vec<_>>())
                    .unwrap_or_default();
                for changed in changes {
                    if let Err(error) = process_changed_object(backend.as_mut(), changed, &runtime)
                    {
                        eprintln!("[volume-worker] targeted-query status=error error={error}");
                    }
                }
            }
            eprintln!("[volume-worker] channel-closed name=pulse-change");
        })
        .expect("failed to spawn PulseAudio change worker");
}

fn process_changed_object(
    backend: &mut dyn AudioSystem,
    changed: ChangedObject,
    runtime: &tokio::runtime::Handle,
) -> Result<(), Box<dyn Error>> {
    match changed {
        ChangedObject::Application(index) => {
            let (volume, mute) = backend.query_application_state(index)?;
            let Some(stable_id) = audio::update_application_state(index, volume, mute) else {
                request_topology_refresh(RefreshReason::Topology);
                return Ok(());
            };
            mixer::update_channel_state(index, volume, mute);
            runtime.spawn(async move {
                let changed = std::collections::HashSet::from([stable_id.clone()]);
                utils::update_stream_deck_buttons_for(&stable_id).await;
                crate::dial::refresh_visible_dials_for(Some(&changed)).await;
                crate::app_column::refresh_visible_columns_for(&changed).await;
                crate::dynamic::refresh_for_audio_changes(&changed).await;
            });
        }
        ChangedObject::Output(index) | ChangedObject::Input(index) => {
            let kind = if matches!(changed, ChangedObject::Output(_)) {
                DeviceKind::Output
            } else {
                DeviceKind::Input
            };
            let (stable_name, volume, mute) = backend.query_device_state(index, kind)?;
            if !audio::update_device_state(&stable_name, kind, volume, mute) {
                request_topology_refresh(RefreshReason::Topology);
                return Ok(());
            }
            runtime.spawn(async move {
                let changed = std::collections::HashSet::from([stable_name]);
                match kind {
                    DeviceKind::Output => {
                        crate::device_dial::refresh_visible_device_dials_for(Some(&changed), None)
                            .await;
                    }
                    DeviceKind::Input => {
                        crate::device_dial::refresh_visible_device_dials_for(None, Some(&changed))
                            .await;
                    }
                }
            });
        }
    }
    Ok(())
}

fn start_refresh_processor(runtime: tokio::runtime::Handle) {
    let receiver = REFRESH_CHANNEL
        .1
        .lock()
        .ok()
        .and_then(|mut slot| slot.take());
    let Some(receiver) = receiver else {
        return;
    };
    std::thread::Builder::new()
        .name("pulse-topology".into())
        .spawn(move || {
            println!("[volume-worker] worker-started name=topology-refresh");
            while let Ok(mut reason) = receiver.recv() {
                while let Ok(next) = receiver.try_recv() {
                    reason = next;
                }
                let seq = REFRESH_SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1;
                let started = Instant::now();
                refresh_debug!("[refresh] seq={seq} entered reason={}", reason.label());
                refresh_debug!("Processing debounced refresh request...");
                let (sender, result) = std::sync::mpsc::sync_channel(1);
                std::thread::Builder::new()
                    .name(format!("pulse-enumerate-{seq}"))
                    .spawn(move || {
                        let _ = sender.send(enumerate_topology(seq));
                    })
                    .expect("failed to spawn topology enumeration");

                match result.recv_timeout(REFRESH_TIMEOUT) {
                    Ok(Ok(topology)) => {
                        let elapsed = started.elapsed().as_millis();
                        runtime.spawn(async move {
                            if let Err(error) = apply_topology(seq, topology).await {
                                eprintln!(
                                    "[volume-worker] topology-refresh status=error seq={seq} error={error}"
                                );
                            } else {
                                refresh_debug!(
                                    "[refresh] seq={seq} finished elapsed_ms={}",
                                    started.elapsed().as_millis()
                                );
                                refresh_debug!("Audio applications refreshed successfully");
                            }
                        });
                        refresh_debug!(
                            "[volume-worker] topology-refresh status=enumerated seq={seq} elapsed_ms={elapsed}"
                        );
                    }
                    Ok(Err(error)) => eprintln!(
                        "[volume-worker] topology-refresh status=error seq={seq} error={error}"
                    ),
                    Err(_) => eprintln!(
                        "[volume-worker] backend-timeout operation=topology-refresh seq={seq} elapsed_ms={}",
                        started.elapsed().as_millis()
                    ),
                }
            }
            eprintln!("[volume-worker] channel-closed name=topology-refresh");
        })
        .expect("failed to spawn topology refresh worker");
}

struct Topology {
    applications: Vec<audio::AppInfo>,
    outputs: Vec<audio::AudioDeviceInfo>,
    inputs: Vec<audio::AudioDeviceInfo>,
}

fn enumerate_topology(seq: u64) -> Result<Topology, String> {
    let phase = |name: &str, start: Instant, count: Option<usize>| match count {
        Some(count) => refresh_debug!(
            "[refresh] seq={seq} {name}-complete count={count} elapsed_ms={}",
            start.elapsed().as_millis()
        ),
        None => refresh_debug!(
            "[refresh] seq={seq} {name}-complete elapsed_ms={}",
            start.elapsed().as_millis()
        ),
    };
    let started = Instant::now();
    refresh_debug!("[refresh] seq={seq} controller-create-start");
    let mut backend = audio::PulseAudioSystem::new().map_err(|error| error.to_string())?;
    phase("controller-create", started, None);

    let started = Instant::now();
    refresh_debug!("[refresh] seq={seq} enumerate-sink-inputs-start");
    let applications = backend
        .list_applications()
        .map_err(|error| error.to_string())?;
    phase("enumerate-sink-inputs", started, Some(applications.len()));

    let started = Instant::now();
    refresh_debug!("[refresh] seq={seq} enumerate-sinks-start");
    let outputs = backend
        .list_devices(DeviceKind::Output)
        .map_err(|error| error.to_string())?;
    phase("enumerate-sinks", started, Some(outputs.len()));

    let started = Instant::now();
    refresh_debug!("[refresh] seq={seq} enumerate-sources-start");
    let inputs = backend
        .list_devices(DeviceKind::Input)
        .map_err(|error| error.to_string())?;
    phase("enumerate-sources", started, Some(inputs.len()));
    Ok(Topology {
        applications,
        outputs,
        inputs,
    })
}

async fn apply_topology(seq: u64, topology: Topology) -> Result<(), Box<dyn Error>> {
    refresh_debug!("[refresh] seq={seq} normalize-targets-start");
    let applications = topology.applications;
    refresh_debug!("[refresh] seq={seq} normalize-targets-complete");
    refresh_debug!("[refresh] seq={seq} registry-lock-wait");
    let changes = audio::update_registry(applications.clone(), topology.outputs, topology.inputs);
    refresh_debug!("[refresh] seq={seq} registry-lock-acquired");
    refresh_debug!("[refresh] seq={seq} registry-lock-released");
    refresh_debug!("[refresh] seq={seq} registry-swap-complete");

    let ignored_apps = {
        let settings = crate::plugin::SHARED_SETTINGS.lock().await;
        settings.ignored_apps_list.clone()
    };
    refresh_debug!("[refresh] seq={seq} mixer-lock-wait");
    mixer::update_mixer_channels(applications, &ignored_apps).await;
    refresh_debug!("[refresh] seq={seq} mixer-lock-released");
    let notification_count =
        changes.applications.len() + changes.outputs.len() + changes.inputs.len();
    refresh_debug!("[refresh] seq={seq} notifications-built count={notification_count}");
    refresh_debug!("[refresh] seq={seq} action-notify-start");
    utils::update_stream_deck_buttons().await;
    crate::dial::refresh_visible_dials_for(Some(&changes.applications)).await;
    crate::app_column::refresh_visible_columns_for(&changes.applications).await;
    crate::dynamic::refresh_for_audio_changes(&changes.applications).await;
    crate::device_dial::refresh_visible_device_dials_for(
        Some(&changes.outputs),
        Some(&changes.inputs),
    )
    .await;
    refresh_debug!("[refresh] seq={seq} action-notify-complete");
    Ok(())
}

pub async fn refresh_audio_applications() -> Result<(), Box<dyn Error>> {
    let seq = REFRESH_SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1;
    let operation = tokio::task::spawn_blocking(move || enumerate_topology(seq));
    let topology = tokio::time::timeout(REFRESH_TIMEOUT, operation)
        .await
        .map_err(|_| {
            format!("[volume-worker] backend-timeout operation=explicit-topology-refresh seq={seq}")
        })?
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    apply_topology(seq, topology).await
}

pub fn request_explicit_refresh() {
    request_topology_refresh(RefreshReason::Explicit);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_events_do_not_enter_topology_queue() {
        for _ in 0..500 {
            assert_eq!(
                classify_subscription_event(
                    Some(Facility::SinkInput),
                    Some(Operation::Changed),
                    42,
                ),
                SubscriptionRoute::Change(ChangedObject::Application(42))
            );
        }
    }

    #[test]
    fn sink_and_source_changes_are_targeted() {
        assert_eq!(
            classify_subscription_event(Some(Facility::Sink), Some(Operation::Changed), 7),
            SubscriptionRoute::Change(ChangedObject::Output(7))
        );
        assert_eq!(
            classify_subscription_event(Some(Facility::Source), Some(Operation::Changed), 8),
            SubscriptionRoute::Change(ChangedObject::Input(8))
        );
    }

    #[test]
    fn new_and_removed_objects_request_topology_refresh() {
        for facility in [Facility::SinkInput, Facility::Sink, Facility::Source] {
            for operation in [Operation::New, Operation::Removed] {
                assert_eq!(
                    classify_subscription_event(Some(facility), Some(operation), 3),
                    SubscriptionRoute::Topology
                );
            }
        }
    }

    #[test]
    fn reconnect_backoff_is_bounded() {
        let mut backoff = Duration::from_millis(100);
        for _ in 0..20 {
            backoff = next_reconnect_backoff(backoff);
        }
        assert_eq!(backoff, Duration::from_secs(5));
    }

    #[test]
    fn latest_change_per_object_is_bounded() {
        let mut pending = HashMap::new();
        for _ in 0..10_000 {
            pending.insert(ChangedObject::Application(42), ());
        }
        assert_eq!(pending.len(), 1);
    }
}
