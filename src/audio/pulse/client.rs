use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    error::Error,
    fmt,
    ops::Deref,
    rc::Rc,
};

use libpulse_binding::{
    callbacks::ListResult,
    context::{Context, FlagSet, introspect::Introspector},
    mainloop::standard::{IterateResult, Mainloop},
    operation::{Operation, State},
    proplist::{Proplist, properties},
    volume::ChannelVolumes,
};

#[derive(Debug)]
struct PulseError(String);

impl fmt::Display for PulseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for PulseError {}

fn error(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(PulseError(message.into()))
}

#[derive(Clone, Debug)]
pub struct PlaybackStream {
    pub index: u32,
    pub name: Option<String>,
    pub client: Option<u32>,
    pub volume: ChannelVolumes,
    pub mute: bool,
    pub properties: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct Device {
    pub index: u32,
    pub name: String,
    pub description: Option<String>,
    pub volume: ChannelVolumes,
    pub mute: bool,
    pub icon_name: Option<String>,
    pub monitor_of_sink: Option<u32>,
}

pub struct PulseClient {
    mainloop: Rc<RefCell<Mainloop>>,
    context: Rc<RefCell<Context>>,
    introspector: Introspector,
}

impl PulseClient {
    pub fn connect(application_name: &str) -> Result<Self, Box<dyn Error>> {
        let mut properties =
            Proplist::new().ok_or_else(|| error("failed to create PulseAudio property list"))?;
        properties
            .set_str(properties::APPLICATION_NAME, application_name)
            .map_err(|_| error("failed to set PulseAudio application name"))?;
        let mainloop = Rc::new(RefCell::new(
            Mainloop::new().ok_or_else(|| error("failed to create PulseAudio mainloop"))?,
        ));
        let context = Rc::new(RefCell::new(
            Context::new_with_proplist(
                mainloop.borrow().deref(),
                "OpenDeckVolumeController",
                &properties,
            )
            .ok_or_else(|| error("failed to create PulseAudio context"))?,
        ));
        context
            .borrow_mut()
            .connect(None, FlagSet::NOFLAGS, None)
            .map_err(|pulse_error| error(format!("failed to connect PulseAudio: {pulse_error}")))?;
        loop {
            match mainloop.borrow_mut().iterate(false) {
                IterateResult::Success(_) => {}
                IterateResult::Err(pulse_error) => {
                    return Err(error(format!(
                        "PulseAudio connection iteration failed: {pulse_error}"
                    )));
                }
                IterateResult::Quit(value) => {
                    return Err(error(format!(
                        "PulseAudio mainloop quit during connection: {}",
                        value.0
                    )));
                }
            }
            match context.borrow().get_state() {
                libpulse_binding::context::State::Ready => break,
                libpulse_binding::context::State::Failed
                | libpulse_binding::context::State::Terminated => {
                    return Err(error("PulseAudio connection failed or terminated"));
                }
                _ => {}
            }
        }
        let introspector = context.borrow_mut().introspect();
        Ok(Self {
            mainloop,
            context,
            introspector,
        })
    }

    fn wait<G: ?Sized>(&mut self, operation: Operation<G>) -> Result<(), Box<dyn Error>> {
        loop {
            match operation.get_state() {
                State::Done => return Ok(()),
                State::Cancelled => return Err(error("PulseAudio operation was cancelled")),
                State::Running => {}
            }
            match self.mainloop.borrow_mut().iterate(false) {
                IterateResult::Success(_) => {}
                IterateResult::Err(pulse_error) => {
                    return Err(error(format!(
                        "PulseAudio operation iteration failed: {pulse_error}"
                    )));
                }
                IterateResult::Quit(value) => {
                    return Err(error(format!(
                        "PulseAudio mainloop quit during operation: {}",
                        value.0
                    )));
                }
            }
        }
    }

    fn wait_success(
        &mut self,
        operation: Operation<dyn FnMut(bool)>,
        success: Rc<RefCell<Option<bool>>>,
    ) -> Result<(), Box<dyn Error>> {
        self.wait(operation)?;
        match *success.borrow() {
            Some(true) => Ok(()),
            Some(false) => Err(error("PulseAudio server rejected operation")),
            None => Err(error("PulseAudio operation returned no result")),
        }
    }

    pub fn list_playback_streams(&mut self) -> Result<Vec<PlaybackStream>, Box<dyn Error>> {
        let output = Rc::new(RefCell::new(Vec::new()));
        let callback_output = Rc::clone(&output);
        let operation = self.introspector.get_sink_input_info_list(move |result| {
            if let ListResult::Item(info) = result {
                callback_output.borrow_mut().push(playback_stream(info));
            }
        });
        self.wait(operation)?;
        Ok(output.borrow().clone())
    }

