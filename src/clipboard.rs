//! Clipboard access behind a trait, so Windows can implement the same surface
//! without this file knowing anything about it.
//!
//! v0.1 shells out to wl-clipboard. v0.2 implements `wl_data_device_manager`
//! natively (see docs/how-it-works.md); the trait does not change when that
//! happens, which is the point.

use anyhow::{Context, Result};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub trait Clipboard {
    fn primary(&self) -> Result<Option<String>>;
    fn set_clipboard(&self, text: &str) -> Result<()>;
    fn clipboard(&self) -> Result<Option<String>>;
    /// Backend name, shown by `doctor`.
    #[allow(dead_code)]
    fn name(&self) -> &'static str;
}

pub struct WlClipboard;

fn run_with_timeout(program: &str, args: &[&str], limit: Duration) -> Result<Vec<u8>> {
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .with_context(|| format!("spawning {program}"))?;

    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait()? {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!("{program} timed out");
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    let out = child.wait_with_output()?;
    Ok(out.stdout)
}

impl Clipboard for WlClipboard {
    fn name(&self) -> &'static str {
        "wl-clipboard"
    }

    fn primary(&self) -> Result<Option<String>> {
        // Bounded on purpose: a wedged owner must not block the router.
        let out = run_with_timeout("wl-paste", &["--primary", "-n"], Duration::from_millis(400))?;
        if out.is_empty() {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&out).into_owned()))
    }

    fn clipboard(&self) -> Result<Option<String>> {
        let out = run_with_timeout("wl-paste", &["-n"], Duration::from_millis(400))?;
        Ok(Some(String::from_utf8_lossy(&out).into_owned()).filter(|s| !s.is_empty()))
    }

    /// `wl-copy` forks and then stays alive to serve paste requests, so waiting
    /// on it here would hang for as long as the clipboard is owned.
    ///
    /// Away, but not uncollected. The process this function spawns is the parent
    /// that forks and exits at once; a `Child` dropped without a `wait()` stays
    /// in the process table as `<defunct>` until its parent collects it. Seven
    /// were found under a daemon that had been up ten minutes. So each write
    /// hands its handle to a short-lived thread that blocks in `wait()`: the
    /// write still returns immediately, a copy costs one thread that exits with
    /// its child, and every handle is collected whether the write succeeded or
    /// the child died first.
    fn set_clipboard(&self, text: &str) -> Result<()> {
        let mut child = Command::new("wl-copy")
            .arg("-n")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("spawning wl-copy")?;
        let written = match child.stdin.take() {
            Some(mut stdin) => stdin.write_all(text.as_bytes()),
            None => Ok(()),
        };
        // Reap on both paths: a failed write still leaves a process behind.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        written?;
        Ok(())
    }
}

pub fn default_backend() -> Box<dyn Clipboard> {
    Box::new(WlClipboard)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A handle nobody waits on is a zombie the moment the child exits. The
    /// daemon used to leak one per clipboard write; `set_clipboard` must now
    /// collect it without blocking the caller.
    #[test]
    fn a_written_child_does_not_stay_defunct() {
        // Any command that exits at once stands in for `wl-copy`'s forking
        // parent; the point under test is the reaping, not the clipboard.
        let mut child = Command::new("true").spawn().expect("spawning true");
        let pid = child.id();
        let started = Instant::now();
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        // Give the reaper a moment, then insist the pid is gone from the table.
        while started.elapsed() < Duration::from_secs(2) {
            let stat = Command::new("ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
                .expect("running ps");
            if String::from_utf8_lossy(&stat.stdout).trim().is_empty() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("child {pid} was still in the process table after 2s");
    }
}
