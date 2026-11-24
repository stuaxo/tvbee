pub mod apps;
pub mod debloat;
pub mod diagnose;
pub mod guide;
pub mod display;
pub mod info;
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
        Plugin::builtin("apps", "launching and managing Android apps", apps::commands).with_menu(apps::menu),
        Plugin::builtin("debloat", "disabling unwanted system apps, reversibly", debloat::commands),
        Plugin::builtin("diagnose", "finding the TV and checking the setup", diagnose::commands),
        Plugin::builtin("guide", "programme guides and content providers", guide::commands),
        Plugin::builtin("options", "tveee's own settings", crate::options::commands),
        Plugin::builtin("pairing", "pairing tveee with services on the TV", crate::pairing::commands),
        Plugin::builtin("display", "seeing the TV screen", display::commands).with_menu(display::menu),
        Plugin::builtin("info", "querying the TV's state", info::commands),
        Plugin::builtin("input", "remote-control keys, text, inputs and channels", input::commands).with_menu(input::menu),
        Plugin::builtin("media", "playback keys, volume and mute", media::commands),
        Plugin::builtin("power", "sleep, wake, reboot, shutdown", power::commands).with_menu(power::menu),
        Plugin::builtin("tools", "housekeeping and the CLI itself", tools::commands),
        Plugin::detect(
            "philips",
            "Philips TV features via the JointSpace API",
            philips::detect,
            philips::commands,
        )
        .with_apps(philips::apps)
        .with_providers(philips::providers)
        .with_pairings(philips::pairings),
        Plugin::detect("youtube", "the YouTube app: what's on, search, looking inside it", youtube::detect, youtube::commands)
            .with_apps(youtube::apps)
            .with_providers(youtube::providers)
            .with_menu(youtube::menu),
        #[cfg(feature = "ui")]
        Plugin::builtin("ui", "interactive terminal remote (ratatui)", ui::commands),
    ]
}
