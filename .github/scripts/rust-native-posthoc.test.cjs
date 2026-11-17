// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {spawnSync} = require('node:child_process');
const {test} = require('node:test');
const p = require('./rust-native-posthoc.cjs');

const driver = '4'.repeat(40), source = p.REFERENCE_SOURCE, base = 'ce50f369f2713ed1a2599099458f19bb0750d3e3';
const inputs = () => ({source_sha: source, base_sha: base, mode: 'full', route: 'manual', batch_id: ''});
const event = () => ({repository: {full_name: p.REPOSITORY, default_branch: 'main'}, inputs: inputs()});
const environment = () => ({GITHUB_EVENT_NAME: 'workflow_dispatch', GITHUB_REPOSITORY: p.REPOSITORY,
  GITHUB_REF: 'refs/heads/main', GITHUB_WORKFLOW_REF: `${p.REPOSITORY}/${p.WORKFLOW}@refs/heads/main`,
  GITHUB_SERVER_URL: 'https://github.com', GITHUB_API_URL: 'https://api.github.com',
  GITHUB_SHA: driver, GITHUB_WORKFLOW_SHA: driver, GITHUB_JOB: 'prepare',
  GITHUB_RUN_ID: '123', GITHUB_RUN_ATTEMPT: '2', REQUESTED_SOURCE_SHA: source,
  REQUESTED_BASE_SHA: base, REQUESTED_MODE: 'full', REQUESTED_ROUTE: 'manual', REQUESTED_BATCH_ID: ''});
const context = () => p.requestContext(environment(), event(), driver);

function fixture(t, leaf = '') {
  const container = fs.mkdtempSync(path.join(os.tmpdir(), 'posthoc-offline-'));
  t.after(() => fs.rmSync(container, {recursive: true, force: true}));
  const root = leaf ? path.join(container, leaf) : container;
  if (leaf) fs.mkdirSync(root);
  const run = args => {
    const result = spawnSync('git', args, {cwd: root, encoding: 'utf8', shell: false,
      env: p.gitEnvironment(), timeout: 10000});
    assert.ifError(result.error);
    assert.equal(result.status, 0, result.stderr);
    return result.stdout.trim();
  };
  run(['init', '--quiet']);
  run(['config', 'core.autocrlf', 'false']);
  run(['config', 'user.name', 'Offline fixture']);
  run(['config', 'user.email', 'fixture@example.invalid']);
  run(['remote', 'add', 'origin', `https://github.com/${p.REPOSITORY}.git`]);
  fs.writeFileSync(path.join(root, 'Cargo.lock'), 'offline fixture only\n');
  fs.writeFileSync(path.join(root, '.gitignore'), 'ignored-output\n');
  run(['add', '.']);
  run(['-c', 'commit.gpgsign=false', 'commit', '--quiet', '-m', 'Offline fixture, not native evidence']);
  return {root, run};
}

function stageFixture(t) {
  const sourceFixture = fixture(t, 'source'), workspace = path.dirname(sourceFixture.root);
  const driverRoot = path.join(workspace, 'driver');
  fs.mkdirSync(driverRoot);
  const request = {...context(), source: sourceFixture.run(['rev-parse', 'HEAD'])};
  const env = {...environment(), GITHUB_WORKSPACE: workspace,
    POSTHOC_ARTIFACTS: path.join(workspace, 'receipts'), GITHUB_OUTPUT: path.join(workspace, 'output')};
  // Only unavailable provider/main-lineage/bridge boundaries are fixtures.
  // Actual source Git, filesystem guards, inventories and receipt writes run.
  const services = {readContext: () => request, resolveHead: async () => request.source,
    verifyBridge: inventory => inventory.files,
    clean: (root, commands) => root === driverRoot ? undefined : p.clean(root, commands),
    git: (root, args, commands) => {
      if (root === driverRoot) {
        assert.deepEqual(args, ['rev-parse', 'HEAD']);
        return driver;
      }
      if (args[0] === 'merge-base') return '';
      return p.git(root, args, commands);
    }};
  const read = stage => JSON.parse(fs.readFileSync(path.join(env.POSTHOC_ARTIFACTS, `${stage}.json`), 'utf8'));
  return {...sourceFixture, driverRoot, workspace, request, env, services, read};
}

