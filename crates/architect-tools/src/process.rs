//! Long-running background processes the agent can start, watch, and stop.
//!
//! `run_command` is run-to-completion: it blocks until the process exits.
//! This is for anything that isn't meant to exit — a dev server, a watcher
//! — via [`ProcessRegistry`], constructed once and shared across every turn
//! (unlike [`crate::ToolContext`], which is rebuilt fresh per turn and so
//! cannot hold state that must survive from a `start_process` call to a
//! later `get_process_logs` call — see `crate::tools::process` for the
//! three [`crate::Tool`] impls built on top of this).

use std::{
    collections::HashMap,
    fmt, io,
    path::Path,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, ChildStderr, ChildStdout, Command},
    sync::{
        Mutex, Notify,
        mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    },
};

/// How much of a process's combined stdout/stderr is kept in memory at
/// once — mirrors `run_command`'s `MAX_OUTPUT` cap, but drops from the
/// *front* (oldest content) rather than truncating the tail: for something
/// still running, the most recent output is what matters.
const MAX_LOG: usize = 200_000;

/// How long [`ProcessRegistry::kill_all`] waits for signaled processes to
/// actually report back as no longer running before giving up — bounded so
/// app shutdown can't hang forever on a process that won't die, though in
/// practice `SIGKILL` is near-instant.
const KILL_ALL_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessStatus {
    Running,
    Exited(i32),
    Stopped,
    Failed(String),
}

impl fmt::Display for ProcessStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProcessStatus::Running => write!(f, "running"),
            ProcessStatus::Exited(code) => write!(f, "exited with code {code}"),
            ProcessStatus::Stopped => write!(f, "stopped"),
            ProcessStatus::Failed(message) => write!(f, "failed: {message}"),
        }
    }
}

/// What a [`ProcessRegistry`] reports as it happens — the receiver half
/// handed back by [`ProcessRegistry::new`] is the caller's to forward
/// however it likes. This crate has no knowledge of `apps/desktop`'s
/// `EngineEvent` or any UI at all, the same separation `architect-agent`'s
/// `AgentEvent` keeps from it.
#[derive(Debug, Clone, PartialEq)]
pub enum ProcessEvent {
    Started { id: String, command: String },
    Output { id: String, chunk: String },
    Exited { id: String, status: ProcessStatus },
}

struct Entry {
    command: String,
    log: String,
    status: ProcessStatus,
    /// Signaled by `stop`/`kill_all`; the supervising task (the sole owner
    /// of the real `Child` handle) reacts to it — nothing outside that task
    /// ever touches the child process directly, so there's no lock held
    /// across a `.wait()`/`.kill()` await to race against.
    kill: Arc<Notify>,
}

/// Shared, long-lived home for every process this app has started —
/// constructed once and handed to the `start_process`/`get_process_logs`/
/// `stop_process` tools as an `Arc`, so the same map survives from one
/// tool call to a much later one.
pub struct ProcessRegistry {
    entries: Mutex<HashMap<String, Entry>>,
    next_id: AtomicUsize,
    events: UnboundedSender<ProcessEvent>,
}

impl ProcessRegistry {
    pub fn new() -> (Self, UnboundedReceiver<ProcessEvent>) {
        let (events, receiver) = unbounded_channel();
        (
            Self {
                entries: Mutex::new(HashMap::new()),
                next_id: AtomicUsize::new(1),
                events,
            },
            receiver,
        )
    }

