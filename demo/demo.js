// Where is the text? The page, drawn by pdf.js, with every word wordbox found boxed on top of it, and the
// words listed beside it. Hover a box or a word to light up both. Everything runs in this page: the file
// is never uploaded.
import init, { extract_json } from './wordbox_wasm.js';

// pdf.js and the WebAssembly module load on first use, not with the page, so the page itself stays light
let pdfjs = null;
async function loadPdfjs() {
  if (!pdfjs) {
    const lib = await import('./pdfjs/pdf.min.mjs');
    lib.GlobalWorkerOptions.workerSrc = new URL('./pdfjs/pdf.worker.min.mjs', import.meta.url).href;
    pdfjs = lib;
  }
  return pdfjs;
}

const root = document.getElementById('demo');
const el = (tag, attrs = {}, ...kids) => {
  const n = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k.startsWith('on')) n.addEventListener(k.slice(2), v);
    else if (v !== false && v != null) n.setAttribute(k, v === true ? '' : v);
  }
  n.append(...kids.flat().filter((k) => k != null && k !== false));
  return n;
};

const SAMPLES = [['letter.pdf', 'Letter'], ['rotated.pdf', 'Rotated page'], ['form.pdf', 'Form'], ['garbled.pdf', 'Garbled text']];
const VERDICT = {
  text: 'Text you can use',
  garbled: 'Garbled: the text layer doesn’t decode to real characters',
  invisible: 'An invisible text layer, as on an OCR’d scan',
  none: 'No text on this page',
};

const input = el('input', { type: 'file', id: 'wb-file', accept: 'application/pdf,.pdf', class: 'sr-only' });
const zone = el('div', { class: 'wb-drop' },
  el('p', {}, 'Drop a PDF here, choose one, or try a sample.'),
  el('div', { class: 'wb-actions' },
    el('label', { for: 'wb-file', class: 'wb-btn' }, 'Choose a PDF'), input,
    SAMPLES.map(([f, label]) => el('button', { type: 'button', class: 'wb-btn ghost', onclick: () => sample(f, label) }, label))));
const status = el('p', { class: 'wb-status', 'aria-live': 'polite' });
const bar = el('div', { class: 'wb-bar', hidden: true });
const stage = el('div', { class: 'wb-stage' });
const words = el('div', { class: 'wb-words', 'aria-label': 'Words on this page, by line' });
const view = el('div', { class: 'wb-view', hidden: true }, stage, words);
root.replaceChildren(zone, status, bar, view,
  el('p', { class: 'wb-note' }, 'Your file stays on your device: it’s read in this page and never uploaded.'));

let ready = null;
let result = null, pdf = null, pdfReady = false, index = 0, name = "", showBoxes = true, renderTask = null;

async function load(fileName, bytes) {
  name = fileName;
  status.textContent = 'Reading…';
  await (ready ||= init());
  const t = performance.now();
  result = JSON.parse(extract_json(bytes));
  const ms = performance.now() - t;
  if (result.status !== 'ok') {
    bar.hidden = true; view.hidden = true;
    status.textContent = result.status === 'not_pdf' ? 'That isn’t a PDF.' : 'This PDF is encrypted with a password, or with a method wordbox doesn’t support.';
    return;
  }
  const total = result.pages.reduce((n, p) => n + p.words.length, 0);
  status.textContent = `${name}: ${result.pages.length} page${result.pages.length === 1 ? '' : 's'}, ${total.toLocaleString()} words, found in ${ms < 0.1 ? 'under 0.1' : ms < 10 ? ms.toFixed(1) : Math.round(ms)} ms in your browser.`;
  // the words and boxes don't wait for pdf.js: the drawing is added when it's ready (or left out if it fails)
  if (pdf) pdf.destroy().catch(() => {});
  pdf = null;
  pdfReady = false;
  index = 0;
  show();
  const mine = result;
  loadPdfjs().then((lib) => {
    if (result !== mine) return; // another file was chosen meanwhile
    const task = lib.getDocument({ data: bytes.slice(), isEvalSupported: false });
    pdf = task;
    task.promise.then(() => { if (pdf === task) { pdfReady = true; drawPage(result.pages[index]); } }, () => { if (pdf === task) pdf = null; });
  }, () => { /* without pdf.js the boxes and words still show */ });
}

async function show() {
  const page = result.pages[index];
  bar.hidden = false; view.hidden = false;
  const counts = [`${page.words.length} words`, `${page.words.length ? page.words[page.words.length - 1].line + 1 : 0} lines`];
  const flagged = (k) => page.words.filter((w) => w[k]).length;
  if (flagged('annot')) counts.push(`${flagged('annot')} from annotations`);
  if (flagged('offpage')) counts.push(`${flagged('offpage')} outside the page`);
  if (page.unmapped) counts.push(`${page.unmapped} glyphs undecodable`);
  bar.replaceChildren(
    el('button', { type: 'button', class: 'wb-btn ghost', disabled: index === 0, onclick: () => go(-1), 'aria-label': 'Previous page' }, '‹'),
    el('span', { class: 'wb-page' }, `Page ${index + 1} of ${result.pages.length}`),
    el('button', { type: 'button', class: 'wb-btn ghost', disabled: index === result.pages.length - 1, onclick: () => go(1), 'aria-label': 'Next page' }, '›'),
    el('span', { class: `wb-verdict v-${page.verdict}` }, VERDICT[page.verdict] || page.verdict),
    el('span', { class: 'wb-counts' }, counts.join(' · ')),
    el('label', { class: 'wb-toggle' }, el('input', { type: 'checkbox', checked: showBoxes, onchange: (e) => { showBoxes = e.target.checked; stage.classList.toggle('no-boxes', !showBoxes); } }), ' Boxes'));
  listWords(page);
  await drawPage(page);
}

