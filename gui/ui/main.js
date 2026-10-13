const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const cv = document.getElementById('cv');
const ctx = cv.getContext('2d');
// Remote framebuffer at native size; `cv` shows it scaled with a high-quality resampler.
const fb = document.createElement('canvas');
const fbctx = fb.getContext('2d');
let dirty = false;
function redraw() {
  if (dirty) return;
  dirty = true;
  requestAnimationFrame(() => {
    dirty = false;
    const z = zoomFactor();
    const sharp = Number.isInteger(z) && z >= 1; // whole multiples: keep pixels crisp
    ctx.imageSmoothingEnabled = !sharp;
    ctx.imageSmoothingQuality = 'high';
    ctx.drawImage(fb, 0, 0, cv.width, cv.height);
  });
}
const $ = (id) => document.getElementById(id);

invoke('default_tls').then((t) => { $('tls').checked = t; });
invoke('default_user').then((u) => { $('user').value = u; });
invoke('default_protocol').then(async (p) => {
  $('proto').value = p;
  setProto();
  const pw = await invoke('default_password', { protocol: p });
  if (pw) $('pass').value = pw;
  invoke('autoconnect').then((a) => { if (a) $('go').click(); });
});

$('go').onclick = () => {
  const proto = $('proto').value;
  if (proto === 'html5') {
    reportLayout();
    invoke('connect_html5', { ip: $('ip').value, user: $('user').value, pass: $('pass').value });
  } else if (proto === 'vnc') {
    invoke('connect', { ip: $('ip').value, port: Number($('port').value), tls: $('tls').checked, pass: $('pass').value });
  } else {
    ensureTerm();
    term.reset();
    fit.fit();
    invoke('connect_text', { protocol: proto, ip: $('ip').value, user: $('user').value, pass: $('pass').value, cols: term.cols, rows: term.rows });
    term.focus();
  }
};
$('stop').onclick = () => { invoke('disconnect'); if ($('proto').value === 'html5') invoke('disconnect_html5'); };

listen('vnc-status', (e) => { $('status').textContent = e.payload; $('status').title = e.payload; });
const PAD = 16; // black border on every side (also keeps the last line from being cut off)
document.getElementById('wrap').style.padding = PAD + 'px';
// ---- text size (zoom) --------------------------------------------------
// The console text is pixels, so "text size" scales the whole remote screen.
let zoomSetting = '1';
try { zoomSetting = localStorage.getItem('bobcat.zoom') || localStorage.getItem('dracon.zoom') || '1'; } catch (_) {}
$('zoom').value = zoomSetting;

function zoomFactor() {
  if ($('zoom').value !== 'fit') return Number($('zoom').value);
  const wrap = $('wrap');
  const aw = wrap.clientWidth - 2 * PAD, ah = wrap.clientHeight - 2 * PAD;
  return Math.max(0.1, Math.min(4, aw / fb.width, ah / fb.height));
}
function applyZoom() {
  const z = zoomFactor();
  const dpr = window.devicePixelRatio || 1;
  const cssW = Math.round(fb.width * z), cssH = Math.round(fb.height * z);
  cv.style.width = cssW + 'px';
  cv.style.height = cssH + 'px';
  const pw = Math.round(cssW * dpr), ph = Math.round(cssH * dpr);
  if (cv.width !== pw || cv.height !== ph) { cv.width = pw; cv.height = ph; } // resets the canvas
  redraw();
}
// resize the window to fit the (scaled) remote screen plus toolbar and border
function fitWindow() {
  const z = zoomFactor();
  invoke('fit_window', {
    w: Math.round(fb.width * z) + 2 * PAD,
    h: Math.round(fb.height * z) + $('bar').offsetHeight + $('statusbar').offsetHeight + 2 * PAD,
  });
}
$('zoom').onchange = () => {
  try { localStorage.setItem('bobcat.zoom', $('zoom').value); } catch (_) {}
  if ($('proto').value === 'html5') { invoke('html5_zoom', { factor: $('zoom').value === 'fit' ? 1 : Number($('zoom').value) }); return; }
  if ($('proto').value !== 'vnc') { applyTermFont(); return; }
  applyZoom();
  if ($('zoom').value !== 'fit') fitWindow();
};
window.addEventListener('resize', () => {
  reportLayout();
  if ($('proto').value !== 'vnc') { if (fit) fit.fit(); return; }
  if ($('zoom').value === 'fit') applyZoom();
});

