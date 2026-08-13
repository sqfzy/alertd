use super::CollectError;
use std::{
    io::Read,
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

pub fn run(program: &str, arguments: &[&str], timeout: Duration) -> Result<Output, CollectError> {
    let mut command = Command::new(program);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Output {
                status,
                stdout: stdout_reader.join().map_err(|_| reader_error())??,
                stderr: stderr_reader.join().map_err(|_| reader_error())??,
            });
        }
        if Instant::now() >= deadline {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            #[cfg(not(unix))]
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(CollectError::Timeout(format!(
                "{program} exceeded {timeout:?}"
            )));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn reader_error() -> CollectError {
    CollectError::Invalid("command output reader panicked".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_output_and_enforces_timeout() {
        let output = run("sh", &["-c", "printf ok"], Duration::from_secs(1)).unwrap();
        assert_eq!(output.stdout, b"ok");
        assert!(matches!(
            run("sh", &["-c", "sleep 1"], Duration::from_millis(20)),
            Err(CollectError::Timeout(_))
        ));
    }
}
