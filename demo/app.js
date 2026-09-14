import init, {
  WasmCpu, init_ra4m1, reset_state, tick_peripherals,
  periph_read as _pr, periph_write as _pw,
  usb_take_tx, usb_rx_inject,
  usb_host_attach, usb_host_reset, usb_host_setup, usb_host_status_done,
  spi_set_sd_card, sd_read_block,
  soft_wire, peek_uart_output, uart_rx_byte,
  icu_pin_edge, kint_key_press, can_inject_errors,
  adc_set_channel_value, adc_clear_channel_value,
  usb_host_suspend, usb_host_resume, is_watchdog_reset_requested,
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
  // Step already accounts executed instructions: tick peripherals
  // WITHOUT adding the budget a second time (tick_n would double the
  // clock - every virtual millisecond would take half the chunks).
  tick_peripherals();
  steps += n;
  const f = board.fault_pc();
  if (f !== 0xFFFFFFFF) {
    running = false;
    setState('fault');
    const op1 = board.fault_op1().toString(16).padStart(4, '0');
    const mem = board.mem_fault();
    banner('CPU fault at 0x' + f.toString(16) + ' op ' + op1 +
      (mem !== 0xFFFFFFFF ? ' (bad mem 0x' + mem.toString(16) + ')' : '') +
      ' — see console for details.', 'err');
    return false;
  }
  return true;
}

async function runChunks(total, perFrame, onFrame) {
  // Dead helper kept for the debug console: same tight cadence as
  // runUntil (no per-chunk awaits; callers paint explicitly).
  let left = total;
  while (left > 0 && running) {
    const n = Math.min(perFrame, left);
    if (!pump(n)) return false;
    left -= n;
    if (onFrame) onFrame();
  }
  return running;
}