test('default-main genuine manual context keeps driver, DUT, base, event and attempt separate', () => {
  assert.deepEqual(context(), {repository: p.REPOSITORY, branch: p.BRANCH, event: 'workflow_dispatch',
    ref: 'refs/heads/main', driver, driver_base: p.MAIN_BASE,
    workflow_ref: `${p.REPOSITORY}/${p.WORKFLOW}@refs/heads/main`, workflow_sha: driver,
    source, base, anchor: p.ANCHOR, mode: 'full', route: 'manual', batch_id: null,
    run_id: '123', job: 'prepare', run_attempt: '2'});
});

test('batched mode is a real dispatch with explicit batch provenance, not a synthetic push', () => {
  const env = {...environment(), REQUESTED_ROUTE: 'batch', REQUESTED_BATCH_ID: 'merged-17.1',
    REQUESTED_MODE: 'affected'};
  const ev = {...event(), inputs: {...inputs(), route: 'batch', batch_id: 'merged-17.1', mode: 'affected'}};
  const request = p.requestContext(env, ev, driver);
  assert.equal(request.route, 'batch');
  assert.equal(request.batch_id, 'merged-17.1');
  assert.equal(request.event, 'workflow_dispatch');
  assert.equal(request.mode, 'affected');
  assert.equal(p.debt(request.mode).admitted, false);
});

for (const value of [undefined, null, 12, '', '0'.repeat(40), source.slice(0, 7),
  source.toUpperCase(), ` ${source}`, `${source}\n`, `${source};echo unsafe`, '--help',
  `$(echo ${source})`, 'refs/heads/main', `${source}/../main`]) {
  test(`invalid pin rejected: ${JSON.stringify(value)}`, () => {
    assert.throws(() => p.sha(value));
    for (const [input, variable] of [['source_sha', 'REQUESTED_SOURCE_SHA'], ['base_sha', 'REQUESTED_BASE_SHA']]) {
      assert.throws(() => p.requestContext({...environment(), [variable]: value},
        {...event(), inputs: {...inputs(), [input]: value}}, driver));
    }
  });
}

for (const [key, value] of [
  ['GITHUB_EVENT_NAME', 'push'], ['GITHUB_EVENT_NAME', 'schedule'],
  ['GITHUB_EVENT_NAME', 'pull_request'], ['GITHUB_EVENT_NAME', 'repository_dispatch'],
  ['GITHUB_REPOSITORY', 'foreign/fork'], ['GITHUB_REF', `refs/heads/${p.BRANCH}`],
  ['GITHUB_REF', 'refs/heads/main-other'], ['GITHUB_SHA', source],
  ['GITHUB_WORKFLOW_SHA', source], ['GITHUB_WORKFLOW_REF', `${p.REPOSITORY}/${p.WORKFLOW}@main`],
  ['GITHUB_SERVER_URL', 'https://foreign.invalid'], ['GITHUB_API_URL', 'https://foreign.invalid'],
  ['GITHUB_JOB', 'native'], ['GITHUB_RUN_ID', '0'], ['GITHUB_RUN_ATTEMPT', '-1'],
]) {
  test(`foreign or unsupported context rejected: ${key}=${value}`, () => {
    assert.throws(() => p.requestContext({...environment(), [key]: value}, event(), driver));
  });
}

