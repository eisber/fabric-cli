// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

/**
 * Non-executing post-hoc request facade. No native admission or native pass is
 * possible here, including when every source guard passes. Exit 78 means a
 * prepared but NOT_ADMITTED request, not a successful validation run.
 *
 * Activation prerequisites (separate ownership/authorization, not this patch):
 * 1. Publish/register this path on default main; dispatch there with full source
 *    and base SHAs. Batch dispatch coalesces a named batch without a timer.
 * 2. Add explicit-source/platform APIs in rust/scripts/impact-ci.cjs:
 *    selectRequest({root, head, base, full, platforms}) must verify the request,
 *    call the existing impact-plan CLI, and retain the original graph/consumer/
 *    feature/test selection. nativePoolsForMatrix currently requires all three
 *    platforms. aggregate currently reconstructs all three platforms too; add a
 *    validated platform domain to BOTH paths, without dropping any obligations
 *    inside that domain. Reuse equalSelection, verifyNativePool, selectShard,
 *    verifyManifest and aggregate rather than implementing new test oracles.
 * 3. Wire native ubuntu-24.04 and macos-15 jobs to separate immutable DUT and
 *    trusted-driver checkouts. Do NOT call rust-native-shard.yml unchanged: its
 *    default checkout is the event ref (main), not the DUT. Either give that
 *    workflow an explicit verified source-request API, or own a narrow caller
 *    that invokes the existing source-rooted CLI. Never spoof GITHUB_SHA/event.
 *    validate-impact.cjs already supports --root; its CLI defaults exceed the
 *    native job contract, so supply explicit <=600s command/<=720s total limits,
 *    inside a 15-minute job. Reuse validation-process.executeOwned, per-file
 *    source inventories and evidence aggregation, including failure receipts.
 * 4. Authorize a finite, trusted workflow-code resource policy: platforms,
 *    shards, max-parallel, test count, bytes and full-baseline budget. Dispatch
 *    text is NOT a resource grant. Validate/cap the real plan before launching.
 *    No implicit full fallback on invalid/unknown inputs. Affected admission
 *    needs actual complete native receipts at base, matching platform/arch,
 *    toolchain, selection/workload and source inventory. A Windows parent is
 *    not that proof. Preserve remaining debt for any explicitly scoped pilot.
 * 5. Only after an admitted exact-source native execution and complete,
 *    nonempty, nonignored test/command evidence may an external consumer advance
 *    a platform/workload watermark. Missing/stale/duplicate/skipped/partial,
 *    failed, canceled or source-mutated receipts cannot pass. Acquisition
 *    failure needs actual provider acquisition evidence, never job duration.
 *
 * A future scheduled route requires separate publication/enablement, a trusted
 * finite policy, its own genuine schedule context validator and immutable source
 * resolution. There is intentionally NO schedule or native execution route now.
 */
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {spawnSync} = require('node:child_process');
const GIT_NULL = process.platform === 'win32' ? 'NUL' : os.devNull;

const REPOSITORY = 'eisber/fabric-cli';
const BRANCH = 'dev/marcozo/fab-cli-v2-integration-20261104';
const MAIN_BASE = '9ce72b0e0997f9310e1a25b80fc827a4928742dd';
const ANCHOR = '1c3c819c3c4162af13d23b4dd3aa475b06b16526';
const REFERENCE_SOURCE = '313271f4104a562d853318503f8e14f552ac6dc2';
const WORKFLOW = '.github/workflows/rust-native-posthoc.yml';
const LIMITS = Object.freeze({files: 20000, fileBytes: 16 * 1024 ** 2,
  sourceBytes: 256 * 1024 ** 2, artifactBytes: 4 * 1024 ** 2,
  gitMilliseconds: 30000, providerMilliseconds: 15000,
  nativeJobSeconds: 900, executorSeconds: 720, commandSeconds: 600});
// Zero is deliberate: this proposal supplies no native resource admission.
const ADMISSION = Object.freeze({enabled: false, maxShards: 0, maxParallel: 0,
  maxTests: 0, nativeBaseline: null});
