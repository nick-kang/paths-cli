// Stage the local release binary, or verify every binary before npm publication.
import assert from 'node:assert/strict';
import { chmodSync, copyFileSync, mkdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { parse } from 'smol-toml';

const ROOT = resolve(import.meta.dirname, '..');
const PLATFORMS = ['darwin-x64', 'darwin-arm64', 'linux-x64', 'linux-arm64', 'win32-x64'];
const manifest: { version: string } = JSON.parse(readFileSync(join(ROOT, 'package.json'), 'utf8'));
const cargo = parse(readFileSync(join(ROOT, 'Cargo.toml'), 'utf8'));
assert.ok(cargo.package && typeof cargo.package === 'object' && 'version' in cargo.package, 'Cargo package version is missing');
assert.equal(cargo.package.version, manifest.version, 'Cargo and npm versions differ');
if (process.env.RELEASE_TAG) {
  assert.equal(process.env.RELEASE_TAG, `v${manifest.version}`, 'Release tag must match Cargo and npm versions');
}

if (process.argv.includes('--verify')) {
  for (const platform of PLATFORMS) {
    const binary = join(ROOT, 'bin', platform, platform.startsWith('win32') ? 'paths.exe' : 'paths');
    const stat = statSync(binary, { throwIfNoEntry: false });
    assert.ok(stat?.isFile() && stat.size > 0, `Missing binary: ${binary}`);
    chmodSync(binary, 0o755);
  }
} else {
  const platform = `${process.platform}-${process.arch}`;
  assert.ok(PLATFORMS.includes(platform), `Unsupported platform: ${platform}`);
  const filename = process.platform === 'win32' ? 'paths.exe' : 'paths';
  const destination = join(ROOT, 'bin', platform, filename);
  mkdirSync(dirname(destination), { recursive: true });
  copyFileSync(join(ROOT, 'target', 'release', filename), destination);
  chmodSync(destination, 0o755);
}
chmodSync(join(ROOT, 'bin', 'paths.cjs'), 0o755);