    pub fn playback_stream(&mut self, index: u32) -> Result<PlaybackStream, Box<dyn Error>> {
        let output = Rc::new(RefCell::new(None));
        let callback_output = Rc::clone(&output);
        let operation = self.introspector.get_sink_input_info(index, move |result| {
            if let ListResult::Item(info) = result {
                *callback_output.borrow_mut() = Some(playback_stream(info));
            }
        });
        self.wait(operation)?;
        output
            .borrow_mut()
            .take()
            .ok_or_else(|| error(format!("playback stream {index} was not found")))
    }

    pub fn list_sinks(&mut self) -> Result<Vec<Device>, Box<dyn Error>> {
        let output = Rc::new(RefCell::new(Vec::new()));
        let callback_output = Rc::clone(&output);
        let operation = self.introspector.get_sink_info_list(move |result| {
            if let ListResult::Item(info) = result
                && let Some(device) = sink(info)
            {
                callback_output.borrow_mut().push(device);
            }
        });
        self.wait(operation)?;
        Ok(output.borrow().clone())
    }

    pub fn sink_by_index(&mut self, index: u32) -> Result<Device, Box<dyn Error>> {
        let output = Rc::new(RefCell::new(None));
        let callback_output = Rc::clone(&output);
        let operation = self
            .introspector
            .get_sink_info_by_index(index, move |result| {
                if let ListResult::Item(info) = result {
                    *callback_output.borrow_mut() = sink(info);
                }
            });
        self.wait(operation)?;
        output
            .borrow_mut()
            .take()
            .ok_or_else(|| error(format!("sink {index} was not found")))
    }

    pub fn sink_by_name(&mut self, name: &str) -> Result<Device, Box<dyn Error>> {
        let output = Rc::new(RefCell::new(None));
        let callback_output = Rc::clone(&output);
        let operation = self
            .introspector
            .get_sink_info_by_name(name, move |result| {
                if let ListResult::Item(info) = result {
                    *callback_output.borrow_mut() = sink(info);
                }
            });
        self.wait(operation)?;
        output
            .borrow_mut()
            .take()
            .ok_or_else(|| error(format!("sink {name} was not found")))
    }

    pub fn default_sink(&mut self) -> Result<Device, Box<dyn Error>> {
        let output = Rc::new(RefCell::new(None));
        let callback_output = Rc::clone(&output);
        let operation = self.introspector.get_server_info(move |info| {
            *callback_output.borrow_mut() =
                info.default_sink_name.as_ref().map(ToString::to_string);
        });
        self.wait(operation)?;
        let name = output
            .borrow_mut()
            .take()
            .ok_or_else(|| error("PulseAudio server has no default sink"))?;
        self.sink_by_name(&name)
    }

    pub fn list_sources(&mut self) -> Result<Vec<Device>, Box<dyn Error>> {
        let output = Rc::new(RefCell::new(Vec::new()));
        let callback_output = Rc::clone(&output);
        let operation = self.introspector.get_source_info_list(move |result| {
            if let ListResult::Item(info) = result
                && let Some(device) = source(info)
            {
                callback_output.borrow_mut().push(device);
            }
        });
        self.wait(operation)?;
        Ok(output.borrow().clone())
    }

    pub fn source_by_index(&mut self, index: u32) -> Result<Device, Box<dyn Error>> {
        let output = Rc::new(RefCell::new(None));
        let callback_output = Rc::clone(&output);
        let operation = self
            .introspector
            .get_source_info_by_index(index, move |result| {
                if let ListResult::Item(info) = result {
                    *callback_output.borrow_mut() = source(info);
                }
            });
        self.wait(operation)?;
        output
            .borrow_mut()
            .take()
            .ok_or_else(|| error(format!("source {index} was not found")))
    }

    pub fn source_by_name(&mut self, name: &str) -> Result<Device, Box<dyn Error>> {
        let output = Rc::new(RefCell::new(None));
        let callback_output = Rc::clone(&output);
        let operation = self
            .introspector
            .get_source_info_by_name(name, move |result| {
                if let ListResult::Item(info) = result {
                    *callback_output.borrow_mut() = source(info);
                }
            });
        self.wait(operation)?;
        output
            .borrow_mut()
            .take()
            .ok_or_else(|| error(format!("source {name} was not found")))
    }

    pub fn client_metadata(
        &mut self,
    ) -> Result<HashMap<u32, BTreeMap<String, String>>, Box<dyn Error>> {
        let output = Rc::new(RefCell::new(HashMap::new()));
        let callback_output = Rc::clone(&output);
        let operation = self.introspector.get_client_info_list(move |result| {
            if let ListResult::Item(info) = result {
                callback_output
                    .borrow_mut()
                    .insert(info.index, client_properties(&info.proplist));
            }
        });
        self.wait(operation)?;
        Ok(output.borrow().clone())
    }

    pub fn set_playback_volume(
        &mut self,
        index: u32,
        volume: &ChannelVolumes,
    ) -> Result<(), Box<dyn Error>> {
        let success = Rc::new(RefCell::new(None));
        let callback_success = Rc::clone(&success);
        let operation = self.introspector.set_sink_input_volume(
            index,
            volume,
            Some(Box::new(move |value| {
                *callback_success.borrow_mut() = Some(value);
            })),
        );
        self.wait_success(operation, success)
    }

