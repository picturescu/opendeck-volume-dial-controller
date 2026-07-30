pub mod audio_system;
pub mod pulse;

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{LazyLock, Mutex, RwLock},
};

pub use audio_system::{AppInfo, AudioDeviceInfo, AudioSystem, DeviceKind, is_monitor_source};
pub use pulse::PulseAudioSystem;

pub fn create() -> Box<dyn AudioSystem> {
    Box::new(PulseAudioSystem::new().unwrap())
}

#[derive(Clone, Default)]
struct AudioRegistry {
    initialized: bool,
    applications: Vec<AppInfo>,
    outputs: Vec<AudioDeviceInfo>,
    inputs: Vec<AudioDeviceInfo>,
}

static REGISTRY: LazyLock<RwLock<AudioRegistry>> =
    LazyLock::new(|| RwLock::new(AudioRegistry::default()));

#[derive(Default)]
struct PendingCommands {
    volumes: HashMap<Vec<(u32, bool)>, f64>,
    mutes: VecDeque<(Vec<(u32, bool)>, bool)>,
    device_volumes: HashMap<(String, DeviceKind), f64>,
    device_mutes: VecDeque<(String, DeviceKind, bool)>,
}

static PENDING_COMMANDS: LazyLock<Mutex<PendingCommands>> =
    LazyLock::new(|| Mutex::new(PendingCommands::default()));

static COMMAND_WAKE: LazyLock<std::sync::mpsc::SyncSender<()>> = LazyLock::new(|| {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("pulse-command".into())
        .spawn(move || {
            println!("[volume-worker] worker-started name=pulse-command");
            let mut system = create();
            while receiver.recv().is_ok() {
                let pending = PENDING_COMMANDS
                    .lock()
                    .map(|mut pending| std::mem::take(&mut *pending))
                    .unwrap_or_default();
                execute_pending(system.as_mut(), pending);
            }
            eprintln!("[volume-worker] channel-closed name=pulse-command");
        })
        .expect("failed to spawn PulseAudio command worker");
    sender
});

fn execute_pending(system: &mut dyn AudioSystem, pending: PendingCommands) {
    for (targets, mute) in pending.mutes {
        for (index, is_device) in targets {
            if let Err(error) = system.mute_volume(index, mute, is_device) {
                eprintln!("[audio-command] mute update failed: {error}");
            }
        }
    }
    for (targets, percent) in pending.volumes {
        for (index, is_device) in targets {
            if let Err(error) = system.set_volume_percent(index, percent, is_device) {
                eprintln!("[audio-command] volume update failed: {error}");
            }
        }
    }
    for (stable_name, kind, mute) in pending.device_mutes {
        if let Err(error) = system.mute_device(&stable_name, kind, mute) {
            eprintln!("[audio-command] device mute update failed: {error}");
        }
    }
    for ((stable_name, kind), percent) in pending.device_volumes {
        if let Err(error) = system.set_device_target(&stable_name, kind, percent) {
            eprintln!("[audio-command] device volume update failed: {error}");
        }
    }
}

pub fn set_application_group_volume(targets: Vec<(u32, bool)>, percent: f64) {
    if let Ok(mut pending) = PENDING_COMMANDS.lock() {
        pending.volumes.insert(targets, percent);
    }
    let _ = COMMAND_WAKE.try_send(());
}

pub fn set_application_group_mute(targets: Vec<(u32, bool)>, mute: bool) {
    if let Ok(mut pending) = PENDING_COMMANDS.lock() {
        pending.mutes.push_back((targets, mute));
    }
    let _ = COMMAND_WAKE.try_send(());
}

pub fn set_device_volume(stable_name: String, kind: DeviceKind, percent: f64) {
    if let Ok(mut pending) = PENDING_COMMANDS.lock() {
        pending.device_volumes.insert((stable_name, kind), percent);
    }
    let _ = COMMAND_WAKE.try_send(());
}

pub fn set_device_mute(stable_name: String, kind: DeviceKind, mute: bool) {
    if let Ok(mut pending) = PENDING_COMMANDS.lock() {
        pending.device_mutes.push_back((stable_name, kind, mute));
    }
    let _ = COMMAND_WAKE.try_send(());
}

#[derive(Default)]
pub struct RegistryChanges {
    pub applications: HashSet<String>,
    pub outputs: HashSet<String>,
    pub inputs: HashSet<String>,
}

