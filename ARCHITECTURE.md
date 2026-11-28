# Architecture

A map of tveee's code: where things are, and how they fit. For using
tveee, see the README.

## The crates

```
tveee (this directory)      the binary: the command line, and the list of plugins
crates/tveee-core           everything that isn't a front end or a vendor's
  builtin/                    the built-in plugins: apps, power, media, guide, info, ...
  engine/                     the headless engine (behind the `engine` feature)
crates/tveee-tui            `tveee ui`: the terminal front end (ratatui)
plugins/philips             JointSpace, Ambilight, live TV, NetTV
plugins/youtube             the YouTube app (devtools), chapters, previews
plugins/kodi                Kodi's JSON-RPC: playing, its library, add-ons
```

Each plugin depends only on `tveee-core`; `tveee-core` depends on no front
end (CI checks it pulls in no terminal crates), so a GUI could use it too.
`tveee-core` and the plugins build with Rust 1.85; `tveee-tui` needs 1.90.

## Talking to the TV (`tveee-core`)

* **`Tv`** (`tv.rs`): adb, over the network: `shell`, `adb`, `exec-out`,
  port forwards, file pushes. Every call is logged with its time.
* **Typed requests** (`ask.rs`): anything asked of the TV is a small type
  implementing `Ask`: its request as data (a shell `Cmd`, quoted once, or an
  HTTP request with its auth and retry policy) and its answer parsed in one
  place. `Tv::ask` sends it, asking again when an answer isn't whole;
  `ask_both` puts two shell commands in one adb call.
* **Readings** (`readings.rs`): the shell readings (power, window, audio,
  media sessions, system figures); `Kept` holds the last whole one for
  things that keep reading.
* **Transports**: `http.rs` (a small client for plain HTTP), `https.rs`
  (through `curl`: TLS with self-signed certificates, digest auth),
  `ws.rs` (WebSocket, and Kodi's raw TCP push), `mdns.rs` (discovery),
  `keys.rs` (raw key events straight into the TV's input devices).
* **Record and replay** (`mock.rs`): with `TVEEE_MOCK_DIR`, every adb and
  HTTP call is recorded or answered from a recording, for tests.

## Actions and plugins

* **`ActionDescriptor`** (`actions.rs`): one list of everything tveee can
  do. The command line (`tveee <command>`, help, the README's listing),
  the ui's palette and `.` menu all come from it.
* **`Plugin`** (`plugin.rs`): what a plugin brings: actions, apps,
  providers, pairings, panels, namers, diagnostics. `plugin::install_enabled`
  installs the enabled plugins' parts into registries (at the start, and
  again when one is turned on or off).
* **Providers** (`provider.rs`): an app tveee knows well (YouTube, Kodi,
  live TV) says what's playing, seeks exactly, gives pictures, lists
  (`ContextList`), a guide. `Outcome::Fallback` means "do it the usual way"
  (keys, media sessions).
* **Panels** (`panel.rs`): a plugin's controls as plain data (the Ambilight).

## The engine (`tveee-core/src/engine`)

The ui doesn't talk to the TV: it sends `Action`s to the engine and draws
from its `TvState`. The engine runs on threads (no async):

* **Lanes**: `Keys` (keys and typing, batched), `Act` (things that change the
  TV, one at a time, coalesced), `Read` (things the user waits to see),
  `Idle` (work done only when the user pauses).
* **The refresh** reads the TV every second (every 4s with low bandwidth;
  sooner after the user does something) into a `Snapshot`.
* **Push**: providers that can say what changed (Kodi's notifications) wake
  the refresh.
* Front ends hold a `Handle`: `state()`, `send(Action)`, `dispatch(id,
  args)` for any action, and change events.

## The terminal ui (`tveee-tui`)

`App` (`lib.rs`) holds what's on screen and the pop-up open (`Overlay`);
each pop-up has its module (`guide.rs`, `launcher.rs`, `palette.rs`,
`ctxlist.rs` the list pane, `logview.rs`, `ambi.rs`, ...). `drawing.rs` draws
the main screen and shared widgets (scrollbars, buttons). `keymap.rs` maps
keys to `Input`s; `listnav.rs` documents the keys every widget follows.
Screens are tested as text snapshots (`snapshots.rs`, insta) at 120x35 and
60x16.

## Conventions

* Feature branches off `main`, one fix or feature each, merged with
  `--no-ff`; releases are tagged `vX.Y.Z` on main (CI builds them).
* Before merging: `cargo fmt`, `cargo clippy -D warnings`, the tests, and
  the build on Rust 1.85 (`pre-commit` runs fmt and clippy).
* Few dependencies, on purpose: no async runtime, no TLS library (curl),
  our own HTTP and WebSocket clients.
* Screenshots: `tools/contact-sheet/` records `tveee ui` at a terminal size
  and draws contact sheets (wide, narrow, tall) and the README's GIF.
