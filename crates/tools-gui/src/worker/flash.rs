use super::{WorkerResult, emit_log, summary_payload};
use anyhow::{Context, Result, bail, ensure};
use common::flash::FlashStep;
use flash_script::{FlashHost, PortChoice};
use serde_json::json;
use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static PROMPT_ID: AtomicU64 = AtomicU64::new(1);

pub(super) fn validate(path: &str) -> Result<WorkerResult> {
    let (_, resolved) = flash_script::load(Path::new(path))?;
    let steps: Vec<_> = resolved
        .steps
        .iter()
        .map(|step| {
            json!({
                "kind": step.kind(),
                "description": describe_tr(&step.step),
                "file": step.file.as_ref().map(|file| file.display().to_string()),
            })
        })
        .collect();
    summary_payload(
        tr!("flash-script-steps", "count" => steps.len()),
        json!({
            "path": path,
            "name": resolved.name,
            "steps": steps,
        }),
    )
}

pub(super) fn run(path: &str) -> Result<WorkerResult> {
    let (_, resolved) = flash_script::load(Path::new(path))?;
    let mut host = WorkerHost { last_bucket: 0 };
    let summary = flash_script::run(&resolved, &mut host)?;
    Ok(WorkerResult {
        ok: true,
        summary: tr!(
            "flash-script-finished",
            "done" => summary.completed,
            "total" => summary.total,
        ),
        payload: Some(json!({
            "completed": summary.completed,
            "total": summary.total,
        })),
    })
}

fn describe_tr(step: &FlashStep) -> String {
    match step {
        FlashStep::WaitVcom { timeout_secs } => {
            tr!("flash-step-wait-vcom", "secs" => timeout_secs)
        }
        FlashStep::VcomUpload { port, file, .. } => {
            tr!("flash-step-vcom-upload", "file" => file.clone(), "port" => port.clone())
        }
        FlashStep::WaitFastboot { timeout_secs } => {
            tr!("flash-step-wait-fastboot", "secs" => timeout_secs)
        }
        FlashStep::FastbootAssert { variable, value } => {
            tr!("flash-step-assert", "variable" => variable.clone(), "value" => value.clone())
        }
        FlashStep::FastbootFlash { partition, file } => {
            tr!("flash-step-flash", "file" => file.clone(), "partition" => partition.clone())
        }
        FlashStep::FastbootErase { partition } => {
            tr!("flash-step-erase", "partition" => partition.clone())
        }
        FlashStep::FastbootOem { command } => {
            tr!("flash-step-oem", "command" => command.clone())
        }
        FlashStep::FastbootReboot { mode } => tr!(
            "flash-step-reboot",
            "mode" => flash_script::mode_name(*mode),
        ),
        FlashStep::Alert { .. } => tr!("flash-step-alert"),
        FlashStep::Sleep { millis } => tr!("flash-step-sleep", "millis" => millis),
    }
}

fn emit_line(value: serde_json::Value) {
    let line = serde_json::to_string(&value).expect("serializing worker event");
    // Worker stdout must stay line-buffered JSON; lock to keep events atomic.
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "{line}");
    let _ = lock.flush();
}

fn emit_progress(step: usize, total: usize, label: &str) {
    emit_line(json!({
        "t": "progress",
        "step": step,
        "total": total,
        "label": label,
    }));
}

fn next_prompt_id() -> u64 {
    PROMPT_ID.fetch_add(1, Ordering::Relaxed)
}

fn emit_prompt(id: u64, kind: &str, message: &str, options: &[PortChoice]) {
    emit_line(json!({
        "t": "prompt",
        "id": id,
        "kind": kind,
        "message": message,
        "options": options,
    }));
}

fn read_answer(expected_id: u64) -> Result<serde_json::Value> {
    let stdin = std::io::stdin();
    let mut line = String::new();
    let read = stdin.lock().read_line(&mut line);
    let bytes = read.context("reading worker answer")?;
    if bytes == 0 {
        bail!("worker input closed before the prompt was answered");
    }
    let answer: serde_json::Value =
        serde_json::from_str(line.trim()).context("parsing worker answer")?;
    if answer.get("t").and_then(|t| t.as_str()) != Some("answer") {
        bail!("unexpected worker answer message");
    }
    if answer.get("id").and_then(|id| id.as_u64()) != Some(expected_id) {
        bail!("worker answer id mismatch");
    }
    Ok(answer)
}

struct WorkerHost {
    last_bucket: u64,
}

impl FlashHost for WorkerHost {
    fn log(&mut self, message: &str) {
        emit_log(message);
    }

    fn progress(&mut self, step: usize, total: usize, step_ref: &FlashStep) {
        emit_progress(step, total, &describe_tr(step_ref));
    }

    fn transfer(&mut self, sent: u64, total: u64) {
        if flash_script::report_bucket(sent, total, &mut self.last_bucket) {
            emit_log(&tr!("progress-bytes", "sent" => sent, "total" => total));
        }
    }

    fn select_port(&mut self, ports: &[PortChoice]) -> Result<String> {
        let id = next_prompt_id();
        emit_prompt(id, "select_port", &tr!("flash-select-port"), ports);
        let answer = read_answer(id)?;
        let choice = answer
            .get("choice")
            .and_then(|choice| choice.as_str())
            .context("no port selected")?;
        ensure!(
            ports.iter().any(|port| port.name == choice),
            "selected port {choice} is not one of the offered ports"
        );
        Ok(choice.to_owned())
    }

    fn alert(&mut self, message: &str) -> Result<()> {
        let id = next_prompt_id();
        emit_prompt(id, "alert", message, &[]);
        read_answer(id)?;
        Ok(())
    }
}
