#!/usr/bin/env node
// npm only needs this launcher; all CLI behavior lives in Rust.
const { spawnSync } = require('node:child_process');
const { join } = require('node:path');

const platform = `${process.platform}-${process.arch}`;
const binary = join(__dirname, platform, process.platform === 'win32' ? 'paths.exe' : 'paths');
const result = spawnSync(binary, process.argv.slice(2), { stdio: 'inherit' });
if (result.error) {
  console.error(`paths: cannot run the native binary for ${platform}: ${result.error.message}`);
  console.error('Supported: macOS/Linux x64 and arm64, Windows x64. Reinstall paths-cli if your platform is supported.');
  process.exit(1);
}
if (result.signal) process.kill(process.pid, result.signal);
process.exit(result.status ?? 1);