test('source, driver, base, dispatch and provider pin mismatches fail closed', () => {
  assert.throws(() => p.requestContext(environment(), event(), source));
  for (const repository of [{full_name: 'foreign/fork', default_branch: 'main'},
    {full_name: p.REPOSITORY, default_branch: p.BRANCH}]) {
    assert.throws(() => p.requestContext(environment(), {...event(), repository}, driver));
  }
  for (const changed of [{source_sha: driver}, {base_sha: source}, {mode: 'FULL'}, {route: 'scheduled'},
    {source_sha: base}, {command: 'cargo test'}, {budget: 'unlimited'}, {batch_id: 'unexpected'}]) {
    assert.throws(() => p.requestContext(environment(), {...event(), inputs: {...inputs(), ...changed}}, driver));
  }
  assert.throws(() => p.matchResolved(context(), base), /fixed branch head/);
  assert.equal(p.matchResolved(context(), source).resolved_source, source);
  for (const [actual, anchor, ancestor] of [[driver, true, true], [source, false, true], [source, true, false]]) {
    assert.throws(() => p.proveLineage(context(), actual, anchor, ancestor));
  }
  p.proveLineage(context(), source, true, true);
});

test('invalid batch text, injection and silently defaulted full mode are not accepted', () => {
  for (const batch_id of ['', 'a'.repeat(65), 'batch\nother', '--help', '$(pwd)', '../../main']) {
    assert.throws(() => p.requestContext({...environment(), REQUESTED_ROUTE: 'batch', REQUESTED_BATCH_ID: batch_id},
      {...event(), inputs: {...inputs(), route: 'batch', batch_id}}, driver));
  }
  for (const field of ['mode', 'route', 'source_sha', 'base_sha', 'batch_id']) {
    const incomplete = inputs();
    delete incomplete[field];
    assert.throws(() => p.requestContext(environment(), {...event(), inputs: incomplete}, driver));
  }
});

test('fixed provider lookup is read-only, bounded, authenticated and never redirects', async () => {
  const resolved = await p.resolveHead('fixture-token-not-a-credential', async (url, opts) => {
    assert.equal(url, `https://api.github.com/repos/${p.REPOSITORY}/branches/${encodeURIComponent(p.BRANCH)}`);
    assert.equal(opts.method, 'GET');
    assert.equal(opts.redirect, 'error');
    assert.equal(opts.headers.Authorization, 'Bearer fixture-token-not-a-credential');
    assert.ok(opts.signal instanceof AbortSignal);
    return {status: 200, json: async () => ({name: p.BRANCH, commit: {sha: source}})};
  });
  assert.equal(resolved, source);
  await assert.rejects(p.resolveHead('', () => assert.fail('No anonymous fallback')));
  for (const response of [{status: 403}, {status: 404}, {status: 500},
    {status: 200, json: async () => ({name: 'main', commit: {sha: source}})},
    {status: 200, json: async () => ({name: p.BRANCH, commit: {sha: '0'.repeat(40)}})}]) {
    await assert.rejects(p.resolveHead('fixture', async () => response));
  }
  for (const message of ['provider timeout', 'provider canceled', 'network unavailable']) {
    await assert.rejects(p.resolveHead('fixture', async () => { throw new Error(message); }), new RegExp(message));
  }
});

test('Git subprocess boundary strips all tokens, event overrides, hooks and command injection', () => {
  const env = p.gitEnvironment({PATH: '/fixture/bin', GH_TOKEN: 'never-forward',
    GITHUB_TOKEN: 'never-forward', FAB_TOKEN: 'never-forward', RUSTC_WRAPPER: 'unsafe',
    GIT_CONFIG_COUNT: '1', GITHUB_SHA: source, CARGO_HOME: '/unsafe', NODE_OPTIONS: '--require unsafe'});
  assert.equal(env.PATH, '/fixture/bin');
  for (const name of ['GH_TOKEN', 'GITHUB_TOKEN', 'FAB_TOKEN', 'RUSTC_WRAPPER', 'GIT_CONFIG_COUNT',
    'GITHUB_SHA', 'CARGO_HOME', 'NODE_OPTIONS']) assert.equal(Object.hasOwn(env, name), false);
  assert.equal(env.GIT_NO_REPLACE_OBJECTS, '1');
  assert.equal(env.GIT_CONFIG_GLOBAL, process.platform === 'win32' ? 'NUL' : os.devNull);
  assert.equal(env.GIT_TERMINAL_PROMPT, '0');
});

