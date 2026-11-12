// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {commandResult, runContracts, engineeringReceipt} = require('./validate-impact.cjs');
const {createManifest, digest, sourceFingerprint, provenance, executionContext, descriptorDigest, executionChecks} =
  require('./execution-evidence.cjs');

function fixture(t) {
  const base = process.env.IMPACT_TEST_ROOT || path.join(__dirname, '..', 'target', 'validation-tests');
  fs.mkdirSync(base, {recursive: true});
  const root = fs.mkdtempSync(path.join(base, 'execution-evidence-'));
  t.after(() => fs.rmSync(root, {force: true, recursive: true}));
  const artifacts = path.join(root, 'artifacts');
  fs.mkdirSync(artifacts);
  return {root, artifacts, python: 'python', platform: 'linux', results: [],
    deadline: Date.now() + 10000, commandTimeout: 1000};
}

test('native plan metadata and execution inventories map to the same content fingerprint', () => {
  const files = [{path: 'rust/lib.rs', state: 'file', size: 12, mode: 0o644, digest: 'a'.repeat(64)},
    {path: 'deleted.txt', state: 'missing'}];
  const executed = [{path: 'deleted.txt', tracked: true, state: 'deleted'},
    {path: 'rust/lib.rs', tracked: true, state: 'file', bytes: 12,
      mode: 0o666, sha256: 'a'.repeat(64)}];
  assert.equal(sourceFingerprint(files), sourceFingerprint(executed));
  const plan = {head: 'b'.repeat(40), fingerprint: 'c'.repeat(64), inputs: {
    inventory: {scope: 'git-tracked-and-untracked', files, digest: digest(files)}}};
  const before = {files: executed, digest: 'd'.repeat(64)};
  const verified = {valid: true, fingerprint: plan.fingerprint};
  const proof = provenance(plan, before, before, verified, verified);
  assert.equal(proof.inventory_digest, digest(files));
  assert.notEqual(proof.inventory_digest, before.digest);
  assert.equal(proof.plan_source, proof.execution_source_before);
  assert.equal(proof.execution_source_before, proof.execution_source_after);
  executed[1].sha256 = 'f'.repeat(64);
  assert.notEqual(sourceFingerprint(files), sourceFingerprint(executed));
  assert.throws(() => sourceFingerprint([...files, files[0]]), /duplicate/);
  assert.throws(() => sourceFingerprint([{path: 'x', state: 'file'}]), /invalid source file hash/);
});

test('compact evidence preserves actual command outcomes, aliases, argv and toolchain output hashes', t => {
  const context = fixture(t);
  const descriptor = {id: 'default:test', aliases: ['default:test', 'profile:test'],
    program: 'cargo', args: ['test', '--lib']};
  commandResult(descriptor, context, (program, args, options) => {
    assert.equal(options.env.FAB_IDENTITY_TEST_ROOT, context.artifacts);
    assert.equal(options.env.FAB_UPDATE_TEST_ROOT, context.artifacts);
    assert.equal(options.env.FAB_CLI_V2_TEST_ROOT, context.artifacts);
    return {status: 0, stdout: '', cleanup_confirmed: true, cleanup_errors: []};
  });
  const output = 'cargo 1.94.0 (fixture)\n';
  commandResult({id: 'toolchain:cargo', program: 'cargo', args: ['--version'], capture: true},
    context, () => ({status: 0, stdout: output, cleanup_confirmed: true, cleanup_errors: []}));
  const manifest = createManifest(context, {head: 'a'.repeat(40), platform: 'linux',
    shard: 'default-core', selection_digest: 'b'.repeat(64), plan_fingerprint: 'c'.repeat(64)});
  assert.deepEqual(manifest.commands[0].aliases, descriptor.aliases);
  assert.equal(manifest.commands[0].argv_sha256, digest(descriptor.args));
  assert.equal(manifest.commands[0].descriptor_sha256, descriptorDigest(descriptor, context));
  assert.equal(manifest.commands[0].cleanup_confirmed, true);
  assert.equal(manifest.commands[0].exit, 0);
  assert.equal(manifest.commands[1].output, output);
  assert.equal(manifest.commands[1].stdout_sha256, context.results[1].stdout_sha256);
  assert.ok(!JSON.stringify(manifest).includes(context.artifacts));
  assert.ok(!Object.hasOwn(manifest.commands[0], 'args'));
  assert.ok(!Object.hasOwn(manifest.commands[0], 'env'));
});

