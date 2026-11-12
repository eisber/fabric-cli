// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const {spawnSync} = require('node:child_process');
const {sourceInventory, sourceDifferences} = require('./validate-impact.cjs');

const BRANCH = 'dev/marcozo/fab-cli-v2-integration-20261104';
const SHA = /^[a-f0-9]{40}$/;
const PROFILES = [
  {bin: 'fab-cli-v2', features: ['headless']},
  {bin: 'fab-rs', features: []},
];
const digest = value => crypto.createHash('sha256').update(value).digest('hex');

function integrationEvent(name, event, ref) {
  return (name === 'pull_request' && event.pull_request?.base?.ref === BRANCH) ||
    (name === 'push' && ref === `refs/heads/${BRANCH}`);
}

function sourcePin(name, event, ref, expected, actual, parents, baseIsAncestor) {
  assert.ok(integrationEvent(name, event, ref), 'Not an integration PR or push');
  assert.equal(event.repository?.full_name, 'eisber/fabric-cli');
  assert.match(expected, SHA);
  assert.equal(actual, expected, 'Checkout must equal the exact event commit');
  const source = name === 'pull_request' ? event.pull_request.head.sha : event.after;
  const base = name === 'pull_request' ? event.pull_request.base.sha : event.before;
  assert.match(source, SHA);
  assert.match(base, SHA);
  assert.notEqual(base, '0'.repeat(40), 'A newly created branch has no validated base');
  if (name === 'pull_request') {
    assert.deepEqual(parents, [base, source], 'PR merge must combine exactly the recorded base and source');
  } else {
    assert.equal(source, expected, 'Push source must equal checkout');
    assert.equal(event.deleted, false);
    assert.equal(baseIsAncestor, true, 'Push must preserve the previous integration ancestry');
  }
  return {event: name, ref, source, base, checkout: actual, parents};
}

function buildArgs(profile, offline) {
  return ['build', '--locked', ...(offline ? ['--offline'] : []),
    '-p', 'fabric-cli', '--bin', profile.bin, '--no-default-features',
    ...(profile.features.length ? ['--features', profile.features.join(',')] : [])];
}

function smokeCases(bin) {
  return [
    {args: ['--help'], kind: 'help', exit: 0},
    {args: ['--version'], kind: 'version', exit: 0},
    ...(bin === 'fab-rs'
      ? [{args: ['--client-secret', 'must-not-be-echoed'], kind: 'baseline-error', exit: 2}]
      : [{args: ['get', 'must-not-be-echoed'], kind: 'v2-error', exit: 2}]),
  ];
}

function verifyOutput(bin, version, item, result) {
  assert.equal(result.status, item.exit, `${bin} ${item.kind}: unexpected exit`);
  assert.equal(result.signal, null, 'Smoke process must exit normally');
  assert.equal(result.error, undefined, 'Smoke process must start and finish within its deadline');
  if (item.kind === 'help') {
    assert.equal(result.stderr, '');
    assert.ok(result.stdout.includes(`Usage: ${bin}`));
    if (bin === 'fab-rs') {
      for (const text of ['Commands:', '--help', '--version', 'ls', 'get']) {
        assert.ok(result.stdout.includes(text), `Missing help field ${text}`);
      }
    } else {
      for (const text of ['experimental explicit resource head', 'get <path>', 'search <scope>',
        'exists <path>', 'no identity fallback or interactive sign-in']) {
        assert.ok(result.stdout.includes(text), `Missing headless help field ${text}`);
      }
    }
  } else if (item.kind === 'version') {
    assert.equal(result.stderr, '');
    assert.equal(result.stdout, `${bin} ${version}\n`);
  } else {
    assert.equal(result.stdout, '');
    assert.ok(!result.stderr.includes('must-not-be-echoed'));
    const value = JSON.parse(result.stderr);
    if (item.kind === 'baseline-error') {
      assert.ok(result.stderr.endsWith('\n'));
      assert.deepEqual(value, {error: {code: 'invalid_arguments',
        message: 'Invalid arguments. Run fab-rs --help for available commands and usage.'}});
    } else {
      assert.deepEqual(Object.keys(value).sort(), ['error', 'evidence']);
      assert.deepEqual(Object.keys(value.error).sort(), ['code', 'message']);
      assert.equal(value.error.code, 'InvalidTarget');
      assert.equal(value.error.message, 'get requires an absolute typed resource or returned-handle path');
      assert.equal(value.evidence, null);
    }
  }
}

