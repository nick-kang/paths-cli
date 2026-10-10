// Integration test: real npm/pnpm/Cargo workspaces, local registry/Git, and global npm installation.
import assert from 'node:assert/strict';
import { once } from 'node:events';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
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
  result?: { name: string; version: string; method: string; commit?: string; repository?: string; path: string };
  error?: string;
};
type Packed = { filename: string; shasum: string };

async function run(args: string[], cwd: string, env: NodeJS.ProcessEnv, code = 0, stderrPattern?: RegExp): Promise<string> {
  const child = spawn(args[0], args.slice(1), {
    cwd, env, stdio: ['ignore', 'pipe', 'pipe'], timeout: 120_000,
  });
  let stdout = '';
  let stderr = '';
  child.stdout.setEncoding('utf8').on('data', (chunk: string) => { stdout += chunk; });
  child.stderr.setEncoding('utf8').on('data', (chunk: string) => { stderr += chunk; });
  const [status, signal] = await once(child, 'close');
  assert.equal(status, code, `${JSON.stringify(args)} exited ${status} (${signal})\n${stdout}\n${stderr}`);
  if (stderrPattern) assert.match(stderr, stderrPattern);
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
  if (!first && manager === 'npm') {
    console.error(await run(['npm', 'ls', '--all', '--long', '--json', '--include=dev', '--include=optional', '--workspaces', '--include-workspace-root'], project, env));
  }
  assert.ok(first, batch);
  assert.deepEqual(Object.keys(first).sort(), ['commit', 'method', 'name', 'path', 'repository', 'version']);
  assert.equal(first.version, '1.0.0');
  assert.equal(first.commit, commits['1.0.0']);
  assert.equal(first.repository, REPOSITORY);
  assert.equal(first.method, 'published');
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

function writeCrate(path: string, name: string, version: string, dependencies = ''): void {
  mkdirSync(join(path, 'src'), { recursive: true });
  writeFileSync(join(path, 'Cargo.toml'), `[package]\nname = ${JSON.stringify(name)}\nversion = ${JSON.stringify(version)}\nedition = "2021"\n\n[dependencies]\n${dependencies}`);
  writeFileSync(join(path, 'src', 'lib.rs'), '');
}

async function checkCargo(temp: string, env: NodeJS.ProcessEnv, registry: string, cli: string): Promise<void> {
  const project = join(temp, 'cargo');
  const cargoHome = join(temp, 'cargo-home');
  const cargoEnv = { ...env, CARGO_HOME: cargoHome, CARGO_NET_OFFLINE: 'true' };
  const deps = join(temp, 'cargo-deps');
  for (const [directory, name, version] of [
    ['shared-v1', 'shared', '1.0.0'], ['shared-v2', 'shared', '2.0.0'],
    ['optional-v1', 'optional-leaf', '1.0.0'], ['optional-v2', 'optional-leaf', '2.0.0'],
    ['transitive', 'transitive-leaf', '0.1.0'], ['b-only', 'b-only', '0.1.0'],
    ['paths-fixture', 'paths-fixture', '2.0.0'],
  ]) writeCrate(join(deps, directory), name, version);
  const pathDep = (directory: string) => `path = ${JSON.stringify(join(deps, directory))}`;
  writeCrate(join(deps, 'parent'), 'parent', '0.1.0', `transitive-leaf = { ${pathDep('transitive')} }\n`);
  const git = join(temp, 'cargo-git');
  writeCrate(git, 'git-fixture', '0.3.0');
  await run(['git', 'init', '--initial-branch=main'], git, env);
  await run(['git', 'add', '.'], git, env);
  await run(['git', '-c', 'user.name=Paths Test', '-c', 'user.email=paths@example.test', 'commit', '-m', 'fixture'], git, env);

  mkdirSync(project);
  writeFileSync(join(project, 'Cargo.toml'), '[workspace]\nmembers = ["apps/a", "apps/b", "apps/empty"]\ndefault-members = ["apps/b", "apps/a"]\nresolver = "2"\n');
  const a = join(project, 'apps', 'a');
  const b = join(project, 'apps', 'b');
  writeCrate(a, 'app-a', '0.1.0', [
    `parent = { ${pathDep('parent')} }`,
    `paths-fixture = { ${pathDep('paths-fixture')} }`,
    `renamed-crate = { package = "shared", ${pathDep('shared-v1')} }`,
    `new-shared = { package = "shared", ${pathDep('shared-v2')} }`,
    `optional-leaf = { ${pathDep('optional-v1')}, optional = true }`,
    `git-fixture = { git = ${JSON.stringify(pathToFileURL(git).href)} }`,
  ].join('\n'));
  writeCrate(b, 'app-b', '0.2.0', [
    `shared = { ${pathDep('shared-v2')} }`,
    `optional-leaf = { ${pathDep('optional-v2')} }`,
    `b-only = { ${pathDep('b-only')} }`,
  ].join('\n'));
  writeCrate(join(project, 'apps', 'empty'), 'app-empty', '0.1.0');
  // The only Git fetch is file://; subsequent CLI calls cannot use the public network.
  await run(['cargo', 'generate-lockfile'], project, { ...cargoEnv, CARGO_NET_OFFLINE: 'false' });
  const lockPath = join(project, 'Cargo.lock');
  const lock = readFileSync(lockPath, 'utf8');
  const cache = join(temp, 'cargo-paths-cache');
  const args = [cli, '--cache-dir', cache];
  const sharedV1 = join(deps, 'shared-v1');
  const sharedV2 = join(deps, 'shared-v2');
  const batchRequests = ['shared', 'not-installed', 'shared@2.0.0', 'shared@9.0.0', 'shared@^1.0.0', 'git-fixture'];
  const batch = await run([...args, ...batchRequests, '--json'], project, cargoEnv, 1);
  const results: Result[] = batch.split('\n').map((line) => JSON.parse(line));
  assert.deepEqual(results.map((result) => result.request), batchRequests);
  assert.equal(results[0].result?.path, sharedV1);
  assert.equal(results[2].result?.path, sharedV2);
  assert.ok(results[1].error);
  assert.ok(results[3].error);
  assert.ok(results[4].error);
  for (const result of results.flatMap((entry) => entry.result ? [entry.result] : [])) {
    assert.deepEqual(Object.keys(result).sort(), ['method', 'name', 'path', 'version']);
    assert.equal(result.method, 'cargo');
    assert.ok(existsSync(join(result.path, 'Cargo.toml')));
  }
  const gitResult = results.at(-1)?.result;
  assert.equal(gitResult?.version, '0.3.0');
  assert.ok(gitResult?.path.startsWith(join(cargoHome, 'git', 'checkouts')));
  assert.equal(await run([...args, 'renamed-crate'], a, cargoEnv), sharedV1);
  assert.equal(await run([...args, 'renamed_crate'], a, cargoEnv), sharedV1);
  assert.equal(await run([...args, 'transitive-leaf'], a, cargoEnv), join(deps, 'transitive'));
  assert.equal(await run([...args, 'optional-leaf'], a, cargoEnv), join(deps, 'optional-v1'));
  assert.equal(await run([...args, 'shared'], b, cargoEnv), sharedV2);
  assert.equal(await run([...args, 'app-b'], b, cargoEnv), b);
  assert.equal(await run([...args, '--filter', 'app-b', 'shared'], project, cargoEnv), sharedV2);
  assert.equal(await run([...args, '--filter', 'apps/b', 'shared'], join(a, 'src'), cargoEnv), sharedV2);
  assert.equal(await run([...args, '--filter', 'app-empty', 'shared'], project, cargoEnv, 0, /warning:/), sharedV1);
  assert.equal(await run([...args, 'b-only'], a, cargoEnv, 0, /warning:/), join(deps, 'b-only'));
  const invalidFilter: Result = JSON.parse(await run([...args, '--filter', 'missing', 'shared', '--json'], project, cargoEnv, 1));
  assert.ok(invalidFilter.error);
  assert.equal(readFileSync(lockPath, 'utf8'), lock);
  assert.equal(existsSync(cache), false);
  const invalidCache = join(temp, 'cargo-cache-file');
  writeFileSync(invalidCache, 'untouched');
  assert.equal(await run([cli, '--cache-dir', invalidCache, 'app-a'], a, cargoEnv), a);
  assert.equal(readFileSync(invalidCache, 'utf8'), 'untouched');

  for (const manager of ['npm', 'pnpm']) {
    const manifestPath = join(project, 'package.json');
    const manifest = {
      name: 'mixed-project', version: '1.0.0', packageManager: `${manager}@${await run([manager, '--version'], project, env)}`,
      dependencies: { 'paths-fixture': '1.0.0', 'node-only': 'npm:paths-fixture@1.0.0', 'renamed-crate': 'npm:paths-fixture@1.0.0' },
    };
    writeJson(manifestPath, manifest);
    writeFileSync(join(project, '.npmrc'), `registry=${registry}\nfetch-retries=0\naudit=false\nfund=false\nminimum-release-age=0\n`);
    const install = [manager, 'install', '--ignore-scripts'];
    if (manager === 'pnpm') install.push('--no-frozen-lockfile');
    await run(install, project, env);
    const mixedCache = join(temp, `mixed-${manager}-cache`);
    const mixedArgs = [cli, '--cache-dir', mixedCache];
    const collision: Result = JSON.parse(await run([...mixedArgs, 'paths-fixture', '--json'], project, cargoEnv, 1, /--package-manager/));
    assert.match(collision.error ?? '', /found in both Node and Cargo/);
    await run([...mixedArgs, 'renamed-crate@1.0.0'], project, cargoEnv, 1, /found in both Node and Cargo/);
    assert.equal(existsSync(mixedCache), false);
    assert.equal(await run([...mixedArgs, 'shared'], project, cargoEnv), sharedV1);
    assert.equal(await run([cli, '--cache-dir', invalidCache, 'shared'], project, cargoEnv), sharedV1);
    assert.equal(existsSync(mixedCache), false);
    assert.equal(await run([...mixedArgs, 'shared'], b, cargoEnv), sharedV2);
    assert.equal(await run([...mixedArgs, '--filter', 'app-b', 'shared'], project, cargoEnv, 0, /cannot inspect Node/), sharedV2);
    assert.equal(await run([...mixedArgs, 'paths-fixture@2.0.0'], project, cargoEnv), join(deps, 'paths-fixture'));
    assert.equal(existsSync(mixedCache), false);

    const requests = ['shared', 'paths-fixture', 'node-only', 'not-installed', 'paths-fixture@9.0.0', 'shared'];
    const mixed: Result[] = (await run([...mixedArgs, ...requests, '--json'], project, cargoEnv, 1)).split('\n').map((line) => JSON.parse(line));
    assert.deepEqual(mixed.map((entry) => entry.request), requests);
    assert.equal(mixed[0].result?.method, 'cargo');
    assert.match(mixed[1].error ?? '', /--package-manager/);
    const node = mixed[2].result;
    assert.ok(node);
    assert.equal(node.name, 'paths-fixture');
    assert.equal(node.method, 'published');
    assert.deepEqual(Object.keys(node).sort(), ['commit', 'method', 'name', 'path', 'repository', 'version']);
    assert.match(mixed[3].error ?? '', /not found in Node or Cargo/);
    assert.match(mixed[4].error ?? '', /not found in Node or Cargo/);
    assert.equal(mixed[5].result?.path, sharedV1);
    assert.equal(await run([...mixedArgs, 'paths-fixture@1.0.0'], project, cargoEnv), node.path);
    assert.equal(await run([...mixedArgs, '--package-manager', manager, 'paths-fixture'], project, cargoEnv), node.path);
    assert.equal(await run([...mixedArgs, '--package-manager', 'cargo', 'paths-fixture'], project, cargoEnv), join(deps, 'paths-fixture'));

    // A broken source URL must not hide a collision or trigger a checkout.
    const installedPath = realpathSync.native(join(project, 'node_modules', 'paths-fixture', 'package.json'));
    const installed = readFileSync(installedPath, 'utf8');
    writeJson(installedPath, { ...JSON.parse(installed), repository: 'https://invalid.invalid/owner/repo' });
    await run([cli, '--cache-dir', invalidCache, 'paths-fixture'], project, cargoEnv, 1, /found in both Node and Cargo/);
    writeFileSync(installedPath, installed);

    rmSync(lockPath);
    assert.equal(await run([...mixedArgs, 'node-only'], project, cargoEnv, 0, /warning: cannot inspect Cargo.*cargo fetch/s), node.path);
    await run([...mixedArgs, 'not-installed'], project, cargoEnv, 1, /Node: not found; Cargo:.*cargo fetch/s);
    assert.equal(existsSync(lockPath), false);
    writeFileSync(lockPath, lock);
    writeJson(manifestPath, { ...manifest, packageManager: 'yarn@4.0.0' });
    assert.equal(await run([...mixedArgs, 'shared'], project, cargoEnv, 0, /warning: cannot inspect Node/), sharedV1);
    rmSync(manifestPath);
    rmSync(join(project, 'node_modules'), { recursive: true });
    for (const filename of ['package-lock.json', 'pnpm-lock.yaml']) rmSync(join(project, filename), { force: true });
    console.log(`${manager}/Cargo: unique matches, collisions, overrides, exact versions, batch errors, and cache bypass passed`);
  }
  assert.equal(readFileSync(lockPath, 'utf8'), lock);

  rmSync(lockPath);
  await run([...args, 'app-a'], a, cargoEnv, 1, /cargo fetch/);
  assert.equal(existsSync(lockPath), false);
  writeFileSync(lockPath, lock);
  const manifestPath = join(a, 'Cargo.toml');
  const manifest = readFileSync(manifestPath, 'utf8');
  writeFileSync(manifestPath, manifest.replace('version = "0.1.0"', 'version = "0.1.1"'));
  await run([...args, 'app-a'], a, cargoEnv, 1, /cargo fetch/);
  assert.equal(readFileSync(lockPath, 'utf8'), lock);
  assert.equal(existsSync(cache), false);
  console.log('Cargo: local Git/path sources, versions, aliases, features, workspaces, JSON, lock preservation, and cache bypass passed');
}

async function main(): Promise<void> {
  assert.ok(Number(process.versions.node.split('.')[0]) >= 26, 'Run the integration test with Node.js 26+');
  // Match Rust's canonical paths, including expansion of Windows 8.3 aliases.
  const temp = realpathSync.native(mkdtempSync(join(tmpdir(), 'paths-smoke-')));
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
      await checkCargo(temp, env, registry, cli);
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
