import init, {
  WasmCpu, init_ra4m1, reset_state, tick_n,
  periph_read as _pr, periph_write as _pw,
  usb_take_tx, usb_rx_inject,
  usb_host_attach, usb_host_reset, usb_host_setup, usb_host_status_done,
} from './pkg/uno_r4_minima_wasm.js';

const APP_BASE = 0x4000;
const PORT_BASE = 0x40040000;
const USBFS = 0x40090000;
const CHUNK = 48000;

const $ = (id) => document.getElementById(id);
const banner = (msg, kind) => {
  const b = $('banner');
  if (!msg) { b.className = 'banner hidden'; return; }
  b.className = 'banner ' + kind;
  b.textContent = msg;
};

/* ---------- emulator driver (mirrors the Rust proof harness) ---------- */
let board = null, running = false, steps = 0, curFw = null;

function boot(fwBytes) {
  reset_state();
  init_ra4m1();
  const dv = new DataView(fwBytes.buffer, fwBytes.byteOffset, fwBytes.byteLength);
  const sp = dv.getUint32(0, true), pc = dv.getUint32(4, true);
  board = new WasmCpu(sp, pc);
  board.load_firmware(fwBytes, APP_BASE);
  board.reset_cpu(sp, pc);
  board.set_deliver_irqs(true);
  steps = 0;
  banner(null);
  setState('paused');
}

function pump(n) {
  board.step(n);
  tick_n(n);
  steps += n;
  const f = board.fault_pc();
  if (f !== 0xFFFFFFFF) {
    running = false;
    setState('fault');
    banner('CPU fault at 0x' + f.toString(16) + ' — see console for details.', 'err');
    return false;
  }
  return true;
}

async function runChunks(total, perFrame, onFrame) {
  let left = total;
  while (left > 0 && running) {
    const n = Math.min(perFrame, left);
    if (!pump(n)) return false;
    left -= n;
    if (onFrame) onFrame();
    await new Promise((r) => setTimeout(r, 0));
  }
  return running;
}

async function runUntil(budget, cond) {
  let left = budget;
  while (left > 0 && !cond()) {
    const n = Math.min(CHUNK, left);
    if (!pump(n)) return false;
    left -= n;
    await new Promise((r) => setTimeout(r, 0));
  }
  return true;
}

function usbQuiesce() {
  // Like the Rust helper: never pipeline SETUP over an unfinished
  // transfer (CTRT latched means the device is still busy).
  return runUntil(400000, () => (periphRead(USBFS + 0x40, 2) & (1 << 11)) === 0);
}

function ctlIn(req, val, idx, len, want, drain) {
  return (async () => {
    await usbQuiesce();
    usb_host_setup(req, val, idx, len);
    const got = [];
    await runUntil(1200000, () => {
      got.push(...usb_take_tx());
      return got.length >= want;
    });
    await runUntil(400000, () => false);
    usb_host_status_done();
    await runUntil(400000, () => false);
    got.push(...usb_take_tx());
    if (drain) drain.push(...got);
    return got;
  })();
}

async function ctlOut(req, val, idx, data) {
  await usbQuiesce();
  usb_host_setup(req, val, idx, data.length);
  await runUntil(400000, () => false);
  if (data.length) usb_rx_inject(0, data);
  await runUntil(900000, () => false);
}

/* ---------------- board visuals ---------------- */
const gpioCells = [];
function buildGpio() {
  const g = $('gpio-grid');
  for (let p = 0; p < 12; p++) for (let b = 0; b < 16; b++) {
    const d = document.createElement('div');
    d.className = 'pin'; d.title = `P${p}${b.toString().padStart(2, '0')}`;
    g.appendChild(d); gpioCells.push(d);
  }
  const mk = (id, n, y) => {
    const g2 = $(id);
    for (let i = 0; i < n; i++) {
      const c = document.createElementNS('http://www.w3.org/2000/svg', 'circle');
      c.setAttribute('cx', 30 + i * 19); c.setAttribute('cy', y);
      c.setAttribute('r', 5); g2.appendChild(c);
    }
  };
  mk('hdr-top', 15, 22); mk('hdr-bottom', 15, 218);
}