listen('vnc-resize', (e) => {
  fb.width = e.payload.w;
  fb.height = e.payload.h;
  applyZoom();
  if ($('zoom').value !== 'fit') fitWindow();
});
listen('vnc-frame', (e) => {
  const { x, y, w, h, data } = e.payload;
  const bin = atob(data);
  const px = new Uint8ClampedArray(bin.length);
  for (let i = 0; i < bin.length; i++) px[i] = bin.charCodeAt(i);
  fbctx.putImageData(new ImageData(px, w, h), x, y);
  redraw();
});

// ---- input -------------------------------------------------------------
const SPECIAL = {
  Backspace: 0xff08, Tab: 0xff09, Enter: 0xff0d, Escape: 0xff1b, Delete: 0xffff,
  Home: 0xff50, ArrowLeft: 0xff51, ArrowUp: 0xff52, ArrowRight: 0xff53, ArrowDown: 0xff54,
  PageUp: 0xff55, PageDown: 0xff56, End: 0xff57, Insert: 0xff63,
  Shift: 0xffe1, Control: 0xffe3, CapsLock: 0xffe5, Alt: 0xffe9, Meta: 0xffeb,
  ContextMenu: 0xff67,
};
for (let i = 1; i <= 12; i++) SPECIAL['F' + i] = 0xffbd + i;

function keysym(e) {
  if (e.key in SPECIAL) {
    const base = SPECIAL[e.key];
    return e.location === 2 && base >= 0xffe1 && base <= 0xffeb && base !== 0xffe5 ? base + 1 : base;
  }
  const cp = e.key.codePointAt(0);
  if (e.key.length === 0 || [...e.key].length !== 1) return null;
  return cp < 0x100 ? cp : 0x01000000 + cp;
}

const down = new Map(); // e.code -> keysym currently held
const key = (d, k) => invoke('send_key', { down: d, keysym: k });

async function paste() {
  if ($('proto').value === 'html5') {
    try {
      const n = await invoke('html5_paste');
      $('status').textContent = `pasted ${n} characters`;
    } catch (err) {
      $('status').textContent = `paste failed: ${err}`;
    }
    return;
  }
  if ($('proto').value !== 'vnc') {
    try {
      const text = await invoke('clipboard_text');
      term.paste(text); // handles bracketed paste mode
      $('status').textContent = `pasted ${text.length} characters`;
    } catch (err) {
      $('status').textContent = `paste failed: ${err}`;
    }
    return;
  }
  // release held modifiers so the pasted text isn't typed with Ctrl/Shift down
  for (const k of down.values()) key(false, k);
  down.clear();
  try {
    const n = await invoke('paste_clipboard');
    $('status').textContent = `pasted ${n} characters`;
  } catch (err) {
    $('status').textContent = `paste failed: ${err}`;
  }
}
$('paste').onclick = () => { paste(); const p = $('proto').value; if (p === 'vnc') cv.focus(); else if (p !== 'html5') term.focus(); };

cv.addEventListener('keydown', (e) => {
  if (e.ctrlKey && e.shiftKey && e.code === 'KeyV') { e.preventDefault(); paste(); return; }
  const k = keysym(e);
  if (k === null) return;
  e.preventDefault();
  down.set(e.code, k);
  key(true, k);
});
cv.addEventListener('keyup', (e) => {
  const k = down.get(e.code);
  if (k === undefined) return;
  e.preventDefault();
  down.delete(e.code);
  key(false, k);
});
cv.addEventListener('blur', () => {
  for (const k of down.values()) key(false, k);
  down.clear();
});

