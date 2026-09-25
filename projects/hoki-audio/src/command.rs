//! Bounded direct-child execution for the ordered mixer worker.
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Reap(Child);

impl Drop for Reap {
    fn drop(&mut self) {
        // Also cover setup/read failures. A reaped Child cannot target a reused
        // PID: std retains its exit status and kill then returns an error.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn nonblocking(pipe: &impl AsRawFd) -> io::Result<()> {
    let fd = pipe.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn read_once(
    pipe: &mut impl Read,
    bytes: &mut Vec<u8>,
    budget: &mut usize,
) -> io::Result<(bool, bool)> {
    let mut chunk = [0u8; 8192];
    match pipe.read(&mut chunk) {
        Ok(0) => Ok((true, false)),
        Ok(count) => {
            if count > *budget {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "command output limit exceeded",
                ));
            }
            *budget -= count;
            bytes.extend_from_slice(&chunk[..count]);
            Ok((false, true))
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok((false, false))
        }
        Err(error) => Err(error),
    }
}

pub fn output(command: &mut Command, timeout: Duration, budget: usize) -> io::Result<Output> {
    output_cancellable(command, timeout, budget, || false)
}

pub fn output_cancellable(
    command: &mut Command,
    timeout: Duration,
    mut budget: usize,
    cancelled: impl Fn() -> bool,
) -> io::Result<Output> {
    if cancelled() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "Cancelled"));
    }
    let started = Instant::now();
    let mut child = Reap(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let mut out_pipe = child.0.stdout.take().expect("piped stdout");
    let mut err_pipe = child.0.stderr.take().expect("piped stderr");
    nonblocking(&out_pipe)?;
    nonblocking(&err_pipe)?;
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let (mut out_eof, mut err_eof) = (false, false);
    let mut status = None;
    loop {
        if cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "Cancelled"));
        }
        if started.elapsed() >= timeout {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("command timed out after {timeout:?}"),
            ));
        }
        // One chunk per pipe per turn: continuous output must not postpone the
        // deadline or starve the other pipe.
        let mut progress = false;
        if !out_eof {
            let (eof, read) = read_once(&mut out_pipe, &mut stdout, &mut budget)?;
            out_eof = eof;
            progress |= read;
        }
        if !err_eof {
            let (eof, read) = read_once(&mut err_pipe, &mut stderr, &mut budget)?;
            err_eof = eof;
            progress |= read;
        }
        if status.is_none() {
            status = child.0.try_wait()?;
        }
        if out_eof && err_eof {
            if let Some(status) = status {
                return Ok(Output {
                    status,
                    stdout,
                    stderr,
                });
            }
        }
        if !progress {
            let mut pipes = [
                libc::pollfd {
                    fd: if out_eof { -1 } else { out_pipe.as_raw_fd() },
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: if err_eof { -1 } else { err_pipe.as_raw_fd() },
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // Wake immediately for output/EOF rather than adding fixed delay
            // to every command. Periodic checks also cover closed pipes while
            // the direct child is still running.
            let wait_ms = timeout
                .saturating_sub(started.elapsed())
                .as_millis()
                .min(100) as libc::c_int;
            if unsafe { libc::poll(pipes.as_mut_ptr(), pipes.len() as libc::nfds_t, wait_ms) } < 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn preserves_exit_status_and_both_output_pipes_beyond_pipe_capacity() {
        let result = output(
            Command::new("sh").args([
                "-c",
                "head -c 131072 /dev/zero; head -c 131072 /dev/zero >&2; exit 7",
            ]),
            Duration::from_secs(5),
            262144,
        )
        .unwrap();
        assert_eq!(result.status.code(), Some(7));
        assert_eq!(result.stdout, vec![0; 131072]);
        assert_eq!(result.stderr, vec![0; 131072]);
    }

    #[test]
    fn output_limit_is_shared_between_stdout_and_stderr() {
        let error = output(
            Command::new("sh").args(["-c", "printf 1234; printf 5678 >&2"]),
            Duration::from_secs(2),
            7,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn closed_output_pipes_do_not_disable_the_process_deadline() {
        let error = output(
            Command::new("sh").args(["-c", "exec 1>&- 2>&-; exec sleep 30"]),
            Duration::from_millis(50),
            1024,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn cancelling_discovery_reaps_the_child_before_its_deadline() {
        let started = Instant::now();
        let error = output_cancellable(
            Command::new("sleep").arg("30"),
            Duration::from_secs(5),
            1024,
            || started.elapsed() >= Duration::from_millis(50),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn timed_out_process_does_not_block_the_next_ordered_job() {
        let worker = crate::worker::Worker::new();
        let (sender, receiver) = mpsc::channel();
        let first = sender.clone();
        worker.submit(move || {
            let error = output(
                Command::new("sh").args(["-c", "exec sleep 30"]),
                Duration::from_millis(50),
                1024,
            )
            .unwrap_err();
            first.send(error.kind() == io::ErrorKind::TimedOut).unwrap();
        });
        worker.submit(move || {
            let result = output(
                Command::new("sh").args(["-c", "printf next"]),
                Duration::from_secs(2),
                1024,
            )
            .unwrap();
            sender
                .send(result.status.success() && result.stdout == b"next")
                .unwrap();
        });
        assert!(receiver.recv_timeout(Duration::from_secs(5)).unwrap());
        assert!(receiver.recv_timeout(Duration::from_secs(5)).unwrap());
    }
}