pub fn update_registry(
    applications: Vec<AppInfo>,
    outputs: Vec<AudioDeviceInfo>,
    inputs: Vec<AudioDeviceInfo>,
) -> RegistryChanges {
    let mut changes = RegistryChanges::default();
    if let Ok(mut registry) = REGISTRY.write() {
        changes.applications = changed_application_ids(&registry.applications, &applications);
        changes.outputs = changed_device_ids(&registry.outputs, &outputs);
        changes.inputs = changed_device_ids(&registry.inputs, &inputs);
        *registry = AudioRegistry {
            initialized: true,
            applications,
            outputs,
            inputs,
        };
    }
    changes
}

fn changed_application_ids(previous: &[AppInfo], next: &[AppInfo]) -> HashSet<String> {
    fn fingerprints(apps: &[AppInfo]) -> HashMap<String, Vec<(u32, u32, bool)>> {
        let mut values = HashMap::<String, Vec<(u32, u32, bool)>>::new();
        for app in apps {
            values
                .entry(crate::icons::stable_application_id(app))
                .or_default()
                .push((app.uid, app.vol_percent.to_bits(), app.mute));
        }
        for value in values.values_mut() {
            value.sort_unstable();
        }
        values
    }
    let previous = fingerprints(previous);
    let next = fingerprints(next);
    previous
        .keys()
        .chain(next.keys())
        .filter(|id| previous.get(*id) != next.get(*id))
        .cloned()
        .collect()
}

fn changed_device_ids(previous: &[AudioDeviceInfo], next: &[AudioDeviceInfo]) -> HashSet<String> {
    let fingerprint = |devices: &[AudioDeviceInfo]| {
        devices
            .iter()
            .map(|device| {
                (
                    device.stable_name.clone(),
                    (device.vol_percent.to_bits(), device.mute),
                )
            })
            .collect::<HashMap<_, _>>()
    };
    let previous = fingerprint(previous);
    let next = fingerprint(next);
    previous
        .keys()
        .chain(next.keys())
        .filter(|id| previous.get(*id) != next.get(*id))
        .cloned()
        .collect()
}

pub fn registry_applications() -> Option<Vec<AppInfo>> {
    let registry = REGISTRY.read().ok()?;
    registry.initialized.then(|| registry.applications.clone())
}

pub fn registry_devices(kind: DeviceKind) -> Option<Vec<AudioDeviceInfo>> {
    let registry = REGISTRY.read().ok()?;
    registry.initialized.then(|| match kind {
        DeviceKind::Output => registry.outputs.clone(),
        DeviceKind::Input => registry.inputs.clone(),
    })
}

pub fn update_application_state(index: u32, volume: f32, mute: bool) -> Option<String> {
    let mut registry = REGISTRY.write().ok()?;
    let app = registry
        .applications
        .iter_mut()
        .find(|app| app.uid == index)?;
    app.vol_percent = volume;
    app.mute = mute;
    Some(crate::icons::stable_application_id(app))
}