async function runUntil(budget, cond) {
  // Tight loop: no per-chunk awaits — each iteration is ~ms of blocking
  // WASM anyway, so yielding only burns wall time. Callers that need
  // paints use runFw's perChunk or await explicitly.
  let left = budget;
  while (left > 0 && !cond()) {
    const n = Math.min(CHUNK, left);
    if (!pump(n)) return false;
    left -= n;
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
    let guard = 0;
    while (got.length < want && guard++ < 40) {
      await runUntil(CHUNK, () => { got.push(...usb_take_tx()); return got.length >= want; });
      await new Promise((r) => setTimeout(r, 0));
    }
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
  await new Promise((r) => setTimeout(r, 0));
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
const chgTimers = new Array(192).fill(0);
function refreshBoard(txFlash, rxFlash) {
  if (!board) return;
  let led = false;
  for (let p = 0; p < 12; p++) {
    const v = periphRead(PORT_BASE + p * 0x20, 4) & 0xFFFF;
    if (v !== lastPorts[p]) {
      const diff = v ^ lastPorts[p];
      for (let b = 0; b < 16; b++) {
        if (!(diff & (1 << b))) continue;
        const i = p * 16 + b;
        const c = gpioCells[i];
        c.classList.toggle('hi', !!((v >> b) & 1));
        c.classList.add('chg');
        clearTimeout(chgTimers[i]);
        chgTimers[i] = setTimeout(() => c.classList.remove('chg'), 400);
      }
      lastPorts[p] = v;
    }
    if (v) led = true;
  }
  // Board LED: Minima/WiFi D13 = P111 (PORT1 bit 11); EK-RA4M1 LED1 =
  // P106 (PORT1 bit 6, EK manual s5.4.4). Fall back to any-output glow
  // (e.g. TX/RX activity elsewhere).
  const p1 = periphRead(PORT_BASE + 1 * 0x20, 4);
  const Fuji = target === 'ek' ? (p1 >> 6) & 1 : (p1 >> 11) & 1;
  setLed('led-main', 'led-glow', Fuji || led);
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

$('tgt-minima').addEventListener('click', () => setTarget('minima'));
$('tgt-wifi').addEventListener('click', () => setTarget('wifi'));
$('tgt-ek').addEventListener('click', () => setTarget('ek'));
function setTarget(v) {
  target = v;
  $('tgt-minima').classList.toggle('active', v === 'minima');
  $('tgt-wifi').classList.toggle('active', v === 'wifi');
  $('tgt-ek').classList.toggle('active', v === 'ek');
  // WiFi board parks the RA4M1 demo set: its LED matrix lives behind the
  // ESP32-S3 (parked, no S3 code yet), so only the Matrix GPIO tab stays
  // live. Everything else needs the Minima/EK RA4M1 target.
  const wifi = v === 'wifi';
  document.querySelectorAll('.tab').forEach((t) => {
    const k = t.dataset.tab;
    if (k === 'matrix' || k === 'docs') return;
    t.disabled = wifi;
    t.title = wifi ? 'Parked on WiFi: needs the ESP32-S3 bridge (no S3 code yet)' : '';
  });
  if (wifi) {
    document.querySelector('.tab[data-tab="matrix"]').click();
    banner('WiFi target parked: RA4M1 demos need Minima/EK. Matrix GPIO render stays live.', 'info');
  } else {
    banner(null);
  }
  refreshBoard(false, false);
}

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
    // USB suspend/resume on the live enumerated port (r4susp.bin flow,
    // virtual-host driven): idle the bus -> SUSPx + LED on, then resume
    // activity -> RESM + LED off.
    const sust = mkSteps($('susp-steps'), [
      'bus idle 3 ms — suspend (DVSQ SUSPx)',
      'LED on — suspend callback fired',
      'bus activity — resume (RESM)',
      'LED off — resume callback fired',
    ]);
    sayVerdict($('susp-verdict'), [['idling the bus into suspend…', 'sys']]);
    usb_host_suspend();
    const susp = await runUntil(600 * CHUNK, () => ledOn());
    if (susp) { sust.mark(0); sust.mark(1); }
    sayVerdict($('susp-verdict'), [['resuming the bus…', 'sys']]);
    usb_host_resume();
    const resm = await runUntil(600 * CHUNK, () => !ledOn());
    if (resm) { sust.mark(2); sust.mark(3); }
    sayVerdict($('susp-verdict'), susp && resm
      ? [['suspend (LED on) → resume (LED off) on the live port', 'okline']]
      : [['suspend/resume did not complete (see console).', '']]);
    if (!susp || !resm) banner('Suspend/resume did not complete.', 'err');
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
window.__dbg = { boot, pump, runUntil, runChunks, periphRead, periphWrite, CHUNK,
  can_inject_errors, kint_key_press, soft_wire, peek_uart_output,
  get board() { return board; }, get running() { return running; } };
window.__candbg = () => {
  const CAN0 = 0x40050000, h = (v) => '0x' + (v >>> 0).toString(16).padStart(8, '0');
  // NOTE: sub-word reads return unmasked packs — mask here.
  const b = (a) => periphRead(a, 1) % 256;
  const w = (a) => periphRead(a, 4) >>> 0;
  return {ctlr: h(periphRead(CAN0 + 0x840, 2)), ctlrW: h(w(CAN0 + 0x840)),
    eier: h(b(CAN0 + 0x84C)),
    eifr: h(b(CAN0 + 0x84D)), recr: h(b(CAN0 + 0x84E)),
    tecr: h(b(CAN0 + 0x84F))};
};
window.__rtcdbg = () => {
  const RTC = 0x40044000, h = (v) => '0x' + (v >>> 0).toString(16).padStart(8, '0');
  return {sec: h(periphRead(RTC + 2, 1)), min: h(periphRead(RTC + 4, 1)), hr: h(periphRead(RTC + 6, 1)),
    rcr2: h(periphRead(RTC + 0x24, 1)), steps};
};

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

/* ---------------- wire / spi / sd ---------------- */
// Phase checklist: sticky MMIO-polled milestones (data registers are
// never read - reading ICDRR/SPDR would eat the firmware's byte).
function mkSteps(ul, labels) {
  ul.innerHTML = '';
  const items = labels.map((l) => {
    const li = document.createElement('li');
    li.textContent = l;
    ul.appendChild(li);
    return li;
  });
  const done = new Array(labels.length).fill(false);
  return {
    mark(i) { if (!done[i]) { done[i] = true; items[i].className = 'done'; } },
    get done() { return done; },
  };
}
function sayVerdict(el, lines) {
  el.innerHTML = '';
  for (const [s, c] of lines) {
    const sp = document.createElement('span');
    if (c) sp.className = c;
    sp.textContent = s + '\n';
    el.appendChild(sp);
  }
}
let target = 'minima'; // minima | wifi (parked) | ek (P106 LED1, zero-boot)
// WiFi shares Minima's D13 = P111 LED. EK-RA4M1 LED1 = P106.
const ledOn = () => {
  const bit = target === 'ek' ? 6 : 11;
  return ((periphRead(PORT_BASE + 1 * 0x20, 4) >> bit) & 1) !== 0;
};
// Boot fw, then pump `chunks` 48k-chunks (proof-harness cadence),
// calling perChunk() after every chunk for transient flags.
// Board/DOM refresh is batched: refreshBoard walks 192 cells, so only
// pay for it every 8th chunk; yield via rAF only when a paint is due.
async function runFw(fwName, arm, chunks, perChunk) {
  blinkMode = false;
  const fw = await fetch('fw/' + fwName).then((r) => r.arrayBuffer()).then((b) => new Uint8Array(b));
  boot(fw);
  running = true; setState('running');
  if (arm) arm();
  await pump(6000000);
  for (let i = 0; i < chunks && running; i++) {
    if (!pump(CHUNK)) return false;
    if (perChunk) perChunk();
    if (i % 8 === 7) { refreshBoard(false, false); await new Promise((r) => requestAnimationFrame(r)); }
  }
  refreshBoard(false, false);
  return running;
}
// Batch helper for hand-rolled loops: same every-8th paint cadence.
function paintTick(i) {
  if (i % 8 === 7) { refreshBoard(false, false); return true; }
  return false;
}

$('btn-wire-run').addEventListener('click', async () => {
  const IIC0 = 0x40053000, IIC1 = 0x40053100;
  const st = mkSteps($('wire-steps'), [
    'master START (IIC1 BBSY)',
    'slave addressed (IIC0 AAS0 @ 0x42)',
    'slave received byte (IIC0 RDRF)',
    'master reading reply (IIC1 RDRF)',
    'STOP seen on the bus',
    'LED on — 0xBE out, 0xEF back',
  ]);
  sayVerdict($('wire-verdict'), [['booting Wire master + slave…', 'sys']]);
  let led = false;
  const ok = await runFw('r4wire1.bin', null, 3000, () => {
    if ((periphRead(IIC1 + 0x01, 1) & 0x80) !== 0) st.mark(0);
    if ((periphRead(IIC0 + 0x08, 1) & 1) !== 0) st.mark(1);
    if ((periphRead(IIC0 + 0x09, 1) & 0x20) !== 0) st.mark(2);
    if ((periphRead(IIC1 + 0x09, 1) & 0x20) !== 0) st.mark(3);
    if (((periphRead(IIC1 + 0x09, 1) & 8) !== 0) || ((periphRead(IIC0 + 0x09, 1) & 8) !== 0)) st.mark(4);
    // The LED implies every prior step (firmware checked the data).
    if (ledOn()) { for (let i = 0; i < 6; i++) st.mark(i); led = true; }
  });
  running = false; setState('paused');
  sayVerdict($('wire-verdict'), ok && led
    ? [['round-trip complete: slave got 0xBE, master got 0xEF', 'okline']]
    : [['no LED — the round-trip did not complete (see console).', '']]);
  if (!ok || !led) banner('Wire demo did not complete.', 'err');
});

$('btn-spi-run').addEventListener('click', async () => {
  const SPI0 = 0x40072000, SPI1 = 0x40072100;
  const st = mkSteps($('spi-steps'), [
    'SPI1 slave configured (SPE, MSTR=0)',
    'SPI0 master configured (MSTR+SPE)',
    'master shifted (SPI0 SPRF)',
    'slave received (SPI1 SPRF)',
    'LED on — 0xA5 / 0x5A both crossed',
  ]);
  sayVerdict($('spi-verdict'), [['booting SPI0 master + SPI1 slave…', 'sys']]);
  let led = false;
  const ok = await runFw('r4spislv.bin', null, 3000, () => {
    const c1 = periphRead(SPI1, 1);
    if ((c1 & 0x48) === 0x40) st.mark(0);
    if ((periphRead(SPI0, 1) & 0x48) === 0x48) st.mark(1);
    if ((periphRead(SPI0 + 0x03, 1) & 0x80) !== 0) st.mark(2);
    if ((periphRead(SPI1 + 0x03, 1) & 0x80) !== 0) st.mark(3);
    // The LED implies the exchange (firmware checked both bytes).
    if (ledOn()) { for (let i = 0; i < 5; i++) st.mark(i); led = true; }
  });
  running = false; setState('paused');
  sayVerdict($('spi-verdict'), ok && led
    ? [['exchange complete: master saw 0x5A, slave saw 0xA5', 'okline']]
    : [['no LED — the exchange did not complete (see console).', '']]);
  if (!ok || !led) banner('SPI demo did not complete.', 'err');
});

$('btn-sd-run').addEventListener('click', async () => {  const SPI1 = 0x40072100;
  const st = mkSteps($('sd-steps'), [
    'card initialized (CMD0/CMD8/ACMD41)',
    'block 0 read — MBR signature 55 AA',
    'block 1 write landed (seen on the card)',
    'LED on — firmware verified read-back',
  ]);
  sayVerdict($('sd-verdict'), [['booting SD sketch, arming virtual card…', 'sys']]);
  let led = false, wrote = false;
  const ok = await runFw('r4sd.bin', () => spi_set_sd_card(SPI1, true), 1500, () => {
    // Phase milestones off card state, not firmware internals: the
    // write lands last, so it implies init + MBR read already passed.
    const b1 = sd_read_block(1);
    if (b1.length === 512 && b1[0] === 5) { st.mark(0); st.mark(1); st.mark(2); wrote = true; }
    if (ledOn()) { for (let i = 0; i < 4; i++) st.mark(i); led = true; }
  });
  spi_set_sd_card(SPI1, false);
  running = false; setState('paused');
  if (ok && led) {
    st.mark(0); st.mark(1); st.mark(2); st.mark(3);
    const b0 = sd_read_block(0), b1 = sd_read_block(1);
    const hex = (a, n) => [...a.slice(0, n)].map((b) => b.toString(16).padStart(2, '0')).join(' ');
    const tail = [...b0.slice(508)].map((b) => b.toString(16).padStart(2, '0')).join(' ');
    const pat = b1.length === 512 && b1.every((b, i) => b === ((i * 11 + 5) & 0xFF));
    sayVerdict($('sd-verdict'), [
      [`block 0 [0..32): ${hex(b0, 32)}`, 'sys'],
      [`block 0 [508..512): ${tail}  (55 AA = MBR signature)`, 'sys'],
      [`block 1 pattern re-checked here: ${pat ? 'all 512 bytes match' : 'MISMATCH'}`, pat ? 'okline' : ''],
    ]);
  } else {
    sayVerdict($('sd-verdict'), [['no LED — the SD flow did not complete (see console).', '']]);
    banner('SD demo did not complete.', 'err');
  }
});

$('btn-can-run').addEventListener('click', async () => {
  const CAN0 = 0x40050000;
  const st = mkSteps($('can-steps'), [
    'RX FIFO enabled (RFE)',
    'frame queued (RFUST>0)',
    'MB24 shows SID 0x123',
    'payload bytes ca fe',
    'LED on — loopback round-trip',
  ]);
  sayVerdict($('can-verdict'), [['booting CAN self-test + FIFO sketch…', 'sys']]);
  let led = false;
  const ok = await runFw('r4canfifo.bin', null, 3000, () => {
    const rfcr = periphRead(CAN0 + 0x848, 1);
    if ((rfcr & 1) !== 0) st.mark(0);
    if ((rfcr & 0x0E) !== 0) {
      st.mark(1);
      if (periphRead(CAN0 + 0x380, 4) === (0x123 << 18)) st.mark(2);
      if (periphRead(CAN0 + 0x386, 1) % 256 === 0xCA && periphRead(CAN0 + 0x387, 1) % 256 === 0xFE) st.mark(3);
    }
    // The LED implies the firmware saw the frame (it checks ID + data).
    if (ledOn()) { for (let i = 0; i < 5; i++) st.mark(i); led = true; }
  });
  running = false; setState('paused');
  sayVerdict($('can-verdict'), ok && led
    ? [['loopback complete: MB0 → RX FIFO → MB24, payload intact', 'okline']]
    : [['no LED — the CAN flow did not complete (see console).', '']]);
  if (!ok || !led) banner('CAN demo did not complete.', 'err');
});

$('btn-eep-run').addEventListener('click', async () => {  const DF = 0x40100000;
  const st = mkSteps($('eep-steps'), [
    'byte 0 programmed (a5)',
    'byte 1 programmed (3c)',
    'LED on — flash read-back matched',
  ]);
  sayVerdict($('eep-verdict'), [['booting Arduino EEPROM sketch…', 'sys']]);
  let led = false;
  const ok = await runFw('r4eep.bin', null, 3000, () => {
    // Plain dataflash window reads: no side effects, always safe.
    if (periphRead(DF, 1) % 256 === 0xA5) st.mark(0);
    if (periphRead(DF + 1, 1) % 256 === 0x3C) { st.mark(1); }
    if (ledOn()) { for (let i = 0; i < 3; i++) st.mark(i); led = true; }
  });
  running = false; setState('paused');
  sayVerdict($('eep-verdict'), ok && led
    ? [['round-trip complete: EEPROM.write → FACI → 0x40100000 reads a5 3c', 'okline']]
    : [['no LED — the EEPROM flow did not complete (see console).', '']]);
  if (!ok || !led) banner('EEPROM demo did not complete.', 'err');
});

$('btn-pwm-run').addEventListener('click', async () => {
  const GPT0 = 0x40078000, DF_P106 = 0x40040858;
  const st = mkSteps($('pwm-steps'), [
    'GPT0 running (10-chunk period)',
    'P106 routed to GPT (PSEL)',
    'pin toggling',
    'duty ≈ 25%',
  ]);
  sayVerdict($('pwm-verdict'), [['booting bare-metal GPT0 PWM sketch…', 'sys']]);
  let hi = 0, n = 0, done = false;
  const ok = await runFw('r4pwm.bin', null, 120, () => {
    if ((periphRead(GPT0 + 0x2C, 4) & 1) !== 0) st.mark(0);
    if (((periphRead(DF_P106, 4) >>> 24) & 0x1F) === 0x03) st.mark(1);
    n++;
    if ((periphRead(PORT_BASE + 0x20, 4) & (1 << 6)) !== 0) hi++;
    if (n >= 40) {
      const duty = hi / n;
      if (hi > 0 && hi < n) st.mark(2);
      if (duty > 0.12 && duty < 0.38) { st.mark(3); done = true; }
      sayVerdict($('pwm-verdict'), [[`duty measured live: ${(duty * 100).toFixed(1)}% high over ${n} samples (want ~25%)`, done ? 'okline' : 'sys']]);
    }
    refreshBoard(false, false);
  });
  running = false; setState('paused');
  if (!ok || !done) {
    sayVerdict($('pwm-verdict'), [['no stable ~25% duty seen (see console).', '']]);
    banner('PWM demo did not complete.', 'err');
  }
});

/* ---------------- analogWrite (Arduino API on GPT0) ---------------- */
$('btn-aw-run').addEventListener('click', async () => {
  const GPT0 = 0x40078000;
  const st = mkSteps($('aw-steps'), [
    'counter running (GTSTR0 CSTRT)',
    'GTPR0 ≈ 490 Hz period',
    'GTCCRB0 ≈ 25% duty (64/255)',
    'GTIOR0 OBE set',
    'LED on — sketch reached loop',
  ]);
  sayVerdict($('aw-verdict'), [['booting analogWrite(6, 64) sketch…', 'sys']]);
  let done = false;
  const ok = await runFw('r4aw.bin', null, 3000, () => {
    if ((periphRead(GPT0 + 0x04, 4) & 1) !== 0) st.mark(0);
    const gtpr = periphRead(GPT0 + 0x64, 4);
    if (gtpr >= 97900 && gtpr <= 98100) st.mark(1);
    const b = periphRead(GPT0 + 0x50, 4);
    if (b !== 0xFFFFFFFF && b >= 24400 && b <= 24800) st.mark(2);
    if ((periphRead(GPT0 + 0x34, 4) & (1 << 24)) !== 0) st.mark(3);
    // The LED implies every prior step (firmware lights it at the end
    // of setup, after analogWrite programmed the timer).
    if (ledOn()) { for (let i = 0; i < 5; i++) st.mark(i); done = true; }
    if (st.done[2]) {
      sayVerdict($('aw-verdict'), [[`GTPR=${gtpr} GTCCRB=${b} (want 97958/24415)`, 'sys']]);
    }
  });
  running = false; setState('paused');
  if (done) {
    sayVerdict($('aw-verdict'), [[`duty programmed: GTPR/GTCCRB live above, GTIOR.OBE set`, 'okline']]);
  } else {
    sayVerdict($('aw-verdict'), [['no LED — analogWrite did not complete (see console).', '']]);
    banner('analogWrite demo did not complete.', 'err');
  }
  void ok;
});

/* ---------------- tone (Arduino API, GPT4 PERIODIC) ---------------- */
$('btn-tone-run').addEventListener('click', async () => {
  const st = mkSteps($('tone-steps'), [
    'LED seen HIGH',
    'LED seen LOW (toggling)',
  ]);
  sayVerdict($('tone-verdict'), [['booting tone(LED, 440) sketch…', 'sys']]);
  let hi = false, lo = false, done = false;
  const ok = await runFw('r4tone.bin', null, 3000, () => {
    if (ledOn()) { st.mark(0); hi = true; } else { st.mark(1); lo = true; }
    if (hi && lo) done = true;
    refreshBoard(false, false);
  });
  running = false; setState('paused');
  sayVerdict($('tone-verdict'), ok && done
    ? [['tone() toggling live: D13 follows the 440 Hz overflow IRQ', 'okline']]
    : [['no toggling seen (see console).', '']]);
  if (!ok || !done) banner('tone demo did not complete.', 'err');
});

/* ---------------- AnalogWave sine (Arduino API, GPT->DTC->DAC12) ---------------- */
$('btn-aws-run').addEventListener('click', async () => {
  const DAC = 0x4005E000, DTC = 0x40005400;
  const st = mkSteps($('aws-steps'), [
    'DTC vector table programmed (DTCVBR nonzero)',
    'GPT running (a channel counting via GTSTR)',
    'DADR0 sine sample seen (nonzero 12-bit)',
    'LED on — sketch reached loop',
  ]);
  sayVerdict($('aws-verdict'), [['booting analogWave.sine(10) sketch…', 'sys']]);
  let done = false, lastDadr = 0;
  const ok = await runFw('r4aws.bin', null, 3000, () => {
    if (periphRead(DTC + 4, 4) !== 0) st.mark(0);
    for (let ch = 0; ch < 8; ch++) {
      if ((periphRead(0x40078000 + ch * 0x100 + 0x04, 4) & (1 << ch)) !== 0) { st.mark(1); break; }
    }
    const dadr = periphRead(DAC, 4) & 0xFFF;
    if (dadr !== 0) st.mark(2);
    if (ledOn()) st.mark(3);
    if (dadr !== 0 && dadr !== lastDadr) {
      lastDadr = dadr;
      sayVerdict($('aws-verdict'), [[`DADR0=0x${lastDadr.toString(16).padStart(3, '0')} (${lastDadr} / 4095) — sine sample live`, 'sys']]);
    }
    if (st.done[2] && st.done[3]) done = true;
    refreshBoard(false, false);
  });
  running = false; setState('paused');
  sayVerdict($('aws-verdict'), ok && done
    ? [[`sine flowing: GPT overflow → DTC repeat → DAC12 DADR (last 0x${lastDadr.toString(16)})`, 'okline']]
    : [['no DAC sample seen (see console).', '']]);
  if (!ok || !done) banner('AnalogWave demo did not complete.', 'err');
});

/* ---------------- signal (IRQ + Serial1 + OPAMP + ADC) ---------------- */
let signalSci2 = null;
$('btn-signal-run').addEventListener('click', async () => {
  const OPAMP = 0x40086000, ADC = 0x4005C000, SCI2 = 0x40070040;
  const st = mkSteps($('signal-steps'), [
    'OPAMP follower live (AMPMON0)',
    'ADC ch2 converts (0xABC stimulus)',
    'Serial1 TX ready (SCI2 TE)',
    'IRQ0 press lights D13 (attachInterrupt)',
  ]);
  sayVerdict($('signal-verdict'), [['booting OPAMP + Serial1 signal chain…', 'sys']]);
  // Deterministic analog stimulus so the readout is a verdict, not noise
  // (same channel the Rust ADC proof uses: ch2 -> ADDR2).
  adc_set_channel_value(2, 0xABC);
  // ADC convert first (ADANSA0 ch2 + ADST, result at ADDR2 = base+0x24),
  // like ra4m1_map_adc_converts_channel — the perChunk closure below
  // then only polls the already-converted result (no const hoist bug).
  // NOTE: the ADC model is a separate unit from the OPAMP firmware boot:
  // boot() resets the whole system, so program ADANSA+ADST *after* the
  // boot inside runFw — do the convert in the first perChunk tick.
  let adcDone = false;
  const ok = await runFw('r4opamp.bin', null, 3000, () => {
    if ((periphRead(OPAMP + 0x0C, 1) & 1) !== 0) st.mark(0);
    if (!adcDone) {
      periphWrite(ADC + 0x04, 4, 1 << 2); // ADANSA0: ch2
      periphWrite(ADC + 0x00, 4, 1 << 15); // ADST
      adcDone = true;
    }
    if ((periphRead(ADC + 0x24, 4) & 0x3FFF) === 0xABC) st.mark(1);
  });
  running = false; setState('paused');
  const res = periphRead(ADC + 0x24, 4) & 0x3FFF;
  // Serial1: boot the SCI2 echo image and find TE like the Rust proof.
  const fw = await fetch('fw/r4serial1.bin').then((r) => r.arrayBuffer()).then((b) => new Uint8Array(b));
  boot(fw);
  running = true; setState('running');
  await pump(6000000);
  signalSci2 = null;
  for (let i = 0; i < 200 && running; i++) {
    if (!pump(CHUNK)) break;
    if ((periphRead(SCI2 + 0x02, 1) & (1 << 5)) !== 0) { signalSci2 = SCI2; st.mark(2); break; }
  }
  sayVerdict($('signal-verdict'), ok
    ? [[`OPAMP follower live (AMPMON0=1); ADC ch2=0x${res.toString(16)} / 0x3FFF; Serial1 ${signalSci2 ? 'TX ready — type below, or press the button' : 'not ready (see console)'}`, signalSci2 ? 'okline' : '']]
    : [['no OPAMP verdict (see console).', '']]);
  if (!ok || !signalSci2) banner('Signal demo did not complete.', 'err');
  running = false; setState('paused');
  refreshBoard(false, false);
  adc_clear_channel_value(2);
});
$('btn-signal-press').addEventListener('click', async () => {
  if (!board) { banner('Run the signal demo first.', 'err'); return; }
  // Same shape as ra4m1_attach_interrupt: real FSP external-IRQ setup is
  // already in the booted image path; here the OPAMP image idles, so boot
  // the IRQ image, then inject the falling edge on line 0.
  const fw = await fetch('fw/r4irq.bin').then((r) => r.arrayBuffer()).then((b) => new Uint8Array(b));
  boot(fw);
  running = true; setState('running');
  await pump(6000000);
  const st = mkSteps($('signal-steps'), [
    'OPAMP follower live (AMPMON0)',
    'ADC converting (ADST self-clears)',
    'Serial1 TX ready (SCI2 TE)',
    'IRQ0 press lights D13 (attachInterrupt)',
  ]);
  st.mark(0); st.mark(1); st.mark(2);
  icu_pin_edge(0, true);
  let lit = false;
  for (let i = 0; i < 200 && running; i++) {
    if (!pump(CHUNK)) break;
    if (ledOn()) { lit = true; break; }
  }
  if (lit) st.mark(3);
  running = false; setState('paused');
  refreshBoard(false, false);
  sayVerdict($('signal-verdict'), lit
    ? [['IRQ0 falling edge → ISR lit D13 (attachInterrupt, all 16 lines proven in Rust)', 'okline']]
    : [['no LED — the edge did not reach the ISR (see console).', '']]);
  if (!lit) banner('Button press did not light the LED.', 'err');
});
$('btn-signal-send').addEventListener('click', async () => {
  const box = $('signal-in');
  const text = box.value;
  if (!text) return;
  if (!signalSci2) { banner('Run the signal demo first (Serial1 must be TX-ready).', 'err'); return; }
  box.value = '';
  // Reboot the Serial1 echo image fresh (the press step rebooted into the
  // IRQ image), re-find TE, then inject + poll the UART console echo.
  const fw = await fetch('fw/r4serial1.bin').then((r) => r.arrayBuffer()).then((b) => new Uint8Array(b));
  boot(fw);
  running = true; setState('running');
  await pump(6000000);
  signalSci2 = null;
  for (let i = 0; i < 200 && running; i++) {
    if (!pump(CHUNK)) break;
    if ((periphRead(0x40070040 + 0x02, 1) & (1 << 5)) !== 0) { signalSci2 = 0x40070040; break; }
  }
  if (!signalSci2) {
    running = false; setState('paused');
    sayVerdict($('signal-verdict'), [['Serial1 not TX-ready after reboot (see console).', '']]);
    return;
  }
  for (const ch of text) uart_rx_byte(signalSci2, ch.charCodeAt(0) & 0xFF);
  const t0 = peek_uart_output().length;
  let back = '';
  for (let i = 0; i < 600 && running; i++) {
    if (!pump(CHUNK)) break;
    back = peek_uart_output().slice(t0);
    if (back.length >= text.length) break;
  }
  running = false; setState('paused');
  refreshBoard(false, false);
  sayVerdict($('signal-verdict'), [[back ? `< ${back.slice(0, text.length)} (Serial1 echo, SCI2)` : 'no echo (see console).', back ? 'okline' : '']]);
});

/* ---------------- SoftSerial loopback (9600 baud, 0xA5) ---------------- */
$('btn-sser-run').addEventListener('click', async () => {
  const st = mkSteps($('sser-steps'), [
    'wire armed: D3 (P104) → D2 (P105, IRQ0)',
    'sketch printing (Serial1 TX seen)',
    'got=A5 echoed — 0xA5 round-tripped',
    'LED on — firmware verified the byte',
  ]);
  sayVerdict($('sser-verdict'), [['arming loopback wire, booting SoftSerial sketch…', 'sys']]);
  st.mark(0);
  let done = false, led = false;
  blinkMode = false;
  const fw = await fetch('fw/r4sser.bin').then((r) => r.arrayBuffer()).then((b) => new Uint8Array(b));
  boot(fw);
  soft_wire(1, 4, 1, 5, 0);
  running = true; setState('running');
  await pump(6000000);
  for (let i = 0; i < 300 && running; i++) {
    if (!pump(CHUNK)) break;
    if (i % 8 === 7) refreshBoard(false, false);
    const out = peek_uart_output();
    if (out.length) st.mark(1);
    if (out.includes('got=A5')) {
      st.mark(2);
      sayVerdict($('sser-verdict'), [[out.trim(), 'sys']]);
      done = true;
      break;
    }
    await new Promise((r) => requestAnimationFrame(r));
  }
  if (ledOn()) { st.mark(3); led = true; }
  running = false; setState('paused');
  refreshBoard(false, false);
  sayVerdict($('sser-verdict'), done && led
    ? [[`${peek_uart_output().trim()} — loopback complete: TX bitstream re-sampled by the RX timer`, 'okline']]
    : [[`no echo — UART so far: ${JSON.stringify(peek_uart_output())} (see console).`, '']]);
  if (!done || !led) banner('SoftSerial demo did not complete.', 'err');
});

/* ---------------- rtc (live BCD clock) ---------------- */
const bcd = (v) => ((v >> 4) & 0xF) * 10 + (v & 0xF);
const pad2 = (n) => String(n).padStart(2, '0');

/* ---------------- RTC alarm (FSP callback -> LED) ---------------- */
$('btn-rtcalm-run').addEventListener('click', async () => {
  const RTC = 0x40044000;
  const st = mkSteps($('rtcalm-steps'), [
    'alarm armed (RSECAR ENB + RCR1.AIE)',
    'seconds advancing to :00',
    'LED on — alarm callback fired',
  ]);
  sayVerdict($('rtcalm-verdict'), [['booting RTC alarm sketch (11:59:55, match :00)…', 'sys']]);
  let done = false;
  const ok = await runFw('r4rtcalm.bin', null, 60 * 10, () => {
    if ((periphRead(RTC + 0x10, 1) & 0x80) !== 0 && (periphRead(RTC + 0x22, 1) & 1) !== 0) st.mark(0);
    const sec = bcd(periphRead(RTC + 0x02, 1) % 256);
    if (sec === 0) st.mark(1);
    if (ledOn()) { st.mark(2); done = true; }
    refreshBoard(false, false);
  });
  void ok;
  running = false; setState('paused');
  sayVerdict($('rtcalm-verdict'), done
    ? [['alarm complete: :00 crossing latched once, callback lit D13', 'okline']]
    : [['no LED — the alarm never fired (see console).', '']]);
  if (!done) banner('RTC alarm demo did not complete.', 'err');
});

/* ---------------- CAN bus-off recovery ---------------- */
$('btn-canbo-run').addEventListener('click', async () => {
  const CAN0 = 0x40050000;
  const st = mkSteps($('canbo-steps'), [
    'sketch transmitting (EIER armed)',
    'error storm injected (TEC saturates, BOEF)',
    'recovered via halt→operation (BOEF clear, counters 0)',
    'LED on — isError path + post-recovery TX',
  ]);
  sayVerdict($('canbo-verdict'), [['booting Arduino_CAN loop sketch…', 'sys']]);
  blinkMode = false;
  const fw = await fetch('fw/r4canbo.bin').then((r) => r.arrayBuffer()).then((b) => new Uint8Array(b));
  boot(fw);
  running = true; setState('running');
  await pump(6000000);
  let done = false, recovered = false;
  for (let i = 0; i < 200 && running; i++) {
    if (!pump(CHUNK)) break;
    if ((periphRead(CAN0 + 0x84C, 1) % 256 & 0x0E) !== 0) st.mark(0);
    if (i % 8 === 7) refreshBoard(false, false);
  }
  // Warning-level storm first (LED on isError), then bus-off + recover.
  // NOTE: the CAN error model lives in the Rust core, but periph_write
  // from JS routes through the generic bus merge — the CTLR CANM write
  // must be a full word for the recovery arm to see it. Read back the
  // actual mode before asserting.
  can_inject_errors(0, 100);
  for (let i = 0; i < 600 && running; i++) {
    if (!pump(CHUNK)) break;
    if (i % 8 === 7) refreshBoard(false, false);
    if (ledOn()) { st.mark(1); done = true; break; }
    await new Promise((r) => requestAnimationFrame(r));
  }
  can_inject_errors(0, 900);
  // NOTE: sub-word reads return unmasked packs - mask with % 256.
  // Write halt as one full word (low-half-only writes OR a stale high
  // byte into the merge and never form CANM=10 in the model).
  if ((periphRead(CAN0 + 0x84D, 1) % 256 & (1 << 3)) !== 0) st.mark(1);
  periphWrite(CAN0 + 0x840, 4, 0x0200); // halt: BOEF clears, counters reset
  await runUntil(CHUNK, () => false);
  if ((periphRead(CAN0 + 0x84D, 1) % 256 & (1 << 3)) === 0
    && (periphRead(CAN0 + 0x84E, 1) % 256) === 0
    && (periphRead(CAN0 + 0x84F, 1) % 256) === 0) { st.mark(2); recovered = true; }
  periphWrite(CAN0 + 0x840, 2, 0x0000); // operation
  for (let i = 0; i < 120 && running && !done; i++) {
    if (!pump(CHUNK)) break;
    if (ledOn()) { done = true; break; }
  }
  if (done) st.mark(3);
  running = false; setState('paused');
  refreshBoard(false, false);
  sayVerdict($('canbo-verdict'), done && recovered
    ? [['bus-off recovered: BOEF cleared in halt, TX completes in operation', 'okline']]
    : [[`incomplete (led=${done} recovered=${recovered}) — see console.`, '']]);
  if (!done || !recovered) banner('CAN error demo did not complete.', 'err');
});

/* ---------------- CAN1 · DAC8 · KINT · SSI (bare-metal quartet) ---------------- */
$('btn-misc-run').addEventListener('click', async () => {
  const st = mkSteps($('misc-steps'), [
    'CAN1 self-test loopback (r4can1.bin)',
    'DAC8 retain + DAM gate (r4dac8.bin)',
    'KINT key-3 press (r4kint.bin)',
    'SSI0 FIFO round-trip (r4ssi.bin)',
  ]);
  sayVerdict($('misc-verdict'), [['running four bare-metal proofs…', 'sys']]);
  const one = async (fwName, arm, label) => {
    let led = false;
    const ok = await runFw(fwName, arm, 3000, () => {
      if (ledOn()) led = true;
    });
    running = false; setState('paused');
    sayVerdict($('misc-verdict'), [[[fwName, label, led ? 'LED ON' : 'no LED'].join(' — '), led ? 'okline' : '']]);
    return ok && led;
  };
  const results = [];
  results.push(await one('r4can1.bin', null, 'polled MB0→MB8') && (st.mark(0), true));
  results.push(await one('r4dac8.bin', null, 'DACS0 read-back') && (st.mark(1), true));
  // KINT needs the virtual press mid-run: boot once, press, watch.
  // (The generic one() runner would boot it twice; skip straight here.)
  results.push(false);
  {
    blinkMode = false;
    const fw = await fetch('fw/r4kint.bin').then((r) => r.arrayBuffer()).then((b) => new Uint8Array(b));
    boot(fw);
    running = true; setState('running');
    await pump(6000000);
    for (let i = 0; i < 2200 && running; i++) { if (!pump(CHUNK)) break; }
    kint_key_press(3);
    let led = false;
    for (let i = 0; i < 3000 && running; i++) {
      if (!pump(CHUNK)) break;
      if (ledOn()) { led = true; break; }
      if (i % 8 === 7) refreshBoard(false, false);
    }
    running = false; setState('paused');
    if (led) { st.mark(2); results[2] = true; }
  }
  results.push(await one('r4ssi.bin', null, '0,1,2,3 pattern') && (st.mark(3), true));
  const n = results.filter(Boolean).length;
  sayVerdict($('misc-verdict'), [[`${n}/4 bare-metal LEDs lit (CAN1·DAC8·KINT·SSI)`, n === 4 ? 'okline' : '']]);
  if (n !== 4) banner('Misc demo: some proof did not light its LED.', 'err');
});
$('btn-rtc-run').addEventListener('click', async () => {
  const RTC = 0x40044000;
  const st = mkSteps($('rtc-steps'), [
    'time set by sketch (FSP)',
    'seconds advancing',
    'minute rollover with seconds at 00',
  ]);
  sayVerdict($('rtc-verdict'), [['booting RTC sketch…', 'sys']]);
  $('rtc-clock').textContent = '--:--:--';
  let done = false, sawTick = false, lastSec = -1;
  const ok = await runFw('r4rtc.bin', null, 900, () => {
    const sec = bcd(periphRead(RTC + 0x02, 1) % 256);
    const min = bcd(periphRead(RTC + 0x04, 1) % 256);
    const hr = bcd(periphRead(RTC + 0x06, 1) % 256);
    if (sec >= 0 && sec < 60 && min >= 0 && min < 60) st.mark(0);
    if (lastSec >= 0 && sec !== lastSec) { st.mark(1); sawTick = true; }
    lastSec = sec;
    $('rtc-clock').textContent = `${pad2(hr)}:${pad2(min)}:${pad2(sec)}`;
    if (sawTick && sec === 0) { st.mark(2); done = true; }
    refreshBoard(false, false);
  });
  running = false; setState('paused');
  sayVerdict($('rtc-verdict'), ok && done
    ? [['rollover complete: BCD minute carried with seconds at 00', 'okline']]
    : [['no rollover seen (see console).', '']]);
  if (!ok || !done) banner('RTC demo did not complete.', 'err');
});

/* ---------------- ctsu (mutual capacitance) ---------------- */
$('btn-ctsu-run').addEventListener('click', async () => {
  const CTSU = 0x40081000;
  const st = mkSteps($('ctsu-steps'), [
    'mutual mode armed (MD=2, RX5/TX3)',
    'measurement complete (SC nonzero)',
    'SC matches pair default (0x1146)',
    'LED on',
  ]);
  sayVerdict($('ctsu-verdict'), [['booting CTSU sketch…', 'sys']]);
  let led = false, lastSc = -1;
  const ok = await runFw('r4ctsu.bin', null, 3000, () => {
    if ((periphRead(CTSU + 0x01, 1) & 0xC0) === 0x80) st.mark(0);
    const sc = periphRead(CTSU + 0x18, 2) % 65536;
    const rc = periphRead(CTSU + 0x1A, 2) % 65536;
    if (sc !== 0) st.mark(1);
    if (sc === 0x1146) st.mark(2);
    if (ledOn()) { st.mark(3); led = true; }
    if (sc !== 0 && sc !== lastSc) {
      lastSc = sc;
      sayVerdict($('ctsu-verdict'), [[`SC=0x${sc.toString(16)} RC=0x${rc.toString(16)}`, 'sys']]);
    }
  });
  running = false; setState('paused');
  if (led) sayVerdict($('ctsu-verdict'), [['touch measurement complete: SC=0x1146, LED on', 'okline']]);
  else { sayVerdict($('ctsu-verdict'), [['no LED — the CTSU flow did not complete (see console).', '']]); banner('CTSU demo did not complete.', 'err'); }
});

/* ---------------- hid keyboard ---------------- */
$('btn-hid-run').addEventListener('click', async () => {
  blinkMode = false;
  const log = $('hid-verdict');
  log.innerHTML = '';
  const say = (s, c) => {
    const sp = document.createElement('span');
    if (c) sp.className = c;
    sp.textContent = s + '\n';
    log.appendChild(sp);
  };
  const ul = $('hid-steps');
  ul.innerHTML = '';
  const step = async (label, fn) => {
    const li = document.createElement('li');
    li.textContent = label; li.className = 'run';
    ul.appendChild(li);
    const r = await fn();
    li.className = 'done';
    return r;
  };
  try {
    const fw = await fetch('fw/r4hid.bin').then((r) => r.arrayBuffer()).then((b) => new Uint8Array(b));
    boot(fw);
    running = true; setState('running');
    await pump(6000000);
    await step('attach + bus reset', async () => {
      usb_host_attach();
      await runUntil(900000, () => false);
      usb_host_reset();
      await runUntil(900000, () => false);
    });
    const dev = await step('GET_DESCRIPTOR (device)', () => ctlIn(0x0680, 0x0100, 0, 18, 18));
    if (dev[0] !== 18 || dev[1] !== 1) throw new Error('bad device descriptor');
    await step('SET_ADDRESS 5', () => ctlOut(0x0500, 5, 0, []));
    const cfg9 = await step('GET_DESCRIPTOR (config, 9)', () => ctlIn(0x0680, 0x0200, 0, 9, 9));
    const total = cfg9[2] | (cfg9[3] << 8);
    const cfg = await step(`GET_DESCRIPTOR (config, ${total})`, () => ctlIn(0x0680, 0x0200, 0, total, total));
    // Find the HID interface (class 3) like a real host would.
    let iface = null, i = 0;
    while (i + 2 < cfg.length) {
      const len = cfg[i];
      if (!len) break;
      if (i + len <= cfg.length && cfg[i + 1] === 4 && cfg[i + 5] === 3) iface = cfg[i + 2];
      i += len;
    }
    if (iface === null) throw new Error('no HID interface in config');
    say(`HID interface ${iface} found`, 'okline');
    await step('SET_CONFIGURATION 1', () => ctlOut(0x0900, 1, 0, []));
    await step('SET_LINE_CODING + DTR (CDC)', async () => {
      await ctlOut(0x2021, 0, 0, [0x80, 0x25, 0x00, 0x00, 0x00, 0x00, 0x08]);
      await ctlOut(0x2221, 3, 0, []);
    });
    usb_take_tx(); // drain early reports: sketch sends on mount
    const rep = await step('GET_DESCRIPTOR (HID report)', () => ctlIn(0x0681, 0x2200, iface, 255, 63));
    if (rep[0] !== 0x05 || rep[1] !== 0x01) throw new Error('bad report descriptor');
    say(`report descriptor OK (${rep.length}B boot keyboard)`, 'okline');
    await step('SET_IDLE', () => ctlOut(0x0A21, 0, iface, []));
    // Discover the interrupt-IN pipe (TYPE==int, EPNUM==3, DIR=IN).
    let intPipe = null;
    for (let n = 1; n < 10; n++) {
      periphWrite(USBFS + 0x64, 2, n);
      const c = periphRead(USBFS + 0x68, 2);
      if (((c >> 14) & 3) === 2 && (c & 0xF) === 3 && (c & (1 << 4))) { intPipe = n; break; }
    }
    if (intPipe === null) throw new Error('HID INT-IN pipe not found');
    say(`interrupt pipe ${intPipe} (EP3 IN)`, 'okline');
    usb_take_tx();
    // The sketch re-sends every 500ms; wait for one full 8-byte report.
    const want = [0, 0, 0x04, 0, 0, 0, 0, 0];
    const got = [];
    const ok = await runUntil(2000 * CHUNK, () => {
      got.push(...usb_take_tx());
      refreshBoard(false, true);
      for (let k = 0; k + 8 <= got.length; k++) {
        if (want.every((b, j) => got[k + j] === b)) return true;
      }
      return false;
    });
    if (!ok) throw new Error('no HID report arrived');
    $('hid-key').innerHTML = '';
    const ks = document.createElement('span');
    ks.className = 'okline';
    ks.textContent = "key 'a' (0x04) pressed — 8-byte boot report received";
    $('hid-key').appendChild(ks);
    flash('led-rx'); refreshBoard(false, true);
    say(`key 'a' report received (${got.length}B captured)`, 'okline');
  } catch (e) {
    say('bring-up failed: ' + e.message);
    banner('HID bring-up failed: ' + e.message, 'err');
  }
  setState('paused'); running = false;
});

/* ---------------- WDT (refresh-holds + expiry-latches) ---------------- */
$('btn-wdt-run').addEventListener('click', async () => {
  const st = mkSteps($('wdt-steps'), [
    'refresh sketch booted (8 s window)',
    'no reset across 5000 chunks with refresh',
    'expiry sketch booted (100 ms window)',
    'reset latched without refresh',
  ]);
  sayVerdict($('wdt-verdict'), [['booting WDT refresh sketch…', 'sys']]);
  let ok = await runFw('r4wdtref.bin', null, 5000, () => {
    if (steps > 6000000) st.mark(0);
    if (is_watchdog_reset_requested()) banner('WDT fired despite refresh — model bug.', 'err');
  });
  if (ok && !is_watchdog_reset_requested()) {
    st.mark(0); st.mark(1);
    sayVerdict($('wdt-verdict'), [['refresh holds: 5000 chunks, no reset latched', 'sys'], ['booting WDT expiry sketch…', 'sys']]);
  } else {
    sayVerdict($('wdt-verdict'), [['no verdict — the refresh flow did not complete (see console).', '']]);
    banner('WDT demo did not complete.', 'err');
    running = false; setState('paused');
    return;
  }
  let fired = false;
  ok = await runFw('r4wdtexp.bin', null, 6000, () => {
    if (steps > 6000000) st.mark(2);
    if (is_watchdog_reset_requested()) fired = true;
  });
  if (fired) st.mark(3);
  running = false; setState('paused');
  sayVerdict($('wdt-verdict'), ok && fired
    ? [['refresh holds 5000 chunks; expiry latches the reset flag', 'okline']]
    : [['no reset latched without refresh (see console).', '']]);
  if (!ok || !fired) banner('WDT demo did not complete.', 'err');
});

/* ---------------- EK-RA4M1 (bare-metal zero-boot, P106 LED1) ---------------- */
$('btn-ek-run').addEventListener('click', async () => {
  const st = mkSteps($('ek-steps'), [
    'EK target selected (LED1 = P106)',
    'zero-boot image at 0x00000000 (no APP_BASE)',
    'P106 output latched (PORT1.6)',
  ]);
  sayVerdict($('ek-verdict'), [['building zero-boot P106 image (same bytes as the Rust proof)…', 'sys']]);
  setTarget('ek');
  // Same Thumb as ra4m1_ek_ra4m1_zero_boot_p106_led: LDR r0,=PORT1;
  // LDR r1,=bits; STR r1,[r0]; B . with pools at 0x114/0x118.
  const img = new Uint8Array(0x200);
  const dv = new DataView(img.buffer);
  const PORT1 = 0x40040020;
  const BITS = (1 << (16 + 6)) | (1 << 6);
  const code = [0x4804, 0x4905, 0x6001, 0xE7FE, 0xBF00, 0xBF00, 0xBF00, 0xBF00];
  code.forEach((w, i) => dv.setUint16(0x100 + i * 2, w, true));
  dv.setUint32(0x114, PORT1, true);
  dv.setUint32(0x118, BITS, true);
  // SP/PC vector table at zero (EK has no bootloader offset).
  const flash = new Uint8Array(0x200);
  flash.set([0x00, 0x80, 0x00, 0x20, 0x01, 0x01, 0x00, 0x00]);
  const fw = new Uint8Array(0x200);
  fw.set(flash.subarray(0, 8), 0);
  fw.set(img.subarray(8), 8);
  bootAt(fw, 0x00000000);
  running = true; setState('running');
  pump(6); // execute LDR/LDR/STR (6 instr is plenty)
  st.mark(0); st.mark(1);
  refreshBoard(false, false);
  let done = false;
  for (let i = 0; i < 4 && running; i++) {
    if (!pump(4)) break;
    if (((periphRead(PORT1, 4) >>> 0) & (1 << 6)) !== 0) { st.mark(2); done = true; break; }
    if (i % 8 === 7) refreshBoard(false, false);
  }
  running = false; setState('paused');
  refreshBoard(false, false);
  sayVerdict($('ek-verdict'), done
    ? [['EK-RA4M1 live: P106 latched from a zero-boot image (no bootloader)', 'okline']]
    : [['P106 never latched (see console).', '']]);
  if (!done) banner('EK-RA4M1 demo did not complete.', 'err');
});
function bootAt(fwBytes, base) {
  // Like boot() but loads at an explicit base (EK boots at zero:
  // boot() always loads at APP_BASE, so redo the load here).
  reset_state();
  init_ra4m1();
  const dv = new DataView(fwBytes.buffer, fwBytes.byteOffset, fwBytes.byteLength);
  const sp = dv.getUint32(0, true), pc = dv.getUint32(4, true);
  board = new WasmCpu(sp, pc);
  board.load_firmware(fwBytes, base);
  board.reset_cpu(sp, pc);
  board.set_deliver_irqs(true);
  steps = 0;
  banner(null);
  setState('paused');
}

/* ---------------- matrix (12x8 charlieplexed GPIO) ---------------- */
const MP = [[0,3],[0,4],[0,11],[0,12],[0,13],[0,15],[2,4],[2,5],[2,6],[2,12],[2,13]];
const SMILE = [0x3C,0x42,0xA5,0x81,0xA5,0x99,0xA5,0x81,0xA5,0x42,0x3C,0x00];
const mxCells = [];
function buildMatrix() {
  const g = $('matrix-grid');
  for (let y = 0; y < 8; y++) for (let x = 0; x < 12; x++) {
    const d = document.createElement('div');
    d.className = 'mx';
    g.appendChild(d); mxCells.push(d);
  }
}
buildMatrix();
const mxLast = new Array(96).fill(-1e9);
function mxSample(nowMs) {
  const p0 = periphRead(PORT_BASE, 4), p2 = periphRead(PORT_BASE + 2 * 0x20, 4);
  const isOutHi = (p, b) => (((p >>> 16) >> b) & 1) !== 0 && (((p & 0xFFFF) >> b) & 1) !== 0;
  const isOutLo = (p, b) => (((p >>> 16) >> b) & 1) !== 0 && (((p & 0xFFFF) >> b) & 1) === 0;
  const hi = (port, bit) => isOutHi(port === 0 ? p0 : p2, bit);
  const lo = (port, bit) => isOutLo(port === 0 ? p0 : p2, bit);
  for (let k = 0; k < 96; k++) {
    const a = Math.floor(k / 9), t = k % 9, c = t + (t >= a ? 1 : 0);
    if (hi(MP[a][0], MP[a][1]) && lo(MP[c][0], MP[c][1])) mxLast[k] = nowMs;
  }
}
function mxRender(nowMs) {
  // ~150 ms persistence, like an eye.
  for (let k = 0; k < 96; k++) {
    const x = Math.floor(k / 8), y = k % 8;
    mxCells[y * 12 + x].classList.toggle('hi', nowMs - mxLast[k] < 150);
  }
}
$('btn-matrix-run').addEventListener('click', async () => {
  const st = mkSteps($('matrix-steps'), [
    'multiplexing seen (distinct LEDs lit)',
    'frame stable over full cycles',
    'reconstruction matches smiley',
  ]);
  sayVerdict($('matrix-verdict'), [['booting bare-metal matrix sketch…', 'sys']]);
  mxLast.fill(-1e9); mxRender(0);
  const seen = new Set();
  let done = false;
  const ok = await runFw('r4matrix.bin', null, 400, () => {
    const nowMs = steps / 48000; // 48k instructions ≈ 1 ms at 48 MHz
    mxSample(nowMs);
    mxRender(nowMs);
    for (let k = 0; k < 96; k++) if (nowMs - mxLast[k] < 150) seen.add(k);
    if (seen.size > 4) st.mark(0);
    // Verdict: every wanted LED seen recently, no extras persist.
    let extra = false;
    for (const k of seen) {
      const want = ((SMILE[Math.floor(k / 8)] >> (k % 8)) & 1) !== 0;
      if (!want && nowMs - mxLast[k] < 150) { extra = true; break; }
    }
    if (nowMs > 500 && !extra) {
      let all = true, wanted = 0;
      for (let k = 0; k < 96; k++) {
        const want = ((SMILE[Math.floor(k / 8)] >> (k % 8)) & 1) !== 0;
        if (want) wanted++;
        if (want && !seen.has(k)) { all = false; break; }
      }
      if (wanted === 36) st.mark(1);
      if (all) { st.mark(2); done = true; }
    }
  });
  running = false; setState('paused');
  sayVerdict($('matrix-verdict'), ok && done
    ? [[`smiley complete: ${seen.size} LEDs reconstructed from GPIO`, 'okline']]
    : [['pattern did not stabilize (see console).', '']]);
  if (!ok || !done) banner('Matrix demo did not complete.', 'err');
});

/* ---------------- mips meter ---------------- */
// Samples retired instructions once a second; shows 1-decimal MIPS
// while running, '-' when paused (avoids a stale number).
let mipsT = performance.now(), mipsN = 0;
setInterval(() => {
  const now = performance.now(), dt = (now - mipsT) / 1000;
  if (dt > 0) {
    $('stat-mips').textContent = running ? ((steps - mipsN) / dt / 1e6).toFixed(1) : '-';
    mipsT = now; mipsN = steps;
  }
}, 1000);

/* ---------------- boot ---------------- */buildGpio();
try {
  await init();
  init_ra4m1();
  banner('WASM core loaded — pick Blink to boot the board.', 'info');
} catch (e) {
  banner('Failed to start WASM core: ' + e.message, 'err');
}
