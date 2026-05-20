use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn hide_console(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}

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
    let flag = cancel_flag.clone();
    let id = job_id.clone();
    let app_clone = app.clone();

    tauri::async_runtime::spawn(async move {
        run_pipeline(app_clone, id, input_path, output_dir, flag).await;
    });

    JobHandle { id: job_id, cancel_flag }
}

async fn run_pipeline(
    app: AppHandle,
    job_id: String,
    input_path: String,
    output_dir: String,
    cancel: Arc<AtomicBool>,
) {
    let potree = match locate_binary("PotreeConverter") {
        Some(p) => p,
        None => {
            run_stub(app, job_id, input_path, output_dir, cancel).await;
            return;
        }
    };

    let lower = input_path.to_lowercase();
    let needs_e57_translate = lower.ends_with(".e57");

    let pipeline_started = Instant::now();
    let mut temp_las: Option<PathBuf> = None;

    // STAGE 1: E57 → LAS via PDAL (only if input is E57)
    let las_for_potree: PathBuf = if needs_e57_translate {
        let pdal = match locate_binary("pdal") {
            Some(p) => p,
            None => {
                emit_error(&app, &job_id, "pdal_missing",
                    "PDAL binary not found. E57 input requires bundled PDAL. Found PotreeConverter only.");
                return;
            }
        };
        let temp = std::env::temp_dir().join(format!("scan2bim-{}.las", &job_id));
        emit_progress(&app, &job_id, 0.0, 0, estimate_points(&input_path), 0, 2,
            "Reading E57", pipeline_started);
        match run_pdal_translate(&app, &job_id, &pdal, &input_path, &temp, &cancel, pipeline_started).await {
            Ok(()) => {
                temp_las = Some(temp.clone());
                temp
            }
            Err(stage_err) => {
                cleanup_temp(temp_las.as_deref());
                match stage_err {
                    StageError::Cancelled => emit_cancelled(&app, &job_id),
                    StageError::Failed(code, msg) => emit_error(&app, &job_id, &code, &msg),
                }
                return;
            }
        }
    } else {
        PathBuf::from(&input_path)
    };

    // STAGE 2: LAS → Potree octree via PotreeConverter
    let out_name = Path::new(&input_path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".into());
    let out_path = PathBuf::from(&output_dir).join(&out_name);
    if let Err(e) = std::fs::create_dir_all(&out_path) {
        cleanup_temp(temp_las.as_deref());
        emit_error(&app, &job_id, "mkdir_failed", &e.to_string());
        return;
    }

    let stage_index_potree = if needs_e57_translate { 1 } else { 0 };
    let stage_total = if needs_e57_translate { 2 } else { 1 };
    let progress_base = if needs_e57_translate { 50.0 } else { 0.0 };
    let progress_scale = if needs_e57_translate { 0.5 } else { 1.0 };

    let total_points = estimate_points(&input_path);

    let pc_result = run_potree_converter(
        &app, &job_id, &potree, &las_for_potree, &out_path, &cancel,
        pipeline_started, total_points,
        stage_index_potree, stage_total, progress_base, progress_scale,
    ).await;

    cleanup_temp(temp_las.as_deref());

    match pc_result {
        Ok(()) => {
            let out_bytes = dir_size(&out_path);
            if out_bytes < 8192 {
                emit_error(&app, &job_id, "empty_output",
                    &format!("PotreeConverter exited 0 but produced only {out_bytes} bytes \u{2014} likely failed to parse the LAS (see terminal logs)"));
                return;
            }
            let _ = app.emit("convert:done", DonePayload {
                job_id,
                output_path: out_path.to_string_lossy().into_owned(),
                duration_ms: pipeline_started.elapsed().as_millis() as u64,
                output_bytes: out_bytes,
                points_total: total_points,
            });
        }
        Err(StageError::Cancelled) => emit_cancelled(&app, &job_id),
        Err(StageError::Failed(code, msg)) => emit_error(&app, &job_id, &code, &msg),
    }
}

enum StageError {
    Cancelled,
    Failed(String, String),
}

async fn run_pdal_translate(
    app: &AppHandle,
    job_id: &str,
    pdal: &Path,
    input: &str,
    output: &Path,
    cancel: &Arc<AtomicBool>,
    started: Instant,
) -> Result<(), StageError> {
    eprintln!("[pdal] SPAWN: {} translate {:?} {:?}", pdal.display(), input, output.display());

    let mut cmd = Command::new(pdal);
    cmd.arg("translate").arg(input).arg(output);
    apply_pdal_env(&mut cmd, pdal);
    cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    hide_console(&mut cmd);

    let mut child = cmd.spawn().map_err(|e| {
        eprintln!("[pdal] SPAWN FAILED: {}", e);
        StageError::Failed("pdal_spawn_failed".into(), e.to_string())
    })?;

    pipe_lines(app, "pdal stdout", child.stdout.take().unwrap());
    pipe_lines(app, "pdal stderr", child.stderr.take().unwrap());

    let total_points_est = estimate_points(input);
    let input_bytes = std::fs::metadata(input).map(|m| m.len()).unwrap_or(0);
    // E57 -> uncompressed LAS observed ratio ~2.5x (271 MB E57 -> 680 MB LAS in our test).
    let estimated_las_bytes = (input_bytes as f64 * 2.5) as u64;
    let output_path = output.to_path_buf();

    // Poll the temp LAS file size to drive real progress. PDAL writes points to it
    // monotonically as it reads the E57. Map current/estimated to 0-48% so the
    // PotreeConverter stage starts at a clearly different position.
    let progress_task = {
        let app = app.clone();
        let job_id = job_id.to_string();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            loop {
                if cancel.load(Ordering::SeqCst) { return; }
                let current_bytes = std::fs::metadata(&output_path).map(|m| m.len()).unwrap_or(0);
                let raw_pct = if estimated_las_bytes > 0 {
                    (current_bytes as f64 / estimated_las_bytes as f64) * 100.0
                } else {
                    let elapsed_secs = started.elapsed().as_secs_f64();
                    elapsed_secs * 0.5
                };
                let mapped_pct = (raw_pct * 0.48).min(48.0);
                let elapsed_ms = started.elapsed().as_millis() as u64;
                let bytes_per_sec = if elapsed_ms > 0 {
                    current_bytes as f64 / (elapsed_ms as f64 / 1000.0)
                } else { 0.0 };
                let remaining_ms = if mapped_pct > 1.0 && mapped_pct < 48.0 {
                    Some(((elapsed_ms as f64 / mapped_pct) * (48.0 - mapped_pct)) as u64)
                } else { None };
                let _ = app.emit("convert:progress", ProgressPayload {
                    job_id: job_id.clone(),
                    percent: mapped_pct,
                    points_done: ((raw_pct.min(100.0) / 100.0) * total_points_est as f64) as u64,
                    points_total: total_points_est,
                    points_per_sec: bytes_per_sec / 12.0,
                    remaining_ms,
                    stage_index: 0,
                    stage_total: 2,
                    stage_label: "Reading E57 (PDAL)".into(),
                });
                tokio::time::sleep(std::time::Duration::from_millis(600)).await;
            }
        })
    };

    let status_result = poll_with_cancel(&mut child, cancel).await;
    progress_task.abort();

    match status_result {
        WaitOutcome::Cancelled => Err(StageError::Cancelled),
        WaitOutcome::Exited(status) if status.success() => {
            let bytes = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
            eprintln!("[pdal] EXIT 0  LAS size = {} bytes", bytes);
            if bytes < 1024 {
                return Err(StageError::Failed("pdal_empty_output".into(),
                    format!("PDAL produced only {bytes} bytes \u{2014} E57 read likely failed")));
            }
            Ok(())
        }
        WaitOutcome::Exited(status) => {
            eprintln!("[pdal] EXIT {}", status.code().unwrap_or(-1));
            Err(StageError::Failed(
                format!("pdal_exit_{}", status.code().unwrap_or(-1)),
                "PDAL translate exited with non-zero status (see terminal logs)".into(),
            ))
        }
        WaitOutcome::WaitFailed(e) => Err(StageError::Failed("pdal_wait_failed".into(), e)),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_potree_converter(
    app: &AppHandle,
    job_id: &str,
    binary: &Path,
    input_las: &Path,
    output_dir: &Path,
    cancel: &Arc<AtomicBool>,
    pipeline_started: Instant,
    total_points: u64,
    stage_index: u32,
    stage_total: u32,
    progress_base: f64,
    progress_scale: f64,
) -> Result<(), StageError> {
    eprintln!("[potree] SPAWN: {} -i {:?} -o {:?} --encoding BROTLI",
        binary.display(), input_las.display(), output_dir.display());

    let mut potree_cmd = Command::new(binary);
    potree_cmd
        .arg("-i").arg(input_las)
        .arg("-o").arg(output_dir)
        .arg("--encoding").arg("BROTLI")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    apply_potree_env(&mut potree_cmd, binary);
    hide_console(&mut potree_cmd);
    let mut child = potree_cmd.spawn().map_err(|e| {
        eprintln!("[potree] SPAWN FAILED: {}", e);
        StageError::Failed("potree_spawn_failed".into(), e.to_string())
    })?;

    pipe_lines(app, "potree stderr", child.stderr.take().unwrap());

    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let stage_started = Instant::now();
    let mut last_percent: f64 = 0.0;

    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill().await;
            return Err(StageError::Cancelled);
        }
        tokio::select! {
            line_res = lines.next_line() => {
                match line_res {
                    Ok(Some(l)) => {
                        eprintln!("[potree stdout] {}", l);
                        let _ = app.emit("convert:log", format!("[potree stdout] {}", l));
                        if let Some(p) = parse_percent(&l) { last_percent = p; }
                        let stage_elapsed = stage_started.elapsed().as_millis() as u64;
                        let pts_done = ((last_percent / 100.0) * total_points as f64) as u64;
                        let mapped = progress_base + last_percent * progress_scale;
                        let _ = app.emit("convert:progress", ProgressPayload {
                            job_id: job_id.into(),
                            percent: mapped,
                            points_done: pts_done,
                            points_total: total_points,
                            points_per_sec: if stage_elapsed > 0 { pts_done as f64 / (stage_elapsed as f64 / 1000.0) } else { 0.0 },
                            remaining_ms: if last_percent > 1.0 {
                                Some(((stage_elapsed as f64 / last_percent) * (100.0 - last_percent)) as u64)
                            } else { None },
                            stage_index,
                            stage_total,
                            stage_label: stage_label_from_line(&l).unwrap_or_else(|| "Building octree".into()),
                        });
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
            status = child.wait() => {
                let _ = pipeline_started;
                return match status {
                    Ok(s) if s.success() => Ok(()),
                    Ok(s) => Err(StageError::Failed(
                        format!("potree_exit_{}", s.code().unwrap_or(-1)),
                        "PotreeConverter exited with non-zero status (see terminal logs)".into(),
                    )),
                    Err(e) => Err(StageError::Failed("potree_wait_failed".into(), e.to_string())),
                };
            }
        }
    }

    match child.wait().await {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(StageError::Failed(
            format!("potree_exit_{}", s.code().unwrap_or(-1)),
            "PotreeConverter exited with non-zero status".into(),
        )),
        Err(e) => Err(StageError::Failed("potree_wait_failed".into(), e.to_string())),
    }
}

enum WaitOutcome {
    Cancelled,
    Exited(std::process::ExitStatus),
    WaitFailed(String),
}

async fn poll_with_cancel(child: &mut Child, cancel: &Arc<AtomicBool>) -> WaitOutcome {
    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill().await;
            return WaitOutcome::Cancelled;
        }
        tokio::select! {
            r = child.wait() => {
                return match r {
                    Ok(s) => WaitOutcome::Exited(s),
                    Err(e) => WaitOutcome::WaitFailed(e.to_string()),
                };
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(300)) => { continue; }
        }
    }
}