const BRIDGE_BLOBS = Object.freeze({
  '.github/workflows/rust-native-shard.yml': 'df141e5eb9ae49bb05fd2612e52790201691f8ee',
  'rust/scripts/impact-ci.cjs': '4fd12bef3805787984b65773792c50e3ec45ebfd',
  'rust/scripts/impact-plan.cjs': '170099e9802cc29654a417bd3563811ac95b922c',
  'rust/scripts/impact-shards.cjs': '9748f6ff55b21f1d889de34d6aa3a6e62e0943ee',
  'rust/scripts/validate-impact.cjs': '26d56b2b321bf629f8bd51bae6a0db3b117e7c7a',
  'rust/scripts/validation-process.cjs': 'a71c11d2d1fc1cfbe6efeed7c35e624fff5601dc',
  'rust/scripts/source-inventory.cjs': '47215a7c04c3304b6a7e3d249c9024ea52506da5',
  'rust/scripts/execution-evidence.cjs': 'e083ea74b0eb968d983620d1a3607eb9cd52b586',
});
const REQUIRED_INPUTS = ['rust/Cargo.toml', 'rust/Cargo.lock', 'rust/rust-toolchain.toml',
  'rust/validation/impact-graph.json', ...Object.keys(BRIDGE_BLOBS)];
const hash = data => crypto.createHash('sha256').update(data).digest('hex');

function sha(value) {
  assert.equal(typeof value, 'string', 'Expected a full lowercase 40-hex commit');
  assert.match(value, /^[a-f0-9]{40}$/, 'Expected a full lowercase 40-hex commit');
  assert.notEqual(value, '0'.repeat(40), 'Zero is not a commit');
  return value;
}

function requestContext(env, event, actualDriver) {
  assert.equal(env.GITHUB_EVENT_NAME, 'workflow_dispatch', 'Only genuine dispatch is implemented');
  assert.equal(env.GITHUB_REPOSITORY, REPOSITORY);
  assert.equal(event.repository?.full_name, REPOSITORY);
  assert.equal(event.repository?.default_branch, 'main');
  assert.equal(env.GITHUB_REF, 'refs/heads/main', 'Register and dispatch on default main');
  assert.equal(env.GITHUB_WORKFLOW_REF, `${REPOSITORY}/${WORKFLOW}@refs/heads/main`);
  assert.equal(env.GITHUB_SERVER_URL, 'https://github.com');
  assert.equal(env.GITHUB_API_URL, 'https://api.github.com');
  assert.equal(env.GITHUB_JOB, 'prepare');
  assert.equal(sha(actualDriver), sha(env.GITHUB_SHA), 'Driver checkout differs from event SHA');
  assert.equal(sha(env.GITHUB_WORKFLOW_SHA), actualDriver, 'Workflow and driver differ');
  for (const key of ['GITHUB_RUN_ID', 'GITHUB_RUN_ATTEMPT']) {
    assert.match(env[key] || '', /^[1-9][0-9]{0,19}$/, `Invalid ${key}`);
  }
  const inputs = event.inputs;
  assert.ok(inputs && typeof inputs === 'object' && !Array.isArray(inputs), 'Missing dispatch inputs');
  assert.deepEqual(Object.keys(inputs).sort(),
    ['base_sha', 'batch_id', 'mode', 'route', 'source_sha'], 'Unknown or missing dispatch input');
  for (const [input, variable] of Object.entries({source_sha: 'REQUESTED_SOURCE_SHA',
    base_sha: 'REQUESTED_BASE_SHA', mode: 'REQUESTED_MODE', route: 'REQUESTED_ROUTE',
    batch_id: 'REQUESTED_BATCH_ID'})) {
    assert.equal(typeof inputs[input], 'string', `Invalid ${input}`);
    assert.equal(inputs[input], env[variable], `Dispatch/environment mismatch: ${input}`);
  }
  const source = sha(inputs.source_sha), base = sha(inputs.base_sha);
  assert.notEqual(source, base, 'Source and comparison base must differ');
  assert.notEqual(source, actualDriver, 'DUT must not be the default-branch driver');
  assert.ok(['full', 'affected'].includes(inputs.mode), 'Explicit full or affected mode required');
  assert.ok(['manual', 'batch'].includes(inputs.route), 'Only manual or batched dispatch is implemented');
  if (inputs.route === 'batch') assert.match(inputs.batch_id, /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/);
  else assert.equal(inputs.batch_id, '', 'Manual requests have no batch ID');
  return {repository: REPOSITORY, branch: BRANCH, event: env.GITHUB_EVENT_NAME,
    ref: env.GITHUB_REF, driver: actualDriver, driver_base: MAIN_BASE,
    workflow_ref: env.GITHUB_WORKFLOW_REF, workflow_sha: env.GITHUB_WORKFLOW_SHA,
    source, base, anchor: ANCHOR, mode: inputs.mode, route: inputs.route,
    batch_id: inputs.batch_id || null, run_id: env.GITHUB_RUN_ID,
    job: env.GITHUB_JOB, run_attempt: env.GITHUB_RUN_ATTEMPT};
}

