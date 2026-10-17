const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const cv = document.getElementById('cv');
const ctx = cv.getContext('2d');
// Remote framebuffer at native size; `cv` shows it scaled with a high-quality resampler.
const fb = document.createElement('canvas');
const fbctx = fb.getContext('2d');
// Redraws are batched into one per animation frame. With no argument the whole picture is drawn
// (zoom, resize); with a rectangle (frame pixels) only that part, which is much cheaper on a big screen.
let drawQueued = false, drawAll = false, drawBox = null;
function redraw(box) {
  if (!box) drawAll = true;
  else if (!drawAll) {
    drawBox = drawBox
      ? { x0: Math.min(drawBox.x0, box.x0), y0: Math.min(drawBox.y0, box.y0), x1: Math.max(drawBox.x1, box.x1), y1: Math.max(drawBox.y1, box.y1) }
      : box;
  }
  if (drawQueued) return;
  drawQueued = true;
  requestAnimationFrame(() => {
    drawQueued = false;
    const z = zoomFactor();
    const sharp = Number.isInteger(z) && z >= 1; // whole multiples: keep pixels crisp
    ctx.imageSmoothingEnabled = !sharp;
    ctx.imageSmoothingQuality = 'high';
    const b = drawAll ? null : drawBox;
    drawAll = false;
    drawBox = null;
    if (!b || !fb.width || !fb.height) {
      ctx.drawImage(fb, 0, 0, cv.width, cv.height);
      return;
    }
    // the changed part in canvas pixels, widened by 2 frame pixels (the resampler looks around a
    // pixel) and rounded out to whole canvas pixels so neighbouring draws meet without seams
    const sx = cv.width / fb.width, sy = cv.height / fb.height, pad = 2;
    const x0 = Math.max(0, b.x0 - pad), y0 = Math.max(0, b.y0 - pad);
    const x1 = Math.min(fb.width, b.x1 + pad), y1 = Math.min(fb.height, b.y1 + pad);
    const dx0 = Math.floor(x0 * sx), dy0 = Math.floor(y0 * sy);
    const dx1 = Math.min(cv.width, Math.ceil(x1 * sx)), dy1 = Math.min(cv.height, Math.ceil(y1 * sy));
    if (dx1 <= dx0 || dy1 <= dy0) return;
    ctx.drawImage(fb, dx0 / sx, dy0 / sy, (dx1 - dx0) / sx, (dy1 - dy0) / sy, dx0, dy0, dx1 - dx0, dy1 - dy0);
  });
}
const $ = (id) => document.getElementById(id);
// protocols drawn on the canvas (the remote framebuffer): they share zoom, paste, copy and the on-screen keyboard
const isCanvas = (p) => p === 'vnc' || p === 'amt' || p === 'ard' || p === 'rdp';

invoke('default_tls').then((t) => { $('tls').checked = t; });
invoke('default_user').then((u) => { $('user').value = u; });
invoke('default_ip').then((ip) => { if (ip) $('ip').value = ip; });
// The password saved in the keychain for this IP: the iDRAC login (user + IP) for SSH, SOL
// and HTML5, the VNC password (empty user) for VNC. savedFor remembers whose entry is
// loaded so unticking Remember can delete it. The keychain is read only while Remember is ticked.
let savedFor = null;
async function fillSaved() {
  // only touch the keychain (which can prompt) while Remember is ticked
  if (!$('remember').checked) { savedFor = null; return; }
  const who = { user: $('proto').value === 'vnc' ? '' : $('user').value || ($('proto').value === 'amt' || $('proto').value === 'amtsol' ? 'admin' : ''), ip: $('ip').value };
  const pw = await invoke('saved_password', who);
  if (pw && (!$('pass').value || savedFor)) {
    $('pass').value = pw;
    savedFor = who;
  } else if (!pw) {
    if (savedFor) $('pass').value = ''; // it was the other protocol's saved password
    savedFor = null;
  }
}
$('ip').onchange = $('user').onchange = () => { $('pass').value = ''; fillSaved(); };
$('pass').oninput = () => { savedFor = null; };
$('remember').onchange = () => { if ($('remember').checked) fillSaved(); };

