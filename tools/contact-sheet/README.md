# Contact sheets and the demo GIF

Screenshots of `tveee ui` at different terminal sizes, for the README and
for checking how screens look. Two steps:

1. `record.py` runs `tveee ui` in a pseudo-terminal of a given size, presses
   the keys of a script, and saves an asciicast with a marker wherever a
   shot is wanted.
2. `render.py` draws the marked screens as a contact sheet (PNG), or the
   whole recording as an animated GIF.

Needs Python 3, Pillow (`python3-pil`) and the DejaVu fonts
(`fonts-dejavu-core`), and a TV: the screens show what the TV is doing.
The scripts only open panes and move around; they never play anything.
Mind the sound all the same: whatever is already playing keeps playing.

## The sheets

```sh
cargo build --release --features ui
T=target/release/tveee
R=tools/contact-sheet/record.py
P=tools/contact-sheet/render.py

# wide (a desktop terminal), 4 shots a row
python3 $R $T /tmp/wide.cast 120 34 wide && python3 $P /tmp/wide.cast docs/tveee-sheet-wide.png 4
# narrow (a corner pane)
python3 $R $T /tmp/narrow.cast 60 22 narrow && python3 $P /tmp/narrow.cast docs/tveee-sheet-narrow.png 5
# tall (a portrait or half-screen window)
python3 $R $T /tmp/tall.cast 80 60 tall && python3 $P /tmp/tall.cast docs/tveee-sheet-tall.png 5
```

Shots added later can go under a sheet already made (same width): give the
number of the first new shot and the old sheet, e.g.
`python3 $P /tmp/extra-wide.cast docs/tveee-sheet-wide.png 4 17 docs/tveee-sheet-wide.png`
(scripts `extra-wide`, `extra-narrow`).

Other scripts: `music` (Kodi already playing music: the visualiser),
`crumbs` and `location` (Kodi's list pane), `help`.

## The GIF

```sh
python3 $R $T /tmp/demo.cast 120 34 wide && python3 $P /tmp/demo.cast docs/tveee-ui.gif --gif 2
```

`--gif FPS` samples the recording that many times a second (2 by
default); frames that don't change are merged.

## Adding a script

Scripts are lists in `record.py`: `(seconds to wait, keys, note)`, where
`keys` is `S` (take a shot, with `note` as its caption), text to send, or
`""` (just wait). Add it to `SCRIPTS` by name.