async function resolveHead(token, request = fetch) {
  assert.ok(typeof token === 'string' && token.length, 'Read-only repository token required');
  const response = await request(
    `https://api.github.com/repos/${REPOSITORY}/branches/${encodeURIComponent(BRANCH)}`,
    {method: 'GET', redirect: 'error', signal: AbortSignal.timeout(LIMITS.providerMilliseconds),
      headers: {Authorization: `Bearer ${token}`, Accept: 'application/vnd.github+json',
        'X-GitHub-Api-Version': '2022-11-28'}});
  assert.equal(response.status, 200, `Fixed branch lookup failed: HTTP ${response.status}`);
  const branch = await response.json();
  assert.equal(branch.name, BRANCH, 'Provider returned a foreign branch');
  return sha(branch.commit?.sha);
}

function matchResolved(context, resolved) {
  assert.equal(sha(resolved), context.source, 'Requested source is not the fixed branch head');
  return {...context, resolved_source: resolved};
}

function proveLineage(context, actual, anchorInBase, baseInSource) {
  assert.equal(sha(actual), sha(context.source), 'Checkout is not the immutable DUT');
  sha(context.base);
  assert.notEqual(context.base, actual);
  assert.equal(anchorInBase, true, 'Base does not descend from the approved anchor');
  assert.equal(baseInSource, true, 'Comparison base is not an ancestor of the DUT');
}

function gitEnvironment(env = process.env) {
  const names = ['PATH', 'Path', 'SystemRoot', 'SYSTEMROOT', 'WINDIR', 'TEMP', 'TMP',
    'PATHEXT', 'COMSPEC'];
  return {...Object.fromEntries(names.filter(name => env[name] !== undefined)
    .map(name => [name, env[name]])), GIT_CONFIG_NOSYSTEM: '1',
  GIT_CONFIG_GLOBAL: GIT_NULL, GIT_TERMINAL_PROMPT: '0', GIT_NO_REPLACE_OBJECTS: '1'};
}

function git(root, args, commands = []) {
  const argv = ['-c', 'core.fsmonitor=false', '-c', 'core.hooksPath=' + GIT_NULL, ...args];
  const result = spawnSync('git', argv, {cwd: root, encoding: 'utf8', shell: false,
    windowsHide: true, timeout: LIMITS.gitMilliseconds, maxBuffer: LIMITS.artifactBytes,
    env: gitEnvironment()});
  commands.push({program: 'git', args, exit: result.status, signal: result.signal,
    error_code: result.error?.code || null, output_sha256: hash(result.stdout || '')});
  assert.ifError(result.error);
  assert.equal(result.signal, null, 'Git canceled or timed out');
  assert.equal(result.status, 0, `Git guard failed: ${args.join(' ')}`);
  return result.stdout.trimEnd();
}

function clean(root, commands) {
  assert.equal(git(root, ['status', '--porcelain=v1', '--untracked-files=all',
    '--ignored=matching'], commands), '', 'Checkout is dirty or contains ignored output');
}

function repositoryRoot(root, commands) {
  const actual = fs.realpathSync(root);
  assert.equal(fs.realpathSync(git(actual, ['rev-parse', '--show-toplevel'], commands)), actual);
  const origin = git(actual, ['remote', 'get-url', 'origin'], commands);
  assert.ok([`https://github.com/${REPOSITORY}`, `https://github.com/${REPOSITORY}.git`]
    .includes(origin), 'Checkout has a foreign repository origin');
  clean(actual, commands);
  return actual;
}

