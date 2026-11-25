pub mod ambilight;
pub mod apps;
pub mod debloat;
pub mod diagnose;
pub mod guide;
pub mod display;
pub mod info;
pub mod kodi;
pub mod input;
pub mod media;
pub mod open;
pub mod philips;
pub mod power;
pub mod search;
pub mod tools;
pub mod youtube;
#[cfg(feature = "ui")]
pub mod ui;

use crate::plugin::Plugin;

/// Commands of optional plugins left out of this build: (command, cargo feature).
pub const NOT_BUILT: &[(&str, &str)] = &[
    #[cfg(not(feature = "ui"))]
    ("ui", "ui"),
];

/// Every plugin, built-in and vendor. Add new ones here.
pub fn all() -> Vec<Plugin> {
    vec![
        Plugin::builtin("apps", "launching and managing Android apps", apps::actions),
        Plugin::builtin("debloat", "disabling unwanted system apps, reversibly", debloat::actions),
        Plugin::builtin("diagnose", "finding the TV and checking the setup", diagnose::actions),
        Plugin::builtin("guide", "programme guides and content providers", guide::actions),
        Plugin::builtin("options", "tveee's own settings", crate::options::actions),
        Plugin::builtin("pairing", "pairing tveee with services on the TV", crate::pairing::actions),
        Plugin::builtin("display", "seeing the TV screen", display::actions),
        Plugin::builtin("info", "querying the TV's state", info::actions),
        Plugin::builtin("input", "remote-control keys, text, inputs and channels", input::actions),
        Plugin::builtin("media", "playback keys, volume and mute", media::actions),
        Plugin::builtin("power", "sleep, wake, reboot, shutdown", power::actions),
        Plugin::builtin("tools", "housekeeping and the CLI itself", tools::actions),
        Plugin::detect(
            "philips",
            "Philips TV features via the JointSpace API",
            philips::detect,
            philips::actions,
        )
        .with_apps(philips::apps)
        .with_providers(philips::providers)
        .with_pairings(philips::pairings),
        Plugin::detect("kodi", "Kodi: what it's playing, exact seeking, artwork, places (its remote control)", kodi::detect, kodi::actions)
            .with_providers(kodi::providers)
            .with_pairings(kodi::pairings),
        Plugin::detect("youtube", "the YouTube app: what's on, search, looking inside it", youtube::detect, youtube::actions)
            .with_apps(youtube::apps)
            .with_providers(youtube::providers),
        #[cfg(feature = "ui")]
        Plugin::builtin("ui", "interactive terminal remote (ratatui)", ui::actions),
    ]
}