test('actual clean Git bytes and manifest-lock inventory are hashed, not a mocked native run', t => {
  const {root, run} = fixture(t), commands = [];
  assert.equal(p.repositoryRoot(root, commands), fs.realpathSync(root));
  const before = p.snapshot(root, commands);
  assert.equal(before.head, run(['rev-parse', 'HEAD']));
  assert.equal(before.tree, run(['rev-parse', 'HEAD^{tree}']));
  assert.equal(before.files.find(file => file.path === 'Cargo.lock').bytes, 21);
  assert.match(before.files.find(file => file.path === 'Cargo.lock').sha256, /^[a-f0-9]{64}$/);
  p.unchanged(before, p.snapshot(root));
  assert.ok(commands.length > 0 && commands.every(command => command.program === 'git' && command.exit === 0));
  assert.throws(() => p.verifyBridge(before), /Missing source input/);
});

test('dirty, staged, ignored, untracked and stat-cache-hidden source mutations fail', t => {
  const {root, run} = fixture(t);
  fs.writeFileSync(path.join(root, 'ignored-output'), 'not evidence');
  assert.throws(() => p.snapshot(root), /dirty|ignored/);
  fs.unlinkSync(path.join(root, 'ignored-output'));
  fs.writeFileSync(path.join(root, 'untracked'), 'not evidence');
  assert.throws(() => p.snapshot(root), /dirty/);
  fs.unlinkSync(path.join(root, 'untracked'));
  fs.writeFileSync(path.join(root, 'Cargo.lock'), 'changed bytes\n');
  assert.throws(() => p.snapshot(root), /dirty/);
  run(['add', 'Cargo.lock']);
  assert.throws(() => p.snapshot(root), /dirty/);
  run(['-c', 'commit.gpgsign=false', 'commit', '--quiet', '-m', 'Changed fixture']);
  run(['update-index', '--assume-unchanged', 'Cargo.lock']);
  fs.writeFileSync(path.join(root, 'Cargo.lock'), 'hidden mutation\n');
  assert.throws(() => p.snapshot(root), /Actual source bytes differ/);
});

test('committed source changes invalidate inventory and never reuse a watermark', t => {
  const {root, run} = fixture(t), before = p.snapshot(root);
  fs.writeFileSync(path.join(root, 'Cargo.lock'), 'new fixture\n');
  run(['add', '.']);
  run(['-c', 'commit.gpgsign=false', 'commit', '--quiet', '-m', 'New fixture source']);
  const after = p.snapshot(root);
  assert.notEqual(after.sha256, before.sha256);
  assert.throws(() => p.unchanged(before, after), /DUT revision changed/);
  assert.throws(() => p.unchanged(before, {...before, sha256: 'changed'}), /inventory changed/);
  assert.equal(p.debt('affected').watermark, null);
});

test('foreign checkout origin and symlink/submodule tree entries cannot qualify', t => {
  const {root, run} = fixture(t);
  run(['remote', 'set-url', 'origin', 'https://github.com/foreign/fork.git']);
  assert.throws(() => p.repositoryRoot(root), /foreign repository/);
  const blob = run(['rev-parse', 'HEAD:Cargo.lock']);
  run(['update-index', '--add', '--cacheinfo', `120000,${blob},link`]);
  run(['-c', 'commit.gpgsign=false', 'commit', '--quiet', '-m', 'Symlink metadata fixture']);
  assert.throws(() => p.snapshot(root));
});

test('helper API pin change fails rather than copying or silently trusting another executor', () => {
  const files = [...Object.entries(p.BRIDGE_BLOBS).map(([name, blob]) => ({path: name, blob})),
    ...['rust/Cargo.toml', 'rust/Cargo.lock', 'rust/rust-toolchain.toml',
      'rust/validation/impact-graph.json'].map(name => ({path: name, blob: '5'.repeat(40)}))];
  assert.equal(p.verifyBridge({files}).length, 12);
  const changed = files.map(file => ({...file}));
  changed[0].blob = '6'.repeat(40);
  assert.throws(() => p.verifyBridge({files: changed}), /API changed/);
});