test('failed executions retain contradictions rather than creating successful evidence', t => {
  const context = fixture(t);
  assert.throws(() => commandResult({id: 'failing', program: 'fixture', args: []}, context,
    () => ({status: 0, timed_out: true, cancelled: true,
      cleanup_confirmed: false, cleanup_errors: ['descendant remained']})), /Validation failed/);
  const record = createManifest(context, {platform: 'linux'}).commands[0];
  assert.equal(record.status, 'timed_out');
  assert.equal(record.exit, 0);
  assert.equal(record.timed_out, true);
  assert.equal(record.cancelled, true);
  assert.equal(record.cleanup_confirmed, false);
  assert.deepEqual(record.cleanup_errors, ['descendant remained']);
});

test('contract completion links actual command indices and frozen finally without marking a failed check complete', t => {
  const context = fixture(t);
  fs.mkdirSync(path.join(context.root, 'rust', 'validation'), {recursive: true});
  fs.mkdirSync(path.join(context.root, 'rust', 'tests', 'compatibility'), {recursive: true});
  fs.writeFileSync(path.join(context.root, 'rust', 'tests', 'compatibility', 'requirements.txt'),
    'jmespath==1.0.1\n');
  const checks = [{id: 'oracle-tools'}, {id: 'comparison', depends_on: ['oracle-tools']},
    {id: 'frozen-verify'}];
  fs.writeFileSync(path.join(context.root, 'rust', 'validation', 'impact-graph.json'),
    JSON.stringify({checks}));
  const pinned = path.join(context.root, 'pinned');
  fs.mkdirSync(pinned);
  context.plan = {checks: checks.map(check => check.id), profiles: []};
  const registry = {validateRegistry: () => {}, commandsFor: id => [{
    program: 'python', args: [id],
    kind: id === 'oracle-tools' ? 'prepare-oracle' :
      id === 'frozen-verify' ? 'verify-oracle' : 'command',
  }]};
  const execute = (program, args) => ({status: args[0] === 'comparison' ? 1 : 0,
    stdout: args[0] === 'oracle-tools' ? JSON.stringify({rust_root: pinned}) : '',
    cleanup_confirmed: true, cleanup_errors: []});
  assert.throws(() => runContracts(context, registry,
    command => commandResult(command, context, execute), () => ({status: 0})), /comparison/);
  assert.deepEqual(context.completedChecks, [
    {id: 'oracle-tools', profiles: [], descriptors: [{kind: 'prepare-oracle', command: 0,
      descriptor_sha256: context.results[0].descriptor_sha256}]},
    {id: 'frozen-verify', profiles: [], descriptors: [{kind: 'verify-oracle', command: 2,
      descriptor_sha256: context.results[2].descriptor_sha256}]},
  ]);
  const manifest = createManifest(context, {platform: 'linux'});
  assert.equal(manifest.commands[1].status, 'failed');
  assert.deepEqual(manifest.checks, context.completedChecks);
});

test('profile markers record their exact qualifying native profiles', t => {
  const context = fixture(t);
  fs.mkdirSync(path.join(context.root, 'rust', 'validation'), {recursive: true});
  fs.writeFileSync(path.join(context.root, 'rust', 'validation', 'impact-graph.json'),
    JSON.stringify({checks: [{id: 'interactive'}]}));
  context.plan = {checks: ['interactive'], tests: [],
    profiles: [{id: 'native', checks: ['interactive'], platforms: ['linux'], packages: ['sample'],
      tests: [{package: 'sample', target: 'lib'}]}]};
  runContracts(context, {validateRegistry: () => {}, commandsFor: () => []},
    () => assert.fail('Markers must use already executed core profiles'));
  assert.deepEqual(context.completedChecks,
    [{id: 'interactive', descriptors: [], profiles: ['native']}]);
});

test('executor and aggregate share the engineering preflight inventory contract without a fixed count', t => {
  const context = fixture(t);
  const inventory = require('./engineering-inventory.json');
  for (const platform of ['windows', 'linux', 'macos']) {
    const skipped = platform === 'windows' ? inventory.windows_skips : [];
    const report = {kind: 'engineering-unit-preflight', head: 'a'.repeat(40),
      platform, shard: 'default-core', passed: true, suites: [{
        pattern: inventory.pattern, expected: inventory.count, discovered: inventory.count,
        run: inventory.count, passed: true, failures: 0, errors: 0, expected_failures: 0, skipped,
      }]};
    const expected = {head: report.head, platform, shard: report.shard};
    const totals = {tests_passed: inventory.count - skipped.length, tests_ignored: skipped.length};
    assert.deepEqual(engineeringReceipt({...expected, report}), totals);
    const receipt = path.join(context.artifacts, `engineering-${platform}.json`);
    fs.writeFileSync(receipt, JSON.stringify(report));
    assert.deepEqual(engineeringReceipt({...expected, receipt}), totals);
    report.suites[0].run--;
    assert.throws(() => engineeringReceipt({...expected, report}), /exact native inventory/);
  }
});

