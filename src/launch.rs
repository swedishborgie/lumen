//! Launch-child session management for `--launch` / `--desktop` mode.
//!
//! The desktop we stream (`dbus-run-session startplasma-wayland` and friends)
//! is spawned by [`crate::tasks::spawn_launch_task`] inside its own POSIX
//! session so that lumen can always clean it up on exit:
//!
//! * KDE's session startup registers with logind, which moves the session
//!   processes out of lumen's systemd cgroup and into `user.slice`.  From
//!   systemd's point of view the desktop is lumen's, but lumen itself is the
//!   only thing that can reap it.  Without a private session, every lumen
//!   exit (crash, systemd restart, clean shutdown) leaks a full Plasma
//!   session — and a leaked `kwin_wayland_wrapper` will happily respawn a
//!   kwin that re-attaches to the *next* lumen instance, defeating
//!   `--exit-on-compositor-loss`.
//!
//! * The fatal-exit tracker counts only Wayland clients that are descendants
//!   of the launch child (`lumen_compositor::is_descendant_of`), so a stray
//!   kwin from a dead session cannot cancel the grace timer.

use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// SIGTERM grace period before escalating to SIGKILL when tearing the
/// desktop down on exit.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Shared handle to the launch child, kept alive across exit paths.
#[derive(Default, Clone)]
pub struct ChildRef(pub Arc<Mutex<Option<Child>>>);

impl ChildRef {
    /// Store the freshly spawned child.
    pub fn set(&self, child: Child) {
        *self.0.lock().unwrap() = Some(child);
    }

