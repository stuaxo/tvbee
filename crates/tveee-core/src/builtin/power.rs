use crate::actions::{ActionDescriptor as A, Cx, Output};
use crate::error::Result;

const CAT: &str = "power";

fn sleep(cx: &Cx, _: &[String]) -> Result<Output> {
    cx.tv.keyevent(223)?;
    Ok("sent the TV to standby".into())
}

fn wake(cx: &Cx, _: &[String]) -> Result<Output> {
    cx.tv.keyevent(224)?;
    Ok("woke the TV".into())
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
        A::new("power.sleep", CAT, "sleep (standby)", sleep).cli("sleep", "", "send the TV to standby").key('s'),
        A::new("power.wake", CAT, "wake", wake).cli("wake", "", "wake from standby").key('w'),
        A::new("power.reboot", CAT, "reboot", reboot)
            .cli("reboot", "", "reboot the TV")
            .key('R')
            .confirm("Reboot the TV? It will be unreachable for about a minute."),
        A::new("power.off", CAT, "power off", power_off)
            .cli("shutdown", "", "power off completely (not standby)")
            .key('X')
            .confirm(
                "Power off the TV? It can't be woken over the network afterwards;\nyou'll need the remote or the TV's power button.",
            ),
    ]
}
