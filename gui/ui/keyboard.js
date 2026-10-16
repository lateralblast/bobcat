// On-screen keyboard. Click keys to send them to the console.
// Shift/Ctrl/Alt/Meta are one-shot: click to latch, applied to the next key.
// Layouts only change the labels and the characters (X11 keysyms) that are
// sent; how the server turns those into key presses depends on the server.
const P = (lo, hi) => [lo, hi]; // key with unshifted and shifted characters

const FKEYS = ['Esc', ...Array.from({ length: 12 }, (_, i) => 'F' + (i + 1))];
const NAV = ['Ctrl', 'Alt', 'Meta', 'Space', 'Left', 'Down', 'Right', 'Del', 'Home', 'End', 'PgUp', 'PgDn', 'Ins'];

const LAYOUTS = {
  US: [
    FKEYS,
    [P('`', '~'), P('1', '!'), P('2', '@'), P('3', '#'), P('4', '$'), P('5', '%'), P('6', '^'), P('7', '&'), P('8', '*'), P('9', '('), P('0', ')'), P('-', '_'), P('=', '+'), 'Backspace'],
    ['Tab', ...'qwertyuiop', P('[', '{'), P(']', '}'), P('\\', '|')],
    ['CapsLock', ...'asdfghjkl', P(';', ':'), P("'", '"'), 'Enter'],
    ['Shift', ...'zxcvbnm', P(',', '<'), P('.', '>'), P('/', '?'), 'Up'],
    NAV,
  ],
  UK: [
    FKEYS,
    [P('`', '¬'), P('1', '!'), P('2', '"'), P('3', '£'), P('4', '$'), P('5', '%'), P('6', '^'), P('7', '&'), P('8', '*'), P('9', '('), P('0', ')'), P('-', '_'), P('=', '+'), 'Backspace'],
    ['Tab', ...'qwertyuiop', P('[', '{'), P(']', '}')],
    ['CapsLock', ...'asdfghjkl', P(';', ':'), P("'", '@'), P('#', '~'), 'Enter'],
    ['Shift', P('\\', '|'), ...'zxcvbnm', P(',', '<'), P('.', '>'), P('/', '?'), 'Up'],
    NAV,
  ],
  DE: [
    FKEYS,
    [P('^', '°'), P('1', '!'), P('2', '"'), P('3', '§'), P('4', '$'), P('5', '%'), P('6', '&'), P('7', '/'), P('8', '('), P('9', ')'), P('0', '='), P('ß', '?'), P('´', '`'), 'Backspace'],
    ['Tab', ...'qwertzuiopü', P('+', '*')],
    ['CapsLock', ...'asdfghjklöä', P('#', "'"), 'Enter'],
    ['Shift', P('<', '>'), ...'yxcvbnm', P(',', ';'), P('.', ':'), P('-', '_'), 'Up'],
    NAV,
  ],
  FR: [
    FKEYS,
    [P('²', '²'), P('&', '1'), P('é', '2'), P('"', '3'), P("'", '4'), P('(', '5'), P('-', '6'), P('è', '7'), P('_', '8'), P('ç', '9'), P('à', '0'), P(')', '°'), P('=', '+'), 'Backspace'],
    ['Tab', ...'azertyuiop', P('^', '¨'), P('$', '£')],
    ['CapsLock', ...'qsdfghjklm', P('ù', '%'), P('*', 'µ'), 'Enter'],
    ['Shift', P('<', '>'), ...'wxcvbn', P(',', '?'), P(';', '.'), P(':', '/'), P('!', '§'), 'Up'],
    NAV,
  ],
};
const OSK_NAMED = {
  Esc: 0xff1b, Backspace: 0xff08, Tab: 0xff09, Enter: 0xff0d, CapsLock: 0xffe5, Space: 0x20,
  Up: 0xff52, Down: 0xff54, Left: 0xff51, Right: 0xff53, Del: 0xffff, Home: 0xff50,
  End: 0xff57, PgUp: 0xff55, PgDn: 0xff56, Ins: 0xff63,
};
for (let i = 1; i <= 12; i++) OSK_NAMED['F' + i] = 0xffbd + i;
const OSK_MODS = { Shift: 0xffe1, Ctrl: 0xffe3, Alt: 0xffe9, Meta: 0xffeb };
const OSK_WIDE = { Backspace: 2, Tab: 1.5, CapsLock: 1.8, Enter: 2.2, Shift: 2.2, Space: 6 };