invoke('default_protocol').then(async (p) => {
  $('proto').value = p;
  setProto();
  const pw = await invoke('default_password', { protocol: p });
  if (pw) $('pass').value = pw;
  await fillSaved();
  setTimeout(() => updateIloRow(), 300); // BOBCAT_IP / BOBCAT_PROTOCOL preselect values
  invoke('autoconnect').then((a) => { if (a) $('go').click(); });
});

$('go').onclick = () => {
  const proto = $('proto').value;
  const remember = $('remember').checked;
  // Remember was ticked off for a loaded entry: delete it
  if (!remember && savedFor) { invoke('forget_password', savedFor); savedFor = null; }
  if (proto === 'java') {
    invoke('java_zoom', { factor: $('zoom').value }); // the iLO console starts at the selected picture size
    invoke('connect_java', { ip: $('ip').value, user: $('user').value, pass: $('pass').value, remember });
  } else if (proto === 'html5') {
    reportLayout();
    invoke('connect_html5', { ip: $('ip').value, user: $('user').value, pass: $('pass').value, remember });
  } else if (proto === 'amt') {
    invoke('connect_amt', { ip: $('ip').value, user: $('user').value || 'admin', pass: $('pass').value, remember });
  } else if (proto === 'ard') {
    invoke('connect_ard', { ip: $('ip').value, port: Number($('port').value), user: $('user').value, pass: $('pass').value, remember });
  } else if (proto === 'rdp') {
    // with "Fit window" the Windows desktop is made as big as the canvas area (and follows the window); otherwise the default size
    const want = $('zoom').value === 'fit' ? rdpArea() : {};
    invoke('connect_rdp', { ip: $('ip').value, port: Number($('port').value), user: $('user').value, pass: $('pass').value, remember, width: want.w, height: want.h });
  } else if (proto === 'vnc') {
    invoke('connect', { ip: $('ip').value, port: Number($('port').value), tls: $('tls').checked, pass: $('pass').value, remember });
  } else {
    ensureTerm();
    term.reset();
    fit.fit();
    invoke('connect_text', { protocol: proto, ip: $('ip').value, user: $('user').value, pass: $('pass').value, remember, cols: term.cols, rows: term.rows });
    term.focus();
  }
};
$('stop').onclick = () => {
  invoke('disconnect');
  if ($('proto').value === 'html5') invoke('disconnect_html5');
  if ($('proto').value === 'java') invoke('disconnect_java');
};

// The Java console needs a Java runtime; the app asks before downloading one.
listen('java-need-runtime', () => { $('jre').style.display = 'flex'; });
listen('java-runtime-ready', () => { $('go').click(); });
$('jredownload').onclick = () => { $('jre').style.display = 'none'; invoke('download_java_runtime'); };
$('jrecancel').onclick = () => { $('jre').style.display = 'none'; };

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
// RDP: the size the Windows desktop can have to fill the canvas area (even width, within what the protocol allows)
function rdpArea() {
  const wrap = $('wrap');
  const clamp = (v) => Math.max(200, Math.min(8192, v));
  return { w: clamp(wrap.clientWidth - 2 * PAD) & ~1, h: clamp(wrap.clientHeight - 2 * PAD) };
}
let rdpResizeTimer = null;
function rdpFollowWindow() {
  if ($('proto').value !== 'rdp' || $('zoom').value !== 'fit') return;
  clearTimeout(rdpResizeTimer);
  rdpResizeTimer = setTimeout(() => { const a = rdpArea(); invoke('rdp_resize', { w: a.w, h: a.h }); }, 400);
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
  if ($('proto').value === 'java') { invoke('java_zoom', { factor: $('zoom').value }); return; }
  if ($('proto').value === 'html5') { invoke('html5_zoom', { factor: $('zoom').value === 'fit' ? 1 : Number($('zoom').value) }); return; }
  if (!isCanvas($('proto').value)) { applyTermFont(); return; }
  applyZoom();
  if ($('zoom').value !== 'fit') fitWindow();
  rdpFollowWindow();
};
window.addEventListener('resize', () => {
  reportLayout();
  rdpFollowWindow();
  if (!isCanvas($('proto').value)) { if (fit) fit.fit(); return; }
  if ($('zoom').value === 'fit') applyZoom();
});

