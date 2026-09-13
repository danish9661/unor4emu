import init, {
  WasmCpu, init_ra4m1, reset_state, tick_peripherals,
  periph_read as _pr, periph_write as _pw,
  usb_take_tx, usb_rx_inject,
  usb_host_attach, usb_host_reset, usb_host_setup, usb_host_status_done,
  spi_set_sd_card, sd_read_block,
  soft_wire, peek_uart_output,
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
  // D13 is the Minima LED = P111 (PORT1 bit 11); fall back to
  // any-output glow (e.g. TX/RX activity elsewhere).
  const d13 = (periphRead(PORT_BASE + 1 * 0x20, 4) >> 11) & 1;
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
const ledOn = () => ((periphRead(PORT_BASE + 1 * 0x20, 4) >> 11) & 1) !== 0; // D13 = P111
// Boot fw, then pump `chunks` 48k-chunks (proof-harness cadence),
// calling perChunk() after every chunk for transient flags.
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
