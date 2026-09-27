// Time wordbox's WebAssembly build in Node, in process, on the same files as pdfjs_speed.mjs.
// Also checks that its output matches the native CLI's for each file (timings aside).
// usage: node wasm_speed.mjs <list.txt> <native-cli.exe>
import { readFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { performance } from 'node:perf_hooks';
import { initSync, extract_json } from '../demo/wordbox_wasm.js';

initSync({ module: readFileSync(new URL('../demo/wordbox_wasm_bg.wasm', import.meta.url)) });
const files = readFileSync(process.argv[2], 'utf8').split('\n').map((l) => l.trim()).filter(Boolean);
const cli = process.argv[3];
let same = 0;
for (const file of files) {
  const data = new Uint8Array(readFileSync(file));
  const t0 = performance.now();
  const json = extract_json(data);
  const ms = performance.now() - t0;
  const native = execFileSync(cli, [file], { maxBuffer: 1 << 30 }).toString();
  const strip = (s) => s.replace(/^\{"file":.*?"micros":[0-9.]+,/, '{').trim();
  const match = strip(native) === json.trim();
  same += match;
  console.log(JSON.stringify({ file, ms: Math.round(ms * 100) / 100, match }));
}
console.error(`wasm output identical to native on ${same} of ${files.length} files`);