test('affected without native baseline stays NOT_ADMITTED; even full gets no implicit budget', () => {
  for (const mode of ['full', 'affected']) {
    const receipt = p.debt(mode);
    assert.equal(receipt.status, 'NOT_ADMITTED');
    assert.equal(receipt.native_status, 'UNRUN');
    assert.equal(receipt.admitted, false);
    assert.equal(receipt.selection, null);
    assert.equal(receipt.selection_digest, null);
    assert.equal(receipt.native_baseline, null);
    assert.equal(receipt.complete_aggregate, false);
    assert.equal(receipt.watermark, null);
    assert.deepEqual(receipt.executed_commands, []);
    assert.equal(receipt.tests_passed, 0);
    assert.deepEqual(receipt.coverage.map(row => [row.platform, row.status, row.steps, row.acquisition, row.arch]),
      [['linux', 'UNRUN', 0, null, null], ['macos', 'UNRUN', 0, null, null]]);
    assert.deepEqual(receipt.resource_policy, {enabled: false, maxShards: 0, maxParallel: 0,
      maxTests: 0, nativeBaseline: null});
    assert.deepEqual(receipt.future_execution_ceiling, {job_seconds: 900, executor_seconds: 720, command_seconds: 600});
  }
  assert.ok(p.debt('affected').reasons.includes('full-native-baseline-required'));
  assert.throws(() => p.debt('unknown'));
});

test('artifact paths are external, bounded and exclusive; failures cannot overwrite earlier proof', t => {
  const {root} = fixture(t), output = path.join(os.tmpdir(), `posthoc-artifact-${path.basename(root)}`);
  for (const value of ['', '.', root, path.join(root, 'artifacts')]) {
    assert.throws(() => p.artifactDirectory(value, [root]));
  }
  assert.equal(p.artifactDirectory(output, [root]), output);
  const file = path.join(root, 'receipt.json');
  p.writeJson(file, {status: 'UNRUN'});
  assert.equal(JSON.parse(fs.readFileSync(file)).status, 'UNRUN');
  assert.throws(() => p.writeJson(file, {status: 'passed'}), /EEXIST/);
  assert.throws(() => p.writeJson(path.join(root, 'oversize.json'),
    {output: 'x'.repeat(p.LIMITS.artifactBytes)}), /byte budget/);
});

test('real request orchestration writes immutable-source UNRUN artifacts and exits non-green', async t => {
  const f = stageFixture(t);
  assert.equal(await p.runStage('pin', f.driverRoot, f.env, f.services), 0);
  assert.equal(fs.readFileSync(f.env.GITHUB_OUTPUT, 'utf8'), `source_sha=${f.request.source}\n`);
  assert.equal(f.read('pin').status, 'PINNED_UNRUN');
  assert.equal(await p.runStage('inspect', f.driverRoot, f.env, f.services), 78);
  const receipt = f.read('inspect');
  assert.equal(receipt.status, 'NOT_ADMITTED');
  assert.equal(receipt.source_verified, true);
  assert.equal(receipt.native_status, 'UNRUN');
  assert.equal(receipt.complete_aggregate, false);
  assert.equal(receipt.context.source, f.request.source);
  assert.notEqual(receipt.context.source, receipt.context.driver);
  assert.equal(receipt.source_before, receipt.source_after);
  assert.equal(receipt.freshness.source_still_current, true);
  assert.ok(receipt.guard_commands.length > 0);
  assert.ok(receipt.guard_commands.every(command => command.program === 'git'));
  assert.deepEqual(receipt.executed_commands, []);
  assert.equal(receipt.tests_passed, 0);
  const before = JSON.parse(fs.readFileSync(path.join(f.env.POSTHOC_ARTIFACTS, 'source-before.json')));
  const after = JSON.parse(fs.readFileSync(path.join(f.env.POSTHOC_ARTIFACTS, 'source-after.json')));
  assert.equal(before.head, f.request.source);
  assert.equal(after.sha256, before.sha256);
  assert.equal(before.sha256, receipt.source_before);
  assert.equal(receipt.watermark, null);
});

