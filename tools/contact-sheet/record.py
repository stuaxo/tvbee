#!/usr/bin/env python3
"""Run `tveee ui` in a pty of COLS x ROWS, press keys from a script, and save
an asciicast (with "m" marker events where a shot is wanted).

    record.py BIN OUT.cast COLS ROWS SCRIPT

It drives the real TV (what's on screen is what the TV is doing): it only
opens panes and moves around, it never plays anything. See README.md."""
import fcntl, json, os, pty, select, struct, sys, termios, time
BIN, OUT, COLS, ROWS, WHICH = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4]), sys.argv[5]
UP, DOWN, ESC, TAB, ENTER = "\x1b[A", "\x1b[B", "\x1b", "\t", "\r"
SDOWN, F2 = "\x1b[1;2B", "\x1bOQ"
S = None  # a shot
def typed(t): return [(0.15, c, "") for c in t]
WIDE = [
    (1.0, S, "first frame"),
    (8, S, "now playing (YouTube, paused)"),
    (0, "t", ""), (8, S, "t: YouTube's chapters and links, in parts"),
    (0, ESC, ""),
    (1, "l", ""), (5, S, "l: the Ambilight panel (beside the system panel)"),
    (0, "L", ""), (1.5, S, "L: changed in the panel (cursor, yellow border)"),
    (0, DOWN, ""), (0.5, DOWN, ""), (0.5, DOWN, ""), (0.5, DOWN, ""), (1, S, "the lights row: the chosen light framed"),
    (0, ENTER, ""), (1.5, S, "Enter on a light: the colour picker"),
    *typed("orange"), (1, S, "typing a colour: its swatch, live"),
    (0, ESC, ""), (0.8, ESC, ""), (0.8, "l", ""),
    (1, ",", ""), (1.5, S, ",: options (low bandwidth, load & memory, predict position)"),
    (0, TAB, ""), (1.5, S, "options: the Plugins tab"), (0, ESC, ""),
    (1, ":", ""), (1.5, S, ":: the command palette"), *typed("ambi"), (1, S, "the palette, typed to narrow it"), (0, ESC, ""),
    (1, ".", ""), (1.5, S, ".: the more menu"), (0, ESC, ""),
    (1, ":", ""), *typed("show the log"), (0.5, ENTER, ""), (1.5, UP, ""), (0.3, SDOWN, ""), (0.3, SDOWN, ""), (1, S, "the log: whole lines selected"),
    (0, ":", ""), (1.5, S, "the commands over the log"), (0, ESC, ""), (0.8, ESC, ""), (0.8, ESC, ""),
    (1, F2, ""), (4, S, "F2: the apps"), (0, ESC, ""),
    (1, "g", ""), (9, S, "g: the guide"), (0, ESC, ""),
    (1, TAB, ""), (2, S, "remote mode: the D-pad in the side column"), (0, TAB, ""),
    (1, "?", ""), (2, S, "?: help"), (0, ESC, ""),
    (1, "q", ""), (2, "", ""),
]
NARROW = [
    (1.0, S, "first frame"),
    (7, S, "now playing"),
    (0, "t", ""), (7, S, "t: chapters and links"), (0, ESC, ""),
    (1, "l", ""), (5, S, "l: the Ambilight, below"),
    (0, "L", ""), (0.5, DOWN, ""), (0.5, DOWN, ""), (0.5, DOWN, ""), (0.5, DOWN, ""), (1, S, "L: changed in the panel"),
    (0, ENTER, ""), (1.5, S, "the colour picker"),
    (0, ESC, ""), (0.8, ESC, ""), (0.8, "l", ""),
    (1, ",", ""), (1.5, S, ",: options"), (0, TAB, ""), (1.5, S, "the Plugins tab"), (0, ESC, ""),
    (1, ":", ""), (1.5, S, ":: the command palette"), (0, ESC, ""),
    (1, "?", ""), (2, S, "?: help"), (0, ESC, ""),
    (1, "q", ""), (2, "", ""),
]
# only the screens added later, to go under the sheets already made
EXTRA_WIDE = [
    (8, ",", ""), (1.5, S, ",: options (low bandwidth, load & memory, predict position)"),
    (0, TAB, ""), (1.5, S, "options: the Plugins tab"), (0, ESC, ""),
    (1, ":", ""), (1.5, S, ":: the command palette"), *typed("ambi"), (1, S, "the palette, typed to narrow it"), (0, ESC, ""),
    (1, ".", ""), (1.5, S, ".: the more menu"), (0, ESC, ""),
    (1, "q", ""), (2, "", ""),
]
EXTRA_NARROW = [
    (8, ",", ""), (1.5, S, ",: options"), (0, TAB, ""), (1.5, S, "the Plugins tab"), (0, ESC, ""),
    (1, ":", ""), (1.5, S, ":: the command palette"), (0, ESC, ""),
    (1, "q", ""), (2, "", ""),
]
MUSIC = [
    (8, S, "Kodi playing music: Listening, z visualiser, no subtitle buttons"),
    (0, "z", ""), (4, S, "z: the visualiser shown"),
    (0, "z", ""), (3, S, "z again: back"),
    (0, "q", ""), (2, "", ""),
]
CRUMBS = [
    (8, "t", ""), (6, S, "t on Kodi's Albums"),
    (0, DOWN, ""), (0.5, DOWN, ""), (0.5, ENTER, ""), (5, S, "an album opened: the breadcrumbs (Albums › ...)"),
    (0, "\x7f", ""), (5, S, "⌫: up again (Kodi's own ..)"),
    (0, ESC, ""), (0.5, "q", ""), (2, "", ""),
]
LOCATION = [
    (8, "t", ""), (6, S, "t: the breadcrumbs (Music › Albums › … › the album)"),
    (0, "\x0c", ""), (1.5, S, "^L: the location, to edit"),
    (0, ESC, ""), (0.8, ESC, ""), (0.5, "q", ""), (2, "", ""),
]
HELPSHOT = [(6, "?", ""), (1.5, S, "help, coloured"), (0, "q", ""), (0.5, "q", ""), (1, "", "")]
# a tall terminal (80x60, say): the narrow screens, and the ones height helps
TALL = [
    (1.0, S, "first frame"),
    (7, S, "now playing"),
    (0, "t", ""), (7, S, "t: the app's list"), (0, ESC, ""),
    (1, "l", ""), (5, S, "l: the Ambilight, below"),
    (0, "L", ""), (0.5, DOWN, ""), (0.5, DOWN, ""), (0.5, DOWN, ""), (0.5, DOWN, ""), (1, S, "L: changed in the panel"),
    (0, ENTER, ""), (1.5, S, "the colour picker"),
    (0, ESC, ""), (0.8, ESC, ""), (0.8, "l", ""),
    (1, "i", ""), (2, S, "i: the system panel"), (0, "i", ""),
    (1, ",", ""), (1.5, S, ",: options"), (0, TAB, ""), (1.5, S, "the Plugins tab"), (0, ESC, ""),
    (1, ":", ""), (1.5, S, ":: the command palette"), (0, ESC, ""),
    (1, "g", ""), (9, S, "g: the guide, tall"), (0, ESC, ""),
    (1, "?", ""), (2, S, "?: help, more of it at once"), (0, ESC, ""),
    (1, "q", ""), (2, "", ""),
]
SCRIPTS = {"wide": WIDE, "narrow": NARROW, "tall": TALL, "extra-wide": EXTRA_WIDE, "extra-narrow": EXTRA_NARROW,
           "music": MUSIC, "crumbs": CRUMBS, "location": LOCATION, "help": HELPSHOT}
if WHICH not in SCRIPTS:
    sys.exit(f"no script '{WHICH}': one of {', '.join(SCRIPTS)}")
SCRIPT = SCRIPTS[WHICH]
pid, fd = pty.fork()
if pid == 0:
    os.environ["TERM"] = "xterm-256color"; os.environ["COLORTERM"] = "truecolor"
    os.execv(BIN, [BIN, "ui", "--no-mouse"])
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
start, events = time.time(), []
def pump(t):
    end = time.time() + t
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], max(0, end - time.time()))
        if r:
            try: d = os.read(fd, 65536)
            except OSError: return
            if not d: return
            events.append([round(time.time() - start, 4), "o", d.decode("utf-8", "replace")])
for wait, keys, note in SCRIPT:
    pump(wait)
    if keys is None:
        events.append([round(time.time() - start, 4), "m", note]); print(f"{time.time()-start:6.1f}s {note}", flush=True)
    elif keys:
        os.write(fd, keys.encode())
try: os.waitpid(pid, 0)
except ChildProcessError: pass
with open(OUT, "w") as f:
    f.write(json.dumps({"version": 2, "width": COLS, "height": ROWS}) + "\n")
    for e in events: f.write(json.dumps(e) + "\n")
