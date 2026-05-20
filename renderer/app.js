const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { getCurrentWindow } = window.__TAURI__.window;

const i18n = {
  de: {
    'drop.idle': 'Punktwolke hier ablegen',
    'drop.dragover': 'Loslassen zum Hinzufügen',
    'drop.or': 'oder',
    'drop.pick': 'Datei wählen',
    'drop.only': 'nur .e57',
    'target': 'ZIEL',
    'output.placeholder': 'Ausgabeordner wählen…',
    'pick': 'wählen',
    'change': 'ändern',
    'convert': 'Konvertieren',
    'cancel': 'Abbrechen',
    'running.title': 'KONVERTIERUNG LÄUFT',
    'running.active': 'AKTIV',
    'done.title': 'FERTIG',
    'done.msg': 'Konvertierung abgeschlossen',
    'done.new': 'Neue Datei',
    'done.open': 'Ordner öffnen',
    'error.title': 'FEHLER',
    'error.msg': 'Konvertierung fehlgeschlagen',
    'error.detail': 'Beim Lesen der e57-Datei ist ein Fehler aufgetreten. Möglicherweise ist sie beschädigt oder unvollständig.',
    'error.logs': 'Logs öffnen',
    'error.retry': 'Erneut versuchen',
    'remaining.suffix': 'verbleibend',
    'points.short': 'Punkte',
    'pointsRate.short': 'Pkt/s',
    'stat.size': 'GRÖSSE',
    'stat.scans': 'SCANS',
    'stat.points': 'PUNKTE',
    'stat.duration': 'DAUER',
    'stat.output': 'AUSGABE',
    'options': 'Optionen',
  },
  en: {
    'drop.idle': 'Drop point cloud here',
    'drop.dragover': 'Release to add',
    'drop.or': 'or',
    'drop.pick': 'choose file',
    'drop.only': '.e57 only',
    'target': 'TARGET',
    'output.placeholder': 'Choose output folder…',
    'pick': 'choose',
    'change': 'change',
    'convert': 'Convert',
    'cancel': 'Cancel',
    'running.title': 'CONVERSION RUNNING',
    'running.active': 'ACTIVE',
    'done.title': 'DONE',
    'done.msg': 'Conversion complete',
    'done.new': 'New file',
    'done.open': 'Open folder',
    'error.title': 'ERROR',
    'error.msg': 'Conversion failed',
    'error.detail': 'An error occurred while reading the e57 file. It may be damaged or incomplete.',
    'error.logs': 'Open logs',
    'error.retry': 'Retry',
    'remaining.suffix': 'remaining',
    'points.short': 'points',
    'pointsRate.short': 'pts/s',
    'stat.size': 'SIZE',
    'stat.scans': 'SCANS',
    'stat.points': 'POINTS',
    'stat.duration': 'DURATION',
    'stat.output': 'OUTPUT',
    'options': 'Options',
  },
};

const PREFS_KEY = 'scan2bim:prefs:v1';

function loadPrefs() {
  try {
    const raw = localStorage.getItem(PREFS_KEY);
    return raw ? JSON.parse(raw) : {};
  } catch {
    return {};
  }
}

function savePrefs() {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify({ lang, outputDir }));
  } catch {}
}

const prefs = loadPrefs();
let lang = prefs.lang === 'de' ? 'de' : 'en';
let currentState = 'idle';
let inputFile = null;
let outputDir = prefs.outputDir ?? null;
let lastDoneOutput = null;

function t(key) { return i18n[lang][key] ?? key; }

function applyI18n() {
  document.querySelectorAll('[data-i18n]').forEach((el) => {
    const key = el.dataset.i18n;
    if (i18n[lang][key]) el.textContent = i18n[lang][key];
  });
}

function setState(name) {
  currentState = name;
  document.querySelectorAll('.state').forEach((el) => {
    el.hidden = el.dataset.state !== name;
  });
}

function fmtBytes(b) {
  if (b == null) return '—';
  if (b < 1024) return `${b} B`;
  if (b < 1024 ** 2) return `${(b / 1024).toFixed(1)} KB`;
  if (b < 1024 ** 3) return `${(b / 1024 ** 2).toFixed(1)} MB`;
  return `${(b / 1024 ** 3).toFixed(2)} GB`;
}

function fmtPoints(n) {
  if (n == null) return '—';
  if (n < 1e3) return `${n}`;
  if (n < 1e6) return `${(n / 1e3).toFixed(0)} K`;
  if (n < 1e9) return `${(n / 1e6).toFixed(0)} M`;
  return `${(n / 1e9).toFixed(2)} B`;
}

function fmtDuration(ms) {
  if (!ms || ms < 0) return '—';
  const s = Math.floor(ms / 1000);
  const m = Math.floor(s / 60);
  const sec = s % 60;
  return `${m}:${String(sec).padStart(2, '0')}`;
}

