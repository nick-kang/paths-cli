// Integration test: real npm/pnpm workspaces, local registry/Git, and global npm installation.
import assert from 'node:assert/strict';
import { once } from 'node:events';
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import spawn from 'cross-spawn';

const ROOT = resolve(import.meta.dirname, '..');
const REPOSITORY = 'https://github.com/paths-fixture/source';

type Manager = 'npm' | 'pnpm';
type Manifest = {
  name: string;
  version: string;
  private?: boolean;
  dependencies?: Record<string, string>;
  optionalDependencies?: Record<string, string>;
  repository?: string;
  gitHead?: string;
  dist?: { tarball: string; shasum: string };
};
type Result = {
  request: string;
  result?: { version: string; commit: string; path: string };
  error?: string;
};
type Packed = { filename: string; shasum: string };

async function run(args: string[], cwd: string, env: NodeJS.ProcessEnv, code = 0): Promise<string> {
  const child = spawn(args[0], args.slice(1), {
    cwd, env, stdio: ['ignore', 'pipe', 'pipe'], timeout: 120_000,
  });
  let stdout = '';
  let stderr = '';
  child.stdout.setEncoding('utf8').on('data', (chunk: string) => { stdout += chunk; });
  child.stderr.setEncoding('utf8').on('data', (chunk: string) => { stderr += chunk; });
  const [status, signal] = await once(child, 'close');
  assert.equal(status, code, `${JSON.stringify(args)} exited ${status} (${signal})\n${stdout}\n${stderr}`);
  return stdout.trim();
}

function writeJson(path: string, value: unknown): void {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, JSON.stringify(value));
}

async function checkManager(
  manager: Manager, temp: string, env: NodeJS.ProcessEnv, registry: string,
  cli: string, commits: Record<string, string>,
): Promise<void> {
  const project = join(temp, manager);
  mkdirSync(project);
  const version = await run([manager, '--version'], project, env);
  writeJson(join(project, 'package.json'), {
    name: 'fixture-root', private: true, version: '1.0.0',
    packageManager: `${manager}@${version}`, workspaces: ['packages/*'],
  });
  writeFileSync(join(project, '.npmrc'), `registry=${registry}\nfetch-retries=0\naudit=false\nfund=false\n`);
  if (manager === 'pnpm') {
    writeFileSync(join(project, 'pnpm-workspace.yaml'), "packages:\n  - 'packages/*'\nminimumReleaseAge: 0\n");
  }
  const workspaces: Record<string, Record<string, string>> = {
    a: { '@fixture/parent': '1.0.0' }, b: { 'paths-fixture': '2.0.0' }, empty: {},
  };
  for (const [name, dependencies] of Object.entries(workspaces)) {
    writeJson(join(project, 'packages', name, 'package.json'), {
      name, version: '1.0.0', private: true, dependencies,
    });
  }
  const install = [manager, 'install', '--ignore-scripts'];
  if (manager === 'pnpm') install.push('--no-frozen-lockfile');
  await run(install, project, env);
  const args = [cli, '--cache-dir', join(temp, `${manager}-cache`)];
  const batch = await run([...args, 'paths-fixture', 'not-installed', 'paths-fixture', '--json'], project, env, 1);
  const results: Result[] = batch.split('\n').map((line) => JSON.parse(line));
  assert.deepEqual(results.map((result) => result.request), ['paths-fixture', 'not-installed', 'paths-fixture']);
  const first = results[0].result;
  assert.ok(first, batch);
  assert.equal(first.version, '1.0.0');
  assert.equal(first.commit, commits['1.0.0']);
  assert.equal(results[2].result?.path, first.path);
  assert.ok(results[1].error);
  const second = (JSON.parse(await run([...args, '--filter', 'b', 'paths-fixture', '--json'], project, env)) as Result).result;
  assert.ok(second);
  assert.equal(second.version, '2.0.0');
  assert.equal(second.commit, commits['2.0.0']);
  assert.notEqual(first.path, second.path);
  assert.equal(readFileSync(join(first.path, 'source.rs'), 'utf8'), '1.0.0');
  assert.equal(readFileSync(join(second.path, 'source.rs'), 'utf8'), '2.0.0');
  assert.equal(await run([...args, '--filter', 'empty', 'paths-fixture'], project, env), first.path);
  const nested = join(project, 'packages', 'b', 'src');
  mkdirSync(nested);
  assert.equal(await run([...args, '--filter', 'packages/b', 'paths-fixture'], nested, env), second.path);
  assert.equal(await run([...args, 'paths-fixture@2.0.0'], project, env), second.path);
  assert.ok(await run([...args, '@fixture/parent'], project, env));

  // Alias-only installation must resolve both the alias and the actual package name.
  const manifestPath = join(project, 'packages', 'a', 'package.json');
  const manifest: Manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
  manifest.dependencies = { alias: 'npm:paths-fixture@1.0.0' };
  writeJson(manifestPath, manifest);
  await run(install, project, env);
  assert.equal(await run([...args, '--filter', 'a', 'alias'], project, env), first.path);
  assert.equal(await run([...args, '--filter', 'a', 'paths-fixture'], project, env), first.path);
  if (manager === 'npm') {
    manifest.dependencies['not-installed'] = '1.0.0';
    writeJson(manifestPath, manifest);
    assert.equal(await run([...args, '--filter', 'a', 'paths-fixture'], project, env), first.path);
  }
  console.log(`${manager} ${version}: workspace, transitive, scoped, alias, exact version, cache, and batch checks passed`);
}