pub fn update_device_state(stable_name: &str, kind: DeviceKind, volume: f32, mute: bool) -> bool {
    let Ok(mut registry) = REGISTRY.write() else {
        return false;
    };
    let devices = match kind {
        DeviceKind::Output => &mut registry.outputs,
        DeviceKind::Input => &mut registry.inputs,
    };
    let Some(device) = devices
        .iter_mut()
        .find(|device| device.stable_name == stable_name)
    else {
        return false;
    };
    device.vol_percent = volume;
    device.mute = mute;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[derive(Default)]
    struct MockBackend {
        applications: Vec<AppInfo>,
        calls: Vec<String>,
        fail: bool,
    }

    impl MockBackend {
        fn result(&self) -> Result<(), Box<dyn Error>> {
            if self.fail {
                Err("mock backend failure".into())
            } else {
                Ok(())
            }
        }
    }

    impl AudioSystem for MockBackend {
        fn list_applications(&mut self) -> Result<Vec<AppInfo>, Box<dyn Error>> {
            Ok(self.applications.clone())
        }

        fn list_devices(
            &mut self,
            _kind: DeviceKind,
        ) -> Result<Vec<AudioDeviceInfo>, Box<dyn Error>> {
            Ok(Vec::new())
        }

        fn set_volume_percent(
            &mut self,
            index: u32,
            percent: f64,
            is_device: bool,
        ) -> Result<(), Box<dyn Error>> {
            self.calls
                .push(format!("stream-volume:{index}:{percent}:{is_device}"));
            self.result()
        }

        fn mute_volume(
            &mut self,
            index: u32,
            mute: bool,
            is_device: bool,
        ) -> Result<(), Box<dyn Error>> {
            self.calls
                .push(format!("stream-mute:{index}:{mute}:{is_device}"));
            self.result()
        }

        fn set_device_target(
            &mut self,
            stable_name: &str,
            kind: DeviceKind,
            percent: f64,
        ) -> Result<(), Box<dyn Error>> {
            self.calls
                .push(format!("device-volume:{stable_name}:{kind:?}:{percent}"));
            self.result()
        }

        fn mute_device(
            &mut self,
            stable_name: &str,
            kind: DeviceKind,
            mute: bool,
        ) -> Result<(), Box<dyn Error>> {
            self.calls
                .push(format!("device-mute:{stable_name}:{kind:?}:{mute}"));
            self.result()
        }

        fn query_application_state(&mut self, _index: u32) -> Result<(f32, bool), Box<dyn Error>> {
            Err("not used".into())
        }

        fn query_device_state(
            &mut self,
            _index: u32,
            _kind: DeviceKind,
        ) -> Result<(String, f32, bool), Box<dyn Error>> {
            Err("not used".into())
        }
    }

    #[test]
    fn rapid_volume_commands_keep_only_latest_value_per_target() {
        let target = vec![(7, false)];
        let mut pending = PendingCommands::default();
        for value in 0..10_000 {
            pending.volumes.insert(target.clone(), f64::from(value));
        }
        assert_eq!(pending.volumes.len(), 1);
        assert_eq!(pending.volumes.get(&target), Some(&9_999.0));
    }

    #[test]
    fn mute_commands_preserve_order() {
        let target = vec![(7, false)];
        let mut pending = PendingCommands::default();
        pending.mutes.push_back((target.clone(), true));
        pending.mutes.push_back((target.clone(), false));
        assert_eq!(pending.mutes.pop_front(), Some((target.clone(), true)));
        assert_eq!(pending.mutes.pop_front(), Some((target, false)));
    }

    #[test]
    fn backend_contract_enumerates_playback_streams() {
        let expected = AppInfo {
            uid: 17,
            app_name: "browser".into(),
            sink_name: Some("stream".into()),
            mute: false,
            vol_percent: 42.0,
            icon_name: None,
            is_device: false,
            is_multi_sink_app: false,
            metadata: Default::default(),
        };
        let mut backend = MockBackend {
            applications: vec![expected],
            ..Default::default()
        };
        let applications = backend.list_applications().unwrap();
        assert_eq!(applications.len(), 1);
        assert_eq!(applications[0].uid, 17);
    }

    #[test]
    fn backend_contract_executes_group_and_device_operations() {
        let mut pending = PendingCommands::default();
        pending.volumes.insert(vec![(1, false), (2, false)], 88.0);
        pending
            .mutes
            .push_back((vec![(1, false), (2, false)], true));
        pending
            .device_volumes
            .insert(("output".into(), DeviceKind::Output), 55.0);
        pending
            .device_volumes
            .insert(("input".into(), DeviceKind::Input), 66.0);
        pending
            .device_mutes
            .push_back(("output".into(), DeviceKind::Output, true));
        pending
            .device_mutes
            .push_back(("input".into(), DeviceKind::Input, true));

        let mut backend = MockBackend::default();
        execute_pending(&mut backend, pending);

        assert!(backend.calls.contains(&"stream-volume:1:88:false".into()));
        assert!(backend.calls.contains(&"stream-volume:2:88:false".into()));
        assert!(backend.calls.contains(&"stream-mute:1:true:false".into()));
        assert!(backend.calls.contains(&"stream-mute:2:true:false".into()));
        assert!(
            backend
                .calls
                .contains(&"device-volume:output:Output:55".into())
        );
        assert!(
            backend
                .calls
                .contains(&"device-volume:input:Input:66".into())
        );
        assert!(
            backend
                .calls
                .contains(&"device-mute:output:Output:true".into())
        );
        assert!(
            backend
                .calls
                .contains(&"device-mute:input:Input:true".into())
        );
    }

    #[test]
    fn backend_command_error_does_not_stop_later_commands() {
        let mut pending = PendingCommands::default();
        pending.volumes.insert(vec![(1, false), (2, false)], 75.0);
        let mut backend = MockBackend {
            fail: true,
            ..Default::default()
        };
        execute_pending(&mut backend, pending);
        assert_eq!(backend.calls.len(), 2);
    }
}
