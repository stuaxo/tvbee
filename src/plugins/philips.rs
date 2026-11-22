//! Philips Android TV extras, built on the TV's "JointSpace" REST service
//! (Restlet on ports 1925 http / 1926 https). Vendor-specific, so only
//! enabled when the Philips system service package is installed.

use crate::error::{failed, usage, Result};
use crate::registry::Command;
use crate::tv::Tv;
use crate::Ctx;

const CAT: &str = "philips";
const JOINTSPACE_PKG: &str = "org.droidtv.xtv";

pub fn detect(tv: &Tv) -> Result<bool> {
    tv.connect()?;
    let out = tv.shell(&["pm", "list", "packages", JOINTSPACE_PKG])?;
    Ok(out.lines().any(|l| l.trim() == format!("package:{JOINTSPACE_PKG}")))
}

fn jointspace(c: &Ctx, args: &[String]) -> Result<()> {
    let path = match args.first() {
        None => "/6/system".to_string(),
        Some(p) if p.starts_with("/6/") => p.clone(),
        Some(_) => return usage("usage: tvbee jointspace [/6/<path>]"),
    };
    let (status, body) = crate::http::get(&c.tv.ip, 1925, &path)?;
    println!("{body}");
    if status == 200 {
        Ok(())
    } else {
        failed(format!("jointspace: HTTP {status} for {path}"))
    }
}

pub fn commands() -> Vec<Command> {
    vec![Command::new(CAT, "jointspace", "[/6/<path>]", "GET a JointSpace path over plain HTTP", jointspace).long(
        "Defaults to /6/system (model, API version, feature list). Only unauthenticated\nroutes work; recordings and most others need digest-auth pairing on :1926.",
    )]
}
