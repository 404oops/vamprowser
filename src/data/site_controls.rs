//! Persistent choices for individual web hosts. Private windows use an
//! in-memory copy instead of this file.

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use serde::{Deserialize, Serialize};

use crate::{
    settings::{Protection, SitePermission},
    state,
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SiteControl {
    pub protection: Option<Protection>,
    pub third_party_cookies: Option<bool>,
    pub camera: Option<SitePermission>,
    pub microphone: Option<SitePermission>,
    pub screen_capture: Option<SitePermission>,
}

impl SiteControl {
    pub fn permission(&self, slot: usize) -> Option<SitePermission> {
        match slot {
            0 => self.camera,
            1 => self.microphone,
            _ => self.screen_capture,
        }
    }

    pub fn set_permission(&mut self, slot: usize, value: Option<SitePermission>) {
        *match slot {
            0 => &mut self.camera,
            1 => &mut self.microphone,
            _ => &mut self.screen_capture,
        } = value;
    }

    fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SiteControls(pub HashMap<String, SiteControl>);

pub type SharedSiteControls = Arc<RwLock<SiteControls>>;

pub fn host(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    if matches!(parsed.scheme(), "http" | "https") {
        parsed.host_str().map(str::to_owned)
    } else {
        None
    }
}

impl SiteControls {
    pub fn load() -> Self {
        state::load_json(state::data_path("site-controls.json")).unwrap_or_default()
    }

    pub fn get(&self, host: &str) -> SiteControl {
        self.0.get(host).cloned().unwrap_or_default()
    }

    pub fn change(&mut self, host: &str, edit: impl FnOnce(&mut SiteControl)) {
        let control = self.0.entry(host.to_owned()).or_default();
        edit(control);
        if control.is_default() {
            self.0.remove(host);
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = state::data_path("site-controls.json")
            .ok_or_else(|| std::io::Error::other("HOME is unset"))?;
        state::write_atomic(&path, &serde_json::to_vec_pretty(self)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_are_isolated_by_host_and_default_entries_disappear() {
        let mut controls = SiteControls::default();
        controls.change("teams.microsoft.com", |s| {
            s.microphone = Some(SitePermission::Allow)
        });
        assert_eq!(
            controls.get("teams.microsoft.com").microphone,
            Some(SitePermission::Allow)
        );
        assert_eq!(
            controls.get("other.microsoft.com").microphone,
            None
        );
        controls.change("teams.microsoft.com", |s| {
            s.microphone = None
        });
        assert!(controls.0.is_empty());
    }

    #[test]
    fn hosts_include_subdomains_and_reject_internal_pages() {
        assert_eq!(
            host("https://Teams.Microsoft.com/chat"),
            Some("teams.microsoft.com".into())
        );
        assert_eq!(host("vamp://settings"), None);
    }

    #[test]
    fn explicit_ask_survives_serialization() {
        let mut controls = SiteControls::default();
        controls.change("teams.microsoft.com", |site| {
            site.microphone = Some(SitePermission::Ask);
        });
        let copy: SiteControls = serde_json::from_slice(&serde_json::to_vec(&controls).unwrap()).unwrap();
        assert_eq!(copy.get("teams.microsoft.com").microphone, Some(SitePermission::Ask));
        assert_eq!(copy.get("example.com").microphone, None);
    }
}
