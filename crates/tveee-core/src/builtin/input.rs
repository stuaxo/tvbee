use crate::Ctx;
use crate::actions::{ActionDescriptor as A, Cx, Output};
use crate::error::{Result, usage};

const CAT: &str = "input";

const KEYS: &[(&str, u32)] = &[
    ("up", 19),
    ("down", 20),
    ("left", 21),
    ("right", 22),
    ("ok", 23),
    ("enter", 66),
    ("back", 4),
    ("home", 3),
    ("menu", 82),
    ("info", 165),
    ("guide", 172),
    ("power", 26),
    ("wake", 224),
    ("sleep", 223),
    ("play", 126),
    ("pause", 127),
    ("stop", 86),
    ("playpause", 85),
    ("next", 87),
    ("prev", 88),
    ("ff", 90),
    ("rw", 89),
    ("rew", 89),
    ("mute", 164),
    ("volup", 24),
    ("voldown", 25),
    ("chup", 166),
    ("chdown", 167),
    ("red", 183),
    ("green", 184),
    ("yellow", 185),
    ("blue", 186),
];

/// input source name -> keycode
const INPUTS: &[(&str, u32)] = &[("hdmi1", 243), ("hdmi2", 244), ("hdmi3", 245), ("hdmi4", 246), ("tv", 170)];