function isolatedEnvironment(home, sandbox, ambient = process.env) {
  const env = {HOME: home, USERPROFILE: home, XDG_CONFIG_HOME: home, XDG_CACHE_HOME: home,
    APPDATA: home, LOCALAPPDATA: home, TEMP: sandbox, TMP: sandbox, TMPDIR: sandbox,
    NO_COLOR: '1', PATH: ''};
  for (const name of ['SystemRoot', 'WINDIR']) {
    if (ambient[name]) env[name] = ambient[name];
  }
  return env;
}

function capture(file, args, options, report, persist, runner = spawnSync) {
  const started = Date.now();
  const result = runner(file, args, {...options, encoding: 'utf8', shell: false,
    windowsHide: true, maxBuffer: 32 * 1024 * 1024});
  const entry = {file, args, exit: result.status, signal: result.signal,
    error: result.error?.message || null, elapsed_seconds: (Date.now() - started) / 1000,
    stdout_sha256: digest(result.stdout || ''), stderr_sha256: digest(result.stderr || '')};
  report.commands.push(entry);
  persist(entry, result);
  assert.equal(result.error, undefined, `${file} failed to execute: ${entry.error}`);
  assert.equal(result.signal, null, `${file} was terminated`);
  return result;
}

function requireUnchanged(before, after) {
  assert.equal(after.digest, before.digest, 'Source bytes changed during smoke');
  assert.deepEqual(sourceDifferences(before, after), [], 'Source inventory changed during smoke');
}