function go(d) { index = Math.max(0, Math.min(result.pages.length - 1, index + d)); show(); }

async function drawPage(page) {
  const width = Math.max(240, stage.clientWidth || 600);
  const scale = width / page.width;
  const canvas = el('canvas', { 'aria-hidden': 'true', hidden: true });
  const overlay = el('div', { class: 'wb-overlay' });
  stage.replaceChildren(canvas, overlay);
  stage.style.height = `${page.height * scale}px`;
  stage.classList.toggle('no-boxes', !showBoxes);
  const want = index;
  page.words.forEach((w, k) => {
    if (w.offpage) return;
    overlay.append(el('div', {
      class: `wb-box${w.annot ? ' annot' : ''}${w.invisible ? ' invisible' : ''}${w.unmapped ? ' unmapped' : ''}`,
      'data-k': k, title: w.t,
      style: `left:${w.x0 * scale}px;top:${w.y0 * scale}px;width:${Math.max(1, (w.x1 - w.x0) * scale)}px;height:${Math.max(1, (w.y1 - w.y0) * scale)}px`,
    }));
  });
  // draw the page only when pdf.js has the document; until then the boxes stand alone
  if (pdf && pdfReady) {
    try {
      const doc = await pdf.promise;
      if (want !== index) return;
      canvas.hidden = false;
      const p = await doc.getPage(index + 1);
      const vp = p.getViewport({ scale });
      const dpr = window.devicePixelRatio || 1;
      canvas.width = Math.floor(vp.width * dpr);
      canvas.height = Math.floor(vp.height * dpr);
      canvas.style.width = `${vp.width}px`;
      canvas.style.height = `${vp.height}px`;
      if (renderTask) renderTask.cancel();
      renderTask = p.render({ canvasContext: canvas.getContext('2d'), viewport: vp, transform: dpr !== 1 ? [dpr, 0, 0, dpr, 0, 0] : null, annotationMode: pdfjs.AnnotationMode.ENABLE });
      await renderTask.promise.catch(() => {});
      stage.dataset.rendered = String(want + 1); // the page number just drawn, for tools/demo_check.mjs
    } catch { /* the words and boxes still show without the drawing */ }
  }
}

function listWords(page) {
  const lines = [];
  page.words.forEach((w, k) => {
    (lines[w.line] ||= []).push(el('span', {
      class: `wb-w${w.annot ? ' annot' : ''}${w.invisible ? ' invisible' : ''}${w.offpage ? ' offpage' : ''}${w.unmapped ? ' unmapped' : ''}`,
      'data-k': k,
    }, w.t));
  });
  words.replaceChildren(...lines.filter(Boolean).map((ws) => el('p', { class: 'wb-line' }, ...ws.flatMap((s, i) => (i ? [' ', s] : [s])))));
  if (!page.words.length) words.replaceChildren(el('p', { class: 'wb-empty' }, 'No words on this page.'));
}

// hover either side lights up both
function light(k, on) {
  for (const n of root.querySelectorAll(`[data-k="${k}"]`)) n.classList.toggle('hot', on);
  if (on) root.querySelector(`.wb-words [data-k="${k}"]`)?.scrollIntoView({ block: 'nearest' });
}
for (const pane of [stage, words]) {
  pane.addEventListener('pointerover', (e) => { const k = e.target.closest('[data-k]')?.dataset.k; if (k) light(k, true); });
  pane.addEventListener('pointerout', (e) => { const k = e.target.closest('[data-k]')?.dataset.k; if (k) light(k, false); });
}

async function sample(file, label) {
  const res = await fetch(new URL(`samples/${file}`, import.meta.url));
  load(`Sample: ${label}`, new Uint8Array(await res.arrayBuffer()));
}

input.addEventListener('change', async () => {
  const f = input.files?.[0];
  if (f) load(f.name, new Uint8Array(await f.arrayBuffer()));
});
zone.addEventListener('dragover', (e) => { e.preventDefault(); zone.classList.add('over'); });
zone.addEventListener('dragleave', () => zone.classList.remove('over'));
zone.addEventListener('drop', async (e) => {
  e.preventDefault();
  zone.classList.remove('over');
  const f = e.dataTransfer?.files?.[0];
  if (f) load(f.name, new Uint8Array(await f.arrayBuffer()));
});
// #sample=letter (or rotated, form, garbled) opens a sample straight away
const fromHash = () => {
  const s = new URLSearchParams(location.hash.slice(1)).get('sample');
  const hit = SAMPLES.find(([f]) => f === `${s}.pdf`);
  if (hit) sample(...hit);
};
window.addEventListener('hashchange', fromHash);
fromHash();

let resizeTimer;
window.addEventListener('resize', () => { clearTimeout(resizeTimer); resizeTimer = setTimeout(() => { if (result && !view.hidden) drawPage(result.pages[index]); }, 150); });