let lastPorts = new Array(12).fill(0);
function refreshBoard(txFlash, rxFlash) {
  let led = false;
  for (let p = 0; p < 12; p++) {
    const v = periphRead(PORT_BASE + p * 0x20, 4) & 0xFFFF;
    if (v !== lastPorts[p]) {
      for (let b = 0; b < 16; b++) {
        const c = gpioCells[p * 16 + b];
        const hi = (v >> b) & 1;
        c.classList.toggle('hi', !!hi);
        c.classList.add('chg');
        setTimeout(() => c.classList.remove('chg'), 400);
      }
      lastPorts[p] = v;
    }
    if (v) led = true;
  }
  // D13 is the Minima LED; fall back to any-output glow.
  const d13 = (periphRead(PORT_BASE + 1 * 0x20, 4) >> 13) & 1;
  setLed('led-main', 'led-glow', d13 || led);
  if (txFlash) flash('led-tx');
  if (rxFlash) flash('led-rx');
  $('stat-steps').textContent = steps.toLocaleString('en-US');
  $('stat-pc').textContent = '0x' + board.get_pc().toString(16).padStart(8, '0');
  const f = board.fault_pc();
  const fe = $('stat-fault');
  if (f === 0xFFFFFFFF) { fe.textContent = 'none'; fe.className = 'mono ok'; }
  else { fe.textContent = '0x' + f.toString(16); fe.className = 'mono bad'; }
}
function setLed(id, glow, on) {
  $(id).classList.toggle('on', !!on);
  if (glow) $(glow).classList.toggle('on', !!on);
}
function flash(id) {
  const e = $(id);
  e.classList.add('on');
  setTimeout(() => e.classList.remove('on'), 120);
}
function setState(s) {
  const e = $('run-state');
  e.className = 'state ' + s;
  e.textContent = s;
}

/* ---------------- tabs ---------------- */
document.querySelectorAll('.tab').forEach((t) => {
  t.addEventListener('click', () => {
    document.querySelectorAll('.tab').forEach((x) => x.classList.remove('active'));
    document.querySelectorAll('.tabpane').forEach((x) => x.classList.remove('active'));
    t.classList.add('active');
    $('pane-' + t.dataset.tab).classList.add('active');
  });
});

/* ---------------- blink ---------------- */
let blinkMode = false;
$('btn-blink-run').addEventListener('click', async () => {
  const fw = await fetch('fw/r4blink.bin').then((r) => r.arrayBuffer()).then((b) => new Uint8Array(b));
  boot(fw);
  curFw = 'blink';
  // Boot through USB/Arduino init like the proofs do.
  running = true; setState('running'); banner(null);
  pump(6000000);
  blinkMode = true;
  frameLoop();
});
$('btn-pause').addEventListener('click', () => { running = false; setState('paused'); });
$('btn-reset').addEventListener('click', async () => {
  running = false;
  if (curFw === 'blink') $('btn-blink-run').click();
});
$('speed').addEventListener('input', (e) => { $('speed-v').textContent = e.target.value; });

async function frameLoop() {
  while (blinkMode && running) {
    const n = parseInt($('speed').value, 10);
    for (let i = 0; i < n && running; i++) {
      if (!pump(CHUNK)) { blinkMode = false; return; }
    }
    refreshBoard(false, false);
    await new Promise((r) => requestAnimationFrame(r));
  }
  if (blinkMode) refreshBoard(false, false);
}

/* ---------------- serial (hello) ---------------- */
const enumUl = $('enum-steps');
function enumStep(label, st) {
  let li = [...enumUl.children].find((x) => x.dataset.k === label);
  if (!li) {
    li = document.createElement('li');
    li.dataset.k = label;
    li.textContent = label;
    enumUl.appendChild(li);
  }
  li.className = st;
}
function termPrint(s, cls) {
  const t = $('term');
  const span = document.createElement('span');
  if (cls) span.className = cls;
  span.textContent = s;
  t.appendChild(span);
  t.scrollTop = t.scrollHeight;
}

$('btn-serial-run').addEventListener('click', async () => {
  blinkMode = false;
  const fw = await fetch('fw/r4serial.bin').then((r) => r.arrayBuffer()).then((b) => new Uint8Array(b));
  boot(fw);
  curFw = 'serial';
  enumUl.innerHTML = '';
  $('term').innerHTML = '';
  running = true; setState('running');
  const step = async (label, fn) => {
    enumStep(label, 'run');
    const r = await fn();
    enumStep(label, 'done');
    return r;
  };
  await pump(6000000);
  usb_host_attach();
  await runUntil(900000, () => false);
  usb_host_reset();
  await runUntil(900000, () => false);
  const dev = await step('GET_DESCRIPTOR (device)', () => ctlIn(0x0680, 0x0100, 0, 18, 18));
  termPrint(`device: ${dev.map((b) => b.toString(16).padStart(2, '0')).join(' ')}\n`, 'sys');
  await step('SET_ADDRESS 5', () => ctlOut(0x0500, 5, 0, []));
  const cfg9 = await step('GET_DESCRIPTOR (config, 9)', () => ctlIn(0x0680, 0x0200, 0, 9, 9));
  const total = cfg9[2] | (cfg9[3] << 8);
  await step(`GET_DESCRIPTOR (config, ${total})`, () => ctlIn(0x0680, 0x0200, 0, total, total));
  await step('SET_CONFIGURATION 1', () => ctlOut(0x0900, 1, 0, []));
  await step('SET_LINE_CODING 9600 8N1', () =>
    ctlOut(0x2021, 0, 0, [0x80, 0x25, 0x00, 0x00, 0x00, 0x00, 0x08]));
  await step('SET_CONTROL_LINE_STATE (DTR)', () => ctlOut(0x2221, 3, 0, []));
  termPrint('— port open, printing… —\n', 'sys');
  const seen = [];
  const ok = await runUntil(1200 * CHUNK, () => {
    const b = usb_take_tx();
    if (b.length) { flash('led-rx'); seen.push(...b); refreshBoard(false, true); }
    const s = String.fromCharCode(...seen);
    if (s.includes('hello')) { termPrint(s); return true; }
    return seen.length > 400;
  });
  if (ok && String.fromCharCode(...seen).includes('hello')) {
    enumStep('hello received', 'done');
  } else {
    banner('No hello arrived — the device may have stalled.', 'err');
  }
  setState('paused'); running = false;
});
$('btn-serial-stop').addEventListener('click', () => { running = false; setState('paused'); });