test('descriptor identities normalize native paths but retain argv, frozen cwd and environment semantics', () => {
  const windows = {root: 'C:\\checkout', artifacts: 'D:\\logs', host_platform: 'win32',
    targetDir: 'E:\\cargo-output', targetSource: 'environment'};
  const posix = {root: '/opt/checkout', artifacts: '/mnt/logs', host_platform: 'linux',
    targetDir: '/cache/cargo-output', targetSource: 'environment'};
  const native = {program: 'python', args: ['-B', 'tests\\compatibility\\compare.py',
    '--probe', 'E:\\cargo-output\\debug\\examples\\probe.exe', '--artifacts', 'D:\\logs\\observed'],
  cwd: 'D:\\logs\\pinned-python\\source\\rust', env: {TMPDIR: 'D:\\logs'}};
  const other = {program: 'python', args: ['-B', 'tests/compatibility/compare.py',
    '--probe', '/cache/cargo-output/debug/examples/probe.exe', '--artifacts', '/mnt/logs/observed'],
  cwd: '/mnt/logs/pinned-python/source/rust', env: {TMPDIR: '/mnt/logs'}};
  const expected = descriptorDigest(native, windows);
  assert.equal(descriptorDigest(other, posix), expected);
  assert.notEqual(descriptorDigest({...native, args: ['-c', 'pass']}, windows), expected);
  assert.notEqual(descriptorDigest({...native, cwd: 'C:\\checkout\\rust'}, windows), expected);
  assert.notEqual(descriptorDigest({...native, env: {TMPDIR: 'C:\\checkout\\rust'}}, windows), expected);
  assert.notEqual(descriptorDigest(native, {...windows, targetSource: 'default'}), expected);
});

test('one snapshotted Cargo target context drives execution, registry probes and private identity hashes', t => {
  const context = fixture(t);
  const target = path.join(context.artifacts, 'native-target');
  const native = executionContext(context, {CARGO_TARGET_DIR: target});
  context.targetDir = native.targetDir;
  context.targetSource = native.targetSource;
  assert.equal(native.targetSource, 'environment');
  const command = {id: 'target-fixture', program: 'cargo', args: ['test']};
  commandResult(command, context, (program, args, options) => {
    assert.equal(options.env.CARGO_TARGET_DIR, target);
    return {status: 0, stdout: '', cleanup_confirmed: true, cleanup_errors: []};
  });

  test('execution dependency closure follows platform skips and rejects unknown or cyclic graph checks', () => {
    const graph = {checks: [{id: 'owner', depends_on: ['linux-only', 'shared']},
      {id: 'linux-only', platforms: ['linux'], depends_on: ['linux-child']},
      {id: 'linux-child'}, {id: 'shared'}]};
    assert.deepEqual(executionChecks(['owner'], 'linux', graph),
      ['linux-child', 'linux-only', 'shared', 'owner']);
    assert.deepEqual(executionChecks(['owner'], 'windows', graph), ['shared', 'owner']);
    assert.throws(() => executionChecks(['unknown'], 'linux', graph), /unknown execution dependency/);
    graph.checks[3].depends_on = ['owner'];
    assert.throws(() => executionChecks(['owner'], 'linux', graph), /cyclic execution dependency/);
  });
  const manifest = createManifest(context, {platform: 'linux'});
  assert.equal(manifest.execution.cargo_target.source, 'environment');
  assert.match(manifest.execution.cargo_target.path_sha256, /^[a-f0-9]{64}$/);
  assert.ok(!JSON.stringify(manifest).includes(target));
  const descriptors = require('./validation-checks.cjs').commandsFor('native-read', context);
  assert.equal(descriptors[0].env.CARGO_TARGET_DIR, target);
  assert.ok(descriptors[1].args.some(arg => arg.startsWith(target)));
  assert.throws(() => commandResult({...command,
    env: {CARGO_TARGET_DIR: path.join(context.root, 'wrong-target')}}, context,
  () => assert.fail('Conflicting Cargo target must not launch')), /differs from the execution metadata/);
  for (const invalid of ['', null, 'invalid\npath']) {
    assert.throws(() => executionContext({root: context.root, artifacts: context.artifacts},
      {CARGO_TARGET_DIR: invalid}), /invalid CARGO_TARGET_DIR/);
  }
});
