// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const {spawnSync} = require('node:child_process');

const REPOSITORY = 'eisber/fabric-cli';
const BRANCH = 'dev/marcozo/fab-cli-v2-integration-20261104';
const ANCHOR = '1c3c819c3c4162af13d23b4dd3aa475b06b16526';
const WORKFLOW = '.github/workflows/integration-smoke-recovery.yml';
const SHA = /^[a-f0-9]{40}$/;
const PROFILES = [{bin: 'fab-cli-v2', features: ['headless']}, {bin: 'fab-rs', features: []}];
const digest = value => crypto.createHash('sha256').update(value).digest('hex');
const writeJson = (file, value) => fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);

function requireSha(value) {
  assert.equal(typeof value, 'string', 'Revision must be a full lowercase 40-hex SHA');
  assert.match(value, SHA, 'Revision must be a full lowercase 40-hex SHA');
  assert.notEqual(value, '0'.repeat(40), 'Zero revision is not a commit');
  return value;
}

function manualContext(env, event, driverHead) {
  assert.equal(env.GITHUB_EVENT_NAME, 'workflow_dispatch', 'Only a genuine manual event is accepted');
  assert.equal(env.GITHUB_REPOSITORY, REPOSITORY);
  assert.equal(event.repository?.full_name, REPOSITORY);
  assert.equal(event.repository?.default_branch, 'main');
  assert.equal(env.GITHUB_REF, 'refs/heads/main', 'Register and dispatch the driver on default main');
  assert.equal(env.GITHUB_WORKFLOW_REF, `${REPOSITORY}/${WORKFLOW}@refs/heads/main`);
  assert.equal(env.GITHUB_SERVER_URL, 'https://github.com');
  assert.equal(env.GITHUB_API_URL, 'https://api.github.com');
  requireSha(env.GITHUB_SHA);
  assert.equal(driverHead, env.GITHUB_SHA, 'Driver checkout must equal the dispatch revision');
  assert.equal(env.GITHUB_WORKFLOW_SHA, driverHead, 'Workflow and driver revisions must agree');
  const source = requireSha(env.REQUESTED_SOURCE_SHA);
  const base = requireSha(env.REQUESTED_BASE_SHA);
  assert.notEqual(source, base, 'Source and base must differ');
  assert.equal(event.inputs?.source_sha, source, 'Source must match the real dispatch input');
  assert.equal(event.inputs?.base_sha, base, 'Base must match the real dispatch input');
  return {event: 'workflow_dispatch', ref: env.GITHUB_REF, requested_source: source,
    requested_base: base, driver: driverHead, workflow_ref: env.GITHUB_WORKFLOW_REF,
    workflow_sha: env.GITHUB_WORKFLOW_SHA, run_id: env.GITHUB_RUN_ID,
    run_attempt: env.GITHUB_RUN_ATTEMPT};
}

async function resolveHead(token, request = fetch) {
  assert.ok(token, 'A read-only repository token is required to resolve the integration head');
  const response = await request(
    `https://api.github.com/repos/${REPOSITORY}/branches/${encodeURIComponent(BRANCH)}`,
    {headers: {Authorization: `Bearer ${token}`, Accept: 'application/vnd.github+json',
      'X-GitHub-Api-Version': '2022-11-28'}, signal: AbortSignal.timeout(30000)});
  assert.equal(response.status, 200, `Integration head lookup failed (HTTP ${response.status})`);
  const branch = await response.json();
  assert.equal(branch.name, BRANCH, 'Provider returned the wrong branch');
  return requireSha(branch.commit?.sha);
}

function manualPin(context, resolvedHead, actual, parents, anchorIsAncestor) {
  assert.equal(context.event, 'workflow_dispatch');
  requireSha(context.requested_source);
  requireSha(context.requested_base);
  assert.equal(requireSha(resolvedHead), context.requested_source,
    'Requested source must equal the current fixed integration branch head');
  assert.equal(actual, context.requested_source, 'Checkout must equal the requested integration SHA');
  assert.equal(parents.length, 2, 'Recovery is limited to an integration merge commit');
  parents.forEach(requireSha);
  assert.equal(parents[0], context.requested_base, 'Base must equal the exact first merge parent');
  assert.notEqual(parents[0], actual);
  assert.equal(anchorIsAncestor, true, 'Integration history must retain the approved recovery anchor');
  return {...context, integration_ref: `refs/heads/${BRANCH}`, resolved_head: resolvedHead,
    source: actual, base: parents[0], checkout: actual, parents, anchor: ANCHOR};
}

function git(root, args) {
  const result = spawnSync('git', args, {cwd: root, encoding: 'utf8', shell: false,
    windowsHide: true, timeout: 30000});
  assert.ifError(result.error);
  assert.equal(result.signal, null, 'Git was terminated');
  assert.equal(result.status, 0, `Git failed: ${args.join(' ')}\n${result.stderr}`);
  return result.stdout.trim();
}