const osk = { el: null, latched: new Set(), btns: [], layout: 'US', rows: null };
try { const l = (localStorage.getItem('bobcat.layout') || localStorage.getItem('dracon.layout')); if (l in LAYOUTS) osk.layout = l; } catch (_) {}

const keysymOf = (ch) => { const cp = ch.codePointAt(0); return cp < 0x100 ? cp : 0x01000000 + cp; };

// Every button: { b, key } where key is a named key string, a letter, or a [lo, hi] pair.
function oskRefresh() {
  const shift = osk.latched.has('Shift');
  for (const { b, key } of osk.btns) {
    if (typeof key === 'string' && key in OSK_MODS) b.classList.toggle('on', osk.latched.has(key));
    if (Array.isArray(key)) b.textContent = shift ? key[1] : key[0];
    else if (typeof key === 'string' && !(key in OSK_NAMED) && !(key in OSK_MODS)) {
      b.textContent = shift ? key.toUpperCase() : key;
    }
  }
}

// Text consoles (SSH / SOL) take bytes, not X11 keysyms.
const TEXT_KEYS = {
  0xff0d: '\r', 0xff08: '\x7f', 0xff09: '\t', 0xff1b: '\x1b', 0xffff: '\x1b[3~',
  0xff50: '\x1b[H', 0xff57: '\x1b[F', 0xff51: '\x1b[D', 0xff52: '\x1b[A', 0xff53: '\x1b[C',
  0xff54: '\x1b[B', 0xff55: '\x1b[5~', 0xff56: '\x1b[6~', 0xff63: '\x1b[2~', 0xffe5: '',
};
const TEXT_FKEYS = ['\x1bOP', '\x1bOQ', '\x1bOR', '\x1bOS', '\x1b[15~', '\x1b[17~', '\x1b[18~', '\x1b[19~', '\x1b[20~', '\x1b[21~', '\x1b[23~', '\x1b[24~'];
function keysymToText(sym, latched) {
  let s;
  if (sym in TEXT_KEYS) s = TEXT_KEYS[sym];
  else if (sym >= 0xffbe && sym <= 0xffc9) s = TEXT_FKEYS[sym - 0xffbe];
  else {
    s = String.fromCodePoint(sym >= 0x01000000 ? sym - 0x01000000 : sym);
    if (latched.has('Ctrl') && s.length === 1) {
      const c = s.toUpperCase().charCodeAt(0);
      if (c >= 0x40 && c <= 0x5f) s = String.fromCharCode(c - 0x40);
      else if (s === '?') s = '\x7f';
      else if (s === ' ') s = '\x00';
    }
  }
  return latched.has('Alt') ? '\x1b' + s : s;
}

async function oskPress(key) {
  if (typeof key === 'string' && key in OSK_MODS) {
    osk.latched.has(key) ? osk.latched.delete(key) : osk.latched.add(key);
    oskRefresh();
    return;
  }
  const shifted = osk.latched.has('Shift');
  let sym;
  if (Array.isArray(key)) sym = keysymOf(shifted ? key[1] : key[0]);
  else if (key in OSK_NAMED) sym = OSK_NAMED[key];
  else sym = keysymOf(shifted ? key.toUpperCase() : key);
  if (!isCanvas($('proto').value)) {
    const text = keysymToText(sym, osk.latched);
    if (text) await invoke('term_input', { data: btoa(String.fromCharCode(...new TextEncoder().encode(text))) });
    osk.latched.clear();
    oskRefresh();
    if (typeof term !== 'undefined' && term) term.focus();
    return;
  }
  const mods = [...osk.latched].map((m) => OSK_MODS[m]);
  for (const m of mods) await key_(true, m);
  await key_(true, sym);
  await key_(false, sym);
  for (const m of mods.reverse()) await key_(false, m);
  osk.latched.clear();
  oskRefresh();
}
const key_ = (d, k) => key(d, k); // `key` comes from main.js

