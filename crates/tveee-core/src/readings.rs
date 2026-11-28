//! Readings from the TV: a shell command and what its answer means, in one
//! place (see the plan's part 7). Each reading asks only for the lines it
//! needs (`grep`): `dumpsys window` is ~23 KB, the lines read from it ~0.6 KB;
//! `dumpsys power` ~6 KB, its line 21 bytes.
//!
//! A busy TV sometimes answers `dumpsys` with nothing, or with a dump cut
//! short: a reading says when an answer isn't a whole one, and it's asked
//! for again ([`Tv::read`]); where something keeps going (the refresh), the
//! last whole one stands in ([`Kept`]).
//!
//! A reading is the simplest kind of [`crate::ask::Ask`]: a shell command
//! with no parameters, and its answer's type. [`Of`] is it as an `Ask`
//! (to batch it with [`Tv::ask_both`], say).

use crate::ask::{Ask, Cmd, Reply, Request};
use crate::error::{Result, failed};
use crate::tv::Tv;

/// A reading from the TV: the command, and what its answer means.
pub trait Reading: Sized {
    /// What's run in the TV's shell.
    fn cmd() -> Cmd;
    /// What it's called, in messages.
    const NAME: &'static str;
    /// The answer, or None when it isn't a whole one (empty, cut short).
    fn parse(out: &str) -> Option<Self>;
}

/// Reading `R`, as something to [`Tv::ask`].
pub struct Of<R>(std::marker::PhantomData<R>);

impl<R> Of<R> {
    pub fn new() -> Of<R> {
        Of(std::marker::PhantomData)
    }
}

impl<R> Default for Of<R> {
    fn default() -> Self {
        Of::new()
    }
}

impl<R: Reading> Ask for Of<R> {
    type Answer = R;
    fn name(&self) -> String {
        R::NAME.to_string()
    }
    fn request(&self, _: &Tv) -> Result<Request> {
        Ok(Request::Shell(R::cmd()))
    }
    fn parse(&self, reply: &Reply) -> Result<Option<R>> {
        Ok(R::parse(&reply.body))
    }
}

impl Tv {
    /// Read `R`, asking again once if the answer isn't a whole one; Ok(None)
    /// if it still isn't (an adb failure is an error).
    pub fn read_whole<R: Reading>(&self) -> Result<Option<R>> {
        self.ask_whole(&Of::<R>::new())
    }

    /// Read `R`; an error if the TV doesn't give a whole answer.
    pub fn read<R: Reading>(&self) -> Result<R> {
        match self.read_whole()? {
            Some(r) => Ok(r),
            None => failed(format!("the TV didn't answer {} (twice)", R::NAME)),
        }
    }
}

/// The last whole reading, standing in for one the TV didn't answer: for
/// something that keeps reading (the refresh), where "no answer" isn't
/// "nothing" (standby, nothing in front).
#[derive(Debug)]
pub struct Kept<R> {
    last: Option<R>,
}

impl<R> Default for Kept<R> {
    fn default() -> Self {
        Kept { last: None }
    }
}

impl<R: Reading + Clone> Kept<R> {
    /// What [`Tv::read_whole`] gave, kept if whole; else the last whole one
    /// (an error if there's never been one).
    pub fn settle(&mut self, got: Result<Option<R>>) -> Result<R> {
        match got? {
            Some(r) => {
                self.last = Some(r.clone());
                Ok(r)
            }
            None => match &self.last {
                Some(r) => {
                    crate::tlog!("{}: using the last whole answer", R::NAME);
                    Ok(r.clone())
                }
                None => failed(format!("the TV didn't answer {}", R::NAME)),
            },
        }
    }
}

/// Whether the TV is awake, asleep or dreaming (`dumpsys power`).
#[derive(Debug, Clone, PartialEq)]
pub struct Power {
    /// "Awake", "Asleep", "Dreaming", "Dozing".
    pub wakefulness: String,
}

impl Power {
    pub fn awake(&self) -> bool {
        self.wakefulness == "Awake"
    }
}

impl Reading for Power {
    fn cmd() -> Cmd {
        Cmd::new("dumpsys").arg("power").grep("mWakefulness=")
    }
    const NAME: &'static str = "power";
    fn parse(out: &str) -> Option<Power> {
        // mWakefulness=Awake (and on newer Android mWakefulnessChanging=...)
        let w = out.lines().find_map(|l| l.trim().strip_prefix("mWakefulness="))?;
        (!w.is_empty()).then(|| Power { wakefulness: w.to_string() })
    }
}

/// What's in front, and which uid owns each app's windows (`dumpsys window`,
/// only those lines).
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    /// The focused window: `org.x/org.x.Activity` (None: nothing has focus,
    /// as while the TV starts).
    pub focus: Option<String>,
    /// The lines read, for [`Window::uid`].
    pub text: String,
}

impl Window {
    /// The package in front.
    pub fn package(&self) -> Option<String> {
        let pkg = self.focus.as_deref()?.split(['/', ' ']).next()?;
        (!pkg.is_empty()).then(|| pkg.to_string())
    }

    /// The uid owning `pkg`'s windows.
    pub fn uid(&self, pkg: &str) -> Option<u32> {
        crate::builtin::info::window_uid(&self.text, pkg)
    }
}

impl Reading for Window {
    fn cmd() -> Cmd {
        Cmd::new("dumpsys").arg("window").grep_e("mCurrentFocus=|mOwnerUid=")
    }
    const NAME: &'static str = "window";
    fn parse(out: &str) -> Option<Window> {
        // whole: it says what has focus (null while nothing has)
        out.contains("mCurrentFocus=").then(|| Window { focus: crate::builtin::info::focus_window(out), text: out.to_string() })
    }
}

