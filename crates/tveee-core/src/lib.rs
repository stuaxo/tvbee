//! tveee's core: talking to the TV (adb, HTTP, HTTPS, websockets), the
//! action registry, what plugins are made of (providers, pairings, panels,
//! apps), the built-in plugins, and, with the `engine` feature, the engine
//! interactive front ends run on. No terminal in here: the command line is
//! the `tveee` binary, the terminal remote is `tveee-tui`, and vendor
//! plugins (Philips, YouTube, Kodi) are crates of their own.

pub mod actions;
pub mod ask;
pub mod builtin;
pub mod crypto;
pub mod devtools;
pub mod dotenv;
#[cfg(feature = "engine")]
pub mod engine;
pub mod error;
pub mod favourites;
pub mod frames;
pub mod http;
pub mod https;
pub mod keys;
pub mod labels;
pub mod lineups;
pub mod log;
pub mod mdns;
pub mod mock;
pub mod more;
pub mod nowplaying;
pub mod options;
pub mod pairing;
pub mod panel;
pub mod plugin;
#[cfg(feature = "engine")]
pub mod preview;
pub mod provider;
pub mod readings;
pub mod registry;
pub mod store;
pub mod tv;
pub mod watch;
pub mod ws;

use plugin::PluginState;
use registry::Command;
use tv::Tv;

/// What a command gets: the TV, and what the enabled plugins offer.
pub struct Ctx {
    pub tv: Tv,
    /// Commands of the enabled plugins only.
    pub commands: Vec<Command>,
    /// Actions of the enabled plugins, in plugin order.
    pub actions: Vec<actions::ActionDescriptor>,
    /// Those with a menu key.
    pub menu: Vec<actions::ActionDescriptor>,
    pub plugins: Vec<PluginState>,
    /// Every plugin, enabled or not (`plugins --refresh` probes them all).
    pub all: fn() -> Vec<plugin::Plugin>,
    /// The enabled plugins' parts of `tveee diagnose`.
    pub diagnoses: Vec<fn(&Tv) -> Vec<plugin::Finding>>,
    /// Whether this build has the terminal remote (`tveee ui`).
    pub has_ui: bool,
}