function screenResized(w, h) {
  fb.width = w;
  fb.height = h;
  applyZoom();
  if ($('zoom').value !== 'fit') fitWindow();
}
fbctx.imageSmoothingEnabled = false; // copies inside the picture must be exact
function copyRect(sx, sy, dx, dy, w, h) {
  fbctx.drawImage(fb, sx, sy, w, h, dx, dy, w, h); // the server scrolled or moved something
  redraw({ x0: dx, y0: dy, x1: dx + w, y1: dy + h });
}
function putRect(x, y, w, h, px) {
  fbctx.putImageData(new ImageData(px, w, h), x, y);
  redraw({ x0: x, y0: y, x1: x + w, y1: y + h });
}
// The picture arrives as raw bytes on a channel (kind 0: rectangle x, y, w, h + RGBA pixels;
// kind 1: screen size w, h; kind 2: copy sx, sy, dx, dy, w, h inside the picture; little-endian u16s), which avoids base64 and JSON for big screens.
const frameChannel = new window.__TAURI__.core.Channel();
frameChannel.onmessage = (msg) => {
  let buf = msg;
  if (Array.isArray(buf)) buf = new Uint8Array(buf).buffer;
  else if (ArrayBuffer.isView(buf)) buf = buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength);
  const dv = new DataView(buf);
  const u16 = (at) => dv.getUint16(at, true);
  // one message holds all of a server update: messages one after another
  for (let o = 0; o < buf.byteLength;) {
    const kind = dv.getUint8(o);
    if (kind === 1) { screenResized(u16(o + 1), u16(o + 3)); o += 5; }
    else if (kind === 2) { copyRect(u16(o + 1), u16(o + 3), u16(o + 5), u16(o + 7), u16(o + 9), u16(o + 11)); o += 13; }
    else if (kind === 0) {
      const x = u16(o + 1), y = u16(o + 3), w = u16(o + 5), h = u16(o + 7);
      putRect(x, y, w, h, new Uint8ClampedArray(buf, o + 9, w * h * 4));
      o += 9 + w * h * 4;
    } else break; // not a message this knows
  }
};
invoke('set_frame_channel', { channel: frameChannel });
// the same through events, used only if the channel was not set (older backend)
listen('vnc-resize', (e) => screenResized(e.payload.w, e.payload.h));
listen('vnc-copy', (e) => copyRect(...e.payload));
listen('vnc-frame', (e) => {
  const { x, y, w, h, data } = e.payload;
  const bin = atob(data);
  const px = new Uint8ClampedArray(bin.length);
  for (let i = 0; i < bin.length; i++) px[i] = bin.charCodeAt(i);
  putRect(x, y, w, h, px);
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
  if ($('proto').value === 'java') {
    $('status').textContent = 'typing the clipboard into the Java console...';
    try {
      const n = await invoke('java_paste');
      $('status').textContent = `pasted ${n} characters`;
    } catch (err) {
      $('status').textContent = `paste failed: ${err}`;
    }
    return;
  }
  if ($('proto').value === 'html5') {
    try {
      const text = await invoke('clipboard_text');
      const keys = pasteKeys(text).filter((k) => k.code).map((k) => ({ key: k.ch, code: k.code, key_code: k.keyCode, shift: k.shift }));
      const n = await invoke('html5_paste', { keys });
      $('status').textContent = `pasted ${n} characters`;
    } catch (err) {
      $('status').textContent = `paste failed: ${err}`;
    }
    return;
  }
  if (!isCanvas($('proto').value)) {
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
    const text = await invoke('clipboard_text');
    const keys = pasteKeys(text).map((k) => [k.keysym, k.shift]);
    const n = await invoke('paste_clipboard', { keys });
    $('status').textContent = `pasted ${n} characters`;
  } catch (err) {
    $('status').textContent = `paste failed: ${err}`;
  }
}
$('paste').onclick = () => { paste(); const p = $('proto').value; if (isCanvas(p)) cv.focus(); else if (p !== 'html5') term.focus(); };

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
  moveWaiting = null; // this event carries the newest position
  const { x, y } = pos(e);
  return invoke('send_pointer', { buttons: buttons | extra, x, y });
};
// Mouse moves are sent at most once per animation frame, with the newest position. A mouse reports
// hundreds of moves a second, and each one used to be its own call: dragging a window then queued
// up far more moves than the Mac could draw and it kept replaying them after the mouse stopped.
let moveWaiting = null, moveQueued = false;
function pointerMove(e) {
  moveWaiting = { buttons, ...pos(e) };
  if (moveQueued) return;
  moveQueued = true;
  requestAnimationFrame(() => {
    moveQueued = false;
    const m = moveWaiting;
    moveWaiting = null;
    if (m) invoke('send_pointer', m);
  });
}