// Drag the panel by its header. The first drag switches it from CSS centring
// to explicit left/top so it stays where it is dropped.
function oskDrag(el, handle) {
  let dx = 0, dy = 0, dragging = false;
  handle.addEventListener('pointerdown', (e) => {
    if (e.target.tagName === 'BUTTON') return;
    const r = el.getBoundingClientRect();
    el.style.transform = 'none';
    el.style.bottom = 'auto';
    el.style.left = r.left + 'px';
    el.style.top = r.top + 'px';
    dx = e.clientX - r.left;
    dy = e.clientY - r.top;
    dragging = true;
    handle.setPointerCapture(e.pointerId);
    e.preventDefault();
  });
  handle.addEventListener('pointermove', (e) => {
    if (!dragging) return;
    const maxX = window.innerWidth - el.offsetWidth;
    const maxY = window.innerHeight - el.offsetHeight;
    el.style.left = Math.max(0, Math.min(maxX, e.clientX - dx)) + 'px';
    el.style.top = Math.max(0, Math.min(maxY, e.clientY - dy)) + 'px';
  });
  const stop = () => { dragging = false; };
  handle.addEventListener('pointerup', stop);
  handle.addEventListener('pointercancel', stop);
}

function oskBuildKeys() {
  osk.rows.innerHTML = '';
  osk.btns = [];
  for (const row of LAYOUTS[osk.layout]) {
    const r = document.createElement('div');
    r.className = 'osk-row';
    for (const key of row) {
      const b = document.createElement('button');
      b.textContent = Array.isArray(key) ? key[0] : key;
      const w = OSK_WIDE[key] || 1; // key width in units; fixed size, never shrinks
      b.style.width = `${w * 42 + (w - 1) * 4}px`;
      b.onmousedown = (e) => e.preventDefault(); // keep focus where it is
      b.onclick = () => oskPress(key);
      osk.btns.push({ b, key });
      r.appendChild(b);
    }
    osk.rows.appendChild(r);
  }
  oskRefresh();
}

function oskSetLayout(name) {
  osk.layout = name;
  try { localStorage.setItem('bobcat.layout', name); } catch (_) {}
  if ($('kblayout')) $('kblayout').value = name;
  if (!osk.el) return; // the on-screen keyboard is not built yet: it picks the layout up when it is
  for (const [n, b] of Object.entries(osk.layoutBtns)) b.classList.toggle('on', n === name);
  oskBuildKeys();
}

function oskBuild() {
  const el = document.createElement('div');
  el.id = 'osk';
  const head = document.createElement('div');
  head.className = 'osk-head';
  const title = document.createElement('span');
  title.textContent = 'On-screen keyboard';
  head.appendChild(title);
  const right = document.createElement('span');
  right.className = 'osk-layouts';
  osk.layoutBtns = {};
  for (const name of Object.keys(LAYOUTS)) {
    const lb = document.createElement('button');
    lb.textContent = name;
    lb.title = { US: 'US English', UK: 'UK English', DE: 'German (QWERTZ)', FR: 'French (AZERTY)' }[name];
    lb.onmousedown = (e) => e.preventDefault();
    lb.onclick = () => oskSetLayout(name);
    osk.layoutBtns[name] = lb;
    right.appendChild(lb);
  }
  const x = document.createElement('button');
  x.textContent = '×';
  x.onclick = () => oskToggle(false);
  right.appendChild(x);
  head.appendChild(right);
  el.appendChild(head);
  oskDrag(el, head);
  osk.rows = document.createElement('div');
  el.appendChild(osk.rows);
  document.body.appendChild(el);
  osk.el = el;
  oskSetLayout(osk.layout);
}

