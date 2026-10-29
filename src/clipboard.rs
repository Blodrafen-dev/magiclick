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
    /// on it hangs forever. Hand it the data and walk away.
    fn set_clipboard(&self, text: &str) -> Result<()> {
        let mut child = Command::new("wl-copy")
            .arg("-n")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("spawning wl-copy")?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes())?;
        }
        // Deliberately no wait().
        Ok(())
    }
}

pub fn default_backend() -> Box<dyn Clipboard> {
    Box::new(WlClipboard)
}
