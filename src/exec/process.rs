use std::collections::HashSet;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{ExitStatus, Stdio};
use std::sync::Mutex;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::pipe::Receiver;
use tokio::process::{Child, Command};

use crate::exec::collect::Collector;
use crate::exec::{DRAIN_TIMEOUT, ExecError, ExecOutput, ExecSpec, Executor, Exit};

pub struct ProcessExecutor {
    live: Mutex<HashSet<i32>>,
    scrubbed: Vec<String>,
}

impl ProcessExecutor {
    pub fn scrubbing(names: impl IntoIterator<Item = String>) -> Self {
        Self {
            live: Mutex::new(HashSet::new()),
            scrubbed: names.into_iter().collect(),
        }
    }
}

#[async_trait]
impl Executor for ProcessExecutor {
    async fn run(&self, spec: ExecSpec) -> Result<ExecOutput, ExecError> {
        if !spec.cwd.is_dir() {
            return Err(ExecError::NoWorkingDir(spec.cwd));
        }

        let Some(program) = spec.argv.first().cloned() else {
            return Err(ExecError::EmptyArgv);
        };
        let (reader, writer) = pipe()?;

        let mut command = Command::new(&program);
        command
            .args(&spec.argv[1..])
            .current_dir(&spec.cwd)
            .stdout(Stdio::from(writer.try_clone()?))
            .stderr(Stdio::from(writer))
            .stdin(if spec.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .kill_on_drop(true);

        for name in &self.scrubbed {
            command.env_remove(name);
        }

        let parent = std::process::id() as libc::pid_t;
        unsafe {
            command.as_std_mut().pre_exec(move || {
                detach_from_tty()?;
                set_parent_death_signal(parent)
            });
        }

        let spawned = command.spawn();

        // The parent's copies of the pipe live in `command` until it drops.
        // Hold them and the read below never reaches end of file.
        drop(command);

        let mut child = spawned.map_err(|source| ExecError::Spawn {
            program: program.clone(),
            source,
        })?;

        let group = child.id().expect("just spawned") as i32;
        self.live.lock().expect("live groups").insert(group);

        if let Some(payload) = spec.stdin
            && let Some(mut sink) = child.stdin.take()
        {
            tokio::spawn(async move {
                let _ = sink.write_all(payload.as_bytes()).await;
                let _ = sink.shutdown().await;
            });
        }

        let mut reader = Receiver::from_owned_fd(reader)?;
        let mut collector = Collector::new(spec.max_output_bytes);

        let waited = tokio::time::timeout(
            spec.timeout,
            drain_until_exit(&mut reader, &mut collector, &mut child),
        )
        .await;

        // Always, not only on timeout: a command that leaves the group alive
        // would otherwise hold the pipe open and hang the read below.
        kill_group(group);
        self.live.lock().expect("live groups").remove(&group);

        let status = match waited {
            Ok(status) => Some(status?),
            Err(_) => {
                let _ = child.wait().await;
                None
            }
        };

        if tokio::time::timeout(DRAIN_TIMEOUT, drain_to_end(&mut reader, &mut collector))
            .await
            .is_err()
        {
            tracing::warn!(
                group,
                "output drain abandoned; something outlived the process group"
            );
        }
        let (output, truncated) = collector.finish();

        let exit = match status {
            None => Exit::TimedOut,
            Some(status) => match (status.code(), status.signal()) {
                (Some(code), _) => Exit::Code(code),
                (None, Some(signal)) => Exit::Signal(signal),
                (None, None) => Exit::Code(-1),
            },
        };

        Ok(ExecOutput {
            output,
            truncated,
            exit,
        })
    }

    async fn shutdown(&self) {
        let groups: Vec<i32> = self.live.lock().expect("live groups").drain().collect();

        for group in groups {
            tracing::warn!(group, "killing a command still running at shutdown");
            kill_group(group);
        }
    }
}

fn kill_group(group: i32) {
    unsafe { libc::kill(-group, libc::SIGKILL) };
}

/// One pipe, both ends close-on-exec, so stdout and stderr can share a single
/// writer and land in the order the command actually produced them.
fn pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as libc::c_int; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    for fd in fds {
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
            return Err(std::io::Error::last_os_error());
        }
    }
    unsafe { Ok((OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1]))) }
}