let copyMode = false;
let selStart = null;
const sel = $('sel');
function setCopyMode(on) {
  copyMode = on;
  selStart = null;
  sel.style.display = 'none';
  cv.classList.toggle('copying', on);
  $('copy').textContent = on ? 'Cancel copy' : 'Copy';
  if (on) $('status').textContent = 'drag over the text to copy (OCR); Esc cancels';
}
$('copy').onclick = async () => {
  const p = $('proto').value;
  if (isCanvas(p)) { setCopyMode(!copyMode); return; }
  if (p === 'java') {
    $('status').textContent = 'reading the Java console screen...';
    try {
      const text = await invoke('java_copy_text');
      $('status').textContent = `copied ${text.length} characters: ${text.slice(0, 60).replace(/\n/g, ' ')}`;
    } catch (err) {
      $('status').textContent = `copy failed: ${err}`;
    }
    return;
  }
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
  if (copyMode) return; buttons = rfbButtons(e.buttons); pointerMove(e); });
cv.addEventListener('mousedown', (e) => { if (copyMode) return; cv.focus(); buttons = rfbButtons(e.buttons); pointer(e); });
cv.addEventListener('mouseup', (e) => { if (copyMode) return; buttons = rfbButtons(e.buttons); pointer(e); });
cv.addEventListener('contextmenu', (e) => e.preventDefault());
// The wheel is a button press and release (RFB buttons 4 and 5) per wheel event. (Adding the wheel
// movement up and sending a step per 80 pixels made scrolling worse, so it is one step per event.)
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

let prevProto = null;
// show only the controls that apply to the chosen protocol
function setProto() {
  const proto = $('proto').value;
  const kind = proto === 'vnc' || proto === 'amt' || proto === 'ard' || proto === 'rdp' ? 'vnc' : proto === 'html5' ? 'html5' : proto === 'java' ? 'java' : 'text';
  const hide = (sel, yes) => document.querySelectorAll(sel).forEach((el) => el.classList.toggle('hidden', yes));
  hide('.vnc-only', kind !== 'vnc');
  hide('.amt-only', proto !== 'amt' && proto !== 'amtsol'); // virtual media (IDE-R) of an AMT machine
  hide('.vnc-net', proto !== 'vnc' && proto !== 'ard' && proto !== 'rdp'); // port (and TLS) of a VNC server: the iDRAC's, a Mac's, or Windows' RDP port
  hide('.vnc-tls', proto !== 'vnc');           // TLS is only for the iDRAC's VNC server
  hide('.login-only', proto === 'vnc');          // iDRAC user name: SSH, SOL and HTML5 log in
  hide('.no-html5', kind === 'html5');          // paste / copy / keyboard / text size are for the console views
  hide('.layout-sel', kind === 'java' || kind === 'text'); // the keyboard layout pasting follows
  document.querySelectorAll('#zoom .java-only').forEach((o) => { o.hidden = o.disabled = kind !== 'java'; }); // Stretch is for the iLO Java console
  if (kind !== 'java' && $('zoom').value === 'stretch') $('zoom').value = 'fit';
  hide('.no-java', kind === 'java');            // the Java viewer is a window of its own
  hide('.java-mac', kind === 'java' && !/Mac|Linux/.test(navigator.platform)); // Copy and Paste reach it on macOS and Linux
  $('wrap').classList.toggle('hidden', kind !== 'vnc');
  $('term').classList.toggle('hidden', kind !== 'text');
  $('html5note').classList.toggle('hidden', kind !== 'html5');
  $('javanote').classList.toggle('hidden', kind !== 'java');
  $('pass').placeholder = proto === 'vnc' ? 'VNC password' : proto === 'amt' || proto === 'amtsol' ? 'AMT password' : proto === 'ard' ? 'Mac password' : proto === 'rdp' ? 'Windows password' : 'iDRAC password';
  $('user').placeholder = proto === 'ard' ? 'Mac user name' : proto === 'rdp' ? 'Windows user (DOMAIN\\user for a domain)' : 'iDRAC / AMT user';
  // each protocol has its own default port (iDRAC VNC 5901, Mac 5900, Windows RDP 3389): the port follows the protocol while it still holds the previous default
  const ports = { ard: '5900', rdp: '3389' };
  const dflt = (p) => ports[p] || '5901';
  if (prevProto && prevProto !== proto && $('port').value === dflt(prevProto)) $('port').value = dflt(proto);
  prevProto = proto;
  // the Certificate button shows the TLS certificate (VNC, HTML5) or SSH host key; SOL has neither
  $('certbtn').classList.toggle('hidden', proto === 'sol' || proto === 'java' || proto === 'ard');
  $('certbtn').textContent = proto === 'ssh' ? 'Host key' : 'Certificate';
  if (kind === 'text') { ensureTerm(); fit.fit(); }
  reportLayout();
  invoke('default_password', { protocol: proto }).then((pw) => { if (pw) $('pass').value = pw; else fillSaved(); });
}
$('proto').onchange = () => {
  invoke('disconnect'); invoke('disconnect_html5'); invoke('disconnect_java');
  if ($('proto').value !== 'amt' && $('proto').value !== 'amtsol') invoke('ider_unmount'); // virtual media belongs to the AMT views
  setProto();
};