/// The audio service's dump (`dumpsys audio`): the music stream's volume
/// and mute, and which apps are playing.
#[derive(Debug, Clone, PartialEq)]
pub struct Audio {
    /// The dump, for what reads it whole.
    pub text: String,
}

impl Audio {
    /// (current, max) music volume for the output in use.
    pub fn volume(&self) -> Option<(u32, u32)> {
        crate::builtin::media::parse_volume(&self.text)
    }
    /// Whether the music stream is muted (None: it doesn't say).
    pub fn muted(&self) -> Option<bool> {
        crate::builtin::media::parse_muted(&self.text)
    }
    /// The apps playing audio: (uid, state).
    pub fn players(&self) -> Vec<(u32, String)> {
        crate::builtin::media::parse_audio_players(&self.text)
    }
}

impl Reading for Audio {
    fn cmd() -> Cmd {
        Cmd::new("dumpsys").arg("audio")
    }
    const NAME: &'static str = "audio";
    fn parse(out: &str) -> Option<Audio> {
        // the service always has something to say: nothing is the busy TV
        (!out.trim().is_empty()).then(|| Audio { text: out.to_string() })
    }
}

/// The media sessions (`dumpsys media_session`): what apps say they play.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaSessions {
    /// The dump, for what reads it whole.
    pub text: String,
}

impl MediaSessions {
    pub fn sessions(&self) -> Vec<crate::builtin::media::MediaInfo> {
        crate::builtin::media::parse_media_sessions(&self.text)
    }
}

impl Reading for MediaSessions {
    fn cmd() -> Cmd {
        Cmd::new("dumpsys").arg("media_session")
    }
    const NAME: &'static str = "media sessions";
    fn parse(out: &str) -> Option<MediaSessions> {
        (!out.trim().is_empty()).then(|| MediaSessions { text: out.to_string() })
    }
}

/// The system's figures (memory, load, uptime, storage, cores), in one
/// shell call.
impl Reading for crate::builtin::info::System {
    fn cmd() -> Cmd {
        Cmd::raw(crate::builtin::info::SYSTEM_CMD)
    }
    const NAME: &'static str = "system";
    fn parse(out: &str) -> Option<Self> {
        // whole: the memory part, at least
        let s = crate::builtin::info::parse_system(out);
        s.mem.is_some().then_some(s)
    }
}

impl Tv {
    /// `dumpsys audio`'s text (empty if the TV didn't give a whole one).
    pub fn audio_text(&self) -> Result<String> {
        Ok(self.read_whole::<Audio>()?.map(|a| a.text).unwrap_or_default())
    }
    /// `dumpsys media_session`'s text (empty if the TV didn't give a whole one).
    pub fn media_text(&self) -> Result<String> {
        Ok(self.read_whole::<MediaSessions>()?.map(|m| m.text).unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: &str = "    mOwnerUid=10075 mShowToOwnerOnly=true package=com.google.android.youtube.tv appop=NONE\n    mOwnerUid=1000 mShowToOwnerOnly=true package=org.droidtv.zapster appop=NONE\n  mCurrentFocus=Window{97e228d u0 com.google.android.youtube.tv/com.google.android.apps.youtube.tv.activity.MainActivity}\n";

    #[test]
    fn power() {
        let p = Power::parse("  mWakefulness=Awake\n  mWakefulnessChanging=false\n").unwrap();
        assert!(p.awake());
        assert!(!Power::parse("mWakefulness=Asleep\n").unwrap().awake());
        // the busy-TV answers: not whole
        assert_eq!(Power::parse(""), None);
        assert_eq!(Power::parse("mWakefulness=\n"), None);
    }

    #[test]
    fn window() {
        let w = Window::parse(WINDOW).unwrap();
        assert_eq!(w.package().as_deref(), Some("com.google.android.youtube.tv"));
        assert_eq!(w.uid("org.droidtv.zapster"), Some(1000));
        assert_eq!(w.uid("com.google.android.youtube.tv"), Some(10075));
        // nothing in focus yet is an answer; an empty one isn't
        assert_eq!(Window::parse("  mCurrentFocus=null\n").unwrap().package(), None);
        assert_eq!(Window::parse(""), None);
    }

    #[test]
    fn audio_sessions_system() {
        // an empty answer (the busy TV) isn't a whole one
        assert_eq!(Audio::parse("  \n"), None);
        assert_eq!(MediaSessions::parse(""), None);
        let a = Audio::parse("- STREAM_MUSIC:\n   Muted: true\n   Max: 60\n   Current: 2 (speaker): 30\n   Devices: speaker\n").unwrap();
        assert_eq!((a.muted(), a.volume()), (Some(true), Some((30, 60))));
        assert_eq!(<crate::builtin::info::System as Reading>::parse(""), None);
        assert_eq!(<crate::builtin::info::System as Reading>::cmd().render(), crate::builtin::info::SYSTEM_CMD);
    }

    #[test]
    fn kept_stands_in() {
        let mut k = Kept::<Power>::default();
        // never answered: an error
        assert!(k.settle(Ok(None)).is_err());
        let awake = Power { wakefulness: "Awake".into() };
        assert_eq!(k.settle(Ok(Some(awake.clone()))).unwrap(), awake);
        // a busy-TV answer: the last whole one
        assert_eq!(k.settle(Ok(None)).unwrap(), awake);
        // adb failing is still an error
        assert!(k.settle(crate::error::failed("adb gone")).is_err());
    }
}
