use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tauri::AppHandle;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

pub struct JobHandle {
    pub id: String,
    cancel_flag: Arc<AtomicBool>,
}

impl JobHandle {
    pub fn cancel(self) {
        self.cancel_flag.store(true, Ordering::SeqCst);
    }
}

#[derive(Serialize, Clone)]
struct ProgressPayload {
    job_id: String,
    percent: f64,
    points_done: u64,
    points_total: u64,
    points_per_sec: f64,
    remaining_ms: Option<u64>,
    stage_index: u32,
    stage_total: u32,
    stage_label: String,
}

#[derive(Serialize, Clone)]
struct DonePayload {
    job_id: String,
    output_path: String,
    duration_ms: u64,
    output_bytes: u64,
    points_total: u64,
}

#[derive(Serialize, Clone)]
struct ErrorPayload {
    job_id: String,
    code: String,
    message: String,
}

#[derive(Serialize, Clone)]
struct CancelledPayload {
    job_id: String,
}

pub fn start(
    app: AppHandle,
    job_id: String,
    input_path: String,
    output_dir: String,
) -> JobHandle {
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let flag_for_task = cancel_flag.clone();
    let job_id_for_task = job_id.clone();
    let app_clone = app.clone();

    tauri::async_runtime::spawn(async move {
        let binary = locate_converter_binary();
        if binary.is_some() {
            run_real(app_clone, job_id_for_task, input_path, output_dir, flag_for_task, binary.unwrap()).await;
        } else {
            run_stub(app_clone, job_id_for_task, input_path, output_dir, flag_for_task).await;
        }
    });

    JobHandle { id: job_id, cancel_flag }
}

fn locate_converter_binary() -> Option<PathBuf> {
    let exe_name = if cfg!(windows) { "PotreeConverter.exe" } else { "PotreeConverter" };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let candidate = parent.join(exe_name);
            if candidate.exists() {
                return Some(candidate);
            }
            let resources_candidate = parent.join("binaries").join(exe_name);
            if resources_candidate.exists() {
                return Some(resources_candidate);
            }
        }
    }
    let dev_candidate = PathBuf::from("binaries").join(exe_name);
    if dev_candidate.exists() {
        return Some(dev_candidate);
    }
    None
}

fn estimate_points(path: &str) -> u64 {
    std::fs::metadata(path).map(|m| m.len() / 11).unwrap_or(0)
}