function readContext(driver, env) {
  assert.equal(git(driver, ['status', '--porcelain']), '', 'Driver checkout must be clean');
  return manualContext(env, JSON.parse(fs.readFileSync(env.GITHUB_EVENT_PATH, 'utf8')),
    git(driver, ['rev-parse', 'HEAD']));
}

function externalDirectory(value, roots) {
  assert.ok(value && path.isAbsolute(value), 'An absolute external directory is required');
  const directory = path.resolve(value);
  for (const root of roots) {
    const relative = path.relative(root, directory);
    assert.ok(relative && (relative === '..' || relative.startsWith(`..${path.sep}`) ||
      path.isAbsolute(relative)), 'Artifacts and build output must be outside both checkouts');
  }
  return directory;
}

async function preflight(driver, env) {
  const artifacts = externalDirectory(env.RECOVERY_ARTIFACTS, [driver,
    path.join(path.dirname(driver), 'source')]);
  fs.mkdirSync(artifacts, {recursive: false});
  const receipt = {schema: 1, passed: false, event: env.GITHUB_EVENT_NAME};
  try {
    Object.assign(receipt, readContext(driver, env));
    receipt.resolved_head = await resolveHead(env.GH_TOKEN);
    assert.equal(receipt.resolved_head, receipt.requested_source,
      'Requested source must equal the current fixed integration branch head');
    fs.appendFileSync(env.GITHUB_OUTPUT, `source_sha=${receipt.requested_source}\n`);
    receipt.passed = true;
  } catch (error) {
    receipt.error = error.message;
    throw error;
  } finally {
    writeJson(path.join(artifacts, 'preflight.json'), receipt);
  }
}

