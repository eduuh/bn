//! The browser driver for `bn forge login`: `@playwright/cli` as a shell-out.
//!
//! Deliberately thin. [`super::login`] decides what to do; this only opens a headed browser on a
//! persistent profile and evaluates functions in the page. Swapping the driver (a CDP relay for
//! WSL, a different automation tool) means another [`Page`] implementation, nothing else.
//!
//! - **Profile:** `<state_dir>/forge-browser`, a dedicated persistent profile. The first login
//!   signs in there once, and later logins reuse the session. Chrome and Edge refuse automation on
//!   the default user-data directory, which is why it is not the person's everyday profile.
//! - **Session:** `playwright-cli -s=bn-forge`, closed when the login finishes. The profile stays.
//! - **Output:** `--raw eval` prints only the function's return value. The minted key passes
//!   through that pipe into this process and nowhere else. playwright-cli writes its snapshot
//!   files to the working directory, so it runs in a scratch directory removed afterwards.

use super::login::Page;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const SESSION: &str = "bn-forge";
const BIN: &str = "playwright-cli";

pub(crate) struct PlaywrightCli {
    browser: String,
    profile: PathBuf,
    workdir: PathBuf,
    opened: bool,
}

impl PlaywrightCli {
    pub(crate) fn new(browser: &str) -> PlaywrightCli {
        let workdir =
            std::env::temp_dir().join(format!("bn-forge-login-{}", std::process::id()));
        PlaywrightCli {
            browser: browser.to_string(),
            profile: bn_core::resolver::state_dir().join("forge-browser"),
            workdir,
            opened: false,
        }
    }

    fn command(&self, args: &[&str]) -> Result<Output, String> {
        let _ = std::fs::create_dir_all(&self.workdir);
        Command::new(BIN)
            .arg(format!("-s={SESSION}"))
            .args(args)
            .current_dir(&self.workdir)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    format!("{BIN} is not installed: `npm install -g @playwright/cli` (the package is @playwright/cli, not playwright-cli), or use `bn forge login --paste`")
                } else {
                    format!("could not run {BIN}: {e}")
                }
            })
    }

    /// Close the browser session and remove the scratch directory. The profile is kept.
    pub(crate) fn finish(&mut self) {
        if self.opened {
            let _ = self.command(&["close"]);
            self.opened = false;
        }
        let _ = std::fs::remove_dir_all(&self.workdir);
    }
}

fn failure(what: &str, out: &Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let detail = if stderr.trim().is_empty() {
        stdout.trim().lines().last().unwrap_or_default().to_string()
    } else {
        stderr.trim().lines().last().unwrap_or_default().to_string()
    };
    format!("{BIN} {what} failed: {detail}")
}

impl Page for PlaywrightCli {
    fn open(&mut self, url: &str) -> Result<(), String> {
        let _ = std::fs::create_dir_all(&self.profile);
        let browser = format!("--browser={}", self.browser);
        let profile = format!("--profile={}", self.profile.display());
        let out = self.command(&["open", url, "--headed", &browser, &profile])?;
        if !out.status.success() {
            return Err(failure("open", &out));
        }
        self.opened = true;
        Ok(())
    }

    fn eval(&mut self, js: &str) -> Result<String, String> {
        let out = self.command(&["--raw", "eval", js])?;
        if !out.status.success() {
            // Never include stdout here: on success it is the page's answer, which may be the key.
            let stderr = String::from_utf8_lossy(&out.stderr);
            let detail = stderr.trim().lines().last().unwrap_or("no detail");
            return Err(format!("{BIN} eval failed: {detail}"));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

impl Drop for PlaywrightCli {
    fn drop(&mut self) {
        self.finish();
    }
}