function fmtRemaining(ms) {
  if (ms == null) return '~ —';
  const s = Math.max(0, Math.floor(ms / 1000));
  const m = Math.floor(s / 60);
  const sec = s % 60;
  return `~ ${m}:${String(sec).padStart(2, '0')} ${t('remaining.suffix')}`;
}

const appWindow = getCurrentWindow();

document.querySelectorAll('.dot').forEach((el) => {
  el.addEventListener('click', async () => {
    const action = el.dataset.action;
    if (action === 'minimize') await appWindow.minimize();
    else if (action === 'maximize') await appWindow.toggleMaximize();
    else if (action === 'close') await appWindow.close();
  });
});

document.querySelectorAll('.lang-opt').forEach((el) => {
  el.addEventListener('click', () => {
    document.querySelectorAll('.lang-opt').forEach((x) => x.classList.remove('active'));
    el.classList.add('active');
    lang = el.dataset.lang;
    applyI18n();
    refreshOutputDisplay();
    savePrefs();
  });
});

function applyInitialLang() {
  document.querySelectorAll('.lang-opt').forEach((el) => {
    el.classList.toggle('active', el.dataset.lang === lang);
  });
}

const appEl = document.querySelector('.app');

appWindow.onDragDropEvent(async (event) => {
  const p = event.payload;
  if (p.type === 'over' || p.type === 'enter') {
    if (currentState !== 'idle' && currentState !== 'ready') return;
    appEl.classList.add('dragging');
    const titleEl = document.querySelector('.state-idle .dz-title');
    if (titleEl) titleEl.textContent = t('drop.dragover');
  } else if (p.type === 'leave') {
    appEl.classList.remove('dragging');
    const titleEl = document.querySelector('.state-idle .dz-title');
    if (titleEl) titleEl.textContent = t('drop.idle');
  } else if (p.type === 'drop') {
    appEl.classList.remove('dragging');
    const titleEl = document.querySelector('.state-idle .dz-title');
    if (titleEl) titleEl.textContent = t('drop.idle');
    if (currentState !== 'idle' && currentState !== 'ready') return;
    const path = p.paths?.find((x) => x.toLowerCase().endsWith('.e57'));
    if (!path) return;
    const info = await invoke('inspect_file', { path });
    if (info) acceptInput(info);
  }
});

document.getElementById('pickFileBtn').addEventListener('click', async () => {
  const info = await invoke('pick_e57');
  if (info) acceptInput(info);
});

[document.getElementById('pickOutputBtnIdle'), document.getElementById('pickOutputBtnReady')].forEach((btn) => {
  btn.addEventListener('click', async () => {
    const dir = await invoke('pick_output');
    if (!dir) return;
    outputDir = dir;
    refreshOutputDisplay();
    refreshReadyCTA();
    savePrefs();
  });
});

function refreshOutputDisplay() {
  const els = [document.getElementById('outputDisplayIdle'), document.getElementById('outputDisplayReady')];
  els.forEach((el) => {
    if (!el) return;
    if (outputDir) {
      el.textContent = outputDir;
      el.classList.remove('placeholder');
    } else {
      el.textContent = t('output.placeholder');
      el.classList.add('placeholder');
    }
  });
}

function refreshReadyCTA() {
  const ready = !!(inputFile && outputDir);
  for (const id of ['convertBtnIdle', 'convertBtnReady']) {
    const btn = document.getElementById(id);
    if (!btn) continue;
    btn.disabled = !ready;
    btn.classList.toggle('cta-disabled', !ready);
  }
}

function acceptInput(info) {
  inputFile = info;
  document.getElementById('readyFileName').textContent = info.name;
  document.getElementById('statSize').textContent = fmtBytes(info.size);
  document.getElementById('statScans').textContent = '—';
  document.getElementById('statPoints').textContent = '—';
  refreshOutputDisplay();
  refreshReadyCTA();
  setState('ready');
}

document.getElementById('convertBtnIdle').addEventListener('click', startConvert);
document.getElementById('convertBtnReady').addEventListener('click', startConvert);

async function startConvert() {
  if (!inputFile) return;
  if (!outputDir) {
    const dir = await invoke('pick_output');
    if (!dir) return;
    outputDir = dir;
    refreshOutputDisplay();
    refreshReadyCTA();
    savePrefs();
  }
  setState('running');
  document.getElementById('runningFileName').textContent = inputFile.name;
  document.getElementById('percentValue').textContent = '0';
  document.getElementById('progressFill').style.width = '0%';
  document.getElementById('pointsProgress').textContent = `0 / 0 ${t('points.short')}`;
  document.getElementById('pointsRate').textContent = `— ${t('pointsRate.short')}`;
  document.getElementById('remaining').textContent = '~ —';
  document.getElementById('stageLabel').textContent = '—';
  try {
    await invoke('convert_start', { inputPath: inputFile.path, outputDir });
  } catch (e) {
    document.getElementById('errorCode').textContent = String(e);
    setState('error');
  }
}

