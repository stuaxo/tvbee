//! Readings from the TV: a shell command and what its answer means, in one
//! place (see the plan's part 7). Each reading asks only for the lines it
//! needs (`grep`): `dumpsys window` is ~23 KB, the lines read from it ~0.6 KB;
//! `dumpsys power` ~6 KB, its line 21 bytes.
//!
//! A busy TV sometimes answers `dumpsys` with nothing, or with a dump cut
//! short: a reading says when an answer isn't a whole one, and it's asked
//! for again ([`Tv::read`]); where something keeps going (the refresh), the
//! last whole one stands in ([`Kept`]).

use crate::error::{Result, failed};
use crate::tv::Tv;

/// A reading from the TV: the command, and what its answer means.
pub trait Reading: Sized {
    /// What's run in the TV's shell (a pipeline is fine).
    const CMD: &'static str;
    /// What it's called, in messages.
    const NAME: &'static str;
    /// The answer, or None when it isn't a whole one (empty, cut short).
    fn parse(out: &str) -> Option<Self>;
}

impl Tv {
    /// Read `R`, asking again once if the answer isn't a whole one; Ok(None)
    /// if it still isn't (an adb failure is an error).
    pub fn read_whole<R: Reading>(&self) -> Result<Option<R>> {
        for tries in 0..2 {
            let out = self.shell(&[R::CMD])?;
            if let Some(r) = R::parse(&out) {
                return Ok(Some(r));
            }
            crate::tlog!("{}: not a whole answer ({} bytes){}", R::NAME, out.len(), if tries == 0 { "; asking again" } else { "" });
        }
        Ok(None)
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
    const CMD: &'static str = "dumpsys power | grep mWakefulness=";
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
    const CMD: &'static str = "dumpsys window | grep -E 'mCurrentFocus=|mOwnerUid='";
    const NAME: &'static str = "window";
    fn parse(out: &str) -> Option<Window> {
        // whole: it says what has focus (null while nothing has)
        out.contains("mCurrentFocus=").then(|| Window { focus: crate::builtin::info::focus_window(out), text: out.to_string() })
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