async function runSmoke(driver, env) {
  const root = fs.realpathSync(path.join(path.dirname(driver), 'source'));
  const artifacts = externalDirectory(env.RECOVERY_ARTIFACTS, [driver, root]);
  const output = path.join(artifacts, 'execution');
  fs.mkdirSync(output, {recursive: false});
  const started = Date.now();
  const report = {schema: 1, passed: false, event: env.GITHUB_EVENT_NAME,
    qualification: 'manual exact-revision provisional integration smoke only',
    commands: [], profiles: PROFILES, run_id: env.GITHUB_RUN_ID,
    run_attempt: env.GITHUB_RUN_ATTEMPT, image: env.ImageVersion || null};
  const save = () => writeJson(path.join(output, 'receipt.json'), report);
  let before;
  let inventory;
  let smoke;
  try {
    save();
    assert.equal(process.platform, 'win32', 'Native smoke is hosted Windows only');
    assert.equal(env.RUNNER_ENVIRONMENT, 'github-hosted', 'No shared local native builds');
    assert.equal(env.RUNNER_OS, 'Windows');
    const context = readContext(driver, env);
    const preflightReceipt = JSON.parse(fs.readFileSync(path.join(artifacts, 'preflight.json'), 'utf8'));
    assert.equal(preflightReceipt.passed, true, 'A successful preflight is required');
    for (const [key, value] of Object.entries(context)) {
      assert.equal(preflightReceipt[key], value, `Preflight provenance changed: ${key}`);
    }
    const resolved = await resolveHead(env.GH_TOKEN);
    const actual = git(root, ['rev-parse', 'HEAD']);
    const parents = git(root, ['show', '-s', '--format=%P', 'HEAD']).split(' ');
    git(root, ['merge-base', '--is-ancestor', ANCHOR, actual]);
    report.source = manualPin(context, resolved, actual, parents, true);
    assert.equal(git(root, ['status', '--porcelain']), '', 'Source checkout must be clean');
    // Load only after authenticating the fixed repository, head, checkout and ancestry.
    smoke = require(path.join(root, 'rust/scripts/integration-smoke.cjs'));
    inventory = require(path.join(root, 'rust/scripts/validate-impact.cjs'));
    assert.equal(smoke.BRANCH, BRANCH);
    assert.deepEqual(smoke.PROFILES, PROFILES, 'Do not broaden or reduce the two-profile minimum');
    const persist = (entry, result) => {
      const prefix = String(report.commands.length).padStart(2, '0');
      for (const stream of ['stdout', 'stderr']) {
        fs.writeFileSync(path.join(output, `${prefix}-${stream}.log`), result[stream] || '');
      }
      save();
    };
    const buildEnv = {...env};
    delete buildEnv.GH_TOKEN;
    delete buildEnv.GITHUB_TOKEN;
    const run = (file, args, options = {}) => smoke.capture(file, args,
      {cwd: path.join(root, 'rust'), env: buildEnv, timeout: 30000, ...options}, report, persist);
    before = inventory.sourceInventory(root);
    writeJson(path.join(output, 'source-before.json'), before);
    report.driver_files = Object.fromEntries([WORKFLOW, '.github/scripts/integration-smoke-recovery.cjs']
      .map(file => [file, digest(fs.readFileSync(path.join(driver, file)))]));
    const contracts = run(process.execPath, ['--test', 'rust/scripts/integration-smoke.test.cjs',
      'rust/scripts/integration-notebook-smoke.test.cjs'], {cwd: root, timeout: 60000});
    assert.equal(contracts.status, 0, 'Existing smoke and routing contracts must pass unchanged');
    report.lockfile_sha256 = digest(fs.readFileSync(path.join(root, 'rust/Cargo.lock')));
    const manifest = fs.readFileSync(path.join(root, 'rust/Cargo.toml'), 'utf8');
    const version = manifest.match(/^version\s*=\s*"([^"]+)"$/m)?.[1];
    assert.ok(version, 'Workspace version is required');
    const toolchain = fs.readFileSync(path.join(root, 'rust/rust-toolchain.toml'), 'utf8')
      .match(/^channel\s*=\s*"([^"]+)"$/m)?.[1];
    assert.ok(toolchain, 'Pinned toolchain is required');
    const target = externalDirectory(env.CARGO_TARGET_DIR, [driver, root]);
    assert.ok(!fs.existsSync(target), 'Require a fresh external hosted target');
    report.target_cache = 'cold; no cross-run target cache';
    const rustc = run('rustc', ['--version', '--verbose']);
    const cargo = run('cargo', ['--version']);
    assert.equal(rustc.status, 0);
    assert.equal(cargo.status, 0);
    assert.ok(rustc.stdout.startsWith(`rustc ${toolchain} `));
    assert.ok(cargo.stdout.startsWith(`cargo ${toolchain} `));
    assert.match(rustc.stdout, /^host: x86_64-pc-windows-msvc\r?$/m);
    report.toolchain = {pin: toolchain, rustc: rustc.stdout, cargo: cargo.stdout};
    const sandbox = path.join(output, 'sandbox');
    const home = path.join(sandbox, 'home');
    fs.mkdirSync(home, {recursive: true});
    const isolated = smoke.isolatedEnvironment(home, sandbox);
    report.binaries = [];
    for (const [index, profile] of PROFILES.entries()) {
      const build = run('cargo', smoke.buildArgs(profile, index !== 0), {timeout: 20 * 60 * 1000});
      assert.equal(build.status, 0, `Build failed for ${profile.bin}`);
      const executable = path.join(target, 'debug', `${profile.bin}.exe`);
      const hash = digest(fs.readFileSync(executable));
      report.binaries.push({bin: profile.bin, sha256: hash});
      for (const item of smoke.smokeCases(profile.bin)) {
        const result = run(executable, item.args, {cwd: sandbox, env: isolated, timeout: 10000});
        smoke.verifyOutput(profile.bin, version, item, result);
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
        const after = inventory.sourceInventory(root);
        writeJson(path.join(output, 'source-after.json'), after);
        writeJson(path.join(output, 'source-differences.json'), inventory.sourceDifferences(before, after));
        smoke.requireUnchanged(before, after);
        assert.equal(git(root, ['rev-parse', 'HEAD']), report.source.checkout);
        assert.equal(git(root, ['status', '--porcelain']), '', 'Source checkout must remain clean');
        readContext(driver, env);
      }
    } catch (error) {
      report.passed = false;
      report.integrity_error = error.message;
      throw error;
    } finally {
      report.elapsed_seconds = (Date.now() - started) / 1000;
      save();
      if (env.GITHUB_STEP_SUMMARY) {
        fs.appendFileSync(env.GITHUB_STEP_SUMMARY,
          `## Manual exact-revision integration smoke\n\nPassed: ${report.passed}\n\n` +
          `Event/source/base/checkout/driver: ${JSON.stringify(report.source || null)}\n\n` +
          'Two debug bin profiles only; no optional-feature, live-resource or merge-queue qualification.\n');
      }
    }
  }
}

module.exports = {REPOSITORY, BRANCH, ANCHOR, WORKFLOW, PROFILES,
  requireSha, manualContext, resolveHead, manualPin, externalDirectory, preflight, runSmoke};
if (require.main === module) {
  const driver = fs.realpathSync(path.resolve(__dirname, '../..'));
  Promise.resolve().then(() => {
    assert.equal(process.argv.length, 3);
    if (process.argv[2] === 'preflight') return preflight(driver, process.env);
    assert.equal(process.argv[2], 'run', 'Expected preflight or run');
    return runSmoke(driver, process.env);
  }).catch(error => { console.error(error.stack); process.exitCode = 1; });
}