// ---- AMT virtual media (IDE-R): serve an ISO from this computer to the AMT machine ----------
try { const last = localStorage.getItem('bobcat-iso'); if (last) $('iso').value = last; } catch (e) {}
$('isobrowse').onclick = async () => {
  const p = await invoke('pick_iso');
  if (p) { $('iso').value = p; try { localStorage.setItem('bobcat-iso', p); } catch (e) {} }
};
$('isomount').onclick = () => {
  const iso = $('iso').value.trim();
  if (!iso) { $('iderstatus').textContent = 'choose an ISO first'; return; }
  if (!$('pass').value) { $('iderstatus').textContent = 'enter the AMT password first'; return; }
  try { localStorage.setItem('bobcat-iso', iso); } catch (e) {}
  invoke('ider_mount', { ip: $('ip').value, user: $('user').value || 'admin', pass: $('pass').value, iso, start: $('iderstart').value, remember: $('remember').checked });
};
$('isounmount').onclick = () => invoke('ider_unmount');
listen('ider-status', (e) => {
  $('iderstatus').textContent = e.payload.text;
  $('isomount').disabled = e.payload.mounted;
  $('isounmount').disabled = !e.payload.mounted;
});
$('isounmount').disabled = true;

// ---- AMT power, boot and redirection controls ---------------------------------------------
const amtArgs = () => ({ ip: $('ip').value, user: $('user').value || 'admin', pass: $('pass').value });
const amtNote = (text) => { $('amtnote').textContent = text; $('amtnote').title = text; };
// a disruptive button needs a second click within 4 s (the page cannot rely on confirm())
function confirmed(btn, action) {
  return () => {
    if (!$('pass').value) { amtNote('enter the AMT password first'); return; }
    if (btn.classList.contains('armed')) {
      btn.classList.remove('armed'); btn.textContent = btn.dataset.label; clearTimeout(btn._t); action();
    } else {
      btn.dataset.label = btn.textContent; btn.textContent = 'Sure?'; btn.classList.add('armed');
      btn._t = setTimeout(() => { btn.classList.remove('armed'); btn.textContent = btn.dataset.label; }, 4000);
    }
  };
}
const needPass = (f) => () => { if (!$('pass').value) { amtNote('enter the AMT password first'); return; } f(); };
$('amtstatus').onclick = needPass(() => invoke('amt_status', amtArgs()));
document.querySelectorAll('#amtrow button[data-power]').forEach((b) => {
  const run = () => invoke('amt_power', { ...amtArgs(), action: b.dataset.power });
  b.onclick = b.classList.contains('confirm') ? confirmed(b, run) : needPass(run);
});
$('amtbootset').onclick = needPass(() => invoke('amt_boot', { ...amtArgs(), what: $('amtboot').value, sol: $('amtbootsol').checked }));
$('amtider').onclick = needPass(() => invoke('amt_ider_enable', amtArgs()));
$('amtbootiso').onclick = confirmed($('amtbootiso'), () => {
  const iso = $('iso').value.trim();
  if (!iso) { amtNote('choose an ISO first'); return; }
  try { localStorage.setItem('bobcat-iso', iso); } catch (e) {}
  invoke('amt_boot_iso', { ...amtArgs(), iso, sol: $('amtbootsol').checked });
});
listen('amt-note', (e) => amtNote(e.payload.text));