fn pipe_lines(app: &AppHandle, tag: &'static str, stream: impl tokio::io::AsyncRead + Unpin + Send + 'static) {
    let app = app.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            eprintln!("[{}] {}", tag, line);
            let _ = app.emit("convert:log", format!("[{}] {}", tag, line));
        }
    });
}

fn apply_potree_env(cmd: &mut Command, binary: &Path) {
    if let Some(parent) = binary.parent() {
        let parent_str = parent.to_string_lossy().to_string();
        #[cfg(target_os = "linux")]
        {
            let existing = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
            let new_val = if existing.is_empty() { parent_str.clone() } else { format!("{}:{}", parent_str, existing) };
            cmd.env("LD_LIBRARY_PATH", new_val);
        }
        #[cfg(target_os = "macos")]
        {
            let existing = std::env::var("DYLD_LIBRARY_PATH").unwrap_or_default();
            let new_val = if existing.is_empty() { parent_str.clone() } else { format!("{}:{}", parent_str, existing) };
            cmd.env("DYLD_LIBRARY_PATH", new_val);
        }
        #[cfg(target_os = "windows")]
        { let _ = parent_str; }
    }
}

fn apply_pdal_env(cmd: &mut Command, pdal_binary: &Path) {
    if let Some(bin_dir) = pdal_binary.parent() {
        if let Some(library_dir) = bin_dir.parent() {
            let gdal = library_dir.join("share").join("gdal");
            let proj = library_dir.join("share").join("proj");
            if gdal.exists() { cmd.env("GDAL_DATA", &gdal); }
            if proj.exists() { cmd.env("PROJ_LIB", &proj); }
            cmd.env(
                "PATH",
                format!("{};{}", bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()),
            );
        }
    }
}