/** Inventory actual bytes, not just HEAD or Git stat-cache metadata. Never load DUT code. */
function snapshot(root, commands = []) {
  clean(root, commands);
  const listing = git(root, ['ls-tree', '-r', '-z', '--full-tree', 'HEAD'], commands);
  const entries = listing.split('\0').filter(Boolean);
  assert.ok(entries.length > 0 && entries.length <= LIMITS.files, 'Source file count is out of bounds');
  let bytes = 0;
  const files = entries.map(entry => {
    const match = /^(100644|100755) blob ([a-f0-9]{40})\t(.+)$/.exec(entry);
    assert.ok(match, 'Source must contain only regular tracked files, not submodules or symlinks');
    const [, mode, blob, name] = match;
    assert.ok(!name.includes('\\') && !name.split('/').some(part =>
      !part || part === '.' || part === '..') && !path.isAbsolute(name), 'Unsafe source path');
    let file = root;
    for (const part of name.split('/')) {
      file = path.join(file, part);
      assert.equal(fs.lstatSync(file).isSymbolicLink(), false, 'Source path contains a symlink');
    }
    const stat = fs.statSync(file);
    assert.ok(stat.isFile() && stat.size <= LIMITS.fileBytes, 'Source file exceeds limit');
    bytes += stat.size;
    assert.ok(bytes <= LIMITS.sourceBytes, 'Source byte budget exceeded');
    const content = fs.readFileSync(file);
    assert.equal(content.length, stat.size, 'Source changed during inventory');
    const actualBlob = crypto.createHash('sha1')
      .update(`blob ${content.length}\0`).update(content).digest('hex');
    assert.equal(actualBlob, blob, `Actual source bytes differ from Git: ${name}`);
    return {path: name, mode, blob, bytes: content.length, sha256: hash(content)};
  });
  clean(root, commands);
  const result = {head: sha(git(root, ['rev-parse', 'HEAD'], commands)),
    tree: sha(git(root, ['rev-parse', 'HEAD^{tree}'], commands)), bytes, files};
  return {...result, sha256: hash(JSON.stringify(result))};
}

function verifyBridge(inventory) {
  const files = new Map(inventory.files.map(file => [file.path, file]));
  for (const name of REQUIRED_INPUTS) assert.ok(files.has(name), `Missing source input: ${name}`);
  for (const [name, blob] of Object.entries(BRIDGE_BLOBS)) {
    assert.equal(files.get(name).blob, blob, `Executor API changed; re-audit required: ${name}`);
  }
  return REQUIRED_INPUTS.map(name => files.get(name));
}

function unchanged(before, after) {
  assert.equal(after.head, before.head, 'DUT revision changed');
  assert.equal(after.tree, before.tree, 'DUT tree changed');
  assert.equal(after.sha256, before.sha256, 'Source inventory changed');
}

function debt(mode) {
  assert.ok(['full', 'affected'].includes(mode));
  return {status: 'NOT_ADMITTED', native_status: 'UNRUN', admitted: false,
    reasons: ['executor-source-platform-bridge-required', 'trusted-finite-resource-policy-required',
      ...(mode === 'affected' ? ['full-native-baseline-required'] : ['full-native-baseline-unrun'])],
    native_baseline: null, selection: null, selection_digest: null,
    complete_aggregate: false, watermark: null, executed_commands: [], tests_passed: 0,
    coverage: ['linux', 'macos'].map(platform => ({platform, arch: null, toolchain: null,
      selection: null, acquisition: null, native_run: null, native_job: null, steps: 0,
      status: 'UNRUN', remaining_debt: 'All native workloads; no baseline or subset established'})),
    resource_policy: ADMISSION, future_execution_ceiling: {
      job_seconds: LIMITS.nativeJobSeconds, executor_seconds: LIMITS.executorSeconds,
      command_seconds: LIMITS.commandSeconds},
    reference_source: REFERENCE_SOURCE, bridge_blobs: BRIDGE_BLOBS};
}

function artifactDirectory(value, roots) {
  assert.ok(value && path.isAbsolute(value), 'External absolute artifact directory required');
  const directory = path.resolve(value);
  for (const root of roots) {
    const relative = path.relative(root, directory);
    assert.ok(relative && (relative === '..' || relative.startsWith(`..${path.sep}`) ||
      path.isAbsolute(relative)), 'Artifacts must be outside both checkouts');
  }
  assert.equal(fs.realpathSync(path.dirname(directory)), path.dirname(directory),
    'Artifact parent must not be a symlink');
  return directory;
}

