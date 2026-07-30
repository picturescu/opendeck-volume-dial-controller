use std::collections::HashMap;

use serde::Serialize;

use crate::{audio, icons::stable_application_id};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationTargetOption {
    pub id: String,
    pub name: String,
    pub detail: String,
    pub available: bool,
}

pub fn application_target_inventory() -> (Vec<ApplicationTargetOption>, Option<String>) {
    crate::audio::pulse::pulse_monitor::request_explicit_refresh();
    let Some(applications) = audio::registry_applications() else {
        return (
            Vec::new(),
            Some("Audio registry is not initialized".to_owned()),
        );
    };
    let mut grouped = HashMap::<String, ApplicationTargetOption>::new();
    for app in applications {
        let id = stable_application_id(&app);
        let detail = if app.is_device {
            "System output".to_owned()
        } else if app.is_multi_sink_app {
            "All active streams".to_owned()
        } else {
            app.sink_name.unwrap_or_default()
        };
        grouped
            .entry(id.clone())
            .or_insert(ApplicationTargetOption {
                id,
                name: app.app_name,
                detail,
                available: true,
            });
    }
    let mut targets = grouped.into_values().collect::<Vec<_>>();
    targets.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.detail.cmp(&right.detail))
    });
    (targets, None)
}
