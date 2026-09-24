use std::{
    io::{Read, Seek},
    process::{Command, Output, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use wait_timeout::ChildExt;

pub const TIMEOUT: Duration = Duration::from_secs(120);

pub fn run(command: &mut Command) -> Result<Output> {
    // File-backed output avoids pipe deadlocks on large dependency graphs.
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .spawn()
        .with_context(|| {
            format!(
                "unable to start {}",
                command.get_program().to_string_lossy()
            )
        })?;
    let Some(status) = child.wait_timeout(TIMEOUT)? else {
        let _ = child.kill();
        let _ = child.wait();
        bail!("command timed out after {} seconds", TIMEOUT.as_secs());
    };
    stdout.rewind()?;
    stderr.rewind()?;
    let mut output = Output {
        status,
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    stdout.read_to_end(&mut output.stdout)?;
    stderr.read_to_end(&mut output.stderr)?;
    Ok(output)
}

pub fn checked(command: &mut Command) -> Result<String> {
    let output = run(command)?;
    if !output.status.success() {
        bail!(
            "{} failed: {}",
            command.get_program().to_string_lossy(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}
