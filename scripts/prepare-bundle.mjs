// Builds the binaries that ship next to the desktop app and puts them where the Tauri bundler
// expects them (docs/10-build-release.md §3):
//   * omnizip(.exe)        — CLI, bundled as an "external binary" (sidecar);
//   * omnizip_shell.dll    — Windows context-menu extension, bundled as a resource.
// Called by `beforeBuildCommand` in apps/desktop/src-tauri/bundle.windows.json (`pnpm bundle:windows`).
import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync, existsSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const tauriDir = path.join(root, 'apps/desktop/src-tauri');
const out = path.join(tauriDir, 'binaries');
mkdirSync(out, { recursive: true });

const triple =
  process.env.TAURI_ENV_TARGET_TRIPLE ||
  execFileSync('rustc', ['-vV'], { encoding: 'utf8' }).match(/^host: (.+)$/m)[1].trim();
const windows = triple.includes('windows');
const exe = windows ? '.exe' : '';

const packages = ['omnizip-cli', ...(windows ? ['omnizip-shell-win'] : [])];
const args = ['build', '--release', '--target', triple, ...packages.flatMap((p) => ['-p', p])];
console.log(`prepare-bundle: cargo ${args.join(' ')}`);
execFileSync('cargo', args, { cwd: root, stdio: 'inherit' });

const built = path.join(root, 'target', triple, 'release');
copyFileSync(path.join(built, `omnizip${exe}`), path.join(out, `omnizip-${triple}${exe}`));
if (windows) {
  copyFileSync(path.join(built, 'omnizip_shell.dll'), path.join(out, 'omnizip_shell.dll'));
}
for (const f of [`omnizip-${triple}${exe}`, ...(windows ? ['omnizip_shell.dll'] : [])]) {
  if (!existsSync(path.join(out, f))) throw new Error(`missing ${f}`);
  console.log(`prepare-bundle: ${f}`);
}
