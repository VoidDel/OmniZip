// i18n consistency check (docs/06-i18n.md §6). Fails CI when:
//  * a language misses keys of en-US or has extra ones (app.json),
//  * an ICU message does not parse or uses other placeholders than en-US,
//  * an error code of the core has no GUI message,
//  * Fluent files of a language define other ids than en-US.
import { readFileSync, readdirSync, existsSync } from 'node:fs';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(path.join(root, 'apps/desktop/package.json'));
const { parse, TYPE } = require('@formatjs/icu-messageformat-parser');

const errors = [];
const fail = (m) => errors.push(m);

const flatten = (obj, prefix = '') =>
  Object.entries(obj).flatMap(([k, v]) =>
    v && typeof v === 'object' ? flatten(v, `${prefix}${k}.`) : [[`${prefix}${k}`, v]],
  );

const placeholders = (ast, out = new Set()) => {
  for (const el of ast) {
    if (el.type === TYPE.argument || el.type === TYPE.number || el.type === TYPE.date || el.type === TYPE.time) out.add(el.value);
    if (el.type === TYPE.plural || el.type === TYPE.select) {
      out.add(el.value);
      for (const opt of Object.values(el.options)) placeholders(opt.value, out);
    }
    if (el.type === TYPE.tag) placeholders(el.children, out);
  }
  return out;
};

const locales = path.join(root, 'locales');
const langs = readdirSync(locales).filter((d) => existsSync(path.join(locales, d, 'app.json')));
const load = (l) => new Map(flatten(JSON.parse(readFileSync(path.join(locales, l, 'app.json'), 'utf8'))));
const en = load('en-US');

const enVars = new Map();
for (const [k, v] of en) {
  try {
    enVars.set(k, placeholders(parse(v, { ignoreTag: true })));
  } catch (e) {
    fail(`en-US ${k}: ${e.message}`);
  }
}

for (const l of langs) {
  const m = load(l);
  for (const k of en.keys()) if (!m.has(k)) fail(`${l}: missing ${k}`);
  for (const k of m.keys()) if (!en.has(k)) fail(`${l}: extra ${k}`);
  for (const [k, v] of m) {
    try {
      const vars = placeholders(parse(v, { ignoreTag: true }));
      const want = enVars.get(k) ?? new Set();
      for (const x of vars) if (!want.has(x)) fail(`${l} ${k}: unknown placeholder {${x}}`);
      for (const x of want) if (!vars.has(x)) fail(`${l} ${k}: missing placeholder {${x}}`);
    } catch (e) {
      fail(`${l} ${k}: ${e.message}`);
    }
  }
}

// Every core error key has a GUI message.
const errorRs = readFileSync(path.join(root, 'crates/omnizip-core/src/error.rs'), 'utf8');
const keyFn = errorRs.slice(errorRs.indexOf('pub const fn key'), errorRs.indexOf('pub const fn exit_code'));
const keys = [...keyFn.matchAll(/=> "([a-z-]+)"/g)].map((m) => m[1]);
if (keys.length < 20) fail('could not read error keys from error.rs');
for (const k of keys) if (!en.has(`errors.${k}`)) fail(`en-US: no GUI message for error ${k}`);

// Fluent id parity.
const ftlIds = (file) =>
  readFileSync(file, 'utf8')
    .split(/\r?\n/)
    .filter((line) => /^[a-z][a-z0-9-]* =/.test(line))
    .map((line) => line.split(' =')[0])
    .sort();
for (const f of ['cli.ftl', 'errors.ftl', 'shell.ftl']) {
  const base = ftlIds(path.join(locales, 'en-US', f));
  for (const l of langs) {
    const p = path.join(locales, l, f);
    if (!existsSync(p)) continue;
    const ids = ftlIds(p);
    const missing = base.filter((x) => !ids.includes(x));
    const extra = ids.filter((x) => !base.includes(x));
    if (missing.length) fail(`${l}/${f}: missing ${missing.join(', ')}`);
    if (extra.length) fail(`${l}/${f}: extra ${extra.join(', ')}`);
  }
}

if (errors.length) {
  console.error(`i18n-check: ${errors.length} problem(s)`);
  for (const e of errors) console.error('  ' + e);
  process.exit(1);
}
console.log(`i18n-check: ${langs.length} languages, ${en.size} GUI keys, ${keys.length} error codes — OK`);
