# tvbee

🔴🟠🟡 Warning slopcoded - made to operate my own TV YMMV  🟡🟠🔴

This is a rust port of [tv.sh](https://github.com/stuaxo/tv.sh) with more functinaltiy.


Command-line remote for Android TV over ADB. So far it has only been tested
on a Philips Android TV (Android 8). A Rust port of the `tv` bash script
(`~/.local/bin/tv` + `~/.local/lib/tv/*.sh`).


## Dependencies:

You will need adb.
You will need to have enabled developer mode on your tv.

For screen mirroring scrcpy is required.


## Install

Download from the [releases page](https://github.com/stuaxo/tvbee/releases):

- **Debian / Ubuntu**: `sudo apt install ./tvbee_<version>-1_amd64.deb`, which
  also pulls in `adb` (and suggests `scrcpy` for `tvbee mirror`).
- **Other Linux**: unpack `tvbee-<version>-x86_64-unknown-linux-gnu.tar.gz` and
  put `tvbee` on your PATH; install `adb` from your distribution.
- **Windows**: unzip `tvbee-<version>-x86_64-pc-windows-msvc.zip`, and install
  Google's [platform-tools](https://developer.android.com/tools/releases/platform-tools)
  so that `adb.exe` is on your PATH.

The release builds include the full-screen remote (`tvbee ui`). From source:

    cargo install --git https://github.com/stuaxo/tvbee              # plain
    cargo install --git https://github.com/stuaxo/tvbee --features ui

The plain build has no dependencies and builds with Debian 13's packaged Rust
(`apt install cargo`); `--features ui` needs Rust 1.88 or newer.

On the TV, turn on network debugging (Developer options). `tvbee findtv` finds
it on the network; point tvbee at it with `TV_IP` (see below) and accept the
prompt on the TV the first time. `tvbee diagnose` checks the whole setup:
adb (on PATH, `$ADB`, or under `$ANDROID_HOME` / `$ANDROID_SDK_ROOT`),
scrcpy, `TV_IP`, and the connection.

On Windows a few things are more limited: `status --watch` and `nowplaying
--watch` need a Unix terminal (use `tvbee ui` instead), `logs <pattern>`
matches plain text rather than a regex unless `grep` is installed, and `time`
compares the clocks without checking the time zone.

## Compatibility

Most commands use plain Android TV / ADB features and should work on any
Android TV with network debugging on. Some parts are Philips-specific:

- the `philips` plugin (the JointSpace API), enabled only when detected
- the iPlayer, Arte and Dailymotion aliases (Philips NetTV web apps), and the
  `livetv` alias (Philips' TV app)
- input switching by keycode (`tvbee input hdmi1`), which depends on the
  TV's firmware

Reports from other TVs are welcome.

## Commands

A few examples:

    tvbee findtv                      # look for TVs on the network
    tvbee diagnose                    # check adb, scrcpy, TV_IP and the connection
    tvbee np                          # what's on: app, title, progress bar, link
    tvbee status --watch              # the same plus power, volume, memory, storage; live
    tvbee launch youtube
    tvbee search youtube cat videos   # or: tvbee search livetv news
    tvbee open https://youtu.be/...   # links go to the app that claims them
    tvbee vol 20
    tvbee seek 50%
    tvbee ui                          # full-screen remote (needs the ui feature)
    tvbee debloat apply --dry-run     # what the debloat profile would change

Every command takes `--help`. The full list, from `tvbee help`:

<!-- commands:start (checked by a test: cargo test readme) -->
```text
Usage: tvbee <command> [args]

Type 'tvbee <command> --help' (or 'tvbee help <command>') for help on a command.

[apps]
    apps         list app aliases installed on the TV
    currentapp   show the app currently in the foreground
    foreground   bring an app's existing task to the front (alias: fg)
    free         force-stop idle RAM-hungry apps
    kill         force-stop one app
    launch       launch an app
    open         open a link in the app that handles it
    quit         quit the current app
    search       search inside an app (YouTube, Kodi, live TV, ...)

[debloat]
    debloat      disable unwanted system apps (reversible)

[diagnose]
    diagnose     check adb, scrcpy, TV_IP and the connection to the TV
    findtv       look for Android / Google TVs on the network

[display]
    mirror       show/control the screen on this PC with scrcpy (alias: scrcpy)
    screenshot   save a PNG of the screen

[info]
    logs         recent logcat, optionally filtered
    now          print the foreground window only
    nowplaying   what's on: app, title, progress bar and link (alias: np)
    status       power state, foreground app, mute and free RAM
    time         show the TV's clock and compare it with this PC

[input]
    back         press the back button
    channel      change TV channel (alias: ch)
    home         go to the home screen
    input        switch input source
    inputs       list the TV's input sources
    key          send any remote key
    text         type text into the focused field

[media]
    ffw          fast-forward by an amount
    mute         mute (no-op if already muted)
    pause        media pause
    play         media play
    playpause    toggle play/pause
    rew          rewind by an amount
    seek         jump to a position (time or percent of the way through)
    stop         media stop
    unmute       unmute (no-op if not muted)
    vol          change volume

[power]
    reboot       reboot the TV
    shutdown     power off completely (not standby)
    sleep        send the TV to standby
    wake         wake from standby

[tools]
    commands     list all command names and aliases
    completion   print a bash/zsh completion script
    help         show this help, or help for one command
    plugins      list plugins and whether they are enabled
    wait         wait until the TV is reachable and booted

Env: TV_IP (default 192.168.1.19); also read from ./.env or ~/.config/tvbee/env
```
<!-- commands:end -->

Not listed above: `ui` (built with `--features ui`) and `jointspace` (from the
`philips` plugin, on TVs where it's detected).

## Plugins

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

Environment: `TVBEE_PLUGINS`, `TV_IP` (default 192.168.1.19).

These can also go in a `.env` file in the current directory, or in
`~/.config/tvbee/env` for every directory (see `.env.example`). Settings
already in the environment win, and only `TV_*` / `TVBEE_*` keys are read.

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

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).
