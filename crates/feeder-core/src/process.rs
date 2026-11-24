use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus};

pub(crate) fn start_in_new_session(cmd: &mut Command) {
    unsafe {
        cmd.pre_exec(|| if libc::setsid() == -1 { Err(io::Error::last_os_error()) } else { Ok(()) });
    }
}

pub(crate) fn is_alive(pid: i64) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else { return false };
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

pub(crate) fn wait_for_exit(id: u32) -> io::Result<()> {
    wait_without_reaping(id, libc::WEXITED | libc::WNOWAIT).map(|_| ())
}

pub(crate) fn has_exited(id: u32) -> io::Result<bool> {
    match wait_without_reaping(id, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT) {
        Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(false),
        result => result,
    }
}

fn wait_without_reaping(id: u32, flags: libc::c_int) -> io::Result<bool> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    if unsafe { libc::waitid(libc::P_PID, id, &mut info, flags) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { info.si_pid() } != 0)
}

pub(crate) fn terminate_group(child: &mut Child) -> io::Result<ExitStatus> {
    if let Ok(group) = libc::pid_t::try_from(child.id())
        && group > 0
    {
        unsafe { libc::kill(-group, libc::SIGKILL) };
    }
    let _ = child.kill();
    child.wait()
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn terminating_the_group_closes_pipes_held_by_its_children() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "/bin/sleep 5 & printf 'provider finished\\n'"])
            .process_group(0)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let started = Instant::now();
        while !has_exited(child.id()).unwrap() && started.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(terminate_group(&mut child).unwrap().success());
        let mut output = String::new();
        child.stdout.take().unwrap().read_to_string(&mut output).unwrap();
        assert_eq!(output, "provider finished\n");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn a_new_session_makes_the_child_its_own_group_leader() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "ps -o pgid= -p $$"]).stdout(Stdio::piped());
        start_in_new_session(&mut cmd);
        let child = cmd.spawn().unwrap();
        let id = child.id();
        let output = child.wait_with_output().unwrap();
        let group: u32 = String::from_utf8_lossy(&output.stdout).trim().parse().unwrap();
        assert_eq!(group, id, "the child should lead its own process group");
    }

    #[test]
    fn liveness_tracks_a_real_process() {
        let mut child = Command::new("/bin/sleep").arg("5").spawn().unwrap();
        let pid = i64::from(child.id());
        assert!(is_alive(pid));
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!is_alive(pid), "pid {pid} was reaped but still reports alive");
        assert!(!is_alive(1_i64 << 40), "a pid outside pid_t's range is never alive");
    }
}