function oskToggle(show) {
  if (!osk.el) oskBuild();
  osk.el.style.display = (show ?? osk.el.style.display === 'none') ? 'block' : 'none';
}
document.getElementById('kbd').onclick = () => oskToggle(osk.el ? undefined : true);

// ---- paste follows the selected layout -------------------------------------
// The characters of the clipboard are typed the way the selected layout's keys would produce them:
// for each character, the key (by position, as the US key with the same place) and whether Shift is
// needed. Characters that are not on the layout (AltGr ones such as @ on a German keyboard) are
// sent as themselves with the US Shift rule.
const US_POS = [
  // [code, keyCode] per key, by row (number row, top row, home row, bottom row)
  [['Backquote', 192], ...'1234567890'.split('').map((d) => ['Digit' + d, d.charCodeAt(0)]), ['Minus', 189], ['Equal', 187]],
  [...'QWERTYUIOP'.split('').map((c) => ['Key' + c, c.charCodeAt(0)]), ['BracketLeft', 219], ['BracketRight', 221], ['Backslash', 220]],
  [...'ASDFGHJKL'.split('').map((c) => ['Key' + c, c.charCodeAt(0)]), ['Semicolon', 186], ['Quote', 222], ['Backslash', 220]],
  [...'ZXCVBNM'.split('').map((c) => ['Key' + c, c.charCodeAt(0)]), ['Comma', 188], ['Period', 190], ['Slash', 191]],
];
const PASTE_MAPS = {};
function pasteMap(name) {
  if (PASTE_MAPS[name]) return PASTE_MAPS[name];
  const map = new Map(); // character -> { shift, code, keyCode }
  const rows = LAYOUTS[name].slice(1, 5).map((r) => r.filter((k) => Array.isArray(k) || (k.length === 1)));
  rows.forEach((keys, r) => {
    // a bottom row with one more key than the US row starts with the extra ISO key (< or \)
    const extra = r === 3 && keys.length > US_POS[3].length ? 1 : 0;
    keys.forEach((key, i) => {
      const pos = extra && i === 0 ? ['IntlBackslash', 226] : (US_POS[r][i - extra] || ['Backslash', 220]);
      const [lo, hi] = Array.isArray(key) ? key : [key, key.toUpperCase()];
      if (!map.has(lo)) map.set(lo, { shift: false, code: pos[0], keyCode: pos[1] });
      if (!map.has(hi)) map.set(hi, { shift: true, code: pos[0], keyCode: pos[1] });
    });
  });
  PASTE_MAPS[name] = map;
  return map;
}
const US_SHIFTED = '~!@#$%^&*()_+{}|:"<>?';
const US_FALLBACK = { Space: ['Space', 32] };

// [{ ch, keysym, shift, code, keyCode }] for the characters of text on the selected layout
function pasteKeys(text) {
  const map = pasteMap(osk.layout);
  const usmap = pasteMap('US');
  const out = [];
  for (const ch of text) {
    if (ch === '\r') continue;
    if (ch === '\n') { out.push({ ch, keysym: 0xff0d, shift: false, code: 'Enter', keyCode: 13 }); continue; }
    if (ch === '\t') { out.push({ ch, keysym: 0xff09, shift: false, code: 'Tab', keyCode: 9 }); continue; }
    if (ch === ' ') { out.push({ ch, keysym: 0x20, shift: false, code: 'Space', keyCode: 32 }); continue; }
    const hit = map.get(ch);
    if (hit) { out.push({ ch, keysym: keysymOf(ch), ...hit }); continue; }
    const us = usmap.get(ch);
    out.push({ ch, keysym: keysymOf(ch), shift: us ? us.shift : (US_SHIFTED.includes(ch) || (ch >= 'A' && ch <= 'Z')), code: us ? us.code : null, keyCode: us ? us.keyCode : 0 });
  }
  return out;
}

$('kblayout').value = osk.layout;
$('kblayout').onchange = () => oskSetLayout($('kblayout').value);
