# tveee

🔴🟠🟡 Warning slopcoded - made to operate my own TV YMMV  🟡🟠🔴

This is a rust port of [tv.sh](https://github.com/stuaxo/tv.sh) with more functionality.

Formerly called tvbee.


Command-line remote for Android TV over ADB. So far it has only been tested
on a Philips Android TV (Android 8). A Rust port of the `tv` bash script
(`~/.local/bin/tv` + `~/.local/lib/tv/*.sh`).

![tveee ui: live TV, the programme guide, options, help, the system panel and launcher, then searching YouTube and playing the video it finds](docs/tveee-ui.gif)

*`tveee ui`: live TV (channel, programme, what's next), the guide (sliding
through time, programme details, filtering, the Favourites and Radio tabs),
options, help, the system panel and the launcher, then searching YouTube for
a video, playing it in remote mode, and seeking.*


## Dependencies:

You will need adb.
You will need to have enabled developer mode on your tv.

For screen mirroring scrcpy is required.


## Install

Download from the [releases page](https://github.com/stuaxo/tveee/releases):

- **Debian / Ubuntu / Raspberry Pi OS**: `sudo apt install
  ./tveee_<version>-1_<arch>.deb` (`amd64`, `arm64`, or `armhf` for 32-bit,
  which runs on every Raspberry Pi), which also pulls in `adb` (and suggests
  `scrcpy` for `tveee mirror`).
- **Other Linux** (any distribution, a NAS such as a Synology, ARM boards):
  unpack `tveee-<version>-<target>.tar.gz` and put `tveee` on your PATH. The
  Linux builds are static (musl), so they don't depend on the system's C
  library. Targets: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`
  (64-bit ARM), `arm-unknown-linux-musleabihf` (32-bit ARM, any Pi) and
  `armv7-unknown-linux-musleabihf` (32-bit ARMv7 and later). tveee still
  needs `adb` (and `curl` for the Philips JointSpace commands) from the system.
- **Windows**: unzip `tveee-<version>-x86_64-pc-windows-msvc.zip`, and install
  Google's [platform-tools](https://developer.android.com/tools/releases/platform-tools)
  so that `adb.exe` is on your PATH.

The release builds include the full-screen remote (`tveee ui`). From source:

    cargo install --git https://github.com/stuaxo/tveee              # plain
    cargo install --git https://github.com/stuaxo/tveee --features ui

The plain build works with Debian 13's packaged Rust (`apt install cargo
build-essential`: a C compiler builds the bundled SQLite that tveee caches the
TV's guide in); `--features ui` needs Rust 1.88 or newer.

On the TV, turn on network debugging (Developer options). `tveee findtv` finds
it on the network; point tveee at it with `TV_IP` (see below) and accept the
prompt on the TV the first time. `tveee diagnose` checks the whole setup:
adb (on PATH, `$ADB`, or under `$ANDROID_HOME` / `$ANDROID_SDK_ROOT`),
scrcpy, `TV_IP`, and the connection.

On Windows a few things are more limited: `status --watch` and `nowplaying
--watch` need a Unix terminal (use `tveee ui` instead), `logs <pattern>`
matches plain text rather than a regex unless `grep` is installed, and `time`
compares the clocks without checking the time zone.

## Compatibility

Most commands use plain Android TV / ADB features and should work on any
Android TV with network debugging on. Some parts are Philips-specific:

- the `philips` plugin (the JointSpace API), enabled only when detected
- the iPlayer, Arte and Dailymotion aliases (Philips NetTV web apps), and the
  `livetv` alias (Philips' TV app)
- input switching by keycode (`tveee input hdmi1`), which depends on the
  TV's firmware

Reports from other TVs are welcome.

## Commands

A few examples:

    tveee findtv                      # look for TVs on the network
    tveee diagnose                    # check adb, scrcpy, TV_IP and the connection
    tveee np                          # what's on: app, title, progress bar, link
    tveee details                     # channel, chapters, description (YouTube, via yt-dlp)
    tveee goto history                # a place in the app; 'tveee goto' lists them
    tveee goto chapter 3              # seek to a chapter of the video
    tveee guide                       # now/next on live TV (from the over-the-air guide)
    tveee guide bbc two               # one channel's programmes today
    tveee watch more 4                # switch live TV by channel number or name
    tveee fav add more 4              # add a favourite (synced with the TV)
    tveee status --watch              # the same plus power, volume, memory, storage; live
    tveee launch youtube
    tveee home --set projectivy       # choose the default home app (see --list)
    tveee search youtube cat videos   # or: tveee search livetv news
    tveee open https://youtu.be/...   # links go to the app that claims them
    tveee vol 20
    tveee seek 50%
    tveee ui                          # full-screen remote (needs the ui feature)
    tveee debloat apply --dry-run     # what the debloat profile would change

Every command takes `--help`. The full list, from `tveee help`:

<!-- commands:start (checked by a test: cargo test readme) -->
```text
Usage: tveee <command> [args]

Type 'tveee <command> --help' (or 'tveee help <command>') for help on a command.

[apps]
    app          what tveee can do with an app, and app-specific actions
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

[guide]
    channels     channels you can watch, with what's on
    fav          favourite channels: list, add, remove
    guide        programme guide: now/next, or one channel's schedule
    onnow        the channel live TV is on, and what's on it now and next
    providers    content providers and what each can do
    watch        switch live TV to a channel, by number or name

[info]
    details      more about what's on: channel, chapters, description
    goto         go to a place in an app, or a link from what's on
    logs         recent logcat, optionally filtered
    now          print the foreground window only
    nowplaying   what's on: app, title, progress bar and link (alias: np)
    status       power state, foreground app, mute and free RAM
    time         show the TV's clock and compare it with this PC

[input]
    back         press the back button
    channel      change TV channel (alias: ch)
    home         go to the home screen, or choose the home app
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

[options]
    options      tveee's own settings: list them, or set one

[pairing]
    pair         pair tveee with a service on the TV
    pairings     services tveee can pair with, and whether it has
    unpair       forget a saved pairing

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

Env: TV_IP (default 192.168.1.19); also read from ./.env or ~/.config/tveee/env
```
<!-- commands:end -->

Not listed above: `ui` (built with `--features ui`), and commands from plugins
that are enabled when the TV has what they're for: `jointspace` (`philips`). Those plugins
also add their apps: iPlayer, Arte, Dailymotion and live TV (`philips`),
YouTube (`youtube`).

## Plugins

Commands are grouped by plugin in `tveee help`, Django-style. A plugin is a
module in `src/plugins/` exporting `commands()`, registered in
`src/plugins/mod.rs`; it can also add apps (`with_apps`) and ui menu items
(`with_menu`). Built-in plugins are always on. Vendor plugins (e.g.
`philips`) carry a detection check:

- `TVEEE_PLUGINS=philips,-other` forces plugins on/off
- otherwise the per-TV cache (`~/.cache/tveee/<ip>.plugins`) is used
- otherwise the TV is probed, but only when one of that plugin's commands was
  asked for (or `tveee plugins --refresh`); `help` never contacts the TV

`tveee plugins` shows what is enabled and why.

    tveee help            # all commands by category
    tveee help launch     # one command
    tveee launch --quiet iplayer
    tveee apps --all      # offline alias list

Environment: `TVEEE_PLUGINS`, `TV_IP` (default 192.168.1.19).

These can also go in a `.env` file in the current directory, or in
`~/.config/tveee/env` for every directory (see `.env.example`). Settings
already in the environment win, and only `TV_*` / `TVEEE_*` keys are read.

Needs `adb` in PATH (and `scrcpy` for `mirror`). The default build uses no
crates beyond std.

## Optional features

- `ui`: `tveee ui`, a full-screen remote (what's on, progress, volume; keys for
  play/pause, seeking and volume). Pulls in `ratatui`/`crossterm`.

      cargo install --path . --features ui

  Without it, `tveee ui` says how to get it. Optional plugins are compiled in
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