fn locate_binary(name: &str) -> Option<PathBuf> {
    let exe_name = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };

    // Dev mode: from CARGO_MANIFEST_DIR (=src-tauri), go up one to scan2bim-converter
    let manifest_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|p| p.to_path_buf());
    if let Some(root) = manifest_root {
        let candidates = if name.eq_ignore_ascii_case("pdal") {
            vec![root.join("binaries").join("pdal").join("bin").join(&exe_name)]
        } else {
            vec![root.join("binaries").join(&exe_name)]
        };
        for c in candidates {
            eprintln!("[locate] dev lookup {}: {}  exists={}", name, c.display(), c.exists());
            if c.exists() { return Some(c); }
        }
    }

    // Production mode: relative to current_exe
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let prod_candidates = if name.eq_ignore_ascii_case("pdal") {
                vec![
                    parent.join("binaries").join("pdal").join("bin").join(&exe_name),
                    parent.join("resources").join("binaries").join("pdal").join("bin").join(&exe_name),
                    parent.join(&exe_name),
                ]
            } else {
                vec![
                    parent.join(&exe_name),
                    parent.join("binaries").join(&exe_name),
                    parent.join("resources").join("binaries").join(&exe_name),
                ]
            };
            for c in prod_candidates {
                eprintln!("[locate] prod lookup {}: {}  exists={}", name, c.display(), c.exists());
                if c.exists() { return Some(c); }
            }
        }
    }

    eprintln!("[locate] {} NOT FOUND", name);
    None
}

