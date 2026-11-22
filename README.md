# tvbee

Command-line remote for a Philips Android TV over ADB. A Rust port of the
`tv` bash script (`~/.local/bin/tv` + `~/.local/lib/tv/*.sh`).

Commands are grouped by plugin in `tvbee help`, Django-style. A plugin is a
module in `src/plugins/` exporting `commands()`, registered in
`src/plugins/mod.rs`. Built-in plugins are always on. Vendor plugins (e.g.
`philips`) carry a detection check:

- `TVBEE_PLUGINS=philips,-other` forces plugins on/off
- otherwise the per-TV cache (`~/.cache/tvbee/<ip>.plugins`) is used
- otherwise the TV is probed, but only when one of that plugin's commands was
  asked for (or `tvbee plugins --refresh`); `help` never contacts the TV

`tvbee plugins` shows what is enabled and why.

    tvbee help            # all commands by category
    tvbee help launch     # one command
    tvbee launch --quiet iplayer
    tvbee apps --all      # offline alias list

Environment: `TVBEE_PLUGINS`, `TV_IP` (default 192.168.1.19), `TV_DEBLOAT` (path to tv-debloat.sh; otherwise `debloat` looks for `scripts/tv-debloat.sh` in the current directory. The script isn't included yet).

Needs `adb` in PATH (and `scrcpy` for `mirror`). The default build uses no
crates beyond std.

## Optional features

- `ui`: `tvbee ui`, a full-screen remote (what's on, progress, volume; keys for
  play/pause, seeking and volume). Pulls in `ratatui`/`crossterm`.

      cargo install --path . --features ui

  Without it, `tvbee ui` says how to get it. Optional plugins are compiled in
  with `#[cfg(feature = ...)]` in `src/plugins/mod.rs` and listed in
  `NOT_BUILT` when left out.

## Not yet ported / planned
- Recordings (list / stop) via the JointSpace API on `https://<tv>:1926/6/recordings/*`;
  needs digest-auth pairing first.
- `status` info line for the NetTV webapps (iPlayer, Dailymotion, Arte): they all
  run inside `org.droidtv.nettvbrowser`, so telling them apart (and finding a
  title or link) still needs probing while one is playing.