// ---- HPE iLO power and IPMI controls (the row shows for an iLO with the console protocols) -----
const iloArgs = () => ({ ip: $('ip').value, user: $('user').value || 'admin', pass: $('pass').value });
const iloNote = (text) => { $('ilonote').textContent = text; $('ilonote').title = text; };
const iloKnown = {}; // ip -> is an iLO
async function updateIloRow() {
  const proto = $('proto').value, ip = $('ip').value.trim();
  const mine = ['html5', 'java', 'ssh', 'sol'].includes(proto) && ip;
  if (!mine) { $('ilorow').classList.add('hidden'); return; }
  if (!(ip in iloKnown)) { try { iloKnown[ip] = await invoke('bmc_is_ilo', { ip }); } catch (_) { iloKnown[ip] = false; } }
  const still = $('ip').value.trim() === ip && ['html5', 'java', 'ssh', 'sol'].includes($('proto').value);
  $('ilorow').classList.toggle('hidden', !(still && iloKnown[ip]));
}
const iloNeedPass = (f) => () => { if (!$('pass').value) { iloNote('enter the iLO password first'); return; } f(); };
function iloConfirmed(btn, action) {
  return () => {
    if (!$('pass').value) { iloNote('enter the iLO password first'); return; }
    if (btn.classList.contains('armed')) {
      btn.classList.remove('armed'); btn.textContent = btn.dataset.label; clearTimeout(btn._t); action();
    } else {
      btn.dataset.label = btn.textContent; btn.textContent = 'Sure?'; btn.classList.add('armed');
      btn._t = setTimeout(() => { btn.classList.remove('armed'); btn.textContent = btn.dataset.label; }, 4000);
    }
  };
}
$('ilostatus').onclick = iloNeedPass(() => invoke('ilo_status', iloArgs()));
document.querySelectorAll('#ilorow button[data-ilo]').forEach((b) => {
  const run = () => invoke('ilo_power', { ...iloArgs(), action: b.dataset.ilo });
  b.onclick = b.classList.contains('confirm') ? iloConfirmed(b, run) : iloNeedPass(run);
});
let iloIpmiOn = null; // last known state of IPMI over LAN (null: not known)
const iloIpmiButton = (on) => {
  iloIpmiOn = on;
  const b = $('iloipmi');
  b.textContent = on ? 'Disable IPMI' : 'Enable IPMI';
  b.classList.toggle('confirm', !!on); // turning it off can cut a running SOL session: click twice
  b.classList.remove('armed'); clearTimeout(b._t);
};
$('iloipmi').onclick = (ev) => {
  const b = ev.currentTarget, run = () => invoke('ilo_ipmi', { ...iloArgs(), on: !iloIpmiOn });
  (b.classList.contains('confirm') ? iloConfirmed(b, run) : iloNeedPass(run))();
};
$('ilosessions').onclick = iloNeedPass(() => invoke('ilo_sessions', iloArgs()));
listen('ilo-ipmi', (e) => iloIpmiButton(e.payload));
listen('ilo-note', (e) => iloNote(e.payload.text));
$('ip').addEventListener('change', () => updateIloRow());
$('proto').addEventListener('change', () => updateIloRow());
setProto();

// ---- HTML5 console embedded below the toolbar ----------------------------
// The console is a second web view inside this window, stacked under this page. This page
// then only needs the height of its toolbar and status bar; tell the app so it can give the
// rest of the window to the console.
function reportLayout() {
  if ($('proto').value !== 'html5' && $('proto').value !== 'java') return; // views placed over the console area
  invoke('html5_layout', { height: Math.ceil($('bar').offsetHeight + $('statusbar').offsetHeight), bar: Math.ceil($('bar').offsetHeight) });
}
new ResizeObserver(reportLayout).observe($('bar'));