/* ---------------- echo ---------------- */
let echoPipes = null;
async function enumEchoBase(log) {
  const fw = await fetch('fw/r4echo.bin').then((r) => r.arrayBuffer()).then((b) => new Uint8Array(b));
  boot(fw);
  running = true; setState('running');
  await pump(6000000);
  usb_host_attach();
  await runUntil(900000, () => false);
  usb_host_reset();
  await runUntil(900000, () => false);
  const dev = await ctlIn(0x0680, 0x0100, 0, 18, 18);
  log(`dev ${dev[0]}/${dev[1]} ok`);
  await ctlOut(0x0500, 5, 0, []);
  const cfg9 = await ctlIn(0x0680, 0x0200, 0, 9, 9);
  const total = cfg9[2] | (cfg9[3] << 8);
  await ctlIn(0x0680, 0x0200, 0, total, total);
  await ctlOut(0x0900, 1, 0, []);
  await ctlOut(0x2021, 0, 0, [0x80, 0x25, 0x00, 0x00, 0x00, 0x00, 0x08]);
  await ctlOut(0x2221, 3, 0, []);
  // Discover bulk pipes from PIPECFG like a real host would.
  let out = null, inp = null;
  for (let n = 1; n < 10; n++) {
    periphWrite(USBFS + 0x64, 2, n);
    const c = periphRead(USBFS + 0x68, 2);
    if (((c >> 14) & 3) === 1 && (c & 0xF) === 2) {
      if (c & (1 << 4)) inp = n; else out = n;
    }
  }
  if (out === null || inp === null) throw new Error('bulk pipes not found');
  log(`bulk OUT pipe ${out}, IN pipe ${inp}`);
  return { out, inp };
}
// periph helpers with explicit width (the free fn takes (addr, width))
function periphRead(a, w) { return _pr(a, w); }
function periphWrite(a, w, v) { _pw(a, w, v); }

$('btn-echo-run').addEventListener('click', async () => {
  blinkMode = false;
  const log = $('echo-log');
  log.innerHTML = '';
  const say = (s, c) => {
    const sp = document.createElement('span');
    if (c) sp.className = c;
    sp.textContent = s + '\n';
    log.appendChild(sp);
  };
  try {
    echoPipes = await enumEchoBase(say);
    say('ready — type below and Send', 'okline');
    $('echo-in').disabled = false;
    $('btn-echo-send').disabled = false;
  } catch (e) {
    say('bring-up failed: ' + e.message);
    banner('Echo bring-up failed: ' + e.message, 'err');
  }
  setState('paused'); running = false;
});

$('btn-echo-send').addEventListener('click', async () => {
  const box = $('echo-in');
  const text = box.value;
  if (!text || !echoPipes) return;
  box.value = '';
  const log = $('echo-log');
  const say = (s, c) => {
    const sp = document.createElement('span');
    if (c) sp.className = c;
    sp.textContent = s + '\n';
    log.appendChild(sp); log.scrollTop = log.scrollHeight;
  };
  say('> ' + text);
  running = true;
  usb_rx_inject(echoPipes.out, new TextEncoder().encode(text));
  const back = [];
  await runUntil(600 * CHUNK, () => {
    const b = usb_take_tx();
    if (b.length) { flash('led-tx'); flash('led-rx'); back.push(...b); refreshBoard(true, true); }
    return back.length >= text.length;
  });
  running = false;
  const s = new TextDecoder().decode(new Uint8Array(back.slice(0, text.length)));
  say('< ' + s, s === text ? 'okline' : '');
  if (s !== text) say(`(mismatch: wanted ${text.length}B, got ${back.length}B)`);
});

/* ---------------- boot ---------------- */
buildGpio();
try {
  await init();
  init_ra4m1();
  banner('WASM core loaded — pick Blink to boot the board.', 'info');
} catch (e) {
  banner('Failed to start WASM core: ' + e.message, 'err');
}