let buttons = 0;
function pos(e) {
  const r = cv.getBoundingClientRect();
  return {
    x: Math.max(0, Math.min(fb.width - 1, Math.round((e.clientX - r.left) * fb.width / r.width))),
    y: Math.max(0, Math.min(fb.height - 1, Math.round((e.clientY - r.top) * fb.height / r.height))),
  };
}
// JS: 1 left, 2 right, 4 middle  ->  RFB: 1 left, 2 middle, 4 right
const rfbButtons = (b) => (b & 1) | ((b & 4) ? 2 : 0) | ((b & 2) ? 4 : 0);
const pointer = (e, extra = 0) => {
  const { x, y } = pos(e);
  return invoke('send_pointer', { buttons: buttons | extra, x, y });
};

let copyMode = false;
let selStart = null;
const sel = $('sel');
function setCopyMode(on) {
  copyMode = on;
  selStart = null;
  sel.style.display = 'none';
  cv.classList.toggle('copying', on);
  $('copy').textContent = on ? 'Cancel copy' : 'Copy text';
  if (on) $('status').textContent = 'drag over the text to copy (OCR); Esc cancels';
}
$('copy').onclick = async () => {
  const p = $('proto').value;
  if (p === 'vnc') { setCopyMode(!copyMode); return; }
  if (p === 'html5') {
    $('status').textContent = 'reading the console screen...';
    try {
      const text = await invoke('html5_copy_text');
      $('status').textContent = `copied ${text.length} characters: ${text.slice(0, 60).replace(/\n/g, ' ')}`;
    } catch (err) {
      $('status').textContent = `copy failed: ${err}`;
    }
    return;
  }
  copyTerm();
};
// Text consoles: copy the selection, or the whole visible screen if nothing is selected.
async function copyTerm() {
  let text = term.getSelection();
  let what = 'selection';
  if (!text) {
    const buf = term.buffer.active;
    const lines = [];
    for (let i = 0; i < term.rows; i++) {
      const l = buf.getLine(buf.viewportY + i);
      if (l) lines.push(l.translateToString(true));
    }
    text = lines.join('\n').replace(/\s+$/, '');
    what = 'screen';
  }
  if (!text) { $('status').textContent = 'nothing to copy'; return; }
  try {
    await invoke('set_clipboard', { text });
    $('status').textContent = `copied ${what} (${text.length} characters)`;
  } catch (err) {
    $('status').textContent = `copy failed: ${err}`;
  }
  term.focus();
}

async function copyRegion(a, b) {
  const x = Math.min(a.x, b.x), y = Math.min(a.y, b.y);
  const w = Math.abs(a.x - b.x), h = Math.abs(a.y - b.y);
  if (w < 4 || h < 4) return;
  const S = 3; // upscale for OCR accuracy
  const t = document.createElement('canvas');
  t.width = w * S; t.height = h * S;
  const tc = t.getContext('2d');
  tc.drawImage(fb, x, y, w, h, 0, 0, w * S, h * S);
  const img = tc.getImageData(0, 0, t.width, t.height);
  const d = img.data;
  let sum = 0;
  for (let i = 0; i < d.length; i += 4) sum += d[i] * 0.3 + d[i + 1] * 0.59 + d[i + 2] * 0.11;
  if (sum / (d.length / 4) < 128) { // light text on dark: invert for tesseract
    for (let i = 0; i < d.length; i += 4) { d[i] = 255 - d[i]; d[i + 1] = 255 - d[i + 1]; d[i + 2] = 255 - d[i + 2]; }
    tc.putImageData(img, 0, 0);
  }
  const b64 = t.toDataURL('image/png').split(',')[1];
  $('status').textContent = 'recognising text...';
  try {
    const text = await invoke('copy_region_text', { pngB64: b64 });
    $('status').textContent = `copied ${text.length} characters: ${text.slice(0, 60).replace(/\n/g, ' ')}`;
  } catch (err) {
    $('status').textContent = `copy failed: ${err}`;
  }
}

