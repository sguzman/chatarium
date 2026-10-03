import fs from 'node:fs';

const root = new URL('./', import.meta.url);
const manifest = JSON.parse(fs.readFileSync(new URL('manifest.json', root), 'utf8'));
const worker = fs.readFileSync(new URL('service-worker.js', root), 'utf8');

function sameSet(actual, expected, label) {
  if (!Array.isArray(actual)) throw new Error(`${label} must be an array`);
  const left = [...actual].sort();
  const right = [...expected].sort();
  if (JSON.stringify(left) !== JSON.stringify(right)) {
    throw new Error(`${label} drifted: ${JSON.stringify(actual)}`);
  }
}

if (manifest.manifest_version !== 3) {
  throw new Error('Edge bridge must remain Manifest V3');
}
if (manifest.name !== 'Chatarium Edge Bridge') {
  throw new Error('unexpected extension name');
}
if (manifest.version !== '0.1.0') {
  throw new Error(`unexpected extension version ${manifest.version}`);
}
sameSet(manifest.permissions, ['scripting', 'storage', 'webRequest'], 'permissions');
sameSet(
  manifest.host_permissions,
  ['https://chatgpt.com/*', 'http://127.0.0.1:43117/*'],
  'host_permissions',
);
if (manifest.background?.service_worker !== 'service-worker.js') {
  throw new Error('unexpected service worker entrypoint');
}
if (manifest.background?.type !== 'module') {
  throw new Error('service worker must remain an ES module');
}
for (const forbidden of ['GM_xmlhttpRequest', 'unsafeWindow', 'Tampermonkey']) {
  if (worker.includes(forbidden)) {
    throw new Error(`retired userscript dependency leaked into Edge bridge: ${forbidden}`);
  }
}
for (const required of [
  "world: 'MAIN'",
  'chrome.webRequest.onBeforeSendHeaders',
  'chrome.webRequest.onBeforeRequest',
  'chrome.storage.session',
  "const ACCOUNT_HEADER = 'ChatGPT-Account-ID'",
  "const BRIDGE_ORIGIN = 'http://127.0.0.1:43117'",
]) {
  if (!worker.includes(required)) {
    throw new Error(`required Edge bridge invariant missing: ${required}`);
  }
}
console.log('Chatarium Edge Bridge package invariants OK');
