use std::env::set_current_dir;
use std::fs::OpenOptions;
use std::os::fd::{
    AsFd as _, AsRawFd as _, BorrowedFd, FromRawFd as _, IntoRawFd as _, OwnedFd, RawFd,
};
use std::time::Duration;
use std::{process, thread};

#[derive(Clone, Copy)]
#[repr(i32)]
enum ExitCodes {
    Ok = 0,
    ChildFailedToFork,
    ChildSetsidFailed,
    GrandchildChdirFailed,
    GrandchildOpenDevNullFailed,
    GrandchildFailedTooSoon,
}

impl From<ExitCodes> for i32 {
    #[expect(clippy::as_conversions, reason = "Enum's backing type is i32")]
    fn from(value: ExitCodes) -> Self {
        value as i32
    }
}

impl TryFrom<i32> for ExitCodes {
    type Error = &'static str;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(ExitCodes::Ok),
            1 => Ok(ExitCodes::ChildFailedToFork),
            2 => Ok(ExitCodes::ChildSetsidFailed),
            3 => Ok(ExitCodes::GrandchildChdirFailed),
            4 => Ok(ExitCodes::GrandchildOpenDevNullFailed),
            5 => Ok(ExitCodes::GrandchildFailedTooSoon),
            _ => Err("Unknown exitcode"),
        }
    }
}

#[derive(Debug)]
pub enum Identity {
    Original,
    Daemon,
}

enum Fork {
    Parent { child: i32 },
    Child,
}

fn wait_for_failure(pid: i32, timeout_ms: u16) -> Result<(), std::io::Error> {
    let exited = match pidfd_open(pid) {
        Ok(pid_fd) => pidfd_exits_within(pid_fd.as_fd(), timeout_ms)?,
        Err(_err) => child_exits_within(pid, timeout_ms)?,
    };

    if exited {
        Err(std::io::Error::other(
            "Grandchild died before `timeout_ms` expiration",
        ))
    } else {
        // didn't fail within `timeout_ms`
        Ok(())
    }
}

fn pidfd_open(pid: i32) -> Result<OwnedFd, std::io::Error> {
    // SAFETY: libc call
    let pid_fd: RawFd = cvt::cvt(unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) })?
        .try_into()
        .expect("File descriptors always fit in `c_int`");

    // SAFETY: `pidfd_open` returned a new open descriptor
    Ok(unsafe { OwnedFd::from_raw_fd(pid_fd) })
}

fn pidfd_exits_within(pid_fd: BorrowedFd<'_>, timeout_ms: u16) -> Result<bool, std::io::Error> {
    let mut poll_fd = libc::pollfd {
        fd: pid_fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };

    // SAFETY: libc call
    let ready = cvt::cvt_r(|| unsafe { libc::poll(&raw mut poll_fd, 1, timeout_ms.into()) })?;

    Ok(ready != 0)
}

fn child_exits_within(pid: i32, timeout_ms: u16) -> Result<bool, std::io::Error> {
    thread::sleep(Duration::from_millis(timeout_ms.into()));

    let mut status = 0;

    // SAFETY: libc call
    let changed_pid = cvt::cvt_r(|| unsafe { libc::waitpid(pid, &raw mut status, libc::WNOHANG) })?;

    Ok(changed_pid != 0)
}

fn wait_for_success(pid: i32) -> Result<(), std::io::Error> {
    let mut status = 0;

    // SAFETY: libc call
    let changed_pid = cvt::cvt_r(|| unsafe { libc::waitpid(pid, &raw mut status, 0) })?;

    if pid != changed_pid {
        return Err(std::io::Error::other(
            "changed pid does not match awaited pit",
        ));
    }

    let status = libc::WEXITSTATUS(status);

    match ExitCodes::try_from(status) {
        Ok(ExitCodes::Ok) => Ok(()),
        Ok(ExitCodes::ChildFailedToFork) => {
            Err(std::io::Error::other("Child did not launch correctly"))
        },

        Ok(ExitCodes::ChildSetsidFailed) => Err(std::io::Error::other("Child setsid failed")),
        Ok(ExitCodes::GrandchildChdirFailed) => {
            Err(std::io::Error::other("GrandChild chdir failed"))
        },
        Ok(ExitCodes::GrandchildOpenDevNullFailed) => {
            Err(std::io::Error::other("GrandChild open /dev/null failed"))
        },
        Ok(ExitCodes::GrandchildFailedTooSoon) => {
            Err(std::io::Error::other("GrandChild failed too soon"))
        },
        Err(error) => Err(std::io::Error::other(format!(
            "Unspecified error code: {}",
            error
        ))),
    }
}

