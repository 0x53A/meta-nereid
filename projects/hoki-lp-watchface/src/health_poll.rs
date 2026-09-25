//! Bounded event pipe plus control-directory notifications. No periodic polling.
use std::{
    io::{self, Read},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::Path,
    process::{Child, ChildStdout, Command, Stdio},
};

pub struct PollReader {
    child: Child,
    pipe: ChildStdout,
    watch: Option<OwnedFd>,
    bytes: Vec<u8>,
}
impl PollReader {
    pub fn start(control: Option<&Path>) -> Result<Self, String> {
        // Install the watch before spawning, so a request during setup is still
        // observed. The caller also examines existing request files each loop.
        let watch = if let Some(dir) = control {
            let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
            if fd < 0 {
                return Err(io::Error::last_os_error().to_string());
            }
            let owned = unsafe { OwnedFd::from_raw_fd(fd) };
            use std::os::unix::ffi::OsStrExt;
            let path =
                std::ffi::CString::new(dir.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
            let mask = libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_CREATE;
            if unsafe { libc::inotify_add_watch(fd, path.as_ptr(), mask) } < 0 {
                return Err(io::Error::last_os_error().to_string());
            }
            Some(owned)
        } else {
            None
        };
        let mut command = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
        command
            .arg("poll-worker")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        use std::os::unix::process::CommandExt;
        let parent_pid = unsafe { libc::getpid() };
        unsafe {
            command.pre_exec(move || {
                // Also cover SIGKILL of the recorder, where Drop cannot run.
                // Only syscall wrappers and errno construction after fork.
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != parent_pid {
                    return Err(io::Error::from_raw_os_error(libc::ECHILD));
                }
                Ok(())
            });
        }
        let mut child = command.spawn().map_err(|e| e.to_string())?;
        let pipe = child.stdout.take().ok_or("missing worker pipe")?;
        let mut result = Self {
            child,
            pipe,
            watch,
            bytes: Vec::with_capacity(16 * 1024),
        };
        let fd = result.pipe.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            // Drop kills/reaps the child on setup failure too.
            return Err(io::Error::last_os_error().to_string());
        }
        if let Some(status) = result.child.try_wait().map_err(|e| e.to_string())? {
            return Err(format!("poll worker exited during setup: {status}"));
        }
        Ok(result)
    }
    pub fn receive(&mut self, timeout_ms: i32) -> Result<Vec<[u8; 88]>, String> {
        let mut fds = [
            libc::pollfd {
                fd: self.pipe.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.watch.as_ref().map_or(-1, |f| f.as_raw_fd()),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
        if rc < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                return Ok(Vec::new());
            }
            return Err(e.to_string());
        }
        if fds[1].revents & libc::POLLIN != 0 {
            let mut notifications = [0u8; 4096];
            // Contents are only a hint. Caller reads authoritative files again.
            unsafe {
                libc::read(
                    fds[1].fd,
                    notifications.as_mut_ptr().cast(),
                    notifications.len(),
                );
            }
        }
        if fds
            .iter()
            .any(|f| f.revents & (libc::POLLERR | libc::POLLNVAL) != 0)
        {
            return Err("poll descriptor failed".into());
        }
        if fds[0].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
            let mut chunk = [0u8; 11264];
            match self.pipe.read(&mut chunk) {
                Ok(0) => {
                    return Err(format!(
                        "poll worker disconnected; {} partial bytes",
                        self.bytes.len()
                    ));
                }
                Ok(n) => self.bytes.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        let complete = self.bytes.len() / 88 * 88;
        let result = self.bytes[..complete]
            .chunks_exact(88)
            .map(|b| b.try_into().unwrap())
            .collect();
        self.bytes.drain(..complete);
        Ok(result)
    }
}
impl Drop for PollReader {
    fn drop(&mut self) {
        // The worker has no activation/output ownership. Killing a blocked poll
        // must not wait for a sensor event; the parent handles HAL deactivation.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