test('later target movement is recorded stale, without repinning or claiming native coverage', async t => {
  const f = stageFixture(t);
  await p.runStage('pin', f.driverRoot, f.env, f.services);
  assert.equal(await p.runStage('inspect', f.driverRoot, f.env,
    {...f.services, resolveHead: async () => base}), 78);
  const receipt = f.read('inspect');
  assert.equal(receipt.context.source, f.request.source);
  assert.equal(receipt.freshness.observed_head, base);
  assert.equal(receipt.freshness.source_still_current, false);
  assert.equal(receipt.status, 'NOT_ADMITTED');
  assert.equal(receipt.watermark, null);
});

test('moved initial head and provider failure retain partial non-admission receipts', async t => {
  for (const [resolveHead, expected] of [
    [async () => base, 'GUARD_FAILURE'],
    [async () => { throw new Error('HTTP403 fixture'); }, 'PROVIDER_LOOKUP_FAILURE_UNRUN'],
    [async () => { throw new Error('timeout fixture'); }, 'PROVIDER_LOOKUP_FAILURE_UNRUN'],
    [async () => { throw new Error('canceled lookup fixture'); }, 'PROVIDER_LOOKUP_FAILURE_UNRUN'],
  ]) {
    const f = stageFixture(t);
    assert.equal(await p.runStage('pin', f.driverRoot, f.env, {...f.services, resolveHead}), 1);
    const receipt = f.read('pin');
    assert.equal(receipt.status, expected);
    assert.equal(receipt.native_status, 'UNRUN');
    assert.equal(receipt.admitted, false);
    assert.ok(receipt.error);
    assert.equal(fs.existsSync(f.env.GITHUB_OUTPUT), false);
  }
});

test('source mutation on a failed inspection stays failed with before evidence retained', async t => {
  const f = stageFixture(t);
  await p.runStage('pin', f.driverRoot, f.env, f.services);
  const mutate = async () => {
    fs.writeFileSync(path.join(f.root, 'Cargo.lock'), 'mutated fixture\n');
    return f.request.source;
  };
  assert.equal(await p.runStage('inspect', f.driverRoot, f.env,
    {...f.services, resolveHead: mutate}), 1);
  const receipt = f.read('inspect');
  assert.equal(receipt.status, 'SOURCE_INTEGRITY_FAILURE');
  assert.equal(receipt.native_status, 'UNRUN');
  assert.match(receipt.source_before, /^[a-f0-9]{64}$/);
  assert.equal(receipt.source_after, undefined);
  assert.ok(fs.existsSync(path.join(f.env.POSTHOC_ARTIFACTS, 'source-before.json')));
  assert.equal(receipt.watermark, null);
});

test('stale or foreign pin artifacts fail dispatch/run/attempt binding, not silently refresh', async t => {
  for (const [key, value] of [['source', base], ['driver', source], ['run_attempt', '1'],
    ['repository', 'foreign/fork'], ['base', driver]]) {
    const f = stageFixture(t);
    await p.runStage('pin', f.driverRoot, f.env, f.services);
    const pin = f.read('pin');
    pin.context[key] = value;
    fs.writeFileSync(path.join(f.env.POSTHOC_ARTIFACTS, 'pin.json'), JSON.stringify(pin));
    assert.equal(await p.runStage('inspect', f.driverRoot, f.env, f.services), 1);
    assert.equal(f.read('inspect').status, 'GUARD_FAILURE');
    assert.equal(f.read('inspect').watermark, null);
  }
});

