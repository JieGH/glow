//! Hardware dimming for external monitors over DDC, through the optional `m1ddc` tool (see docs/how-it-works.md).
//! m1ddc is slow, so one worker thread runs it. It always acts on the latest level, so there is no backlog.

use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const PATHS: [&str; 2] = ["/opt/homebrew/bin/m1ddc", "/usr/local/bin/m1ddc"]; // a GUI app has no shell PATH
const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
const QUIT_TIMEOUT: Duration = Duration::from_secs(5); // longest quit waits for the restore

enum Link {
    Probing,
    Unsupported,
    Ready(String), // the monitor's system UUID
}

/// What Glow knows about one monitor. Kept by UUID, so a re-added screen keeps its baseline.
#[derive(Default)]
struct Monitor {
    baseline: Option<u32>, // the person's own luminance, held while dimmed
    written: Option<u32>,  // what the monitor shows; None = unknown after a failed write
    failed: bool,          // use software dimming until the next dim session starts or ends
}

#[derive(Debug, PartialEq)]
enum Step {
    Idle,
    Forget,
    Read,
    Write(u32),
}

struct State {
    level: f32,
    links: HashMap<u32, Link>, // by CGDirectDisplayID
    monitors: HashMap<String, Monitor>,
    quit: bool,
    done: bool,
}

type Shared = Arc<(Mutex<State>, Condvar)>;

pub struct Ddc(Shared);

fn full(level: f32) -> bool {
    level >= 0.999
}

fn luminance(baseline: u32, level: f32) -> u32 {
    (baseline as f32 * level.clamp(0.0, 1.0)).round() as u32
}

/// (CGDirectDisplayID, system UUID) pairs from `m1ddc display list detailed`.
fn parse_list(out: &str) -> Vec<(u32, String)> {
    let mut found = vec![];
    let mut id = None;
    for line in out.lines().map(str::trim) {
        if line.starts_with('[') {
            id = None;
        } else if let Some(v) = line.strip_prefix("- Display ID:") {
            id = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("- System UUID:") {
            let v = v.trim();
            if let (Some(id), false) = (id, v.is_empty() || v == "(null)") {
                found.push((id, v.to_string()));
            }
        }
    }
    found
}

fn plan(level: f32, m: &Monitor) -> Step {
    if m.failed {
        return Step::Idle;
    }
    match m.baseline {
        None if full(level) => Step::Idle,
        None => Step::Read,
        Some(b) => {
            let target = if full(level) { b } else { luminance(b, level) };
            if m.written != Some(target) {
                Step::Write(target)
            } else if full(level) {
                Step::Forget
            } else {
                Step::Idle
            }
        }
    }
}

/// Run m1ddc and return its output, or None if it fails or takes too long.
fn run(path: &str, args: &[&str]) -> Option<String> {
    let mut child = Command::new(path).args(args).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let start = Instant::now();
    // The output is far smaller than the pipe buffer, so it is safe to read after the exit.
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() < COMMAND_TIMEOUT => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    status.success().then_some(out)
}

fn get(path: &str, uuid: &str) -> Option<u32> {
    run(path, &["display", uuid, "get", "luminance"])?.trim().parse().ok()
}

fn set(path: &str, uuid: &str, v: u32) -> bool {
    run(path, &["display", uuid, "set", "luminance", &v.to_string()]).is_some()
}

fn worker(path: &str, shared: &Shared) {
    let (lock, wake) = &**shared;
    let mut s = lock.lock().unwrap();
    loop {
        if s.quit {
            let restore: Vec<(String, u32)> = s
                .monitors
                .iter()
                .filter_map(|(u, m)| m.baseline.filter(|b| m.written != Some(*b)).map(|b| (u.clone(), b)))
                .collect();
            drop(s);
            for (uuid, b) in restore {
                set(path, &uuid, b);
            }
            lock.lock().unwrap().done = true;
            wake.notify_all();
            return;
        }

        if let Some(id) = s.links.iter().find(|(_, l)| matches!(l, Link::Probing)).map(|(id, _)| *id) {
            drop(s);
            let uuid = run(path, &["display", "list", "detailed"])
                .and_then(|out| parse_list(&out).into_iter().find(|(i, _)| *i == id))
                .map(|(_, u)| u);
            // A monitor seen before may hold a dimmed value, so keep its link even if a read would fail now.
            let known = uuid.as_ref().is_some_and(|u| lock.lock().unwrap().monitors.contains_key(u));
            let uuid = uuid.filter(|u| known || get(path, u).is_some());
            s = lock.lock().unwrap();
            let state = &mut *s;
            if let Some(link) = state.links.get_mut(&id) {
                *link = match uuid {
                    Some(u) => {
                        state.monitors.entry(u.clone()).or_default().failed = false;
                        Link::Ready(u)
                    }
                    None => Link::Unsupported,
                };
            }
            continue;
        }

        let level = s.level;
        let state = &mut *s;
        let job = state.links.values().find_map(|l| {
            let Link::Ready(u) = l else { return None };
            let step = plan(level, state.monitors.get(u)?);
            (step != Step::Idle).then(|| (u.clone(), step))
        });
        let Some((uuid, step)) = job else {
            s = wake.wait(s).unwrap();
            continue;
        };
        let baseline = state.monitors.get(&uuid).and_then(|m| m.baseline);
        if step == Step::Forget {
            if let Some(m) = state.monitors.get_mut(&uuid) {
                *m = Monitor::default();
            }
            continue;
        }
        drop(s);

        // `result`: the value now on the monitor, or Err with what a rescue write left there.
        let result = match step {
            Step::Read => get(path, &uuid).ok_or(None),
            Step::Write(v) if set(path, &uuid, v) => Ok(v),
            // A dim write failed: try to put the baseline back, so software dimming doesn't stack on it.
            _ => Err(baseline.filter(|b| !full(level) && set(path, &uuid, *b))),
        };

        s = lock.lock().unwrap();
        let same_session = full(s.level) == full(level);
        if let Some(m) = s.monitors.get_mut(&uuid) {
            match result {
                Ok(v) => {
                    if step == Step::Read {
                        m.baseline = Some(v);
                    }
                    m.written = Some(v);
                }
                Err(rescued) => {
                    if step != Step::Read {
                        m.written = rescued;
                    }
                    m.failed = same_session;
                }
            }
        }
    }
}

impl Ddc {
    /// None when m1ddc is not installed.
    pub fn find() -> Option<Self> {
        let path = PATHS.into_iter().find(|p| std::path::Path::new(p).is_file())?;
        let state = State { level: 1.0, links: HashMap::new(), monitors: HashMap::new(), quit: false, done: false };
        let shared: Shared = Arc::new((Mutex::new(state), Condvar::new()));
        let theirs = shared.clone();
        std::thread::Builder::new().name("ddc".into()).spawn(move || worker(path, &theirs)).ok()?;
        Some(Ddc(shared))
    }

    fn change(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.0 .0.lock().unwrap());
        self.0 .1.notify_all();
    }

    /// A screen without macOS brightness control was connected: probe it.
    pub fn add(&self, id: u32) {
        self.change(|s| {
            s.links.insert(id, Link::Probing);
        });
    }

    pub fn remove(&self, id: u32) {
        self.change(|s| {
            s.links.remove(&id);
        });
    }

    /// The wanted dim level (0..1) for every DDC monitor; 1.0 restores the person's own luminance.
    pub fn set_level(&self, level: f32) {
        let mut s = self.0 .0.lock().unwrap();
        if s.level == level {
            return;
        }
        if full(s.level) != full(level) {
            s.monitors.values_mut().for_each(|m| m.failed = false); // retry once per change of session
        }
        s.level = level;
        self.0 .1.notify_all();
    }

    /// True once this screen is dimmed over DDC.
    pub fn dims(&self, id: u32) -> bool {
        let s = self.0 .0.lock().unwrap();
        matches!(s.links.get(&id), Some(Link::Ready(u)) if s.monitors.get(u).is_some_and(|m| !m.failed))
    }

    /// True while software dimming must leave this screen alone: DDC dims it, or the probe is still running.
    pub fn holds(&self, id: u32) -> bool {
        matches!(self.0 .0.lock().unwrap().links.get(&id), Some(Link::Probing)) || self.dims(id)
    }

    /// Restore every dimmed monitor and stop the worker. Waits for the writes, but not forever.
    pub fn shutdown(&self) {
        self.change(|s| s.quit = true);
        let (lock, wake) = &*self.0;
        let guard = lock.lock().unwrap();
        let _unused = wake.wait_timeout_while(guard, QUIT_TIMEOUT, |s| !s.done);
    }
}