fn close(fd: i32) -> Result<(), std::io::Error> {
    // SAFETY: libc call
    let res = unsafe { libc::close(fd) };

    let _: i32 = cvt::cvt(res)?;

    Ok(())
}

fn dup2(from: RawFd, to: RawFd) -> Result<(), std::io::Error> {
    // SAFETY: libc call
    cvt::cvt_r(|| unsafe { libc::dup2(from, to) }).map(|_| ())
}

fn fork() -> Result<Fork, std::io::Error> {
    // we're not capturing `EAGAIN` here, as the errors
    // described there aren't resolvable by themselves
    // SAFETY: libc call
    let pid = unsafe { libc::fork() };

    let pid = cvt::cvt(pid)?;

    if pid == 0 {
        Ok(Fork::Child)
    } else {
        Ok(Fork::Parent { child: pid })
    }
}

fn setsid() -> Result<(), std::io::Error> {
    // SAFETY: libc call
    let sid = unsafe { libc::setsid() };

    cvt::cvt(sid).map(|_| ())
}

/// Daemonizes the process.
///
/// # Errors
///
/// * When the `fork` fails in the original process calling `daemonize()`
pub fn daemonize() -> Result<Identity, std::io::Error> {
    DaemonizeOptions::new().daemonize()
}

pub struct DaemonizeOptions {
    timeout_ms: Option<u16>,
}

impl Default for DaemonizeOptions {
    fn default() -> Self {
        DaemonizeOptions::new()
    }
}

impl DaemonizeOptions {
    #[must_use]
    pub fn new() -> Self {
        Self { timeout_ms: None }
    }

    #[must_use]
    pub fn set_timeout_ms(mut self, timeout_ms: u16) -> Self {
        self.timeout_ms = Some(timeout_ms);

        self
    }

