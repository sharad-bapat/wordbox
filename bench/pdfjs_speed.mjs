// Time pdf.js text extraction, in process: getDocument from bytes, then getTextContent for every page.
// Eval is off, as in the demo. Prints one JSON line per file.
// usage: node pdfjs_speed.mjs <list.txt>
import { readFileSync } from 'node:fs';
import { performance } from 'node:perf_hooks';
import { pathToFileURL } from 'node:url';
import * as pdfjs from 'pdfjs-dist/legacy/build/pdf.mjs';

pdfjs.GlobalWorkerOptions.workerSrc = pathToFileURL(new URL('./node_modules/pdfjs-dist/legacy/build/pdf.worker.mjs', import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1')).href;

const files = readFileSync(process.argv[2], 'utf8').split('\n').map((l) => l.trim()).filter(Boolean);
for (const file of files) {
  const data = new Uint8Array(readFileSync(file));
  const t0 = performance.now();
  let items = 0, pages = 0, error = null;
  try {
    const task = pdfjs.getDocument({ data, isEvalSupported: false, verbosity: 0 });
    const doc = await task.promise;
    pages = doc.numPages;
    for (let p = 1; p <= doc.numPages; p++) {
      const page = await doc.getPage(p);
      const tc = await page.getTextContent();
      items += tc.items.length;
      page.cleanup();
    }
    await task.destroy();
  } catch (e) { error = String(e).slice(0, 120); }
  const ms = performance.now() - t0;
  console.log(JSON.stringify({ file, ms: Math.round(ms * 100) / 100, pages, items, error }));
}
