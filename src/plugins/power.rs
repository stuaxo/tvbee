use crate::error::Result;
use crate::menu::MenuItem;
use crate::registry::Command;
use crate::tv::Tv;

const CAT: &str = "power";

pub fn commands() -> Vec<Command> {
    vec![
        Command::new(CAT, "sleep", "", "send the TV to standby", |c, _| c.tv.key(223)),
        Command::new(CAT, "wake", "", "wake from standby", |c, _| c.tv.key(224)),
        Command::new(CAT, "reboot", "", "reboot the TV", |c, _| {
            c.tv.connect()?;
            c.tv.adb(&["reboot"]).map(drop)
        }),
        Command::new(CAT, "shutdown", "", "power off completely (not standby)", |c, _| {
            c.tv.connect()?;
            c.tv.shell(&["reboot", "-p"]).map(drop)
        }),
    ]
}

fn sleep(tv: &Tv) -> Result<String> {
    tv.keyevent(223)?;
    Ok("sent the TV to standby".into())
}

fn wake(tv: &Tv) -> Result<String> {
    tv.keyevent(224)?;
    Ok("woke the TV".into())
}

fn reboot(tv: &Tv) -> Result<String> {
    tv.adb(&["reboot"])?;
    Ok("rebooting; the TV will be back in about a minute".into())
}

fn power_off(tv: &Tv) -> Result<String> {
    tv.shell(&["reboot", "-p"])?;
    Ok("powering off".into())
}

pub fn menu() -> Vec<MenuItem> {
    vec![
        MenuItem::new(CAT, 's', "sleep (standby)", sleep),
        MenuItem::new(CAT, 'w', "wake", wake),
        MenuItem::new(CAT, 'R', "reboot", reboot)
            .confirm("Reboot the TV? It will be unreachable for about a minute."),
        MenuItem::new(CAT, 'X', "power off", power_off).confirm(
            "Power off the TV? It can't be woken over the network afterwards;\nyou'll need the remote or the TV's power button.",
        ),
    ]
}
