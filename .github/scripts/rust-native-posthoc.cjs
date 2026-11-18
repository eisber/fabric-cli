// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

/**
 * Full/affected inspection remains non-executing (exit 78, NOT_ADMITTED).
 * The separate bootstrap-v1 route admits ONLY the frozen partial workload in
 * rust-native-posthoc.md. It does not use or qualify the impact graph, establish
 * a native baseline, or require the separately owned 9ff bridge in the DUT.
 * Inspection requires Git 2.43+ for pinned-tree attributes. Declared text/CRLF
 * checkout is supported; custom filters, ident and encodings are rejected.
 *
 * Full/affected activation still requires the separate compatible source-domain
 * bridge, a real matching native baseline or reviewed full policy, and complete
 * canonical selection/aggregation. Never turn this subset into that proof.
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
const POLICY = Object.freeze({id: 'bootstrap-v1', qualification: 'PARTIAL_FIXED_WORKLOAD',
  maxParallel: 2, maxTests: 2048, outputBytes: 16 * 1024 ** 2,
  bundleBytes: 64 * 1024 ** 2, metadataReserve: 16 * 1024 ** 2,
  maxDailyPlatformJobs: 4, maxDailyBatches: 2, batchIntervalSeconds: 21600,
  nativeBaseline: null});
const HOSTS = Object.freeze({
  linux: {os: 'Linux', platform: 'linux', arch: 'x64', image: 'ubuntu-24.04',
    triple: 'x86_64-unknown-linux-gnu'},
  macos: {os: 'macOS', platform: 'darwin', arch: 'arm64', image: 'macos-15',
    triple: 'aarch64-apple-darwin'},
});
const BOOTSTRAP_BLOBS = Object.freeze({
  'rust/scripts/integration-smoke.cjs': 'fa9b140899537c9fa0776b0df42a6260b459cecb',
  'rust/scripts/validate-impact.cjs': '5246494b32ae4fe60da30f69468d0e3982a5ba61',
  'rust/scripts/validation-process.cjs': BRIDGE_BLOBS['rust/scripts/validation-process.cjs'],
  'rust/scripts/validation-process.py': 'dda079affe45a76f18b6964d0c935c30a12db356',
  'rust/scripts/validation_posix.py': '3f034b1ba4a6168e13f5d4e7dca5ef87d3bb3321',
  'rust/engine/core/contracts/Cargo.toml': '699377c01183961f9af4ddb021caa5e9d203f8c7',
  'rust/engine/core/contracts/src/lib.rs': 'f204e0851510409b7bbfed41348a733eb9958a8a',
  'rust/engine/core/contracts/src/endpoint.rs': 'ebc6361dcd84a4a691640945761b3b0bff20443c',
  'rust/engine/core/contracts/src/operation.rs': 'e4cde89300134ed500a129daddf9801bcbc15786',
  'rust/engine/core/contracts/src/request.rs': '6851c8bbd070bd40d3e7175dc03e02e4b9e710ff',
});
const CORE_TESTS = Object.freeze([
  ...['public_defaults_have_fixed_audiences_and_roots',
    'daily_and_cross_family_overrides_do_not_infer_the_service',
    'rejects_non_hostname_syntax_and_suffix_confusion',
    'path_construction_preserves_encoded_items_and_queries',
    'workspace_item_paths_preserve_opaque_segments_but_reject_dot_segments',
    'parsed_urls_are_bound_to_exact_https_host_and_port',
    'raw_links_reject_parsing_tricks_and_require_exact_queryless_path',
    'relevance_discovery_has_an_explicit_root_not_a_powerbi_route_rewrite']
    .map(name => `endpoint::tests::${name}`),
  ...['incomplete_selection_is_a_typed_error',
    'selection_preserves_explicit_account_tenant_and_audience',
    'identity_failures_remain_distinct_and_sanitized'].map(name => `tests::${name}`),
  ...['context_retains_identity_and_does_not_acquire_or_format_credentials',
    'validation_failures_precede_all_credential_and_execution_access',
    'every_origin_and_daily_override_requests_one_fixed_scope',
    'cancellation_wins_before_validation_or_operation_polling',
    'cancellation_after_token_prevents_executor_creation',
    'cancellation_after_execution_suppresses_success_and_error',
    'cancellation_drops_in_flight_credential_or_execution',
    'credential_and_executor_failures_are_distinct_and_sanitized',
    'request_errors_compare_payloads_without_requiring_payload_debug',
    'ready_request_preserves_three_cancellation_checkpoints'].map(name => `request::tests::${name}`),
].sort());

function sha(value) {
  assert.equal(typeof value, 'string', 'Expected a full lowercase 40-hex commit');
  assert.match(value, /^[a-f0-9]{40}$/, 'Expected a full lowercase 40-hex commit');
  assert.notEqual(value, '0'.repeat(40), 'Zero is not a commit');
  return value;
}

function requestContext(env, event, actualDriver, bootstrap = false) {
  assert.equal(env.GITHUB_EVENT_NAME, 'workflow_dispatch', 'Only genuine dispatch is implemented');
  assert.equal(env.GITHUB_REPOSITORY, REPOSITORY);
  assert.equal(event.repository?.full_name, REPOSITORY);
  assert.equal(event.repository?.default_branch, 'main');
  assert.equal(env.GITHUB_REF, 'refs/heads/main', 'Register and dispatch on default main');
  assert.equal(env.GITHUB_WORKFLOW_REF, `${REPOSITORY}/${WORKFLOW}@refs/heads/main`);
  assert.equal(env.GITHUB_SERVER_URL, 'https://github.com');
  assert.equal(env.GITHUB_API_URL, 'https://api.github.com');
  assert.ok((bootstrap ? ['prepare', 'native'] : ['prepare']).includes(env.GITHUB_JOB),
    'Unexpected workflow job');
  assert.equal(sha(actualDriver), sha(env.GITHUB_SHA), 'Driver checkout differs from event SHA');
  assert.equal(sha(env.GITHUB_WORKFLOW_SHA), actualDriver, 'Workflow and driver differ');
  for (const key of ['GITHUB_RUN_ID', 'GITHUB_RUN_ATTEMPT']) {
    assert.match(env[key] || '', /^[1-9][0-9]{0,19}$/, `Invalid ${key}`);
  }
  let inputs = event.inputs;
  assert.ok(inputs && typeof inputs === 'object' && !Array.isArray(inputs), 'Missing dispatch inputs');
  if (bootstrap && inputs.mode === 'bootstrap') {
    inputs = {...inputs};
    for (const field of ['batch_id', 'repair_of']) {
      if (!Object.hasOwn(inputs, field)) inputs[field] = '';
    }
  }
  assert.deepEqual(Object.keys(inputs).sort(),
    ['base_sha', 'batch_id', 'mode', 'route', 'source_sha',
      ...(bootstrap ? ['driver_sha', 'platforms', 'workload_id', 'repair_of'] : [])].sort(),
    'Unknown or missing dispatch input');
  for (const [input, variable] of Object.entries({source_sha: 'REQUESTED_SOURCE_SHA',
    base_sha: 'REQUESTED_BASE_SHA', mode: 'REQUESTED_MODE', route: 'REQUESTED_ROUTE',
    batch_id: 'REQUESTED_BATCH_ID'})) {
    assert.equal(typeof inputs[input], 'string', `Invalid ${input}`);
    assert.equal(inputs[input], env[variable], `Dispatch/environment mismatch: ${input}`);
  }
  const source = sha(inputs.source_sha), base = sha(inputs.base_sha);
  assert.notEqual(source, base, 'Source and comparison base must differ');
  assert.notEqual(source, actualDriver, 'DUT must not be the default-branch driver');
  assert.ok((bootstrap ? ['bootstrap'] : ['full', 'affected']).includes(inputs.mode),
    'Explicit supported mode required');
  assert.ok(['manual', 'batch'].includes(inputs.route), 'Only manual or batched dispatch is implemented');
  if (inputs.route === 'batch') assert.match(inputs.batch_id, /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/);
  else assert.equal(inputs.batch_id, '', 'Manual requests have no batch ID');
  const extra = {};
  if (bootstrap) {
    const expectedDriver = sha(inputs.driver_sha);
    assert.equal(expectedDriver, env.REQUESTED_DRIVER_SHA, 'Expected driver input/environment mismatch');
    assert.equal(actualDriver, expectedDriver, 'Dispatch differs from the explicitly reviewed driver pin');
    extra.expected_driver = expectedDriver;
    assert.equal(inputs.workload_id, POLICY.id, 'Unknown fixed workload/policy');
    assert.ok(['both', 'linux', 'macos'].includes(inputs.platforms), 'Unknown platform selection');
    assert.equal(inputs.repair_of, '', 'Retries are disabled without a separately reviewed repair policy');
    assert.equal(env.GITHUB_RUN_ATTEMPT, '1', 'Reruns are not admitted');
    extra.workload_id = inputs.workload_id;
    extra.platforms = inputs.platforms === 'both' ? ['linux', 'macos'] : [inputs.platforms];
    extra.repair_of = inputs.repair_of || null;
  }
  return {repository: REPOSITORY, branch: BRANCH, event: env.GITHUB_EVENT_NAME,
    ref: env.GITHUB_REF, driver: actualDriver, driver_base: MAIN_BASE,
    workflow_ref: env.GITHUB_WORKFLOW_REF, workflow_sha: env.GITHUB_WORKFLOW_SHA,
    source, base, anchor: ANCHOR, mode: inputs.mode, route: inputs.route,
    batch_id: inputs.batch_id || null, run_id: env.GITHUB_RUN_ID,
    job: env.GITHUB_JOB, run_attempt: env.GITHUB_RUN_ATTEMPT, ...extra};
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
  GIT_CONFIG_GLOBAL: GIT_NULL, GIT_TERMINAL_PROMPT: '0', GIT_NO_REPLACE_OBJECTS: '1',
  GIT_ATTR_NOSYSTEM: '1'};
}

function git(root, args, commands = [], input) {
  const argv = ['-c', 'core.fsmonitor=false', '-c', 'core.hooksPath=' + GIT_NULL,
    '-c', 'core.attributesFile=' + GIT_NULL, ...args];
  const result = spawnSync('git', argv, {cwd: root, encoding: 'utf8', shell: false,
    windowsHide: true, timeout: LIMITS.gitMilliseconds, maxBuffer: LIMITS.artifactBytes,
    env: gitEnvironment(), input});
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

function checkoutAttributes(root, names, commands) {
  const keys = ['text', 'eol', 'filter', 'ident', 'working-tree-encoding'];
  const fields = git(root, ['check-attr', '--source=HEAD', '-z', '--stdin', ...keys],
    commands, `${names.join('\0')}\0`).split('\0');
  assert.equal(fields.pop(), '', 'Malformed checkout-attribute output');
  assert.equal(fields.length, names.length * keys.length * 3, 'Incomplete checkout attributes');
  const attributes = new Map();
  let index = 0;
  for (const name of names) {
    const values = {};
    for (const key of keys) {
      assert.equal(fields[index++], name, 'Checkout attribute names differ from the pinned tree');
      assert.equal(fields[index++], key, 'Unexpected checkout attribute');
      values[key] = fields[index++];
    }
    attributes.set(name, values);
  }
  return attributes;
}

/** Support declared EOL conversion without invoking Git smudge/clean filters. */
function verifyCheckoutBytes(content, expectedBlob, attributes, name) {
  for (const key of ['filter', 'ident', 'working-tree-encoding']) {
    assert.ok(['unspecified', 'unset'].includes(attributes[key]),
      `Unsupported checkout transformation ${key}: ${name}`);
  }
  const blob = bytes => crypto.createHash('sha1')
    .update(`blob ${bytes.length}\0`).update(bytes).digest('hex');
  let canonical = content, representation = 'blob-bytes';
  if (attributes.text === 'set' && attributes.eol === 'crlf') {
    const text = content.toString('latin1');
    assert.ok(!/(^|[^\r])\n/.test(text), `Expected declared CRLF checkout bytes: ${name}`);
    canonical = Buffer.from(text.replace(/\r\n/g, '\n'), 'latin1');
    representation = 'declared-text-crlf';
  }
  assert.ok(blob(canonical) === expectedBlob ||
    (representation === 'declared-text-crlf' && blob(content) === expectedBlob),
  `Actual source bytes differ from Git: ${name}`);
  return representation;
}