    pub fn set_playback_mute(&mut self, index: u32, mute: bool) -> Result<(), Box<dyn Error>> {
        let success = Rc::new(RefCell::new(None));
        let callback_success = Rc::clone(&success);
        let operation = self.introspector.set_sink_input_mute(
            index,
            mute,
            Some(Box::new(move |value| {
                *callback_success.borrow_mut() = Some(value);
            })),
        );
        self.wait_success(operation, success)
    }

    pub fn set_sink_volume(
        &mut self,
        name: &str,
        volume: &ChannelVolumes,
    ) -> Result<(), Box<dyn Error>> {
        let success = Rc::new(RefCell::new(None));
        let callback_success = Rc::clone(&success);
        let operation = self.introspector.set_sink_volume_by_name(
            name,
            volume,
            Some(Box::new(move |value| {
                *callback_success.borrow_mut() = Some(value);
            })),
        );
        self.wait_success(operation, success)
    }

    pub fn set_sink_mute(&mut self, name: &str, mute: bool) -> Result<(), Box<dyn Error>> {
        let success = Rc::new(RefCell::new(None));
        let callback_success = Rc::clone(&success);
        let operation = self.introspector.set_sink_mute_by_name(
            name,
            mute,
            Some(Box::new(move |value| {
                *callback_success.borrow_mut() = Some(value);
            })),
        );
        self.wait_success(operation, success)
    }

    pub fn set_source_volume(
        &mut self,
        name: &str,
        volume: &ChannelVolumes,
    ) -> Result<(), Box<dyn Error>> {
        let success = Rc::new(RefCell::new(None));
        let callback_success = Rc::clone(&success);
        let operation = self.introspector.set_source_volume_by_name(
            name,
            volume,
            Some(Box::new(move |value| {
                *callback_success.borrow_mut() = Some(value);
            })),
        );
        self.wait_success(operation, success)
    }

    pub fn set_source_mute(&mut self, name: &str, mute: bool) -> Result<(), Box<dyn Error>> {
        let success = Rc::new(RefCell::new(None));
        let callback_success = Rc::clone(&success);
        let operation = self.introspector.set_source_mute_by_name(
            name,
            mute,
            Some(Box::new(move |value| {
                *callback_success.borrow_mut() = Some(value);
            })),
        );
        self.wait_success(operation, success)
    }
}

impl Drop for PulseClient {
    fn drop(&mut self) {
        self.context.borrow_mut().disconnect();
    }
}

fn selected_properties(proplist: &Proplist, keys: &[&str]) -> BTreeMap<String, String> {
    keys.iter()
        .filter_map(|key| {
            proplist
                .get_str(key)
                .map(|value| ((*key).to_owned(), value))
        })
        .collect()
}

fn playback_stream(
    info: &libpulse_binding::context::introspect::SinkInputInfo<'_>,
) -> PlaybackStream {
    PlaybackStream {
        index: info.index,
        name: info.name.as_ref().map(ToString::to_string),
        client: info.client,
        volume: info.volume,
        mute: info.mute,
        properties: selected_properties(
            &info.proplist,
            &[
                "application.desktop",
                "application.id",
                "application.name",
                "application.process.binary",
                "application.process.id",
                "application.icon_name",
                "media.icon_name",
                "media.name",
                "flatpak.app-id",
                "application.flatpak.id",
                "snap.name",
                "application.snap.name",
                "window.x11.display",
                "steam.app.id",
                "SteamAppId",
                "SteamGameId",
                "SteamOverlayGameId",
                "STEAM_COMPAT_APP_ID",
            ],
        ),
    }
}

fn sink(info: &libpulse_binding::context::introspect::SinkInfo<'_>) -> Option<Device> {
    Some(Device {
        index: info.index,
        name: info.name.as_ref()?.to_string(),
        description: info.description.as_ref().map(ToString::to_string),
        volume: info.volume,
        mute: info.mute,
        icon_name: info.proplist.get_str("device.icon_name"),
        monitor_of_sink: None,
    })
}

fn source(info: &libpulse_binding::context::introspect::SourceInfo<'_>) -> Option<Device> {
    Some(Device {
        index: info.index,
        name: info.name.as_ref()?.to_string(),
        description: info.description.as_ref().map(ToString::to_string),
        volume: info.volume,
        mute: info.mute,
        icon_name: info.proplist.get_str("device.icon_name"),
        monitor_of_sink: info.monitor_of_sink,
    })
}

fn client_properties(proplist: &Proplist) -> BTreeMap<String, String> {
    selected_properties(
        proplist,
        &[
            "application.process.binary",
            "application.process.id",
            "pipewire.sec.pid",
            "application.name",
        ],
    )
}
