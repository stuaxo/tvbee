use crate::actions::{ActionDescriptor as A, Cx, Output};
use crate::error::Result;

const CAT: &str = "power";

fn sleep(cx: &Cx, _: &[String]) -> Result<Output> {
    cx.tv.keyevent(223)?;
    Ok("sent the TV to standby".into())
}

/// What the TV says: awake or not. None when it doesn't say (dumpsys
/// sometimes answers with nothing; asked twice).
pub fn awake(tv: &crate::tv::Tv) -> Result<Option<bool>> {
    Ok(tv.read_whole::<crate::readings::Power>()?.map(|p| p.awake()))
}

/// Up to `ms` for the TV to say it's awake.
fn wait_awake(tv: &crate::tv::Tv, ms: u64) -> Result<bool> {
    let until = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    loop {
        if awake(tv)? == Some(true) {
            return Ok(true);
        }
        if std::time::Instant::now() >= until {
            return Ok(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
}

/// The wake key, checked: on some TVs (a Philips, seen live) it does
/// nothing from standby while the power key works. The power key toggles,
/// so it's only sent while the TV says it's asleep.
fn wake(cx: &Cx, _: &[String]) -> Result<Output> {
    let tv = cx.tv;
    if awake(tv)? == Some(true) {
        return Ok("the TV is already awake".into());
    }
    tv.keyevent(224)?;
    if wait_awake(tv, 3000)? {
        return Ok("woke the TV".into());
    }
    if awake(tv)? != Some(false) {
        return crate::error::failed("sent the wake key; the TV doesn't say whether it woke");
    }
    crate::tlog!("wake: still asleep after the wake key; the power key");
    tv.keyevent(26)?;
    if wait_awake(tv, 6000)? {
        return Ok("woke the TV (with the power key)".into());
    }
    crate::error::failed("sent the wake and power keys, but the TV still says it's asleep")
}

fn reboot(cx: &Cx, _: &[String]) -> Result<Output> {
    cx.tv.adb(&["reboot"])?;
    Ok("rebooting; the TV will be back in about a minute".into())
}

fn power_off(cx: &Cx, _: &[String]) -> Result<Output> {
    cx.tv.shell(&["reboot", "-p"])?;
    Ok("powering off".into())
}

pub fn actions() -> Vec<A> {
    vec![
        A::new("power.sleep", CAT, "sleep (standby)", sleep).words(&["standby", "off"]).cli("sleep", "", "send the TV to standby").key('s'),
        A::new("power.wake", CAT, "wake", wake).words(&["on", "standby"]).cli("wake", "", "wake from standby").key('w'),
        A::new("power.reboot", CAT, "reboot", reboot)
            .cli("reboot", "", "reboot the TV")
            .key('R')
            .confirm("Reboot the TV? It will be unreachable for about a minute."),
        A::new("power.off", CAT, "power off", power_off)
            .words(&["shutdown", "shut"])
            .cli("shutdown", "", "power off completely (not standby)")
            .key('X')
            .confirm("Power off the TV? It can't be woken over the network afterwards;\nyou'll need the remote or the TV's power button."),
    ]
}