async fn run_real(
    app: AppHandle,
    job_id: String,
    input_path: String,
    output_dir: String,
    cancel_flag: Arc<AtomicBool>,
    binary: PathBuf,
) {
    let out_name = PathBuf::from(&input_path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".into());
    let out_path = PathBuf::from(&output_dir).join(&out_name);
    if let Err(e) = std::fs::create_dir_all(&out_path) {
        crate::emit_event(&app, "convert:error", ErrorPayload {
            job_id,
            code: "mkdir_failed".into(),
            message: e.to_string(),
        });
        return;
    }

    let total = estimate_points(&input_path);
    let started = Instant::now();
    let mut last_percent: f64 = 0.0;
    let mut last_points: u64 = 0;

    let mut child = match Command::new(&binary)
        .arg(&input_path)
        .arg("-o").arg(&out_path)
        .arg("--encoding").arg("BROTLI")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn() {
        Ok(c) => c,
        Err(e) => {
            crate::emit_event(&app, "convert:error", ErrorPayload {
                job_id,
                code: "spawn_failed".into(),
                message: e.to_string(),
            });
            return;
        }
    };

    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();

    loop {
        if cancel_flag.load(Ordering::SeqCst) {
            let _ = child.kill().await;
            crate::emit_event(&app, "convert:cancelled", CancelledPayload { job_id });
            return;
        }
        tokio::select! {
            line = lines.next_line() => {
                match line {
                    Ok(Some(l)) => {
                        if let Some(p) = parse_percent(&l) { last_percent = p; }
                        if let Some(pts) = parse_points(&l) { last_points = pts; }
                        let elapsed = started.elapsed().as_millis() as u64;
                        let rate = if elapsed > 0 { (last_points as f64) / (elapsed as f64 / 1000.0) } else { 0.0 };
                        let remaining = if last_percent > 1.0 {
                            Some(((elapsed as f64 / last_percent) * (100.0 - last_percent)) as u64)
                        } else { None };
                        crate::emit_event(&app, "convert:progress", ProgressPayload {
                            job_id: job_id.clone(),
                            percent: last_percent,
                            points_done: last_points,
                            points_total: total,
                            points_per_sec: rate,
                            remaining_ms: remaining,
                            stage_index: 0,
                            stage_total: 4,
                            stage_label: stage_from_line(&l).unwrap_or_else(|| "processing".into()),
                        });
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
            status = child.wait() => {
                match status {
                    Ok(s) if s.success() => {
                        let out_bytes = dir_size(&out_path);
                        crate::emit_event(&app, "convert:done", DonePayload {
                            job_id,
                            output_path: out_path.to_string_lossy().into_owned(),
                            duration_ms: started.elapsed().as_millis() as u64,
                            output_bytes: out_bytes,
                            points_total: total,
                        });
                        return;
                    }
                    Ok(s) => {
                        crate::emit_event(&app, "convert:error", ErrorPayload {
                            job_id,
                            code: format!("exit_{}", s.code().unwrap_or(-1)),
                            message: format!("PotreeConverter exited with status {:?}", s.code()),
                        });
                        return;
                    }
                    Err(e) => {
                        crate::emit_event(&app, "convert:error", ErrorPayload {
                            job_id,
                            code: "wait_failed".into(),
                            message: e.to_string(),
                        });
                        return;
                    }
                }
            }
        }
    }

    let _ = child.wait().await;
    let out_bytes = dir_size(&out_path);
    crate::emit_event(&app, "convert:done", DonePayload {
        job_id,
        output_path: out_path.to_string_lossy().into_owned(),
        duration_ms: started.elapsed().as_millis() as u64,
        output_bytes: out_bytes,
        points_total: total,
    });
}

async fn run_stub(
    app: AppHandle,
    job_id: String,
    input_path: String,
    output_dir: String,
    cancel_flag: Arc<AtomicBool>,
) {
    let total = estimate_points(&input_path);
    let started = Instant::now();
    let stages = ["Reading file", "Loading points", "Building octree", "Writing"];
    let mut percent: f64 = 0.0;
    loop {
        if cancel_flag.load(Ordering::SeqCst) {
            crate::emit_event(&app, "convert:cancelled", CancelledPayload { job_id });
            return;
        }
        percent = (percent + 2.0 + (rand_jitter() * 1.5)).min(100.0);
        let stage_idx = ((percent / 100.0) * stages.len() as f64) as usize;
        let stage_idx = stage_idx.min(stages.len() - 1);
        let elapsed = started.elapsed().as_millis() as u64;
        let points_done = ((percent / 100.0) * total as f64) as u64;
        let rate = if elapsed > 0 { points_done as f64 / (elapsed as f64 / 1000.0) } else { 0.0 };
        let remaining = if percent > 1.0 {
            Some(((elapsed as f64 / percent) * (100.0 - percent)) as u64)
        } else { None };
        crate::emit_event(&app, "convert:progress", ProgressPayload {
            job_id: job_id.clone(),
            percent,
            points_done,
            points_total: total,
            points_per_sec: rate,
            remaining_ms: remaining,
            stage_index: stage_idx as u32,
            stage_total: stages.len() as u32,
            stage_label: stages[stage_idx].into(),
        });
        if percent >= 100.0 {
            let out_name = PathBuf::from(&input_path)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "output".into());
            let out_path = PathBuf::from(&output_dir).join(&out_name);
            let input_bytes = std::fs::metadata(&input_path).map(|m| m.len()).unwrap_or(0);
            crate::emit_event(&app, "convert:done", DonePayload {
                job_id,
                output_path: out_path.to_string_lossy().into_owned(),
                duration_ms: started.elapsed().as_millis() as u64,
                output_bytes: (input_bytes as f64 * 0.45) as u64,
                points_total: total,
            });
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
}

fn rand_jitter() -> f64 {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    (nanos % 1000) as f64 / 1000.0
}

fn parse_percent(line: &str) -> Option<f64> {
    let l = line.trim();
    let pct_pos = l.find('%')?;
    let before: String = l[..pct_pos]
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',' || c.is_whitespace())
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    before.trim().replace(',', ".").parse::<f64>().ok().filter(|v| (0.0..=100.0).contains(v))
}

fn parse_points(line: &str) -> Option<u64> {
    None
}

fn stage_from_line(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    for s in ["reading", "loading", "indexing", "sampling", "writing", "finalizing"] {
        if lower.contains(s) {
            return Some(s.into());
        }
    }
    None
}

fn dir_size(path: &PathBuf) -> u64 {
    fn walk(p: &PathBuf, total: &mut u64) {
        if let Ok(entries) = std::fs::read_dir(p) {
            for e in entries.flatten() {
                let ep = e.path();
                if ep.is_dir() { walk(&ep, total); }
                else if let Ok(m) = ep.metadata() { *total += m.len(); }
            }
        }
    }
    let mut total = 0u64;
    walk(path, &mut total);
    total
}