async function main(): Promise<void> {
  assert.ok(Number(process.versions.node.split('.')[0]) >= 26, 'Run the integration test with Node.js 26+');
  const temp = realpathSync(mkdtempSync(join(tmpdir(), 'paths-smoke-')));
  try {
    // Isolate registry credentials and Git settings from the developer's machine.
    const npmrc = join(temp, 'npmrc');
    const gitconfig = join(temp, 'gitconfig');
    writeFileSync(npmrc, '');
    writeFileSync(gitconfig, '');
    const env: NodeJS.ProcessEnv = {
      ...process.env, CI: 'true', npm_config_update_notifier: 'false', COREPACK_ENABLE_DOWNLOAD_PROMPT: '0',
      NPM_CONFIG_USERCONFIG: npmrc, GIT_CONFIG_GLOBAL: gitconfig, GIT_CONFIG_NOSYSTEM: '1',
    };
    // Packaging must reject an incorrect release tag before staging binaries.
    await run([process.execPath, 'scripts/package.ts'], ROOT, { ...env, RELEASE_TAG: 'invalid-tag' }, 1);
    const repo = join(temp, 'source');
    mkdirSync(repo);
    await run(['git', 'init', '--initial-branch=main'], repo, env);
    await run(['git', 'config', 'user.email', 'paths@example.test'], repo, env);
    await run(['git', 'config', 'user.name', 'Paths Test'], repo, env);
    const commits: Record<string, string> = {};
    for (const version of ['1.0.0', '2.0.0']) {
      writeFileSync(join(repo, 'source.rs'), version);
      await run(['git', 'add', '.'], repo, env);
      await run(['git', 'commit', '-m', version], repo, env);
      commits[version] = await run(['git', 'rev-parse', 'HEAD'], repo, env);
    }
    await run(['git', 'config', '--file', gitconfig, `url.${pathToFileURL(repo).href}.insteadOf`, REPOSITORY], repo, env);

    const packages = new Map<string, unknown>();
    const tarballs = new Map<string, Buffer>();
    const server = createServer((request, response) => {
      const path = decodeURIComponent((request.url ?? '/').split('?')[0].slice(1));
      const body = tarballs.get(path) ?? (packages.has(path) ? Buffer.from(JSON.stringify(packages.get(path))) : undefined);
      if (!body) { response.writeHead(404).end(); return; }
      response.writeHead(200, {
        'Content-Length': body.length,
        'Content-Type': packages.has(path) ? 'application/json' : 'application/octet-stream',
      }).end(body);
    });
    server.listen(0, '127.0.0.1');
    await once(server, 'listening');
    try {
      const address = server.address();
      assert.ok(address && typeof address !== 'string');
      const registry = `http://127.0.0.1:${address.port}`;
      const fixtures: Record<string, string[]> = { 'paths-fixture': ['1.0.0', '2.0.0'], '@fixture/parent': ['1.0.0'] };
      for (const [name, versions] of Object.entries(fixtures)) {
        const manifests: Record<string, Manifest> = {};
        for (const version of versions) {
          const manifest: Manifest = { name, version, repository: REPOSITORY, gitHead: commits[version] };
          if (name === '@fixture/parent') manifest.optionalDependencies = { 'paths-fixture': '1.0.0' };
          const fixture = join(temp, 'fixtures', name, version);
          writeJson(join(fixture, 'package.json'), manifest);
          const [packed]: Packed[] = JSON.parse(await run(['npm', 'pack', '--ignore-scripts', '--json'], fixture, env));
          const tarPath = `tarballs/${packed.filename}`;
          tarballs.set(tarPath, readFileSync(join(fixture, packed.filename)));
          manifest.dist = { tarball: `${registry}/${tarPath}`, shasum: packed.shasum };
          manifests[version] = manifest;
          packages.set(`${name}/${version}`, manifest);
        }
        packages.set(name, {
          name, 'dist-tags': { latest: versions.at(-1) }, versions: manifests,
          time: Object.fromEntries(versions.map((version) => [version, '2025-01-01T00:00:00.000Z'])),
        });
      }

      // Exercise npm's actual global bin shim without changing the user's installation.
      const [packed]: Packed[] = JSON.parse(await run(['npm', 'pack', '--json', '--pack-destination', temp], ROOT, env));
      const prefix = join(temp, 'global');
      await run(['npm', 'install', '--global', '--ignore-scripts', '--prefix', prefix, join(temp, packed.filename)], temp, env);
      const cli = process.platform === 'win32' ? join(prefix, 'paths.cmd') : join(prefix, 'bin', 'paths');
      const expected = JSON.parse(readFileSync(join(ROOT, 'package.json'), 'utf8')).version;
      assert.equal(await run([cli, '--version'], temp, env), `paths ${expected}`);
      assert.ok((await run([cli, '--help'], temp, env)).includes('--package-manager'));
      for (const manager of ['npm', 'pnpm'] as const) {
        await checkManager(manager, temp, env, registry, cli, commits);
      }
    } finally {
      server.closeAllConnections();
      await new Promise<void>((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
    }
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
  console.log('npm global installation and native launcher passed');
}

await main();