fn lookup(table: &[(&str, u32)], name: &str) -> Option<u32> {
    table.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

/// A key name from the table, or a raw numeric Android keycode.
pub fn key_code(k: &str) -> Option<u32> {
    lookup(KEYS, k).or_else(|| k.parse().ok())
}

/// `input text` wants `%s` for spaces; single-quote the result for the device shell.
pub fn escape_text(t: &str) -> String {
    format!("'{}'", t.replace(' ', "%s").replace('\'', "'\\''"))
}

/// Keycodes that type a channel number then press Enter (KEYCODE_0 is 7).
pub fn channel_keys(n: &str) -> Vec<u32> {
    n.chars().filter_map(|c| c.to_digit(10)).map(|d| 7 + d).chain([66]).collect()
}

fn key(c: &Cx, args: &[String]) -> Result<Output> {
    let Some(k) = args.first() else { return usage("key: missing key") };
    match key_code(k) {
        Some(code) => c.tv.key(code).map(|()| format!("pressed {k}").into()),
        None => usage(format!("key: unknown key '{k}'")),
    }
}

fn text(c: &Cx, args: &[String]) -> Result<Output> {
    if args.is_empty() {
        return usage("text: missing text");
    }
    crate::keys::type_text(c.tv, &args.join(" "))?;
    Ok("typed it".into())
}

fn input(c: &Cx, args: &[String]) -> Result<Output> {
    match args.first().and_then(|n| lookup(INPUTS, n).map(|code| (n, code))) {
        Some((n, code)) => c.tv.key(code).map(|()| format!("switched to {n}").into()),
        None => {
            let mut names: Vec<_> = INPUTS.iter().map(|(n, _)| *n).collect();
            names.sort();
            usage(format!("usage: tveee input {}", names.join("|")))
        }
    }
}

fn supported_inputs() {
    let mut all: Vec<_> = INPUTS.to_vec();
    all.sort();
    for (n, code) in all {
        println!("{n:<8} keycode {code}");
    }
}

fn inputs(c: &Ctx, args: &[String]) -> Result<()> {
    let all = match args.first().map(String::as_str) {
        None => false,
        Some("--all") => true,
        Some(_) => return usage("usage: tveee inputs [--all]"),
    };
    if !all {
        // what the TV's own input framework reports (format varies by firmware)
        let dump = c.tv.shell(&["dumpsys", "tv_input"]).unwrap_or_default();
        let found: Vec<String> = dump
            .lines()
            .filter(|l| {
                let l = l.to_lowercase();
                ["inputid", "input_id", "hdmi", "tuner", "passthrough"].iter().any(|k| l.contains(k))
            })
            .map(|l| format!("  {}", l.trim_start()))
            .collect();
        if !found.is_empty() {
            println!("{}", found.join("\n"));
            return Ok(());
        }
        eprintln!("tveee inputs: nothing from the TV, showing the inputs this program supports");
    }
    supported_inputs();
    Ok(())
}

/// What `channel` brings to the front: its keys only change channel in the
/// TV's own live TV app.
const LIVE_TV: crate::builtin::apps::Front = crate::builtin::apps::Front::Kind(crate::builtin::apps::Kind::LiveTv);

fn channel(c: &Cx, args: &[String]) -> Result<Output> {
    let arg = args.first().map(String::as_str);
    let number = arg.filter(|n| !n.is_empty() && n.chars().all(|ch| ch.is_ascii_digit()));
    if !matches!(arg, Some("up" | "down")) && number.is_none() {
        return usage("usage: tveee channel up|down|<number>");
    }
    let first = if c.front { LIVE_TV.bring(c.tv)? } else { String::new() };
    let said = match (arg, number) {
        (Some("up"), _) => c.tv.key(166).map(|()| "channel up".to_string())?,
        (Some("down"), _) => c.tv.key(167).map(|()| "channel down".to_string())?,
        // a number: tuned exactly where a provider can (as 'watch'), else typed
        (_, Some(n)) => {
            let p = crate::provider::all().iter().copied().find(|p| p.channels().is_some());
            let cx = crate::provider::Cx { tv: c.tv };
            let tuned = p.and_then(|p| {
                let chans = p.channels()?;
                let list = chans.list(&cx).ok()?;
                let ch = list.iter().find(|ch| ch.number.as_deref() == Some(n))?;
                Some(chans.tune(&cx, ch))
            });
            match tuned {
                Some(r) => r?,
                None => {
                    channel_keys(n).into_iter().try_for_each(|k| c.tv.keyevent(k))?;
                    format!("channel {n}")
                }
            }
        }
        _ => unreachable!("checked above"),
    };
    Ok(format!("{first}{said}").into())
}

macro_rules! input_item {
    ($id:expr, $key:expr, $label:expr, $code:expr) => {{
        fn run(cx: &crate::actions::Cx, _: &[String]) -> Result<crate::actions::Output> {
            cx.tv.keyevent($code)?;
            Ok(format!("switched to {}", $label).into())
        }
        crate::actions::ActionDescriptor::new($id, CAT, $label, run).key($key)
    }};
}

pub fn actions() -> Vec<A> {
    vec![
        A::new("input.key", CAT, "press a remote key", key)
            .cli("key", "<name|code>", "send any remote key")
            .long("Names: up down left right ok enter back home menu info guide power wake sleep\nplay pause stop playpause next prev ff rw mute volup voldown chup chdown\nred green yellow blue, or a numeric Android keycode."),
        A::terminal("input.home", CAT, "home screen", super::apps::home)
            .cli("home", "[--list | --set <app>]", "go to the home screen, or choose the home app")
            .long(
                "With no options, presses Home.\n\
                 \x20 --list        the installed home apps (launchers); * marks the default\n\
                 \x20 --set <app>   make <app> the default home app, e.g. 'tveee home --set projectivy'\n\
                 \x20               (an alias, a package, or package/activity)\n\
                 \n\
                 With no default set, Android asks which home app to use every time Home is\n\
                 pressed (including tveee's 'home' and the ui's h key).",
            ),
        A::new("input.back", CAT, "back", |c, _| c.tv.key(4).map(|()| "back".into())).cli("back", "", "press the back button"),
        A::new("input.text", CAT, "type text", text).cli("text", "\"<string>\"", "type text into the focused field").text("Type:", "text for the focused field"),
        A::new("input.source", CAT, "switch input", input)
            .cli("input", "<name>", "switch input source")
            .choices(&[("HDMI 1", "hdmi1"), ("HDMI 2", "hdmi2"), ("HDMI 3", "hdmi3"), ("HDMI 4", "hdmi4"), ("TV (the tuner)", "tv")]),
        A::terminal("input.list", CAT, "list inputs", inputs)
            .cli("inputs", "[--all]", "list the TV's input sources")
            .long("Queries the TV (dumpsys tv_input) for the inputs it reports; best effort,\nthe format depends on the firmware.\n  --all  list the inputs 'tveee input' supports, without contacting the TV"),
        A::new("input.channel", CAT, "change channel", channel).cli("channel", "up|down|<number>", "change TV channel").aliases(&["ch"]).long(
            "up and down press the channel keys; live TV is brought to the front first\n\
             (they only change channel there). A number tunes exactly, as 'tveee watch'\n\
             does. --no-front leaves what's in front alone for up and down; a number\n\
             still shows live TV (the TV brings it forward itself when told to tune).",
        ).args("up|down|<number>").brings_to_front(LIVE_TV),
        input_item!("input.tuner", 't', "TV tuner", 170),
        input_item!("input.hdmi1", '1', "HDMI 1", 243),
        input_item!("input.hdmi2", '2', "HDMI 2", 244),
        input_item!("input.hdmi3", '3', "HDMI 3", 245),
        input_item!("input.hdmi4", '4', "HDMI 4", 246),
    ]
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn key_names_and_codes() {
        assert_eq!(key_code("ok"), Some(23));
        assert_eq!(key_code("131"), Some(131));
        assert_eq!(key_code("bogus"), None);
    }

    #[test]
    fn text_is_escaped() {
        assert_eq!(escape_text("a b"), "'a%sb'");
        assert_eq!(escape_text("it's"), "'it'\\''s'");
    }

    #[test]
    fn channel_digits_then_enter() {
        assert_eq!(channel_keys("105"), vec![8, 7, 12, 66]);
    }

    #[test]
    fn key_table_has_no_duplicates() {
        for t in [KEYS, INPUTS] {
            let mut n: Vec<_> = t.iter().map(|(n, _)| *n).collect();
            n.sort();
            n.dedup();
            assert_eq!(n.len(), t.len());
        }
    }
}
