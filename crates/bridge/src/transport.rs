//! Spawn agent subprocesses with the host's standard child setup. Shared by all
//! agent backends (this is host infrastructure, like [`crate::process`] and
//! [`crate::proxy`]).

use std::process::Stdio;

use tokio::process::Command;

/// What to launch and how to tweak the child's environment.
pub struct SpawnSpec {
    pub command: String,
    pub args: Vec<String>,
    /// Extra env vars to set (applied after `remove_env`).
    pub extra_env: Vec<(String, String)>,
    /// Env vars to strip from the child.
    pub remove_env: Vec<String>,
}

/// Spawn the agent child: stdin/stdout piped, stderr inherited, `kill_on_drop`.
///
/// The child is placed in its own **session** (new process group, child as
/// leader) so `sid == pgid == child pid`. A new group alone is not enough —
/// `npm exec`/`node` call `setpgid` and escape the tracked group; `setpgid`
/// can't escape the session, so shutdown reaps by session id (see
/// [`crate::process::kill_session`]). `NO_PROXY` is injected (with the LAN
/// ranges) before `extra_env` so local services bypass the global proxy.
pub fn spawn(spec: &SpawnSpec) -> std::io::Result<tokio::process::Child> {
    let mut cmd = Command::new(&spec.command);
    cmd.args(&spec.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);

    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    #[cfg(windows)]
    {
        #[allow(unused_imports)]
        use std::os::windows::process::CommandExt;
        // CREATE_NEW_PROCESS_GROUP (0x200): treat the child's pid as the pgid.
        cmd.creation_flags(0x00000200);
    }

    for k in &spec.remove_env {
        cmd.env_remove(k);
    }

    // Inject LAN exclusions before extra_env so the caller can still override.
    let no_proxy = crate::proxy::build_no_proxy();
    cmd.env("NO_PROXY", &no_proxy).env("no_proxy", &no_proxy);

    for (k, v) in &spec.extra_env {
        cmd.env(k, v);
    }

    cmd.spawn()
}