fn cleanup_temp(p: Option<&Path>) {
    if let Some(p) = p {
        let _ = std::fs::remove_file(p);
    }
}

fn emit_progress(
    app: &AppHandle,
    job_id: &str,
    percent: f64,
    points_done: u64,
    points_total: u64,
    stage_index: u32,
    stage_total: u32,
    label: &str,
    started: Instant,
) {
    let _ = app.emit("convert:progress", ProgressPayload {
        job_id: job_id.into(),
        percent,
        points_done,
        points_total,
        points_per_sec: 0.0,
        remaining_ms: None,
        stage_index,
        stage_total,
        stage_label: label.into(),
    });
    let _ = started;
}

fn emit_error(app: &AppHandle, job_id: &str, code: &str, message: &str) {
    let _ = app.emit("convert:error", ErrorPayload {
        job_id: job_id.into(),
        code: code.into(),
        message: message.into(),
    });
}

fn emit_cancelled(app: &AppHandle, job_id: &str) {
    let _ = app.emit("convert:cancelled", CancelledPayload {
        job_id: job_id.into(),
    });
}

fn estimate_points(path: &str) -> u64 {
    std::fs::metadata(path).map(|m| m.len() / 11).unwrap_or(0)
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

fn stage_label_from_line(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    for s in ["indexing", "sampling", "writing", "finalizing", "counting", "chunking", "reading"] {
        if lower.contains(s) {
            return Some(format!("{}{}", s[..1].to_uppercase(), &s[1..]));
        }
    }
    None
}

fn dir_size(path: &Path) -> u64 {
    fn walk(p: &Path, total: &mut u64) {
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

async fn run_stub(
    app: AppHandle,
    job_id: String,
    input_path: String,
    output_dir: String,
    cancel: Arc<AtomicBool>,
) {
    let total = estimate_points(&input_path);
    let started = Instant::now();
    let stages = ["Reading file", "Loading points", "Building octree", "Writing"];
    let mut percent: f64 = 0.0;
    loop {
        if cancel.load(Ordering::SeqCst) {
            emit_cancelled(&app, &job_id);
            return;
        }
        percent = (percent + 2.0 + rand_jitter() * 1.5).min(100.0);
        let stage_idx = ((percent / 100.0) * stages.len() as f64) as usize;
        let stage_idx = stage_idx.min(stages.len() - 1);
        let elapsed = started.elapsed().as_millis() as u64;
        let points_done = ((percent / 100.0) * total as f64) as u64;
        let _ = app.emit("convert:progress", ProgressPayload {
            job_id: job_id.clone(),
            percent,
            points_done,
            points_total: total,
            points_per_sec: if elapsed > 0 { points_done as f64 / (elapsed as f64 / 1000.0) } else { 0.0 },
            remaining_ms: if percent > 1.0 {
                Some(((elapsed as f64 / percent) * (100.0 - percent)) as u64)
            } else { None },
            stage_index: stage_idx as u32,
            stage_total: stages.len() as u32,
            stage_label: stages[stage_idx].into(),
        });
        if percent >= 100.0 {
            let out_name = Path::new(&input_path)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "output".into());
            let out_path = PathBuf::from(&output_dir).join(&out_name);
            let _ = std::fs::create_dir_all(&out_path);
            let input_bytes = std::fs::metadata(&input_path).map(|m| m.len()).unwrap_or(0);
            let _ = app.emit("convert:done", DonePayload {
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
