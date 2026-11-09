// Copies the built-in themes and their schema from the installed omni-ui package into themes/,
// where the Rust theme service embeds them (`include_str!` in apps/desktop/src-tauri/src/theme.rs).
// Run after `pnpm update omni-ui`; CI fails when the copy is stale (see .github/workflows/ci.yml).
import { cpSync, rmSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(new URL('../apps/desktop/package.json', import.meta.url));
const pkg = dirname(require.resolve('omni-ui/package.json'));
const dest = fileURLToPath(new URL('../themes/', import.meta.url));
for (const dir of ['builtin', 'schema']) {
  rmSync(join(dest, dir), { recursive: true, force: true });
  cpSync(join(pkg, 'themes', dir), join(dest, dir), { recursive: true });
}
console.log('themes/builtin and themes/schema synced from', pkg);