    /// Spawns `command` via `bash -c` and returns its id immediately — the
    /// process keeps running in the background; use [`Self::logs`]/
    /// [`Self::stop`] to interact with it afterward.
    pub async fn start(
        self: &Arc<Self>,
        workspace_root: &Path,
        command: String,
    ) -> Result<String, String> {
        let mut spawn_command = Command::new("bash");
        spawn_command
            .arg("-c")
            .arg(&command)
            .current_dir(workspace_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        {
            // A new process group (pgid = the child's own pid) — what lets
            // `kill_tree` below signal the whole tree `bash -c "..."`
            // spawns (e.g. the real dev-server process `npm run dev`
            // forks), not just the tracked `bash` itself. Without this,
            // stopping only kills the shell wrapper and leaves the actual
            // long-running process orphaned and still running.
            spawn_command.process_group(0);
        }

        let mut child = spawn_command
            .spawn()
            .map_err(|error| format!("could not start the process: {error}"))?;

        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        let id = format!("proc-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let kill = Arc::new(Notify::new());

        self.entries.lock().await.insert(
            id.clone(),
            Entry {
                command: command.clone(),
                log: String::new(),
                status: ProcessStatus::Running,
                kill: kill.clone(),
            },
        );
        let _ = self.events.send(ProcessEvent::Started {
            id: id.clone(),
            command,
        });

        let registry = Arc::clone(self);
        let supervised_id = id.clone();
        tokio::spawn(async move {
            registry
                .supervise(supervised_id, child, stdout, stderr, kill)
                .await;
        });

        Ok(id)
    }

    /// `(command, status, accumulated log)` for a process, or an error
    /// naming the unknown id.
    pub async fn logs(&self, id: &str) -> Result<(String, ProcessStatus, String), String> {
        let entries = self.entries.lock().await;
        let entry = entries
            .get(id)
            .ok_or_else(|| format!("no such process: {id}"))?;
        Ok((
            entry.command.clone(),
            entry.status.clone(),
            entry.log.clone(),
        ))
    }

    /// Requests termination and returns immediately — it does not wait for
    /// the process to actually exit (that would mean either blocking the
    /// agent's turn on a process that ignores `SIGKILL`, which nothing
    /// does but a pathological case, or holding a lock across an
    /// indefinite await; simpler and just as honest to report "requested"
    /// and let a follow-up `get_process_logs` confirm it).
    pub async fn stop(&self, id: &str) -> Result<String, String> {
        let entries = self.entries.lock().await;
        let entry = entries
            .get(id)
            .ok_or_else(|| format!("no such process: {id}"))?;

        match &entry.status {
            ProcessStatus::Running => {
                entry.kill.notify_one();
                Ok(format!(
                    "Stop requested for {id} — call get_process_logs to confirm it exited."
                ))
            }
            other => Ok(format!("{id} is not running ({other})")),
        }
    }

    /// Signals every still-running process to stop and waits (bounded by
    /// [`KILL_ALL_TIMEOUT`]) for them to report back — called once, on app
    /// shutdown. Best-effort: a process that ignores `SIGKILL` (nothing
    /// standard does) would still be abandoned once the timeout elapses.
    pub async fn kill_all(&self) {
        {
            let entries = self.entries.lock().await;
            for entry in entries.values() {
                if entry.status == ProcessStatus::Running {
                    entry.kill.notify_one();
                }
            }
        }

        let deadline = tokio::time::Instant::now() + KILL_ALL_TIMEOUT;
        loop {
            let all_stopped = {
                let entries = self.entries.lock().await;
                entries
                    .values()
                    .all(|entry| entry.status != ProcessStatus::Running)
            };
            if all_stopped || tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Owns the real `Child` handle exclusively for the process's whole
    /// life — reads both output streams and reacts to a kill signal until
    /// the process actually exits, then records the final status.
    async fn supervise(
        self: Arc<Self>,
        id: String,
        mut child: Child,
        stdout: ChildStdout,
        stderr: ChildStderr,
        kill: Arc<Notify>,
    ) {
        let mut stdout_lines = BufReader::new(stdout).lines();
        let mut stderr_lines = BufReader::new(stderr).lines();
        let mut stdout_done = false;
        let mut stderr_done = false;
        let mut killed = false;

        let wait_result: io::Result<std::process::ExitStatus> = loop {
            tokio::select! {
                line = stdout_lines.next_line(), if !stdout_done => {
                    match line {
                        Ok(Some(line)) => self.append_output(&id, line).await,
                        _ => stdout_done = true,
                    }
                }
                line = stderr_lines.next_line(), if !stderr_done => {
                    match line {
                        Ok(Some(line)) => self.append_output(&id, line).await,
                        _ => stderr_done = true,
                    }
                }
                () = kill.notified() => {
                    killed = true;
                    kill_tree(&mut child);
                }
                result = child.wait() => {
                    break result;
                }
            }
        };

        // The child has exited — drain any output still buffered in the
        // pipes. `select!` can resolve `child.wait()` before the final
        // `next_line()` gets polled; a tiny, fast-exiting process like
        // `echo hello` is exactly that case, and the uninterested branch
        // leaves the last line in the pipe, silently dropped. Once the
        // child is gone its write ends are closed, so reading to EOF here
        // terminates.
        if !stdout_done {
            while let Ok(Some(line)) = stdout_lines.next_line().await {
                self.append_output(&id, line).await;
            }
        }
        if !stderr_done {
            while let Ok(Some(line)) = stderr_lines.next_line().await {
                self.append_output(&id, line).await;
            }
        }

        let status = match wait_result {
            Ok(_) if killed => ProcessStatus::Stopped,
            Ok(exit_status) => match exit_status.code() {
                Some(code) => ProcessStatus::Exited(code),
                None => ProcessStatus::Failed("terminated by signal".to_owned()),
            },
            Err(error) => ProcessStatus::Failed(format!("wait failed: {error}")),
        };

        if let Some(entry) = self.entries.lock().await.get_mut(&id) {
            entry.status = status.clone();
        }
        let _ = self.events.send(ProcessEvent::Exited { id, status });
    }

    async fn append_output(&self, id: &str, line: String) {
        {
            let mut entries = self.entries.lock().await;
            if let Some(entry) = entries.get_mut(id) {
                entry.log.push_str(&line);
                entry.log.push('\n');
                if entry.log.len() > MAX_LOG {
                    let excess = entry.log.len() - MAX_LOG;
                    let cut = entry
                        .log
                        .char_indices()
                        .map(|(i, _)| i)
                        .find(|&i| i >= excess)
                        .unwrap_or(entry.log.len());
                    entry.log.drain(..cut);
                }
            }
        }
        let _ = self.events.send(ProcessEvent::Output {
            id: id.to_owned(),
            chunk: line,
        });
    }
}

/// Kills the whole process tree the child spawned, not just the tracked
/// PID. `bash -c "npm run dev"` forks a real subprocess (`npm`, which
/// forks `node`) that a plain `Child::kill`/`start_kill` — SIGKILL to a
/// single PID — never reaches; it would die (the wrapper), and the actual
/// long-running process would be silently orphaned, still running.
/// `start()` places each child in its own process group
/// (`process_group(0)`) specifically so this can signal the entire group
/// at once via a negative pid, the standard Unix idiom for "kill this and
/// everything it spawned."
#[cfg(unix)]
fn kill_tree(child: &mut Child) {
    if let Some(pid) = child.id() {
        // SAFETY: `kill(2)` with a negative pid signals every process in
        // that process group. `pid` is a valid, currently-live process id
        // just read from the `Child` handle; sending a signal to it (or
        // its group) cannot violate memory safety, only fail harmlessly
        // (e.g. `ESRCH` if it already exited) — libc's binding surfaces
        // that as a plain ignorable return code, not UB.
        unsafe {
            libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn kill_tree(child: &mut Child) {
    let _ = child.start_kill();
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn workspace() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[tokio::test]
    async fn starting_a_process_captures_its_output() {
        let dir = workspace();
        let (registry, _events) = ProcessRegistry::new();
        let registry = Arc::new(registry);

        let id = registry
            .start(dir.path(), "echo hello".to_owned())
            .await
            .unwrap();

        // The reader task races the assertion — give it a moment.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let (command, status, log) = registry.logs(&id).await.unwrap();
        assert_eq!(command, "echo hello");
        assert!(matches!(status, ProcessStatus::Exited(0)), "got: {status}");
        assert!(log.contains("hello"), "got: {log}");
    }

    #[tokio::test]
    async fn get_logs_on_an_unknown_id_is_a_clear_error() {
        let (registry, _events) = ProcessRegistry::new();

        let error = registry.logs("proc-999").await.unwrap_err();
        assert!(error.contains("no such process"), "got: {error}");
    }

    #[tokio::test]
    async fn stop_on_an_unknown_id_is_a_clear_error() {
        let (registry, _events) = ProcessRegistry::new();

        let error = registry.stop("proc-999").await.unwrap_err();
        assert!(error.contains("no such process"), "got: {error}");
    }

    #[tokio::test]
    async fn stopping_a_running_process_actually_kills_it() {
        let dir = workspace();
        let (registry, _events) = ProcessRegistry::new();
        let registry = Arc::new(registry);

        let id = registry
            .start(dir.path(), "sleep 30".to_owned())
            .await
            .unwrap();

        let started = tokio::time::Instant::now();
        registry.stop(&id).await.unwrap();

        // Poll rather than a fixed sleep: proves the kill was fast without
        // hard-coding exactly how fast.
        loop {
            let (_, status, _) = registry.logs(&id).await.unwrap();
            if status != ProcessStatus::Running {
                assert_eq!(status, ProcessStatus::Stopped, "got: {status}");
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "stop_process should kill a sleeping process almost instantly, \
                 not let it run anywhere close to its own 30s sleep"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// The actual bug report this fix addresses: `bash -c "npm run dev"`
    /// forks a real subprocess (a grandchild of the tracked `bash`) — a
    /// naive single-PID kill only reaches `bash` itself, leaving the real
    /// long-running process (here, a background `sleep`) orphaned and
    /// still running, which is exactly "the process continues" the report
    /// described. `process_group(0)` + `kill_tree`'s group-wide signal is
    /// what actually reaches it.
    #[tokio::test]
    #[cfg(unix)]
    async fn stopping_a_process_also_kills_the_children_it_spawned() {
        let dir = workspace();
        let (registry, _events) = ProcessRegistry::new();
        let registry = Arc::new(registry);

        // Backgrounds a real child of the tracked `bash`, then prints its
        // pid and waits on it — a shell shape close to what a real dev
        // server launcher (npm -> node) does: the thing that actually
        // needs to be killed is not the tracked process itself.
        let id = registry
            .start(dir.path(), "sleep 30 & echo $!; wait".to_owned())
            .await
            .unwrap();

        let child_pid: u32 = loop {
            let (_, _, log) = registry.logs(&id).await.unwrap();
            if let Some(pid) = log.lines().next().and_then(|line| line.trim().parse().ok()) {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };

        registry.stop(&id).await.unwrap();

        let started = tokio::time::Instant::now();
        loop {
            let (_, status, _) = registry.logs(&id).await.unwrap();
            if status != ProcessStatus::Running {
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(5));
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // `kill -0` (SIGNAL 0) sends nothing — it only reports whether the
        // pid is still alive. A non-zero return means "no such process."
        let still_alive = unsafe { libc::kill(child_pid as libc::pid_t, 0) } == 0;
        assert!(
            !still_alive,
            "the background child sleep (pid {child_pid}) should have been \
             killed along with the shell that spawned it, not orphaned"
        );
    }

    #[tokio::test]
    async fn stopping_an_already_exited_process_says_so_instead_of_erroring() {
        let dir = workspace();
        let (registry, _events) = ProcessRegistry::new();
        let registry = Arc::new(registry);

        let id = registry.start(dir.path(), "true".to_owned()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;

        let message = registry.stop(&id).await.unwrap();
        assert!(message.contains("not running"), "got: {message}");
    }

    #[tokio::test]
    async fn kill_all_stops_every_running_process() {
        let dir = workspace();
        let (registry, _events) = ProcessRegistry::new();
        let registry = Arc::new(registry);

        let a = registry
            .start(dir.path(), "sleep 30".to_owned())
            .await
            .unwrap();
        let b = registry
            .start(dir.path(), "sleep 30".to_owned())
            .await
            .unwrap();

        registry.kill_all().await;

        let (_, status_a, _) = registry.logs(&a).await.unwrap();
        let (_, status_b, _) = registry.logs(&b).await.unwrap();
        assert_eq!(status_a, ProcessStatus::Stopped);
        assert_eq!(status_b, ProcessStatus::Stopped);
    }

    #[tokio::test]
    async fn a_process_that_exits_on_its_own_is_reported_as_exited_not_stopped() {
        let dir = workspace();
        let (registry, mut events) = ProcessRegistry::new();
        let registry = Arc::new(registry);

        registry
            .start(dir.path(), "exit 3".to_owned())
            .await
            .unwrap();

        let mut saw_exit = false;
        while let Some(event) = events.recv().await {
            if let ProcessEvent::Exited { status, .. } = event {
                assert_eq!(status, ProcessStatus::Exited(3));
                saw_exit = true;
                break;
            }
        }
        assert!(saw_exit, "expected a ProcessEvent::Exited");
    }
}