#[cfg(test)]
mod tests {
    use super::{luminance, parse_list, plan, Monitor, Step};

    #[test]
    fn parses_ids_and_uuids_per_block() {
        let out = "[1] Some Monitor (AAAA)\n - Product name:  Some Monitor\n - Display ID:    2\n - System UUID:   AAAA\n\
                   [2] Broken (x)\n - System UUID:   BBBB\n[3] Null (y)\n - Display ID:    3\n - System UUID:   (null)\n\
                   [4] Other (CCCC)\n - Display ID:    1\n - System UUID:   CCCC\n";
        assert_eq!(parse_list(out), vec![(2, "AAAA".to_string()), (1, "CCCC".to_string())]);
        assert!(parse_list("").is_empty());
    }

    #[test]
    fn luminance_scales_the_baseline_and_never_exceeds_it() {
        assert_eq!(luminance(51, 0.5), 26);
        assert_eq!(luminance(80, 0.0), 0);
        assert_eq!(luminance(80, 1.5), 80);
    }

    #[test]
    fn plan_writes_only_during_and_after_a_dim_session() {
        let m = |baseline, written| Monitor { baseline, written, failed: false };
        assert_eq!(plan(1.0, &m(None, None)), Step::Idle); // never dimmed: never write
        assert_eq!(plan(0.5, &m(None, None)), Step::Read);
        assert_eq!(plan(0.5, &m(Some(60), Some(60))), Step::Write(30));
        assert_eq!(plan(0.501, &m(Some(60), Some(30))), Step::Idle); // same integer
        assert_eq!(plan(1.0, &m(Some(60), Some(30))), Step::Write(60));
        assert_eq!(plan(1.0, &m(Some(60), None)), Step::Write(60));
        assert_eq!(plan(1.0, &m(Some(60), Some(60))), Step::Forget);
        assert_eq!(plan(0.5, &Monitor { failed: true, ..m(Some(60), None) }), Step::Idle);
    }
}
