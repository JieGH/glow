//! Update check: asks GitHub for the latest release and tells the person when it is newer.
//! It only links to the download page. Glow is not notarized, so it can't replace itself.
use eframe::egui::Context;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const API: &str = "https://api.github.com/repos/JieGH/glow/releases/latest";
const PAGE: &str = "https://github.com/JieGH/glow/releases/latest";
const EVERY: Duration = Duration::from_secs(24 * 3600);

#[derive(Clone, PartialEq)]
pub enum Status {
    Idle,
    Checking,
    UpToDate,
    Available(String),
    Failed,
}

pub type Shared = Arc<Mutex<Status>>;

/// Check now and then once a day while Glow runs.
pub fn start(ctx: Context) -> Shared {
    let shared: Shared = Arc::new(Mutex::new(Status::Idle));
    let s = shared.clone();
    std::thread::spawn(move || loop {
        check(&s, &ctx);
        std::thread::sleep(EVERY);
    });
    shared
}

/// Check now, from a click.
pub fn check_now(shared: &Shared, ctx: &Context) {
    let (s, c) = (shared.clone(), ctx.clone());
    std::thread::spawn(move || check(&s, &c));
}

pub fn open_page() {
    let _ = Command::new("open").arg(PAGE).spawn();
}

fn check(shared: &Shared, ctx: &Context) {
    {
        let mut s = shared.lock().unwrap();
        if *s == Status::Checking {
            return;
        }
        *s = Status::Checking;
    }
    ctx.request_repaint();
    // shortcut: uses the system curl and anonymous GitHub (60 requests/hour); swap in an HTTP crate if that bites.
    let out = Command::new("curl").args(["-sSL", "--max-time", "10", API]).output();
    let result = match out {
        Ok(o) if o.status.success() => match tag(&String::from_utf8_lossy(&o.stdout)) {
            Some(t) if newer(&t, env!("CARGO_PKG_VERSION")) => Status::Available(t),
            _ => Status::UpToDate, // no release published yet counts as up to date
        },
        _ => Status::Failed,
    };
    *shared.lock().unwrap() = result;
    ctx.request_repaint();
}

/// The `tag_name` value from GitHub's release JSON.
fn tag(json: &str) -> Option<String> {
    let rest = json.split("\"tag_name\"").nth(1)?;
    let rest = &rest[rest.find('"')? + 1..];
    Some(rest[..rest.find('"')?].to_string())
}

/// Is version `a` (like "v0.2.0") newer than `b` (like "0.1.0")?
fn newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| v.trim_start_matches('v').split('.').map(|p| p.parse::<u32>().ok()).collect::<Option<Vec<_>>>();
    matches!((parse(a), parse(b)), (Some(a), Some(b)) if a > b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(newer("v0.2.0", "0.1.0"));
        assert!(newer("v0.10.0", "0.9.0"));
        assert!(!newer("v0.1.0", "0.1.0"));
        assert!(!newer("v0.0.9", "0.1.0"));
        assert!(!newer("nightly", "0.1.0"));
    }

    #[test]
    fn reads_the_tag() {
        assert_eq!(tag(r#"{"url":"x","tag_name": "v0.2.0","name":"y"}"#).as_deref(), Some("v0.2.0"));
        assert_eq!(tag(r#"{"message":"Not Found"}"#), None);
    }
}