cv.addEventListener('mousedown', (e) => {
  if (!copyMode) return;
  selStart = pos(e);
  selStart.cx = e.clientX; selStart.cy = e.clientY;
});
window.addEventListener('mousemove', (e) => {
  if (!copyMode || !selStart) return;
  sel.style.display = 'block';
  sel.style.left = Math.min(selStart.cx, e.clientX) + 'px';
  sel.style.top = Math.min(selStart.cy, e.clientY) + 'px';
  sel.style.width = Math.abs(e.clientX - selStart.cx) + 'px';
  sel.style.height = Math.abs(e.clientY - selStart.cy) + 'px';
});
window.addEventListener('mouseup', (e) => {
  if (!copyMode || !selStart) return;
  const a = selStart;
  selStart = null;
  sel.style.display = 'none';
  copyRegion(a, pos(e));
  setCopyMode(false);
});
window.addEventListener('keydown', (e) => { if (copyMode && e.key === 'Escape') setCopyMode(false); });

cv.addEventListener('mousemove', (e) => {
  if (copyMode) return; buttons = rfbButtons(e.buttons); pointer(e); });
cv.addEventListener('mousedown', (e) => { if (copyMode) return; cv.focus(); buttons = rfbButtons(e.buttons); pointer(e); });
cv.addEventListener('mouseup', (e) => { if (copyMode) return; buttons = rfbButtons(e.buttons); pointer(e); });
cv.addEventListener('contextmenu', (e) => e.preventDefault());
cv.addEventListener('wheel', (e) => {
  e.preventDefault();
  const bit = e.deltaY < 0 ? 8 : 16;
  pointer(e, bit).then(() => pointer(e));
}, { passive: false });

$('cad').onclick = async () => {
  for (const k of [0xffe3, 0xffe9, 0xffff]) await key(true, k);
  for (const k of [0xffff, 0xffe9, 0xffe3]) await key(false, k);
  cv.focus();
};

// ---- certificate viewer ------------------------------------------------
const certKind = () => ($('proto').value === 'ssh' ? 'ssh' : 'tls');
$('certbtn').onclick = async () => {
  const box = $('cert'), txt = $('certtext'), trust = $('certtrust');
  const ssh = certKind() === 'ssh';
  trust.textContent = ssh ? 'Trust this key' : 'Trust this certificate';
  trust.style.display = 'none';
  try {
    const c = await invoke('view_cert', { kind: certKind() });
    const state = { new: 'First seen: saved as trusted for this host',
                    pinned: 'Matches the saved ' + (ssh ? 'key' : 'certificate'),
                    changed: 'CHANGED: does not match the saved ' + (ssh ? 'key' : 'certificate') }[c.state];
    txt.className = c.state === 'changed' ? 'warn' : '';
    txt.textContent = ssh
      ? `Host:       ${c.host}\nKey type:   ${c.key_type}\n\nSHA-256:\n${c.sha256}\n` +
        (c.state === 'changed' ? `\nSaved SHA-256:\n${c.pinned_sha256}\n` : '') + `\n${state}`
      : `Host:       ${c.host}\nSubject:    ${c.subject}\nIssuer:     ${c.issuer}\n` +
        `Valid from: ${c.not_before}\nValid to:   ${c.not_after}\n\nSHA-256:\n${c.sha256}\n` +
        (c.state === 'changed' ? `\nSaved SHA-256:\n${c.pinned_sha256}\n` : '') + `\n${state}`;
    if (c.state === 'changed') trust.style.display = '';
  } catch (err) {
    txt.className = '';
    txt.textContent = String(err);
  }
  box.style.display = 'flex';
};
$('certtrust').onclick = async () => {
  try { await invoke('trust_cert', { kind: certKind() }); $('certtext').textContent = (certKind() === 'ssh' ? 'Host key' : 'Certificate') + ' trusted. Click Connect to retry.'; $('certtrust').style.display = 'none'; }
  catch (err) { $('certtext').textContent = String(err); }
};
$('certclose').onclick = () => { $('cert').style.display = 'none'; };

