// Flatten LocalSend's Apache-2.0 Slang catalogs into English-keyed GTK catalogs.
// Usage: node assets/i18n/import-upstream.mjs /path/to/localsend/app/assets/i18n
// GTK-specific wording is maintained separately in gtk-*.json.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const directory = path.dirname(fileURLToPath(import.meta.url));
const source = process.argv[2];
if (!source) throw new Error('Pass the upstream app/assets/i18n directory');
const locales = [
  { id: 'en', file: 'en.json' },
  { id: 'zh-CN', file: 'zh-CN.json' },
  { id: 'zh-TW', file: 'zh-TW.json' },
  { id: 'ja', file: 'ja.json' },
  { id: 'ko', file: 'ko.json' },
  { id: 'de', file: 'de.json' },
  { id: 'fr', file: 'fr.json' },
  { id: 'es', file: 'es-ES.json' },
  { id: 'ru', file: 'ru.json' },
];
function flatten(value, prefix = '', out = {}) {
  if (typeof value === 'string') out[prefix] = value;
  else if (value && typeof value === 'object') {
    for (const [key, child] of Object.entries(value)) {
      if (key.startsWith('@') || key.startsWith('aliasGenerator') || key === 'whatsNewPage') continue;
      const name = key.split('(')[0];
      flatten(child, prefix ? `${prefix}.${name}` : name, out);
    }
  }
  return out;
}
const sources = locales.map(locale => flatten(JSON.parse(fs.readFileSync(path.join(source, locale.file), 'utf8'))));
function resolve(catalog, key, seen = new Set()) {
  if (seen.has(key)) return null;
  seen.add(key);
  const value = catalog[key];
  if (value?.startsWith('@:')) {
    if (!/^@:[\w.]+$/.test(value)) return null;
    let ref = value.slice(2);
    if (ref.startsWith('.')) ref = key.slice(0, key.lastIndexOf('.')) + ref;
    return resolve(catalog, ref, seen);
  }
  return value ?? null;
}
const placeholders = value => [...value.matchAll(/\{([^{}]+)\}/g)].map(match => match[1]).sort().join(',');
const outputs = locales.map(() => ({}));
const fallback = locales.map(() => []);
for (const key of Object.keys(sources[0])) {
  const english = resolve(sources[0], key);
  if (!english || english.includes('@:') || Object.hasOwn(outputs[0], english)) continue;
  for (let index = 0; index < locales.length; index++) {
    let translated = resolve(sources[index], key);
    if (!translated || translated.includes('@:') || placeholders(english) !== placeholders(translated)) {
      translated = english;
      fallback[index].push(key);
    }
    outputs[index][english] = translated;
  }
}
for (let index = 0; index < locales.length; index++) {
  fs.writeFileSync(path.join(directory, `${locales[index].id}.json`), JSON.stringify(outputs[index], null, 2) + '\n');
  console.log(`${locales[index].id}: ${Object.keys(outputs[index]).length} strings; fallback: ${fallback[index].join(', ') || 'none'}`);
}
