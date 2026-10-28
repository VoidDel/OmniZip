// Builds the binaries that ship next to the desktop app and puts them where the Tauri bundler
// expects them (docs/10-build-release.md §3):
//   * omnizip(.exe)        — CLI, bundled as an "external binary" (sidecar);
//   * omnizip_shell.dll    — Windows context-menu extension, bundled as a resource;
//   * omnizip_menu.msix    — its context-menu package (Windows 11 top-level menu), a resource
//                            the installer registers (platform/windows/msix/pack.ps1 -Sparse);
//   * filetypes/<ext>.ico  — the icons of the archive file types (icons/filetypes,
//                            scripts/gen-filetype-icons.mjs), resources the installer points the
//                            file types at.
// Called by `beforeBuildCommand` in apps/desktop/src-tauri/bundle.windows.json (`pnpm bundle:windows`).
import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync, existsSync, readFileSync, readdirSync } from 'node:fs';
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
  const icons = path.join(root, 'icons/filetypes');
  mkdirSync(path.join(out, 'filetypes'), { recursive: true });
  for (const f of readdirSync(icons).filter((f) => f.endsWith('.ico'))) {
    copyFileSync(path.join(icons, f), path.join(out, 'filetypes', f));
  }
  // Signed with the release certificate (OMNIZIP_SIGN_PFX, OMNIZIP_SIGN_PASSWORD, its subject in
  // OMNIZIP_PUBLISHER) or with the key of the project's test certificate
  // (platform/windows/msix/omnizip-test.cer): OMNIZIP_TEST_PFX / OMNIZIP_TEST_PASSWORD, else
  // %LOCALAPPDATA%\OmniZip\dev-cert (new-test-cert.ps1). The key is never in the repository.
  // Windows does not register an unsigned package: the app then has the classic menu only.
  const version = JSON.parse(readFileSync(path.join(tauriDir, 'tauri.conf.json'), 'utf8')).version;
  const devCert = path.join(process.env.LOCALAPPDATA ?? '', 'OmniZip', 'dev-cert');
  let sign = [];
  if (process.env.OMNIZIP_SIGN_PFX) {
    sign = ['-Sign', process.env.OMNIZIP_SIGN_PFX, '-Password', process.env.OMNIZIP_SIGN_PASSWORD ?? ''];
  } else if (process.env.OMNIZIP_TEST_PFX) {
    sign = ['-Sign', process.env.OMNIZIP_TEST_PFX, '-Password', process.env.OMNIZIP_TEST_PASSWORD ?? ''];
    console.log('prepare-bundle: context-menu package signed with the test certificate');
  } else if (existsSync(path.join(devCert, 'omnizip-test.pfx'))) {
    sign = ['-Sign', path.join(devCert, 'omnizip-test.pfx'), '-Password', readFileSync(path.join(devCert, 'password.txt'), 'utf8').trim()];
    console.log('prepare-bundle: context-menu package signed with the test certificate');
  } else {
    console.warn('prepare-bundle: context-menu package unsigned (no Windows 11 top-level menu)');
  }
  const publisher = process.env.OMNIZIP_PUBLISHER ? ['-Publisher', process.env.OMNIZIP_PUBLISHER] : [];
  const pack = [
    '-NoProfile', '-File', path.join(root, 'platform/windows/msix/pack.ps1'),
    '-Version', version, '-Arch', triple.startsWith('aarch64') ? 'arm64' : 'x64', '-Sparse',
    '-Out', path.join(out, 'omnizip_menu.msix'), ...publisher, ...sign,
  ];
  execFileSync('pwsh', pack, { cwd: root, stdio: 'inherit' });
}
for (const f of [`omnizip-${triple}${exe}`, ...(windows ? ['omnizip_shell.dll', 'omnizip_menu.msix'] : [])]) {
  if (!existsSync(path.join(out, f))) throw new Error(`missing ${f}`);
  console.log(`prepare-bundle: ${f}`);
}