listen('convert:progress', (e) => {
  const d = e.payload;
  document.getElementById('percentValue').textContent = Math.floor(d.percent);
  document.getElementById('progressFill').style.width = `${d.percent}%`;
  document.getElementById('pointsProgress').textContent =
    `${fmtPoints(d.points_done)} / ${fmtPoints(d.points_total)} ${t('points.short')}`;
  document.getElementById('pointsRate').textContent =
    `${fmtPoints(d.points_per_sec)} ${t('pointsRate.short')}`;
  document.getElementById('remaining').textContent = fmtRemaining(d.remaining_ms);
  document.getElementById('stageLabel').textContent =
    `${d.stage_index + 1} / ${d.stage_total} · ${d.stage_label}`;
});

listen('convert:done', (e) => {
  const d = e.payload;
  lastDoneOutput = d.output_path;
  document.getElementById('doneFileName').textContent = inputFile?.name ?? '';
  document.getElementById('doneOutputPath').textContent = `→ ${d.output_path}`;
  document.getElementById('doneDuration').textContent = fmtDuration(d.duration_ms);
  document.getElementById('doneSize').textContent = fmtBytes(d.output_bytes);
  document.getElementById('donePoints').textContent = fmtPoints(d.points_total);
  invoke('job_finished').catch(() => {});
  setState('done');
});

listen('convert:error', (e) => {
  const d = e.payload;
  document.getElementById('errorCode').textContent = `${d.code} · ${d.message}`;
  invoke('job_finished').catch(() => {});
  setState('error');
});

listen('convert:cancelled', () => {
  invoke('job_finished').catch(() => {});
  setState(inputFile && outputDir ? 'ready' : 'idle');
});

document.getElementById('cancelBtn').addEventListener('click', () => invoke('convert_cancel'));

document.getElementById('newFileBtn').addEventListener('click', () => {
  inputFile = null;
  refreshReadyCTA();
  setState('idle');
});

document.getElementById('openFolderBtn').addEventListener('click', () => {
  if (lastDoneOutput) invoke('open_path', { path: lastDoneOutput });
});

document.getElementById('retryBtn').addEventListener('click', () => {
  setState(inputFile && outputDir ? 'ready' : 'idle');
});

const logBuffer = [];
const MAX_LOG_LINES = 500;
listen('convert:log', (e) => {
  logBuffer.push(String(e.payload));
  if (logBuffer.length > MAX_LOG_LINES) logBuffer.splice(0, logBuffer.length - MAX_LOG_LINES);
});

function showLogs() {
  const existing = document.getElementById('logs-modal');
  if (existing) { existing.remove(); return; }
  const modal = document.createElement('div');
  modal.id = 'logs-modal';
  modal.style.cssText = 'position:fixed;inset:0;background:rgba(0,0,0,0.75);z-index:9999;display:flex;align-items:center;justify-content:center;padding:24px;';
  let body;
  if (logBuffer.length) {
    body = logBuffer.join('\n');
  } else if (currentState === 'running') {
    body = '(no output from the converter yet — PDAL is silent during E57 reading, this is normal)';
  } else if (currentState === 'idle' || currentState === 'ready') {
    body = '(start a conversion to see PDAL and PotreeConverter output here)';
  } else {
    body = '(no log lines captured)';
  }
  modal.innerHTML = `
    <div style="background:#1a1410;color:#f5eed7;border:1px solid #3a2820;border-radius:12px;width:100%;max-width:520px;max-height:80vh;display:flex;flex-direction:column;font-family:Manrope,sans-serif;box-shadow:0 8px 40px rgba(0,0,0,0.6);">
      <div style="padding:14px 18px;display:flex;align-items:center;justify-content:space-between;border-bottom:1px solid #3a2820;">
        <strong style="font-size:13px;letter-spacing:1px;">CONVERSION LOG</strong>
        <button id="logs-close" style="background:transparent;color:#f5eed7;border:1px solid #3a2820;border-radius:6px;padding:4px 10px;cursor:pointer;font-family:Manrope,sans-serif;">Close</button>
      </div>
      <pre style="margin:0;padding:14px 18px;flex:1;overflow:auto;font-family:ui-monospace,Menlo,Consolas,monospace;font-size:11px;line-height:1.5;white-space:pre-wrap;word-break:break-all;color:#cfc7b8;"></pre>
    </div>`;
  document.body.appendChild(modal);
  modal.querySelector('pre').textContent = body;
  modal.querySelector('#logs-close').addEventListener('click', () => modal.remove());
  modal.addEventListener('click', (ev) => { if (ev.target === modal) modal.remove(); });
}

document.getElementById('openLogsBtn').addEventListener('click', showLogs);
document.getElementById('logsBtn').addEventListener('click', showLogs);

applyInitialLang();
applyI18n();
refreshOutputDisplay();
refreshReadyCTA();
setState('idle');