/// Keeps reading while waiting, since a command that fills the pipe blocks
/// until someone empties it.
async fn drain_until_exit(
    reader: &mut Receiver,
    collector: &mut Collector,
    child: &mut Child,
) -> std::io::Result<ExitStatus> {
    let mut buffer = [0u8; 8192];
    let mut open = true;
    let mut waiting = std::pin::pin!(child.wait());

    loop {
        tokio::select! {
            status = &mut waiting => return status,
            read = reader.read(&mut buffer), if open => match read {
                Ok(0) => open = false,
                Ok(n) => collector.push(&buffer[..n]),
                Err(_) => open = false,
            },
        }
    }
}

/// What the command wrote before it was reaped, which the kernel still holds.
async fn drain_to_end(reader: &mut Receiver, collector: &mut Collector) {
    let mut buffer = [0u8; 8192];

    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(n) => collector.push(&buffer[..n]),
        }
    }
}

/// A new session, so the command has no controlling terminal and becomes the
/// leader of its own process group. Falls back to a group alone when the
/// caller is already a session leader.
fn detach_from_tty() -> std::io::Result<()> {
    if unsafe { libc::setsid() } != -1 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() != Some(libc::EPERM) {
        return Err(err);
    }
    if unsafe { libc::setpgid(0, 0) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// If the daemon is killed outright, `shutdown` never runs. This asks the
/// kernel to signal the command anyway. Re-checks the parent to close the race
/// where it died between fork and exec.
#[cfg(target_os = "linux")]
fn set_parent_death_signal(parent: libc::pid_t) -> std::io::Result<()> {
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::getppid() } != parent {
        unsafe { libc::raise(libc::SIGTERM) };
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn set_parent_death_signal(_parent: libc::pid_t) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;

    fn spec(argv: &[&str]) -> ExecSpec {
        ExecSpec {
            argv: argv.iter().map(|a| a.to_string()).collect(),
            cwd: std::env::current_dir().unwrap(),
            stdin: None,
            timeout: Duration::from_secs(10),
            max_output_bytes: 50_000,
        }
    }

    async fn run(spec: ExecSpec) -> Result<ExecOutput, ExecError> {
        ProcessExecutor::scrubbing([]).run(spec).await
    }

    #[tokio::test]
    async fn a_command_returns_its_output() {
        let out = run(spec(&["echo", "hi"])).await.unwrap();
        assert_eq!(out.output, "hi\n");
        assert_eq!(out.exit, Exit::Code(0));
    }

    #[tokio::test]
    async fn a_non_zero_exit_is_reported_not_raised() {
        let out = run(spec(&["sh", "-c", "exit 3"])).await.unwrap();
        assert_eq!(out.exit, Exit::Code(3));
    }

    #[tokio::test]
    async fn stdout_and_stderr_keep_the_order_they_were_written() {
        let out = run(spec(&[
            "sh",
            "-c",
            "echo one; echo two >&2; echo three; echo four >&2",
        ]))
        .await
        .unwrap();
        assert_eq!(out.output, "one\ntwo\nthree\nfour\n");
    }

    #[tokio::test]
    async fn an_argument_with_spaces_stays_one_argument() {
        let out = run(spec(&["echo", "a b c"])).await.unwrap();
        assert_eq!(out.output, "a b c\n");
    }

    #[tokio::test]
    async fn stdin_reaches_the_command() {
        let mut s = spec(&["cat"]);
        s.stdin = Some("payload\n".to_string());
        let out = run(s).await.unwrap();
        assert_eq!(out.output, "payload\n");
    }

    #[tokio::test]
    async fn a_command_without_stdin_sees_end_of_file() {
        let out = run(spec(&["cat"])).await.unwrap();
        assert_eq!(out.output, "");
        assert_eq!(out.exit, Exit::Code(0));
    }

    /// Counts the variable in the child's actual environment. Asking the shell
    /// to expand `$PATH` would lie: POSIX `sh` substitutes a default when the
    /// variable is unset.
    fn count_in_env(name: &str) -> ExecSpec {
        let script = format!("env | grep -c '^{name}=' || true");
        spec(&["/bin/sh", "-c", &script])
    }

    #[tokio::test]
    async fn a_scrubbed_variable_never_reaches_the_command() {
        let executor = ProcessExecutor::scrubbing(["HOME".to_string()]);
        let out = executor.run(count_in_env("HOME")).await.unwrap();
        assert_eq!(out.output.trim(), "0");
    }

    #[tokio::test]
    async fn an_unscrubbed_variable_is_still_inherited() {
        let out = run(count_in_env("HOME")).await.unwrap();
        assert_eq!(out.output.trim(), "1");
    }

    #[tokio::test]
    async fn a_command_has_no_controlling_terminal() {
        let out = run(spec(&["/bin/sh", "-c", "tty || true"])).await.unwrap();
        assert!(out.output.contains("not a tty"), "{}", out.output);
    }

    #[tokio::test]
    async fn an_empty_argv_is_an_error_rather_than_a_panic() {
        let mut s = spec(&["true"]);
        s.argv.clear();
        assert!(matches!(run(s).await.unwrap_err(), ExecError::EmptyArgv));
    }

    #[tokio::test]
    async fn a_missing_binary_is_a_spawn_failure() {
        let err = run(spec(&["definitely-not-a-real-binary"]))
            .await
            .unwrap_err();
        assert!(matches!(err, ExecError::Spawn { .. }));
    }

    #[tokio::test]
    async fn a_missing_working_directory_is_distinguishable_from_a_missing_binary() {
        let mut s = spec(&["echo", "hi"]);
        s.cwd = PathBuf::from("/no/such/place");
        let err = run(s).await.unwrap_err();
        assert!(matches!(err, ExecError::NoWorkingDir(_)));
    }

    #[tokio::test]
    async fn a_slow_command_times_out() {
        let mut s = spec(&["sleep", "30"]);
        s.timeout = Duration::from_millis(200);
        let out = run(s).await.unwrap();
        assert_eq!(out.exit, Exit::TimedOut);
    }

    #[tokio::test]
    async fn a_timeout_keeps_what_the_command_printed_first() {
        let mut s = spec(&["/bin/sh", "-c", "echo progress; sleep 30"]);
        s.timeout = Duration::from_millis(300);
        let out = run(s).await.unwrap();
        assert_eq!(out.exit, Exit::TimedOut);
        assert_eq!(out.output, "progress\n");
    }

    #[tokio::test]
    async fn a_timeout_takes_the_whole_process_group() {
        let marker = std::env::temp_dir().join(format!("mcpd-group-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);

        let mut s = spec(&[
            "sh",
            "-c",
            &format!("(sleep 1 && touch {}) & sleep 30", marker.display()),
        ]);
        s.timeout = Duration::from_millis(200);
        assert_eq!(run(s).await.unwrap().exit, Exit::TimedOut);

        tokio::time::sleep(Duration::from_millis(1800)).await;
        assert!(
            !marker.exists(),
            "a backgrounded grandchild outlived the timeout"
        );
    }

    #[tokio::test]
    async fn output_beyond_the_limit_is_truncated() {
        let mut s = spec(&["sh", "-c", "yes 0123456789 | head -c 200000"]);
        s.max_output_bytes = 1000;
        let out = run(s).await.unwrap();
        assert!(out.truncated);
        assert!(out.output.contains("bytes elided"));
        assert!(out.output.len() < 1200);
    }

    #[tokio::test]
    async fn a_command_reading_a_huge_stdin_does_not_deadlock() {
        let mut s = spec(&["cat"]);
        s.stdin = Some("x".repeat(1_000_000));
        s.max_output_bytes = 100;
        let out = run(s).await.unwrap();
        assert!(out.truncated);
        assert_eq!(out.exit, Exit::Code(0));
    }

    #[tokio::test]
    async fn the_working_directory_is_where_the_command_runs() {
        let elsewhere = std::env::temp_dir().canonicalize().unwrap();
        let mut s = spec(&["pwd"]);
        s.cwd = elsewhere.clone();
        let out = run(s).await.unwrap();
        assert_eq!(out.output.trim(), elsewhere.to_str().unwrap());
    }
}