function writeJson(file, value) {
  const body = `${JSON.stringify(value, null, 2)}\n`;
  assert.ok(Buffer.byteLength(body) <= LIMITS.artifactBytes, 'Artifact exceeds byte budget');
  fs.writeFileSync(file, body, {flag: 'wx'});
}

function fixedRoots(driver, workspace) {
  const actual = fs.realpathSync(driver), work = fs.realpathSync(workspace);
  assert.equal(actual, path.join(work, 'driver'), 'Driver must use its fixed isolated checkout');
  assert.equal(fs.realpathSync(path.join(work, 'source')), path.join(work, 'source'),
    'DUT must use its fixed isolated checkout, not a symlink');
  return {driver: actual, source: path.join(work, 'source')};
}

function readContext(driver, env, commands) {
  repositoryRoot(driver, commands);
  git(driver, ['merge-base', '--is-ancestor', MAIN_BASE, 'HEAD'], commands);
  assert.ok(fs.statSync(env.GITHUB_EVENT_PATH).size <= LIMITS.artifactBytes, 'Event exceeds limit');
  return requestContext(env, JSON.parse(fs.readFileSync(env.GITHUB_EVENT_PATH, 'utf8')),
    git(driver, ['rev-parse', 'HEAD'], commands));
}

/** Injectable boundaries permit offline orchestration fixtures, never native-pass mocks. */
async function runStage(mode, driver, env, services = {}) {
  assert.ok(['pin', 'inspect'].includes(mode), 'Invalid request stage');
  const ops = {readContext, resolveHead, fixedRoots, repositoryRoot, git, snapshot,
    verifyBridge, clean, ...services};
  const source = path.join(path.dirname(driver), 'source');
  const directory = artifactDirectory(env.POSTHOC_ARTIFACTS, [driver, source]);
  if (mode === 'pin') fs.mkdirSync(directory, {recursive: false});
  else assert.equal(fs.realpathSync(directory), directory, 'Artifact directory must not be a symlink');
  const receipt = {schema: 1, kind: 'posthoc-native-request', stage: mode,
    ...debt('full'), status: 'GUARD_FAILURE', context: null, guard_commands: [],
    started_at: new Date().toISOString(), runner: {os: env.RUNNER_OS || null,
      arch: env.RUNNER_ARCH || null, actual_os: process.platform, actual_arch: process.arch,
      note: 'Request preparation host only; not a native DUT execution host'}};
  let exit = 1;
  const canceled = signal => {
    receipt.status = 'CANCELED_UNRUN';
    receipt.error = `Request preparation interrupted: ${signal}`;
    receipt.finished_at = new Date().toISOString();
    writeJson(path.join(directory, `${mode}-canceled.json`), receipt);
    process.exit(130);
  };
  const interrupt = () => canceled('SIGINT'), terminate = () => canceled('SIGTERM');
  process.once('SIGINT', interrupt);
  process.once('SIGTERM', terminate);
  try {
    const context = ops.readContext(driver, env, receipt.guard_commands);
    receipt.context = context;
    Object.assign(receipt, debt(context.mode));
    receipt.status = 'GUARD_FAILURE';
    if (mode === 'pin') {
      assert.equal(driver, path.join(fs.realpathSync(env.GITHUB_WORKSPACE), 'driver'));
      receipt.phase = 'provider_resolution';
      const resolved = await ops.resolveHead(env.GH_TOKEN);
      receipt.phase = 'source_pin';
      receipt.context = matchResolved(context, resolved);
      receipt.status = 'PINNED_UNRUN';
      assert.ok(env.GITHUB_OUTPUT, 'Missing workflow output path');
      fs.appendFileSync(env.GITHUB_OUTPUT, `source_sha=${context.source}\n`);
      exit = 0;
    } else {
      ops.fixedRoots(driver, env.GITHUB_WORKSPACE);
      const preflightPath = path.join(directory, 'pin.json');
      assert.ok(fs.statSync(preflightPath).size <= LIMITS.artifactBytes, 'Pin receipt exceeds limit');
      const pin = JSON.parse(fs.readFileSync(preflightPath, 'utf8'));
      assert.equal(pin.schema, 1);
      assert.equal(pin.stage, 'pin');
      assert.equal(pin.status, 'PINNED_UNRUN');
      assert.deepEqual(pin.context, {...context, resolved_source: context.source},
        'Dispatch/driver/run/attempt/source provenance changed after pinning');
      receipt.context = pin.context;
      receipt.phase = 'source_integrity';
      ops.repositoryRoot(source, receipt.guard_commands);
      proveLineage(context, ops.git(source, ['rev-parse', 'HEAD'], receipt.guard_commands),
        ops.git(source, ['merge-base', '--is-ancestor', ANCHOR, context.base], receipt.guard_commands) === '',
        ops.git(source, ['merge-base', '--is-ancestor', context.base, context.source], receipt.guard_commands) === '');
      const before = ops.snapshot(source, receipt.guard_commands);
      receipt.inputs = ops.verifyBridge(before);
      writeJson(path.join(directory, 'source-before.json'), before);
      receipt.source_before = before.sha256;
      receipt.source_tree = before.tree;
      receipt.phase = 'freshness_observation';
      const observed = await ops.resolveHead(env.GH_TOKEN);
      receipt.freshness = {observed_head: observed, observed_at: new Date().toISOString(),
        source_still_current: observed === context.source,
        note: 'Observation only; never repins or substitutes the immutable DUT'};
      receipt.phase = 'source_integrity_after';
      const after = ops.snapshot(source, receipt.guard_commands);
      writeJson(path.join(directory, 'source-after.json'), after);
      receipt.source_after = after.sha256;
      unchanged(before, after);
      ops.clean(driver, receipt.guard_commands);
      assert.equal(ops.git(driver, ['rev-parse', 'HEAD'], receipt.guard_commands), context.driver);
      receipt.source_verified = true;
      receipt.status = 'NOT_ADMITTED';
      exit = 78;
    }
  } catch (error) {
    receipt.status = receipt.phase?.startsWith('source_integrity') ?
      'SOURCE_INTEGRITY_FAILURE' : receipt.phase === 'provider_resolution' ||
      receipt.phase === 'freshness_observation' ? 'PROVIDER_LOOKUP_FAILURE_UNRUN' : 'GUARD_FAILURE';
    receipt.error = error.message;
    console.error(`${receipt.status}: ${error.message}`);
  } finally {
    process.removeListener('SIGINT', interrupt);
    process.removeListener('SIGTERM', terminate);
    receipt.finished_at = new Date().toISOString();
    writeJson(path.join(directory, `${mode}.json`), receipt);
  }
  if (exit === 78) console.error('NOT_ADMITTED: native executor bridge, baseline and finite resource authority absent; Linux/macOS UNRUN.');
  return exit;
}