/** Inventory actual bytes, not just HEAD or Git stat-cache metadata. Never load DUT code. */
function snapshot(root, commands = []) {
  clean(root, commands);
  const listing = git(root, ['ls-tree', '-r', '-z', '--full-tree', 'HEAD'], commands);
  const entries = listing.split('\0').filter(Boolean);
  assert.ok(entries.length > 0 && entries.length <= LIMITS.files, 'Source file count is out of bounds');
  const tracked = entries.map(entry => {
    const match = /^(100644|100755) blob ([a-f0-9]{40})\t(.+)$/.exec(entry);
    assert.ok(match, 'Source must contain only regular tracked files, not submodules or symlinks');
    const [, mode, blob, name] = match;
    assert.ok(!name.includes('\\') && !name.split('/').some(part =>
      !part || part === '.' || part === '..') && !path.isAbsolute(name), 'Unsafe source path');
    return {mode, blob, name};
  });
  const attributes = checkoutAttributes(root, tracked.map(entry => entry.name), commands);
  let bytes = 0;
  const files = tracked.map(({mode, blob, name}) => {
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
    const representation = verifyCheckoutBytes(content, blob, attributes.get(name), name);
    return {path: name, mode, blob, checkout_representation: representation,
      bytes: content.length, sha256: hash(content)};
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

function bootstrapContext(driver, env) {
  repositoryRoot(driver);
  git(driver, ['merge-base', '--is-ancestor', MAIN_BASE, 'HEAD']);
  assert.ok(fs.statSync(env.GITHUB_EVENT_PATH).size <= LIMITS.artifactBytes);
  return requestContext(env, JSON.parse(fs.readFileSync(env.GITHUB_EVENT_PATH, 'utf8')),
    git(driver, ['rev-parse', 'HEAD']), true);
}

function verifyBootstrapInputs(inventory) {
  const files = new Map(inventory.files.map(file => [file.path, file]));
  for (const [name, blob] of Object.entries(BOOTSTRAP_BLOBS)) {
    assert.equal(files.get(name)?.blob, blob, `Bootstrap API/workload changed; re-audit required: ${name}`);
  }
  for (const name of ['rust/Cargo.toml', 'rust/Cargo.lock', 'rust/rust-toolchain.toml',
    'rust/clients/cli/app/Cargo.toml']) assert.ok(files.has(name), `Missing workload input: ${name}`);
}

function bootstrapHost(platform, env, actual = {platform: process.platform, arch: process.arch}) {
  const host = HOSTS[platform];
  assert.ok(host, 'Unknown native host');
  assert.equal(env.RUNNER_ENVIRONMENT, 'github-hosted', 'Native work requires a GitHub-hosted runner');
  assert.equal(actual.platform, host.platform, 'Wrong actual native OS');
  assert.equal(env.RUNNER_OS, host.os, 'Runner OS mismatch');
  assert.equal(env.POSTHOC_IMAGE, host.image, 'Wrong allowlisted runner image');
  const triples = platform === 'linux' ? {x64: host.triple} :
    {arm64: 'aarch64-apple-darwin', x64: 'x86_64-apple-darwin'};
  assert.ok(Object.hasOwn(triples, actual.arch), 'Unsupported actual native architecture');
  assert.equal(env.RUNNER_ARCH, actual.arch === 'x64' ? 'X64' : 'ARM64', 'Runner architecture mismatch');
  assert.ok(env.ImageVersion, 'Missing hosted image version');
  return {platform, os: actual.platform, arch: actual.arch, triple: triples[actual.arch],
    image: host.image, image_version: env.ImageVersion, image_os: env.ImageOS || null,
    runner_name: env.RUNNER_NAME, cpu_count: os.cpus().length,
    cpu_models: [...new Set(os.cpus().map(cpu => cpu.model))], release: os.release()};
}

async function providerJson(endpoint, token, request = fetch) {
  assert.ok(typeof token === 'string' && token.length, 'Read-only provider token required');
  const response = await request(`https://api.github.com/repos/${REPOSITORY}/${endpoint}`, {
    method: 'GET', redirect: 'error', signal: AbortSignal.timeout(LIMITS.providerMilliseconds),
    headers: {Authorization: `Bearer ${token}`, Accept: 'application/vnd.github+json',
      'X-GitHub-Api-Version': '2022-11-28'},
  });
  assert.equal(response.status, 200, `Provider guard failed: HTTP ${response.status}`);
  const body = await response.text();
  assert.ok(Buffer.byteLength(body) <= LIMITS.artifactBytes, 'Provider response exceeds limit');
  return JSON.parse(body);
}

function runTitle(context) {
  const platforms = context.platforms.length === 2 ? 'both' : context.platforms[0];
  return `posthoc:${context.source}:${POLICY.id}:${platforms}:${context.repair_of || 'initial'}`;
}

function admitHistory(context, history, now = Date.now()) {
  assert.equal(context.repair_of, null, 'Retries are disabled without a separately reviewed repair policy');
  const runs = history?.workflow_runs;
  assert.ok(Number.isSafeInteger(history?.total_count) && history.total_count > 0 &&
    history.total_count <= 100 && Array.isArray(runs) && runs.length === history.total_count,
  'Incomplete bounded run history: total_count must equal all returned rows');
  for (const run of runs) {
    assert.ok(run && Number.isSafeInteger(run.id) && run.id > 0, 'Invalid provider run row');
    assert.equal(run.run_attempt, 1, 'Historical rerun allocation is unknown; admission refused');
    assert.equal(run.event, 'workflow_dispatch');
    assert.equal(run.head_branch, 'main');
    sha(run.head_sha);
  }
  assert.equal(new Set(runs.map(run => String(run.id))).size, runs.length, 'Duplicate run history');
  const current = runs.find(run => String(run.id) === context.run_id);
  assert.ok(current, 'Current genuine run missing from provider history');
  assert.equal(current.display_title, runTitle(context), 'Actual run title differs from fixed request');
  assert.equal(current.head_sha, context.driver, 'Provider driver SHA differs');
  const all = runs.filter(run => String(run.id) !== context.run_id).map(run => {
    assert.ok(Number.isFinite(Date.parse(run.created_at)) && Date.parse(run.created_at) <= now,
      'Malformed provider reservation time');
    assert.equal(run.event, 'workflow_dispatch');
    assert.equal(run.head_branch, 'main');
    assert.ok(['queued', 'requested', 'waiting', 'pending', 'in_progress', 'completed']
      .includes(run.status), 'Unknown provider run state');
    if (run.status === 'completed') {
      assert.ok(Number.isFinite(Date.parse(run.updated_at)) &&
        Date.parse(run.updated_at) >= Date.parse(run.created_at) &&
        Date.parse(run.updated_at) <= now, 'Missing verified provider completion timestamp');
    }
    const match = /^posthoc:([a-f0-9]{40}):bootstrap-v1:(both|linux|macos):(initial|[1-9][0-9]{0,19})$/
      .exec(run.display_title);
    assert.ok(match, 'Unclassifiable recent workload history; admission is unknown');
    return {...run, source: match[1], platforms: match[2] === 'both' ? ['linux', 'macos'] : [match[2]],
      repair: match[3] !== 'initial'};
  });
  const parsed = all.filter(run => run.status !== 'completed' ||
    Date.parse(run.updated_at) >= now - 86400000);
  assert.ok(parsed.length + 1 <= POLICY.maxDailyBatches, 'Daily dispatch reservation exhausted');
  assert.ok(parsed.reduce((sum, run) => sum + run.platforms.length, context.platforms.length) <=
    POLICY.maxDailyPlatformJobs, 'Daily native job reservation exhausted');
  for (const run of parsed) {
    if (run.source !== context.source) {
      assert.ok(now - Date.parse(run.created_at) >= POLICY.batchIntervalSeconds * 1000,
        'Coalesce immutable batches for at least six hours');
    }
  }
  const duplicates = all.filter(run => run.source === context.source &&
    run.platforms.some(platform => context.platforms.includes(platform)));
  assert.equal(duplicates.length, 0, 'Duplicate source/platform/workload dispatch refused');
  return {observed_at: new Date(now).toISOString(), reserved_platform_jobs_24h:
    parsed.reduce((sum, run) => sum + run.platforms.length, context.platforms.length),
  reserved_runner_minutes_24h: parsed.reduce((sum, run) => sum + run.platforms.length * 15,
    context.platforms.length * 15), prior_runs: parsed.map(run => run.id)};
}

function portableContext(context) {
  const {job, ...portable} = context;
  return portable;
}

function readJson(file) {
  assert.ok(fs.statSync(file).size <= LIMITS.artifactBytes, 'Receipt exceeds byte budget');
  return JSON.parse(fs.readFileSync(file, 'utf8'));
}

function bundleBytes(directory) {
  return fs.readdirSync(directory, {withFileTypes: true}).reduce((total, entry) => {
    const file = path.join(directory, entry.name);
    const stat = fs.lstatSync(file);
    assert.equal(stat.isSymbolicLink(), false, 'Artifact symlink refused');
    return total + (entry.isDirectory() ? bundleBytes(file) : stat.size);
  }, 0);
}

function verifyCoreOutput(stdout) {
  const names = [...stdout.matchAll(/^test ([A-Za-z0-9_:]+) \.\.\. ok\r?$/gm)]
    .map(match => match[1]).sort();
  assert.deepEqual(names, CORE_TESTS, 'Every frozen lib test must actually pass exactly once');
  const summaries = [...stdout.matchAll(
    /^test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out;.*$/gm)];
  assert.equal(summaries.length, 1, 'Missing or duplicate core test summary');
  assert.deepEqual(summaries[0].slice(1).map(Number), [CORE_TESTS.length, 0, 0, 0, 0],
    'Empty, ignored, filtered, failed or partial tests are not a pass');
  assert.ok(names.length > 0 && names.length <= POLICY.maxTests);
  return {passed: names.length, failed: 0, ignored: 0, filtered: 0, tests: names};
}

function commandEnvironment(env, directories) {
  const names = ['PATH', 'Path', 'SystemRoot', 'SYSTEMROOT', 'WINDIR', 'PATHEXT', 'COMSPEC'];
  return {...Object.fromEntries(names.filter(name => env[name] !== undefined)
    .map(name => [name, env[name]])), ...directories, HOME: directories.HOME,
  XDG_CONFIG_HOME: directories.HOME, XDG_CACHE_HOME: directories.HOME,
  TMPDIR: directories.TMPDIR, TMP: directories.TMPDIR, TEMP: directories.TMPDIR,
  CARGO_NET_RETRY: '1', CARGO_HTTP_TIMEOUT: '30', CARGO_HTTP_LOW_SPEED_LIMIT: '10',
  CARGO_BUILD_JOBS: '2', CARGO_INCREMENTAL: '0', CARGO_PROFILE_DEV_DEBUG: '0',
  CARGO_PROFILE_TEST_DEBUG: '0', RUST_TEST_THREADS: '2', NO_COLOR: '1',
  PYTHONDONTWRITEBYTECODE: '1', GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: GIT_NULL,
  GIT_ATTR_NOSYSTEM: '1', GIT_NO_REPLACE_OBJECTS: '1', GIT_TERMINAL_PROMPT: '0'};
}

function requireNoProviderTokens(env) {
  assert.ok(env.GH_TOKEN === undefined, 'Provider token must not enter native stage');
  assert.ok(env.GITHUB_TOKEN === undefined, 'Provider token must not enter native stage');
}

function finalizeBootstrap(report, operations) {
  const errors = [];
  for (const [phase, operation] of operations) {
    try { operation(); }
    catch (error) {
      errors.push({phase, message: error.message.slice(0, 8192),
        message_sha256: hash(error.message)});
    }
  }
  if (errors.length) {
    report.passed = false;
    report.status = 'SOURCE_OR_ARTIFACT_INTEGRITY_FAILURE';
    report.finalization_errors = errors;
    report.integrity_error = errors[0].message;
  }
  return errors.length === 0;
}

function pipeOutput(pipe, retained, limit, abort) {
  assert.equal(fs.lstatSync(pipe).isFIFO(), true, 'Expected a private output FIFO');
  const reader = fs.openSync(pipe, fs.constants.O_RDONLY | fs.constants.O_NONBLOCK);
  let writer;
  try { writer = fs.openSync(retained, 'wx', 0o600); }
  catch (error) { fs.closeSync(reader); throw error; }
  let observed = 0, bytes = 0, completed, failure = null;
  const observedHash = crypto.createHash('sha256');
  const buffer = Buffer.alloc(65536);
  const pump = () => {
    try {
      for (let reads = 0; reads < 32; reads++) {
        let count;
        try { count = fs.readSync(reader, buffer, 0, buffer.length, null); }
        catch (error) {
          if (['EAGAIN', 'EWOULDBLOCK'].includes(error.code)) break;
          throw error;
        }
        if (!count) break;
        observed += count;
        observedHash.update(buffer.subarray(0, count));
        const keep = Math.min(count, limit - bytes);
        if (keep > 0) { fs.writeFileSync(writer, buffer.subarray(0, keep)); bytes += keep; }
        if (observed > limit) abort();
      }
    } catch (error) {
      failure = error.message;
      abort();
    }
  };
  return {pump, finish: () => {
    if (completed) return completed;
    try { pump(); }
    finally { fs.closeSync(reader); fs.closeSync(writer); }
    completed = {observed_bytes: observed, bytes, observed_sha256: observedHash.digest('hex'),
      censored: observed > limit, error: failure};
    return completed;
  }};
}

async function boundedCommand(executeOwned, descriptor, options) {
  const {directory, deadline, env, report, save, signal, transportRoot} = options;
  assert.ok(Date.now() < deadline, 'Native executor budget exhausted');
  const index = String(report.commands.length + 1).padStart(2, '0');
  const stdout = path.join(directory, `${index}-stdout.log`);
  const stderr = path.join(directory, `${index}-stderr.log`);
  const available = POLICY.bundleBytes - POLICY.metadataReserve - bundleBytes(directory);
  const cap = Math.min(POLICY.outputBytes, Math.floor(available / 2));
  assert.ok(cap > 0, 'Artifact budget exhausted');
  const controller = new AbortController();
  const cancel = () => controller.abort();
  signal?.addEventListener('abort', cancel, {once: true});
  if (signal?.aborted) cancel();
  const entry = {...descriptor, status: 'INCOMPLETE', started_at: new Date().toISOString(),
    output_limit_bytes: cap, timeout_ms: Math.min(descriptor.timeout_ms, LIMITS.commandSeconds * 1000,
      deadline - Date.now())};
  report.commands.push(entry);
  save();
  let censored = false;
  const pipes = transportRoot ? [] : null;
  try {
    if (pipes) {
      for (const [position, stream] of ['stdout', 'stderr'].entries()) {
        const pipe = path.join(transportRoot, `${index}-${stream}.pipe`);
        pipes.push({path: pipe, output: pipeOutput(pipe, position === 0 ? stdout : stderr,
          cap, () => controller.abort())});
      }
    }
  } catch (error) {
    pipes?.forEach(pipe => pipe.output.finish());
    signal?.removeEventListener('abort', cancel);
    entry.error = error.message;
    save();
    throw error;
  }
  const watch = setInterval(() => {
    if (pipes) { pipes.forEach(pipe => pipe.output.pump()); return; }
    for (const file of [stdout, stderr]) {
      if (fs.existsSync(file) && fs.statSync(file).size > cap) {
        censored = true;
        controller.abort();
      }
    }
  }, 20);
  try {
    const result = await executeOwned(descriptor.program, descriptor.args, {
      cwd: descriptor.cwd, env, python: 'python3',
      stdout: pipes?.[0].path || stdout, stderr: pipes?.[1].path || stderr,
      timeout: entry.timeout_ms, deadline, signal: controller.signal,
    });
    for (const [stream, file] of [['stdout', stdout], ['stderr', stderr]]) {
      const capture = pipes?.[stream === 'stdout' ? 0 : 1].output.finish();
      const observed = capture?.observed_bytes ?? fs.statSync(file).size;
      if (observed > cap) { censored = true; fs.truncateSync(file, cap); }
      const bytes = fs.readFileSync(file);
      entry[stream] = {file: path.basename(file), observed_bytes: observed, bytes: bytes.length,
        sha256: hash(bytes), ...(capture ? {observed_sha256: capture.observed_sha256,
          transport_error: capture.error} : {})};
    }
    Object.assign(entry, {exit: result.status, signal: result.signal, timed_out: result.timed_out,
      cancelled: result.cancelled, cleanup_confirmed: result.cleanup_confirmed,
      cleanup_errors: result.cleanup_errors, cleanup_method: result.cleanup_method,
      error: result.error?.message || null, censored, finished_at: new Date().toISOString()});
    assert.equal(censored, false, 'CENSORED: command output exceeded finite byte policy');
    assert.ok(!entry.stdout.transport_error && !entry.stderr.transport_error, 'Output transport failed');
    assert.equal(result.timed_out, false, 'Command timed out');
    assert.equal(result.cancelled, false, 'Command cancelled');
    assert.equal(result.cleanup_confirmed, true, 'Owned process cleanup unconfirmed');
    assert.deepEqual(result.cleanup_errors, [], 'Owned process cleanup failed');
    assert.equal(result.error, null, 'Owned process supervisor failed');
    assert.equal(result.signal, null, 'Command terminated by signal');
    assert.equal(result.status, descriptor.exit ?? 0, `Unexpected command exit: ${descriptor.id}`);
    entry.status = 'COMPLETE';
    return {status: result.status, signal: result.signal, error: undefined,
      stdout: fs.readFileSync(stdout, 'utf8'), stderr: fs.readFileSync(stderr, 'utf8')};
  } finally {
    clearInterval(watch);
    if (pipes) pipes.forEach(pipe => pipe.output.finish());
    signal?.removeEventListener('abort', cancel);
    entry.finished_at ||= new Date().toISOString();
    save();
  }
}

async function bootstrapStage(mode, driver, env) {
  assert.ok(['bootstrap-pin', 'bootstrap-inspect', 'bootstrap-refresh', 'bootstrap',
    'bootstrap-finalize'].includes(mode));
  const root = path.join(path.dirname(driver), 'source');
  const directory = artifactDirectory(env.POSTHOC_ARTIFACTS, [driver, root]);
  if (mode === 'bootstrap-pin') fs.mkdirSync(directory);
  else assert.equal(fs.realpathSync(directory), directory);
  const report = {schema: 2, kind: mode, status: 'FAIL_UNRUN', passed: false,
    qualification: POLICY.qualification, policy: POLICY, native_baseline: null,
    watermark: null, commands: [], started_at: new Date().toISOString(),
    remaining_debt: 'All other CLI/feature/SDK/native obligations; no affected/full qualification'};
  const output = path.join(directory, `${mode}.json`);
  const save = () => {
    const body = `${JSON.stringify(report, null, 2)}\n`;
    assert.ok(Buffer.byteLength(body) <= LIMITS.artifactBytes, 'Receipt exceeds byte budget');
    fs.writeFileSync(output, body);
  };
  const controller = new AbortController();
  const cancel = () => controller.abort();
  assert.equal(fs.existsSync(output), false, 'Stage receipt already exists; no replay');
  let context, before, inventory, smoke, privateRoot, privateCreated = false;
  let exit = 0;
  try {
    save();
    context = bootstrapContext(driver, env);
    report.context = portableContext(context);
    if (mode === 'bootstrap-pin') {
      assert.equal(context.job, 'prepare');
      const defaultBranch = await providerJson('branches/main', env.GH_TOKEN);
      assert.equal(defaultBranch.name, 'main');
      assert.equal(defaultBranch.commit?.sha, context.driver, 'Driver is not current trusted default main');
      matchResolved(context, await resolveHead(env.GH_TOKEN));
      const history = await providerJson(
        'actions/workflows/rust-native-posthoc.yml/runs?event=workflow_dispatch&per_page=100', env.GH_TOKEN);
      report.admission = admitHistory(context, history);
      report.status = 'PINNED_PARTIAL_UNRUN';
      fs.appendFileSync(env.GITHUB_OUTPUT, `source_sha=${context.source}\n` +
        `matrix=${JSON.stringify({include: context.platforms.map(platform =>
          ({platform, image: HOSTS[platform].image}))})}\n`);
    } else {
      const pin = readJson(path.join(directory, 'bootstrap-pin.json'));
      assert.equal(pin.status, 'PINNED_PARTIAL_UNRUN');
      assert.deepEqual(pin.context, portableContext(context), 'Pin provenance mismatch');
      fixedRoots(driver, env.GITHUB_WORKSPACE);
      repositoryRoot(root);
      const parents = git(root, ['show', '-s', '--format=%P', 'HEAD']).split(' ');
      git(root, ['merge-base', '--is-ancestor', ANCHOR, context.source]);
      report.source = require('./integration-smoke-recovery.cjs').manualPin({
        event: context.event, requested_source: context.source, requested_base: context.base,
      }, context.source, git(root, ['rev-parse', 'HEAD']), parents, true);
      before = snapshot(root);
      verifyBootstrapInputs(before);
      report.source_inventory = before.sha256;
      report.source_tree = before.tree;
      if (mode === 'bootstrap-inspect') {
        matchResolved(context, await resolveHead(env.GH_TOKEN));
        writeJson(path.join(directory, 'bootstrap-source-before.json'), before);
        report.status = 'ADMITTED_PARTIAL_UNRUN';
      } else {
        const inspection = readJson(path.join(directory, 'bootstrap-inspect.json'));
        assert.equal(inspection.status, 'ADMITTED_PARTIAL_UNRUN');
        assert.deepEqual(inspection.context, report.context);
        const original = readJson(path.join(directory, 'bootstrap-source-before.json'));
        unchanged(original, before);
        if (mode === 'bootstrap-refresh') {
          report.runner = bootstrapHost(env.POSTHOC_PLATFORM, env);
          matchResolved(context, await resolveHead(env.GH_TOKEN));
          report.status = 'FRESH_PARTIAL_UNRUN';
        } else if (mode === 'bootstrap-finalize') {
          matchResolved(context, await resolveHead(env.GH_TOKEN));
          const native = readJson(path.join(directory, 'bootstrap.json'));
          assert.deepEqual(native.context, report.context);
          assert.equal(native.status, 'COMPLETE_PARTIAL');
          assert.equal(native.passed, true);
          assert.equal(native.source_inventory, before.sha256);
          assert.equal(native.cli_cases?.length, 6);
          assert.deepEqual(native.core?.tests, CORE_TESTS);
          assert.ok(native.commands.length === 13 && native.commands.every(command =>
            command.status === 'COMPLETE' && command.cleanup_confirmed && !command.censored));
          assert.equal(native.private_cleanup?.confirmed, true);
          assert.ok(native.executor_elapsed_seconds > 0 &&
            native.executor_elapsed_seconds <= LIMITS.executorSeconds);
          for (const command of native.commands) {
            for (const stream of ['stdout', 'stderr']) {
              const output = command[stream];
              assert.match(output.file, /^[0-9]{2}-(stdout|stderr)\.log$/);
              const bytes = fs.readFileSync(path.join(directory, output.file));
              assert.ok(bytes.length <= POLICY.outputBytes);
              assert.equal(bytes.length, output.bytes);
              assert.equal(bytes.length, output.observed_bytes);
              assert.equal(hash(bytes), output.sha256, 'Native output hash mismatch');
            }
          }
          assert.deepEqual(verifyCoreOutput(fs.readFileSync(path.join(directory,
            native.commands.find(command => command.id === 'fabric-core:lib').stdout.file), 'utf8')),
          native.core);
          report.native_receipt_sha256 = hash(fs.readFileSync(path.join(directory, 'bootstrap.json')));
          report.freshness = {source: context.source, observed_at: new Date().toISOString()};
          report.passed = true;
          report.status = 'COMPLETE_PARTIAL_CURRENT';
        } else {
          requireNoProviderTokens(env);
          assert.ok(context.platforms.includes(env.POSTHOC_PLATFORM));
          const refresh = readJson(path.join(directory, 'bootstrap-refresh.json'));
          assert.equal(refresh.status, 'FRESH_PARTIAL_UNRUN');
          assert.deepEqual(refresh.context, report.context);
          assert.equal(refresh.source_inventory, before.sha256);
          report.runner = bootstrapHost(env.POSTHOC_PLATFORM, env);
          process.once('SIGINT', cancel);
          process.once('SIGTERM', cancel);
          const executorStarted = Date.now();
          const deadline = executorStarted + LIMITS.executorSeconds * 1000;
          privateRoot = artifactDirectory(env.POSTHOC_PRIVATE, [driver, root, directory]);
          assert.equal(path.dirname(privateRoot), fs.realpathSync(env.RUNNER_TEMP));
          assert.equal(path.basename(privateRoot),
            `posthoc-${context.run_id}-${context.run_attempt}-${env.POSTHOC_PLATFORM}`);
          fs.mkdirSync(privateRoot);
          privateCreated = true;
          const directories = {HOME: path.join(privateRoot, 'home'),
            CARGO_HOME: path.join(privateRoot, 'cargo'), CARGO_TARGET_DIR: path.join(privateRoot, 'target'),
            RUSTUP_HOME: path.join(privateRoot, 'rustup'), TMPDIR: path.join(privateRoot, 'tmp')};
          for (const value of Object.values(directories)) fs.mkdirSync(value);
          const buildEnv = commandEnvironment(env, directories);
          const savedPath = process.env.PATH;
          // Source helpers' internal Git and supervisor children inherit only this allowlist.
          const ambient = {...process.env};
          for (const name of Object.keys(process.env)) delete process.env[name];
          Object.assign(process.env, buildEnv);
          let sourceBefore;
          try {
            smoke = require(path.join(root, 'rust/scripts/integration-smoke.cjs'));
            inventory = require(path.join(root, 'rust/scripts/validate-impact.cjs'));
            const {executeOwned} = require(path.join(root, 'rust/scripts/validation-process.cjs'));
            assert.deepEqual(smoke.PROFILES, require('./integration-smoke-recovery.cjs').PROFILES);
            assert.equal(smoke.BRANCH, BRANCH);
            sourceBefore = inventory.sourceInventory(root);
            writeJson(path.join(directory, 'native-source-before.json'), sourceBefore);
            report.lock_sha256 = hash(fs.readFileSync(path.join(root, 'rust/Cargo.lock')));
            const toolchain = fs.readFileSync(path.join(root, 'rust/rust-toolchain.toml'), 'utf8')
              .match(/^channel\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"$/m)?.[1];
            assert.equal(toolchain, '1.99.1', 'Toolchain policy requires re-audit');
            const manifest = fs.readFileSync(path.join(root, 'rust/Cargo.toml'), 'utf8');
            const version = manifest.match(/^version\s*=\s*"([^"]+)"$/m)?.[1];
            assert.ok(version);
            assert.match(manifest, /^fabric-core = \{ path = "engine\/core\/contracts" \}$/m);
            const pipePaths = Array.from({length: 12}, (_, index) =>
              ['stdout', 'stderr'].map(stream =>
                path.join(privateRoot, `${String(index + 2).padStart(2, '0')}-${stream}.pipe`))).flat();
            const pipeSetup = await boundedCommand(executeOwned, {id: 'private-output-pipes',
              program: 'python3', args: ['-B', '-c',
                'import os,sys; [os.mkfifo(p, 0o600) for p in sys.argv[1:]]', ...pipePaths],
              cwd: privateRoot, timeout_ms: 5000},
            {directory, deadline, env: buildEnv, report, save, signal: controller.signal});
            assert.equal(pipeSetup.stdout, '');
            assert.equal(pipeSetup.stderr, '');
            const run = (id, program, args, extra = {}) => boundedCommand(executeOwned,
              {id, program, args, cwd: path.join(root, 'rust'), timeout_ms: 600000, ...extra},
              {directory, deadline, env: buildEnv, report, save, signal: controller.signal,
                transportRoot: privateRoot});
            await run('private-toolchain', 'rustup', ['toolchain', 'install', toolchain, '--profile', 'minimal']);
            buildEnv.RUSTUP_TOOLCHAIN = toolchain;
            process.env.RUSTUP_TOOLCHAIN = toolchain;
            const rustc = await run('rustc-host', 'rustc', ['--version', '--verbose'], {timeout_ms: 30000});
            const cargo = await run('cargo-version', 'cargo', ['--version'], {timeout_ms: 30000});
            assert.ok(rustc.stdout.startsWith(`rustc ${toolchain} `));
            assert.ok(cargo.stdout.startsWith(`cargo ${toolchain} `));
            assert.equal(rustc.stdout.match(/^host: ([^\r\n]+)\r?$/m)?.[1], report.runner.triple);
            report.toolchain = {pin: toolchain, rustc: rustc.stdout, cargo: cargo.stdout};
            report.cli_cases = [];
            report.binaries = [];
            const sandbox = path.join(privateRoot, 'sandbox');
            fs.mkdirSync(sandbox);
            const isolated = smoke.isolatedEnvironment(directories.HOME, sandbox, {});
            for (const [index, profile] of smoke.PROFILES.entries()) {
              await run(`build:${profile.bin}`, 'cargo', smoke.buildArgs(profile, index !== 0));
              const executable = path.join(directories.CARGO_TARGET_DIR, 'debug', profile.bin);
              const binaryHash = hash(fs.readFileSync(executable));
              report.binaries.push({bin: profile.bin, sha256: binaryHash});
              for (const item of smoke.smokeCases(profile.bin)) {
                const result = await boundedCommand(executeOwned, {id: `${profile.bin}:${item.kind}`,
                  program: executable, args: item.args, cwd: sandbox, timeout_ms: 10000, exit: item.exit},
                {directory, deadline, env: isolated, report, save, signal: controller.signal,
                  transportRoot: privateRoot});
                smoke.verifyOutput(profile.bin, version, item, result);
                report.cli_cases.push({bin: profile.bin, kind: item.kind, exit: result.status,
                  stdout_sha256: hash(result.stdout), stderr_sha256: hash(result.stderr)});
              }
              assert.equal(hash(fs.readFileSync(executable)), binaryHash, 'Executable mutated');
            }
            const core = await run('fabric-core:lib', 'cargo',
              ['test', '--locked', '--offline', '-p', 'fabric-core', '--lib', '--no-default-features',
                '--', '--test-threads=2']);
            report.core = verifyCoreOutput(core.stdout);
            assert.equal(report.cli_cases.length, 6);
            report.executor_elapsed_seconds = (Date.now() - executorStarted) / 1000;
            assert.ok(Date.now() <= deadline, 'Native executor budget exceeded');
            report.passed = true;
            report.status = 'COMPLETE_PARTIAL';
          } finally {
            try {
              if (sourceBefore) {
                const sourceAfter = inventory.sourceInventory(root);
                writeJson(path.join(directory, 'native-source-after.json'), sourceAfter);
                writeJson(path.join(directory, 'native-source-differences.json'),
                  inventory.sourceDifferences(sourceBefore, sourceAfter));
                smoke.requireUnchanged(sourceBefore, sourceAfter);
              }
            } finally {
              for (const name of Object.keys(process.env)) delete process.env[name];
              Object.assign(process.env, ambient);
              assert.equal(process.env.PATH, savedPath);
            }
          }
        }
      }
    }
  } catch (error) {
    report.passed = false;
    report.status = report.commands.length ? 'FAIL_OR_CENSORED_PARTIAL' : 'FAIL_UNRUN';
    const last = report.commands.at(-1);
    report.classification = !last ? 'GUARD_PROVIDER_OR_PREFLIGHT_UNRUN' :
      last.censored ? 'OUTPUT_CENSORED' : last.timed_out ? 'TIMEOUT_OR_BUDGET_CENSORED' :
        last.cancelled ? 'CANCELLED' : last.cleanup_confirmed !== true ?
          'SUPERVISOR_OR_CLEANUP_INFRASTRUCTURE_FAILURE' :
          last.id.startsWith('build:') ? 'SOURCE_BUILD_FAILURE' :
            last.id === 'fabric-core:lib' ? 'SOURCE_TEST_FAILURE' :
              last.id.startsWith('fab-') ? 'SOURCE_CLI_FAILURE' : 'TOOLCHAIN_OR_SETUP_FAILURE';
    report.error = error.message.slice(0, 8192);
    report.error_sha256 = hash(error.message);
    report.error_truncated = error.message.length > report.error.length;
    console.error(`${report.status}: ${error.message}`);
    exit = 1;
  } finally {
    process.removeListener('SIGINT', cancel);
    process.removeListener('SIGTERM', cancel);
    const finalized = finalizeBootstrap(report, [
      ['source_integrity', () => { if (before) {
        const after = snapshot(root);
        unchanged(before, after);
        report.source_after = after.sha256;
        if (mode === 'bootstrap') writeJson(path.join(directory, 'bootstrap-source-after.json'), after);
      }}],
      ['driver_integrity', () => { if (context) {
        clean(driver);
        assert.equal(git(driver, ['rev-parse', 'HEAD']), context.driver);
      }}],
      ['private_cleanup', () => {
        report.private_cleanup = {created: privateCreated, confirmed: false,
          target_cache: 'cold private target/cargo/rustup only; no save or restore'};
        if (privateCreated && fs.existsSync(privateRoot)) {
          fs.rmSync(privateRoot, {recursive: true, force: false});
        }
        report.private_cleanup.confirmed = !privateCreated || !fs.existsSync(privateRoot);
        assert.equal(report.private_cleanup.confirmed, true);
      }],
      ['artifact_budget', () => {
        assert.ok(bundleBytes(directory) <= POLICY.bundleBytes, 'Artifact bundle exceeds finite byte policy');
      }],
      ['driver_hashes', () => {
        report.driver_files = Object.fromEntries([WORKFLOW, '.github/scripts/rust-native-posthoc.cjs',
          '.github/scripts/integration-smoke-recovery.cjs'].map(file =>
          [file, hash(fs.readFileSync(path.join(driver, file)))]));
      }],
    ]);
    if (!finalized) exit = 1;
    report.finished_at = new Date().toISOString();
    save();
  }
  return exit;
}

async function main(argv, env = process.env) {
  if (argv.length === 1 && argv[0] === '--help') {
    console.log('rust-native-posthoc.cjs pin|inspect|bootstrap-pin|bootstrap-inspect|bootstrap-refresh|bootstrap|bootstrap-finalize\n' +
      'Full/affected native execution is UNRUN; bootstrap-v1 is a separate fixed partial workload.\n' +
      'Inspect exits 78 (NOT_ADMITTED); see rust-native-posthoc.md for the finite bootstrap protocol.');
    return 0;
  }
  if (argv.length === 1 && argv[0].startsWith('bootstrap')) {
    return bootstrapStage(argv[0], fs.realpathSync(path.resolve(__dirname, '../..')), env);
  }
  assert.ok(argv.length === 1 && ['pin', 'inspect'].includes(argv[0]), 'Usage: pin|inspect|bootstrap-*|--help');
  return runStage(argv[0], fs.realpathSync(path.resolve(__dirname, '../..')), env);
}

module.exports = {REPOSITORY, BRANCH, MAIN_BASE, ANCHOR, REFERENCE_SOURCE, WORKFLOW,
  LIMITS, ADMISSION, BRIDGE_BLOBS, sha, requestContext, resolveHead, matchResolved,
  proveLineage, gitEnvironment, git, clean, repositoryRoot, checkoutAttributes,
  verifyCheckoutBytes, snapshot, verifyBridge,
  unchanged, debt, artifactDirectory, writeJson, fixedRoots, runStage, main,
  POLICY, HOSTS, BOOTSTRAP_BLOBS, CORE_TESTS, bootstrapContext, verifyBootstrapInputs, bootstrapHost,
  providerJson, runTitle, admitHistory, portableContext, bundleBytes, verifyCoreOutput,
  commandEnvironment, requireNoProviderTokens, finalizeBootstrap, pipeOutput, boundedCommand, bootstrapStage};
if (require.main === module) {
  main(process.argv.slice(2)).then(code => { process.exitCode = code; })
    .catch(error => { console.error(error.message); process.exitCode = 1; });
}