function main(output) {
  const root = fs.realpathSync(path.resolve(__dirname, '../..'));
  assert.ok(output && path.isAbsolute(output), 'An absolute external artifact directory is required');
  const artifacts = path.resolve(output);
  assert.ok(artifacts !== root && !artifacts.startsWith(`${root}${path.sep}`), 'Artifacts must be external');
  fs.mkdirSync(artifacts, {recursive: false});
  const started = Date.now();
  const report = {schema: 1, passed: false, qualification: 'provisional integration smoke only',
    commands: [], profiles: PROFILES, run_id: process.env.GITHUB_RUN_ID,
    run_attempt: process.env.GITHUB_RUN_ATTEMPT, image: process.env.ImageVersion || null};
  const save = () => fs.writeFileSync(path.join(artifacts, 'receipt.json'), `${JSON.stringify(report, null, 2)}\n`);
  const persist = (entry, result) => {
    const prefix = String(report.commands.length).padStart(2, '0');
    for (const stream of ['stdout', 'stderr']) {
      fs.writeFileSync(path.join(artifacts, `${prefix}-${stream}.log`), result[stream] || '');
    }
    save();
  };
  const run = (file, args, options = {}) => capture(file, args,
    {cwd: path.join(root, 'rust'), timeout: 30000, ...options}, report, persist);
  const git = args => {
    const result = run('git', args, {cwd: root});
    assert.equal(result.status, 0, `Git failed: ${args.join(' ')}`);
    return result.stdout.trim();
  };
  let before;
  try {
    save();
    assert.equal(process.platform, 'win32', 'Native smoke is hosted Windows only');
    assert.equal(process.env.RUNNER_ENVIRONMENT, 'github-hosted', 'No shared local native builds');
    assert.equal(process.env.RUNNER_OS, 'Windows');
    const event = JSON.parse(fs.readFileSync(process.env.GITHUB_EVENT_PATH, 'utf8'));
    const name = process.env.GITHUB_EVENT_NAME;
    const actual = git(['rev-parse', 'HEAD']);
    const parents = git(['show', '-s', '--format=%P', 'HEAD']).split(' ');
    const ancestor = name === 'push'
      ? run('git', ['merge-base', '--is-ancestor', event.before, actual], {cwd: root}).status === 0 : false;
    report.source = sourcePin(name, event, process.env.GITHUB_REF, process.env.GITHUB_SHA, actual, parents, ancestor);
    assert.equal(git(['status', '--porcelain']), '', 'Checkout must be clean');
    before = sourceInventory(root);
    fs.writeFileSync(path.join(artifacts, 'source-before.json'), JSON.stringify(before, null, 2));
    report.lockfile_sha256 = digest(fs.readFileSync(path.join(root, 'rust/Cargo.lock')));
    const manifest = fs.readFileSync(path.join(root, 'rust/Cargo.toml'), 'utf8');
    const version = manifest.match(/^version\s*=\s*"([^"]+)"$/m)?.[1];
    assert.ok(version, 'Workspace version is required');
    const toolchain = fs.readFileSync(path.join(root, 'rust/rust-toolchain.toml'), 'utf8')
      .match(/^channel\s*=\s*"([^"]+)"$/m)?.[1];
    assert.ok(toolchain, 'Pinned toolchain is required');
    const target = process.env.CARGO_TARGET_DIR;
    assert.ok(target && path.isAbsolute(target) && !fs.existsSync(target), 'Require a fresh external hosted target');
    assert.ok(!path.resolve(target).startsWith(`${root}${path.sep}`), 'Target must be external');
    report.target_cache = 'cold; no cross-run target cache';
    const rustc = run('rustc', ['--version', '--verbose']);
    const cargo = run('cargo', ['--version']);
    assert.equal(rustc.status, 0);
    assert.equal(cargo.status, 0);
    assert.ok(rustc.stdout.startsWith(`rustc ${toolchain} `));
    assert.ok(cargo.stdout.startsWith(`cargo ${toolchain} `));
    assert.match(rustc.stdout, /^host: x86_64-pc-windows-msvc\r?$/m);
    report.toolchain = {pin: toolchain, rustc: rustc.stdout, cargo: cargo.stdout};
    const sandbox = path.join(artifacts, 'sandbox');
    const home = path.join(sandbox, 'home');
    fs.mkdirSync(home, {recursive: true});
    const env = isolatedEnvironment(home, sandbox);
    report.binaries = [];
    for (const [index, profile] of PROFILES.entries()) {
      // Headless resolves the superset first; baseline can then build offline in the shared target.
      const build = run('cargo', buildArgs(profile, index !== 0), {timeout: 20 * 60 * 1000});
      assert.equal(build.status, 0, `Build failed for ${profile.bin}`);
      const executable = path.join(target, 'debug', `${profile.bin}.exe`);
      const hash = digest(fs.readFileSync(executable));
      report.binaries.push({bin: profile.bin, sha256: hash});
      for (const item of smokeCases(profile.bin)) {
        const result = run(executable, item.args, {cwd: sandbox, env, timeout: 10000});
        verifyOutput(profile.bin, version, item, result);
      }
      assert.equal(digest(fs.readFileSync(executable)), hash, 'Smoke modified its executable');
    }
    report.passed = true;
  } catch (error) {
    report.error = error.message;
    throw error;
  } finally {
    try {
      if (before) {
        const after = sourceInventory(root);
        fs.writeFileSync(path.join(artifacts, 'source-after.json'), JSON.stringify(after, null, 2));
        fs.writeFileSync(path.join(artifacts, 'source-differences.json'),
          JSON.stringify(sourceDifferences(before, after), null, 2));
        requireUnchanged(before, after);
      }
    } catch (error) {
      report.passed = false;
      report.integrity_error = error.message;
      throw error;
    } finally {
      report.elapsed_seconds = (Date.now() - started) / 1000;
      save();
      if (process.env.GITHUB_STEP_SUMMARY) {
        fs.appendFileSync(process.env.GITHUB_STEP_SUMMARY,
          `## Provisional integration smoke\n\nPassed: ${report.passed}; elapsed: ${report.elapsed_seconds}s.\n\n` +
          `Source/base/checkout: ${JSON.stringify(report.source || null)}\n\n` +
          'Two debug bin profiles; no full suites, optional-feature qualification, live identity or network requests tested.\n');
      }
    }
  }
}

module.exports = {BRANCH, PROFILES, integrationEvent, sourcePin, buildArgs, smokeCases,
  verifyOutput, isolatedEnvironment, capture, requireUnchanged};
if (require.main === module) {
  try { assert.equal(process.argv.length, 3); main(process.argv[2]); }
  catch (error) { console.error(error.stack); process.exitCode = 1; }
}