// ---- text consoles (SSH / SOL) via xterm.js ----------------------------
let term = null, fit = null;
const b64dec = (s) => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));
const b64enc = (u8) => btoa(String.fromCharCode(...u8));

function termFont() {
  const z = $('zoom').value === 'fit' ? 1 : Number($('zoom').value);
  return Math.round(14 * z);
}
function applyTermFont() {
  if (!term) return;
  term.options.fontSize = termFont();
  fit.fit();
}
function ensureTerm() {
  if (term) return;
  term = new Terminal({ fontSize: termFont(), cursorBlink: true, theme: { background: '#000000' } });
  fit = new FitAddon.FitAddon();
  term.loadAddon(fit);
  term.open($('term'));
  fit.fit();
  term.onData((d) => invoke('term_input', { data: b64enc(new TextEncoder().encode(fixCursorReport(d))) }));
  term.onResize(({ cols, rows }) => invoke('term_resize', { cols, rows }));
}
// Serial consoles can't tell the server the window size, and some menus (e.g. the
// TrueNAS console menu, prompt_toolkit) ask where the cursor is (ESC[6n) and then
// print newlines up to the row where they THINK the screen ends (66 here). If
// that is below our last row, the screen scrolls. Reporting the cursor as far
// down the screen makes them skip that, so the menu redraws in place. A program
// that measures the screen (moves to 999;999 and then asks) gets the true answer.
let measuringScreen = false;
function fixCursorReport(d) {
  const m = /^\x1b\[(\d+);(\d+)R$/.exec(d);
  if (!m) return d;
  if (measuringScreen) { measuringScreen = false; return d; }
  return `\x1b[${Math.max(Number(m[1]), 500)};${m[2]}R`;
}
listen('term-data', (e) => {
  if (!term) return;
  const bytes = b64dec(e.payload);
  if (/\x1b\[\d{3,};\d{3,}H/.test(new TextDecoder('latin1').decode(bytes))) measuringScreen = true;
  term.write(bytes);
});

// show only the controls that apply to the chosen protocol
function setProto() {
  const proto = $('proto').value;
  const kind = proto === 'vnc' ? 'vnc' : proto === 'html5' ? 'html5' : 'text';
  const hide = (sel, yes) => document.querySelectorAll(sel).forEach((el) => el.classList.toggle('hidden', yes));
  hide('.vnc-only', kind !== 'vnc');
  hide('.login-only', kind === 'vnc');          // iDRAC user name: SSH, SOL and HTML5 log in
  hide('.no-html5', kind === 'html5');          // paste / copy / keyboard / text size are for the console views
  $('wrap').classList.toggle('hidden', kind !== 'vnc');
  $('term').classList.toggle('hidden', kind !== 'text');
  $('html5note').classList.toggle('hidden', kind !== 'html5');
  $('pass').placeholder = kind === 'vnc' ? 'VNC password' : 'iDRAC password';
  // the Certificate button shows the TLS certificate (VNC, HTML5) or SSH host key; SOL has neither
  $('certbtn').classList.toggle('hidden', proto === 'sol');
  $('certbtn').textContent = proto === 'ssh' ? 'Host key' : 'Certificate';
  if (kind === 'text') { ensureTerm(); fit.fit(); }
  reportLayout();
  invoke('default_password', { protocol: proto }).then((pw) => { if (pw) $('pass').value = pw; });
}
$('proto').onchange = () => { invoke('disconnect'); invoke('disconnect_html5'); setProto(); };
setProto();

// ---- HTML5 console embedded below the toolbar ----------------------------
// The console is a second web view inside this window, stacked under this page. This page
// then only needs the height of its toolbar and status bar; tell the app so it can give the
// rest of the window to the console.
function reportLayout() {
  if ($('proto').value !== 'html5') return;
  invoke('html5_layout', { height: Math.ceil($('bar').offsetHeight + $('statusbar').offsetHeight) });
}
new ResizeObserver(reportLayout).observe($('bar'));

