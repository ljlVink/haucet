use crate::worker::{self, JobOp, JobSpec, WorkerResult};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::io::{self, BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobProgress {
    pub step: usize,
    pub total: usize,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobPrompt {
    pub id: u64,
    pub kind: String,
    pub message: String,
    pub options: Vec<PromptChoice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PromptChoice {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub enum JobEvent {
    Log(String),
    Progress(JobProgress),
    Prompt(JobPrompt),
    Done(JobResult),
}
#[derive(Debug, Clone)]
pub struct JobResult {
    pub ok: bool,
    pub cancelled: bool,
    pub summary: String,
    pub payload: Option<serde_json::Value>,
}

pub struct RunningJob {
    worker: ProcessTreeChild,
    rx: Receiver<JobEvent>,
    cancelled: Arc<AtomicBool>,
    started: Instant,
    stdin: Mutex<Option<ChildStdin>>,
    pub op: JobOp,
}

impl RunningJob {
    pub fn poll(&mut self) -> Option<JobEvent> {
        self.rx.try_recv().ok()
    }

    pub fn cancel(&mut self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.worker.terminate();
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn answer_prompt(&self, id: u64, choice: Option<&str>) {
        let answer = serde_json::json!({
            "t": "answer",
            "id": id,
            "choice": choice,
        });
        if let Ok(mut guard) = self.stdin.lock()
            && let Some(stdin) = guard.as_mut()
        {
            let _ = writeln!(stdin, "{answer}");
            let _ = stdin.flush();
        }
    }
}

impl Drop for RunningJob {
    fn drop(&mut self) {
        self.worker.terminate();
    }
}

pub fn start(op: JobOp) -> Result<RunningJob> {
    let spec = JobSpec { op: op.clone() };
    let json = serde_json::to_string(&spec)?;
    let mut command =
        Command::new(std::env::current_exe().context(tr!("locate-executable-error"))?);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
        .env(worker::WORKER_ENV, "1")
        .env(crate::i18n::LANGUAGE_ENV, crate::i18n::language().tag())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut worker = ProcessTreeChild::spawn(&mut command).context(tr!("start-worker-error"))?;

    let pipes = (|| -> Result<_> {
        let mut stdin = worker
            .child
            .stdin
            .take()
            .context(tr!("worker-stdin-error"))?;
        stdin
            .write_all(json.as_bytes())
            .and_then(|()| stdin.write_all(b"\n"))
            .and_then(|()| stdin.flush())
            .context(tr!("worker-send-error"))?;

        let stdout = worker
            .child
            .stdout
            .take()
            .context(tr!("worker-stdout-error"))?;
        let stderr = worker
            .child
            .stderr
            .take()
            .context(tr!("worker-stderr-error"))?;
        Ok((stdin, stdout, stderr))
    })();
    let (stdin, stdout, stderr) = match pipes {
        Ok(pipes) => pipes,
        Err(error) => {
            worker.terminate();
            return Err(error);
        }
    };

    let (tx, rx) = mpsc::channel::<JobEvent>();
    let tx_err = tx.clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_reader = cancelled.clone();

    thread::spawn(move || {
        let mut saw_result = false;
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            match value.get("t").and_then(|t| t.as_str()) {
                Some("log") => {
                    if let Some(text) = value.get("s").and_then(|s| s.as_str()) {
                        let _ = tx.send(JobEvent::Log(text.to_owned()));
                    }
                }
                Some("progress") => {
                    let progress = JobProgress {
                        step: value.get("step").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
                        total: value.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
                        label: value
                            .get("label")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_owned(),
                    };
                    let _ = tx.send(JobEvent::Progress(progress));
                }
                Some("prompt") => {
                    if let Some(prompt) = parse_prompt(&value) {
                        let _ = tx.send(JobEvent::Prompt(prompt));
                    }
                }
                Some("result") => {
                    let result = serde_json::from_value::<WorkerResult>(value)
                        .map(|r| JobResult {
                            ok: r.ok,
                            cancelled: false,
                            summary: r.summary,
                            payload: r.payload,
                        })
                        .unwrap_or_else(|_| JobResult {
                            ok: false,
                            cancelled: false,
                            summary: tr!("worker-result-invalid"),
                            payload: None,
                        });
                    let _ = tx.send(JobEvent::Done(result));
                    saw_result = true;
                    break;
                }
                _ => {}
            }
        }
        if !saw_result {
            let cancelled = cancelled_reader.load(Ordering::SeqCst);
            let _ = tx.send(JobEvent::Done(JobResult {
                ok: false,
                cancelled,
                summary: if cancelled {
                    tr!("worker-cancelled-partial")
                } else {
                    tr!("worker-exited-no-result")
                },
                payload: None,
            }));
        }
    });

    thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let cleaned = crate::util::strip_ansi(&line);
            let trimmed = cleaned.trim_end();
            if !trimmed.is_empty() {
                let _ = tx_err.send(JobEvent::Log(trimmed.to_owned()));
            }
        }
    });

    Ok(RunningJob {
        worker,
        rx,
        cancelled,
        started: Instant::now(),
        stdin: Mutex::new(Some(stdin)),
        op,
    })
}

fn parse_prompt(value: &serde_json::Value) -> Option<JobPrompt> {
    let id = value.get("id")?.as_u64()?;
    Some(JobPrompt {
        id,
        kind: value
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned(),
        message: value
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned(),
        options: serde_json::from_value::<Vec<PromptChoice>>(
            value.get("options").cloned().unwrap_or_default(),
        )
        .unwrap_or_default(),
    })
}

struct ProcessTreeChild {
    child: Child,
    containment: ProcessContainment,
    terminated: bool,
}

impl ProcessTreeChild {
    fn spawn(command: &mut Command) -> io::Result<Self> {
        let (child, containment) = ProcessContainment::spawn(command)?;
        Ok(Self {
            child,
            containment,
            terminated: false,
        })
    }

    fn terminate(&mut self) {
        if std::mem::replace(&mut self.terminated, true) {
            return;
        }
        self.containment.terminate();
        // Keep this fallback for platforms where the process has already left
        // its containment object, then always reap the direct child.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ProcessTreeChild {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(unix)]
struct ProcessContainment {
    process_group: Option<libc::pid_t>,
}

#[cfg(unix)]
impl ProcessContainment {
    fn spawn(command: &mut Command) -> io::Result<(Child, Self)> {
        use std::os::unix::process::CommandExt;

        command.process_group(0);
        let mut child = command.spawn()?;
        let process_group = match libc::pid_t::try_from(child.id()) {
            Ok(process_group) => process_group,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::other("child process ID exceeds pid_t"));
            }
        };
        Ok((
            child,
            Self {
                process_group: Some(process_group),
            },
        ))
    }

    fn terminate(&mut self) {
        if let Some(process_group) = self.process_group.take() {
            // SAFETY: `process_group` is the positive PID returned for the
            // child that `process_group(0)` made the leader of its own group.
            unsafe {
                libc::kill(-process_group, libc::SIGKILL);
            }
        }
    }
}

#[cfg(windows)]
struct ProcessContainment {
    job: Option<std::os::windows::io::OwnedHandle>,
}

#[cfg(windows)]
impl ProcessContainment {
    fn spawn(command: &mut Command) -> io::Result<(Child, Self)> {
        use std::mem::size_of;
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        // SAFETY: null security attributes/name create a private job object.
        let raw_job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw_job.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw_job` is a newly-created, owned Win32 handle.
        let job = unsafe { OwnedHandle::from_raw_handle(raw_job) };

        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `limits` has the structure and size required by the selected
        // information class, and the job handle remains alive for the call.
        let configured = unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            return Err(io::Error::last_os_error());
        }

        let mut child = command.spawn()?;
        // The GUI worker blocks while reading its job from stdin. Assignment
        // therefore completes before it can launch any EROFS subprocess.
        // SAFETY: both handles are valid and remain alive for this call.
        let assigned =
            unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) };
        if assigned == 0 {
            let error = io::Error::last_os_error();
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }

        Ok((child, Self { job: Some(job) }))
    }

    fn terminate(&mut self) {
        // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE terminates every process still in
        // the job, including grandchildren spawned by external EROFS tools.
        drop(self.job.take());
    }
}

#[cfg(not(any(unix, windows)))]
struct ProcessContainment;

#[cfg(not(any(unix, windows)))]
impl ProcessContainment {
    fn spawn(command: &mut Command) -> io::Result<(Child, Self)> {
        command.spawn().map(|child| (child, Self))
    }

    fn terminate(&mut self) {}
}