test('missing pin, missing checkout and unavailable freshness never produce a complete receipt', async t => {
  const missing = stageFixture(t);
  fs.mkdirSync(missing.env.POSTHOC_ARTIFACTS);
  assert.equal(await p.runStage('inspect', missing.driverRoot, missing.env, missing.services), 1);
  assert.equal(missing.read('inspect').status, 'GUARD_FAILURE');
  const failed = stageFixture(t);
  await p.runStage('pin', failed.driverRoot, failed.env, failed.services);
  assert.equal(await p.runStage('inspect', failed.driverRoot, failed.env,
    {...failed.services, resolveHead: async () => { throw new Error('unavailable fixture'); }}), 1);
  assert.equal(failed.read('inspect').status, 'PROVIDER_LOOKUP_FAILURE_UNRUN');
  assert.equal(failed.read('inspect').complete_aggregate, false);
  assert.equal(fs.existsSync(path.join(failed.env.POSTHOC_ARTIFACTS, 'source-before.json')), true);
  assert.throws(() => p.fixedRoots(failed.root, failed.workspace), /fixed isolated checkout/);
  assert.throws(() => p.fixedRoots(failed.driverRoot, path.join(failed.workspace, 'missing')));
});

test('untrusted proof, baseline and budget environment text cannot admit even a fixture request', async t => {
  for (const proof of [null, [], [{status: 'skipped', steps: 0}],
    [{status: 'passed', tests_passed: 0}], [{status: 'passed', tests_ignored: 99}],
    [{head: base, status: 'passed'}], [{head: source}, {head: source}],
    [{status: 'passed', filter: 'unmatched'}]]) {
    const f = stageFixture(t);
    f.env.NATIVE_RECEIPTS = JSON.stringify(proof);
    f.env.NATIVE_BASELINE = source;
    f.env.NATIVE_RESOURCE_POLICY = '{"enabled":true,"maxShards":999999}';
    await p.runStage('pin', f.driverRoot, f.env, f.services);
    assert.equal(await p.runStage('inspect', f.driverRoot, f.env, f.services), 78);
    assert.equal(f.read('inspect').admitted, false);
    assert.equal(f.read('inspect').watermark, null);
    assert.equal(f.read('inspect').tests_passed, 0);
  }
});

test('CLI has no execution, baseline-proof intake, arbitrary path or untrusted receipt route', async () => {
  for (const argv of [[], ['run'], ['execute'], ['aggregate'], ['schedule'], ['pin', '--execute'],
    ['inspect', '--root', '/unsafe'], ['--baseline', 'forged.json']]) {
    await assert.rejects(p.main(argv, {}), /Usage/);
  }
  const result = spawnSync(process.execPath, [path.join(__dirname, 'rust-native-posthoc.cjs'), '--help'],
    {encoding: 'utf8', timeout: 10000, env: p.gitEnvironment()});
  assert.equal(result.status, 0);
  assert.match(result.stdout, /Inspect exits 78 \(NOT_ADMITTED\)/);
  assert.match(result.stdout, /native execution is UNRUN/);
});

test('workflow keeps input text in env and routes to immutable DUT without a native or timer path', () => {
  const workflow = fs.readFileSync(path.join(__dirname, '../workflows/rust-native-posthoc.yml'), 'utf8');
  const commands = [...workflow.matchAll(/^\s+(?:- )?run: (.+)$/gm)].map(match => match[1]);
  assert.deepEqual(commands, ['node --test .github/scripts/rust-native-posthoc.test.cjs',
    'node driver/.github/scripts/rust-native-posthoc.cjs pin',
    'node driver/.github/scripts/rust-native-posthoc.cjs inspect']);
  assert.ok(commands.every(command => !command.includes('${{')));
  assert.equal((workflow.match(/persist-credentials: false/g) || []).length, 3);
  assert.equal((workflow.match(/^\s+contents: read$/gm) || []).length, 1);
  assert.match(workflow, /ref: \$\{\{ steps\.pin\.outputs\.source_sha \}\}\s+path: source/);
  assert.match(workflow, /if: always\(\)/);
  assert.match(workflow, /if-no-files-found: error/);
  assert.doesNotMatch(workflow, /^\s+(schedule|cron|push|pull_request|repository_dispatch|strategy|matrix):/m);
  assert.doesNotMatch(workflow, /continue-on-error|secrets: inherit|cargo |rustc |--execute/);
});