    /// Daemonizes the process.
    ///
    /// # Errors
    ///
    /// * When the `fork` fails in the original process calling `daemonize()`
    pub fn daemonize(self) -> Result<Identity, std::io::Error> {
        // fork() so the parent can exit, this returns control to the command line or shell invoking your program. This step is required so that the new process is guaranteed not to be a process group leader. The next step, setsid(), fails if you're a process group leader.
        match fork() {
            Ok(Fork::Child) => {
                // we're the child
            },
            Ok(Fork::Parent { child: child_pid }) => {
                // We're in the parent
                return wait_for_success(child_pid).map(|()| Identity::Original);
            },
            Err(error) => {
                // We're still in the parent
                return Err(error);
            },
        }

        // setsid() to become a process group and session group leader. Since a controlling terminal is associated with a session, and this new session has not yet acquired a controlling terminal our process now has no controlling terminal, which is a Good Thing for daemons.
        if let Err(_err) = setsid() {
            // TODO define exit code
            process::exit(ExitCodes::ChildSetsidFailed.into());
        }

        // we're now the session group leader

        // fork() again so the parent, (the session group leader), can exit. This means that we, as a non-session group leader, can never regain a controlling terminal.
        match fork() {
            Ok(Fork::Child) => {
                // We're the grand-child, continue
            },
            Ok(Fork::Parent { child: child_pid }) => {
                // let duration = Duration::from_secs(1);

                // We're in the child (i.e. the session group leader)
                match wait_for_failure(child_pid, self.timeout_ms.unwrap_or(1000)) {
                    Ok(()) => {
                        // stop child (us) gracefully
                        process::exit(ExitCodes::Ok.into())
                    },
                    Err(_err) => {
                        process::exit(ExitCodes::GrandchildFailedTooSoon.into());
                    },
                }
            },
            Err(_err) => {
                // We're still in the child, but fork failed
                process::exit(ExitCodes::ChildFailedToFork.into());
            },
        }

        // we're now in the grand-child

        // chdir("/") to ensure that our process doesn't keep any directory in use.
        // Failure to do this could make it so that an administrator couldn't unmount a filesystem, because it was our current directory.
        // [Equivalently, we could change to any directory containing files important to the daemon's operation.]
        if let Err(_err) = set_current_dir("/") {
            // Couldn't chdir to "/", which shouldn't fail
            process::exit(ExitCodes::GrandchildChdirFailed.into());
        }

        // umask(0) so that we have complete control over the permissions of anything we write. We don't know what umask we may have inherited.
        // [This step is optional]
        // SAFETY: libc call
        let _previous_mask = unsafe { libc::umask(0) };

        // close() fds 0, 1, and 2. This releases the standard in, out, and error we inherited from our parent process.
        // We have no way of knowing where these fds might have been redirected to.
        // Note that many daemons use sysconf() to determine the limit _SC_OPEN_MAX. _SC_OPEN_MAX tells you the maximum open files/process.
        // Then in a loop, the daemon can close all possible file descriptors. You have to decide if you need to do this or not.
        // If you think that there might be file-descriptors open you should close them, since there's a limit on number of concurrent file descriptors.

        // Establish new open descriptors for stdin, stdout and stderr. Even if you don't plan to use them, it is still a good idea to have them open.
        // The precise handling of these is a matter of taste; if you have a logfile, for example, you might wish to open it as stdout or stderr, and open `/dev/null' as stdin; alternatively, you could open `/dev/console' as stderr and/or stdout, and `/dev/null' as stdin, or any other combination that makes sense for your particular daemon.

        // we're doing both the closing and establishing new descriptors with a dup2 call instead of close and re-open (and hoping we get 0, 1 & 2)
        let fd = match OpenOptions::new().read(true).write(true).open("/dev/null") {
            Ok(file) => file.into_raw_fd(),
            Err(_err) => {
                // couldn't open /dev/null?
                process::exit(ExitCodes::GrandchildOpenDevNullFailed.into());
            },
        };

        let _r = dup2(fd, libc::STDIN_FILENO);
        let _r = dup2(fd, libc::STDOUT_FILENO);
        let _r = dup2(fd, libc::STDERR_FILENO);

        if fd > 2 {
            // fd is not one of the pre-defined ones, let's close it
            let _r = close(fd);
        }

        Ok(Identity::Daemon)
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::thread;
    use std::time::Duration;

    use pretty_assertions::assert_matches;

    use crate::{Identity, child_exits_within, daemonize, pidfd_open};

    #[test]
    fn child_1() {
        let result = match daemonize() {
            Ok(Identity::Original) => Ok(()),
            Ok(Identity::Daemon) => {
                thread::sleep(Duration::from_secs(2));
                Ok(())
            },
            Err(err) => Err(err),
        };

        assert_matches!(result, Ok(()));
    }

    #[test]
    fn pidfd_open_fails_for_nonexistent_pid() {
        assert_matches!(pidfd_open(i32::MAX), Err(_));
    }

    #[test]
    fn child_exits_within_reports_exited_child() {
        #[expect(clippy::zombie_processes, reason = "`child_exits_within` reaps it")]
        let child = Command::new("true").spawn().expect("`true` spawns");
        let pid = i32::try_from(child.id()).expect("pids fit in `i32`");

        assert_matches!(child_exits_within(pid, 500), Ok(true));
    }

    #[test]
    fn child_exits_within_reports_running_child() {
        let mut child = Command::new("sleep")
            .arg("10")
            .spawn()
            .expect("`sleep` spawns");
        let pid = i32::try_from(child.id()).expect("pids fit in `i32`");

        let result = child_exits_within(pid, 10);

        child.kill().expect("`sleep` is killable");
        child.wait().expect("`sleep` is reapable");

        assert_matches!(result, Ok(false));
    }
}