    /// Poll until the child exits (the launch task's own wait).  Returns the
    /// exit status, or `None` if another exit path already reaped the child.
    ///
    /// `try_wait` is used (rather than the blocking `wait`) so the shutdown
    /// paths can grab the same lock and reap the child in parallel — a
    /// blocked `wait` would hold the lock for the whole lifetime of the
    /// desktop and deadlock the SIGTERM handler.
    pub fn wait(&self) -> Option<std::process::ExitStatus> {
        loop {
            let mut guard = self.0.lock().unwrap();
            let child = guard.as_mut()?;
            match child.try_wait() {
                Ok(Some(s)) => {
                    *guard = None;
                    return Some(s);
                }
                Ok(None) => {
                    // Still running — drop the lock and sleep.
                }
                Err(e) => {
                    tracing::error!(%e, "launch: failed to wait on session leader");
                    *guard = None;
                    return None;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Spawn `cmd` in its own POSIX session (new process-group leader).
///
/// The child's PID equals its session/process-group ID, which is what
/// [`kill_session`] and `is_descendant_of` build on.
///
/// The `pre_exec` hook runs in the child *after* fork, *before* exec, so the
/// session is established before any user code runs and no lumen-side state
/// is touched.
pub fn spawn_session(cmd: &mut Command) -> io::Result<Child> {
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn()
}

/// Tear down the launch child's whole session: SIGTERM the process group,
/// wait up to [`SHUTDOWN_GRACE`], then SIGKILL whatever is left.
///
/// `session_id` is the PID to signal even when the child was already reaped.
/// No-op when `session_id` is `None`.  Signalling an already-empty group is
/// harmless, so this is safe to call on every exit path.
pub fn kill_session(session_id: Option<u32>) {
    let Some(session_id) = session_id else { return };
    let neg_pid = -i32::try_from(session_id).unwrap_or(i32::MAX);
    // ESRCH means the session is already gone — nothing to signal.
    if kill_group(neg_pid, libc::SIGTERM) == 0 {
        tracing::info!(pid = session_id, "launch: sent SIGTERM to desktop session");
        // Give the group a beat to exit before escalating.
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        while group_alive(session_id) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    // Anything that ignored SIGTERM (plasmashell, kwin workers, …) gets
    // SIGKILL'd.  ESRCH means the group is already empty — fine.
    if kill_group(neg_pid, libc::SIGKILL) == 0 {
        tracing::info!(pid = session_id, "launch: SIGKILL sent to desktop session");
        // Give the kernel a beat to reap the rest before we exit.
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        while group_alive(session_id) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// `true` when any process still has `session_id` as its PID, process-group
/// ID, or session ID.  Only used to time the SIGTERM/SIGKILL waits, so a
/// coarse scan of `/proc` is fine.
fn group_alive(session_id: u32) -> bool {
    let entries = match std::fs::read_dir("/proc") {
        Ok(e) => e,
        Err(_) => return false,
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Ok(pid) = name.to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some(after_comm) = stat.rfind(')') else { continue };
        let fields: Vec<&str> = stat[after_comm + 1..].split_whitespace().collect();
        // Fields after ") ": state(0) ppid(1) pgrp(2) session(3) …
        // A zombie (state 'Z') is already dead — it lingers in /proc only
        // until reaped, so it must not count as "alive" here (the session
        // leader is our direct child and stays a zombie after SIGKILL until
        // the launch task reaps it).
        if fields.first() == Some(&"Z") {
            continue;
        }
        if fields.get(2).and_then(|s| s.parse().ok()) == Some(session_id)
            || fields.get(3).and_then(|s| s.parse().ok()) == Some(session_id)
        {
            return true;
        }
    }
    false
}

fn kill_group(neg_pid: i32, sig: i32) -> i32 {
    // -pid targets the whole process group.  libc is used directly: nix isn't
    // a runtime dependency of the root crate.
    unsafe { libc::kill(neg_pid, sig) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_compositor::{is_descendant_of, read_ppid};

    /// Poll `f` until it returns `true` or the 2 s deadline passes.
    fn eventually<F: Fn() -> bool>(what: &str, f: F) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if f() {
                return true;
            }
            if Instant::now() >= deadline {
                panic!("timed out waiting for {what}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

        #[test]
    fn session_spawn_and_kill_takes_down_whole_tree() {
        // Session leader (`sh -c 'sleep 300 & echo $! > FILE; wait'`) plus a
        // background `sleep` it forked.  `wait` keeps the shell alive (so the
        // leader is a real shell, not an exec'd sleep), and both the leader
        // and the sleep share the leader's process group, so a group signal
        // must take down the whole tree.  The child's pid comes from a unique
        // pid file — a /proc scan is racy under parallel tests.
        let pidfile = std::env::temp_dir().join(format!(
            "lumen-sess-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or_default().rsplit("::").next().unwrap_or("t")
        ));
        let pidfile_s = pidfile.to_string_lossy().to_string();
        let mut cmd = Command::new("sh");
        cmd.args(["-c", &format!("sleep 300 & echo $! > {pidfile_s}; wait")])
            .stdin(std::process::Stdio::null());
        let child = spawn_session(&mut cmd).expect("spawn_session");
        let session_id = child.id();
        let self_pid = std::process::id();

        assert_ne!(session_id, self_pid);
        assert!(
            is_descendant_of(session_id, self_pid),
            "session leader must still be our child while alive"
        );
        // The leader is a session leader (sid == its own pid).
        let stat = std::fs::read_to_string(format!("/proc/{session_id}/stat")).unwrap();
        let fields: Vec<&str> = stat[stat.rfind(')').unwrap() + 1..]
            .split_whitespace()
            .collect();
        assert_eq!(
            fields[3].parse::<u32>().unwrap(),
            session_id,
            "session leader's sid must equal its own pid"
        );

        // Wait until the background `sleep` child has written its pid file
        // and is actually a child of the session leader.
        let mut grandchild = None;
        let deadline = Instant::now() + Duration::from_secs(2);
        while grandchild.is_none() && Instant::now() < deadline {
            if let Ok(s) = std::fs::read_to_string(&pidfile) {
                if let Ok(p) = s.trim().parse::<u32>() {
                    if read_ppid(p) == Some(session_id) {
                        grandchild = Some(p);
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let grandchild = grandchild.expect("background sleep child of session leader");

        let child_ref = ChildRef::default();
        child_ref.set(child);
        kill_session(Some(session_id));

        // Reap the session leader (our direct child) — this also clears the
        // zombie that would otherwise still hold the group alive in /proc.
        let status = child_ref.wait();
        assert!(status.is_some(), "wait() must return the child's exit status");
        assert!(child_ref.0.lock().unwrap().is_none());

        // The whole session (leader AND the background sleep) must be gone.
        eventually("session to die", || !group_alive(session_id));
        assert!(
            std::fs::read_to_string(format!("/proc/{grandchild}/stat")).is_err(),
            "the background sleep must have been killed by the group signal"
        );
        // The group is now empty: signalling it must fail with ESRCH.
        assert_eq!(kill_group(-(session_id as i32), libc::SIGTERM), -1);
        let _ = std::fs::remove_file(pidfile);
    }

    #[test]
    fn kill_session_without_id_is_noop() {
        kill_session(None); // must not panic or block
    }

    #[test]
    fn read_ppid_of_self_is_sane() {
        let self_pid = std::process::id();
        let ppid = read_ppid(self_pid).expect("own ppid");
        assert!(ppid >= 1);
    }
}