/** The only runtime modes are source pinning and non-executing inspection. */
async function main(argv, env = process.env) {
  if (argv.length === 1 && argv[0] === '--help') {
    console.log('rust-native-posthoc.cjs pin|inspect\n' +
      'Default-main manual/batch exact-source request facade; native execution is UNRUN.\n' +
      'Inspect exits 78 (NOT_ADMITTED); see module header for activation/API prerequisites.');
    return 0;
  }
  assert.ok(argv.length === 1 && ['pin', 'inspect'].includes(argv[0]), 'Usage: pin|inspect|--help');
  return runStage(argv[0], fs.realpathSync(path.resolve(__dirname, '../..')), env);
}

module.exports = {REPOSITORY, BRANCH, MAIN_BASE, ANCHOR, REFERENCE_SOURCE, WORKFLOW,
  LIMITS, ADMISSION, BRIDGE_BLOBS, sha, requestContext, resolveHead, matchResolved,
  proveLineage, gitEnvironment, git, clean, repositoryRoot, snapshot, verifyBridge,
  unchanged, debt, artifactDirectory, writeJson, fixedRoots, runStage, main};
if (require.main === module) {
  main(process.argv.slice(2)).then(code => { process.exitCode = code; })
    .catch(error => { console.error(error.message); process.exitCode = 1; });
}
