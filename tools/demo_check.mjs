// Drive the demo in headless Chrome over the DevTools protocol, in real time.
// For each sample: wait for the page drawing, collect console errors and exceptions, screenshot, and
// check that hovering the first word lights up its box.
// usage: node tools/demo_check.mjs <base url> <out dir> [chrome.exe]
import { spawn } from 'node:child_process';
import { writeFileSync, mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const [base, outDir, chrome = 'C:/Program Files/Google/Chrome/Application/chrome.exe'] = process.argv.slice(2);
const port = 9333;
const proc = spawn(chrome, ['--headless=new', '--disable-gpu', '--no-first-run', `--remote-debugging-port=${port}`,
  `--user-data-dir=${mkdtempSync(join(tmpdir(), 'wbchk-'))}`, '--window-size=1280,1100', 'about:blank'], { stdio: 'ignore' });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function target() {
  for (let i = 0; i < 50; i++) {
    try { const l = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json(); const p = l.find((t) => t.type === 'page'); if (p) return p; } catch {}
    await sleep(200);
  }
  throw new Error('no Chrome target');
}

const t = await target();
const ws = new WebSocket(t.webSocketDebuggerUrl);
await new Promise((r) => ws.addEventListener('open', r));
let id = 0;
const waiting = new Map();
const log = [];
ws.addEventListener('message', (e) => {
  const m = JSON.parse(e.data);
  if (m.id && waiting.has(m.id)) { waiting.get(m.id)(m); waiting.delete(m.id); }
  if (m.method === 'Runtime.consoleAPICalled' && ['error', 'warning'].includes(m.params.type)) log.push(`console.${m.params.type}: ${m.params.args.map((a) => a.value ?? a.description).join(' ')}`);
  if (m.method === 'Runtime.exceptionThrown') log.push(`exception: ${m.params.exceptionDetails.exception?.description || m.params.exceptionDetails.text}`);
  if (m.method === 'Log.entryAdded' && m.params.entry.level === 'error') log.push(`log: ${m.params.entry.text}`);
});
const send = (method, params = {}) => new Promise((r) => { const i = ++id; waiting.set(i, r); ws.send(JSON.stringify({ id: i, method, params })); });
const evaluate = async (expr) => (await send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true })).result?.result?.value;
await send('Runtime.enable'); await send('Log.enable'); await send('Page.enable');

let failed = 0;
for (const s of ['letter', 'rotated', 'form', 'garbled']) {
  log.length = 0;
  await send('Page.navigate', { url: `${base}#sample=${s}` });
  let state;
  for (let i = 0; i < 60; i++) {
    await sleep(250);
    state = await evaluate(`(() => { const c = document.querySelector('.wb-stage canvas'); return {
      drawn: !!c && !c.hidden && c.width > 0, boxes: document.querySelectorAll('.wb-box').length,
      words: document.querySelectorAll('.wb-w').length, verdict: document.querySelector('.wb-verdict')?.textContent,
      status: document.querySelector('.wb-status')?.textContent, bar: !document.querySelector('.wb-bar').hidden }; })()`);
    if (state?.drawn) break;
  }
  const hover = await evaluate(`(() => { const w = document.querySelector('.wb-words [data-k]'); if (!w) return 'no words';
    w.dispatchEvent(new PointerEvent('pointerover', { bubbles: true }));
    const k = w.dataset.k; const box = document.querySelector('.wb-overlay [data-k="' + k + '"]');
    const ok = w.classList.contains('hot') && (!box || box.classList.contains('hot'));
    w.dispatchEvent(new PointerEvent('pointerout', { bubbles: true })); return ok ? 'ok' : 'not lit'; })()`);
  const shot = await send('Page.captureScreenshot', { format: 'png' });
  writeFileSync(join(outDir, `demo-${s}.png`), Buffer.from(shot.result.data, 'base64'));
  const ok = state?.drawn && state.boxes > 0 && state.words > 0 && hover === 'ok' && !log.some((l) => /exception|console.error/.test(l));
  if (!ok) failed++;
  console.log(JSON.stringify({ sample: s, ok, ...state, hover, log }));
}
ws.close();
proc.kill();
process.exit(failed ? 1 : 0);
