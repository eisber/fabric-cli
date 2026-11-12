// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const {aggregate, requestFor, selectionDigest, equalSelection, readReceipts} = require('./impact-ci.cjs');
const {matrixForPlan, selectShard} = require('./impact-shards.cjs');
const evidence = require('./execution-evidence.cjs');
const registry = require('./validation-checks.cjs');
const engineeringInventory = require('./engineering-inventory.json');
const {runContracts, commandResult} = require('./validate-impact.cjs');

const head = 'a'.repeat(40), base = 'b'.repeat(40);
function fixture(override = {}) {
  const inventoryFiles = [{path: 'rust/engine/core/contracts/src/lib.rs',
    state: 'file', size: 1, digest: 'd'.repeat(64), mode: 0o644}];
  const plan = {schema: 1, head, mode: 'affected', files: ['rust/engine/core/contracts/src/lib.rs'],
    packages: ['fabric-core'], tests: [{package: 'fabric-core', target: 'lib'}],
    profiles: [], checks: [], fingerprint: 'f'.repeat(64), dirty: false, dirty_files: [],
    synthetic_scope: false, head_complete: true, evidence_scope: 'checkout',
    inputs: {head, evidence_scope: 'checkout', inventory: {scope: 'git-tracked-and-untracked',
      files: inventoryFiles, digest: evidence.digest(inventoryFiles)}}, ...override};
  const entries = matrixForPlan(plan).include;
  const executionManifests = [];
  const receipts = entries.map(entry => {
    const shard = selectShard(plan, entry.platform, entry.shard);
    const target = {windows: 'x86_64-pc-windows-msvc', linux: 'x86_64-unknown-linux-gnu',
      macos: 'x86_64-apple-darwin'}[entry.platform];
    const toolchain = {cargo: 'cargo 1.94.0 (fixture)',
      rustc: `rustc 1.94.0 (fixture)\nhost: ${target}`, rustup: `1.94.0-${target} (default)`};
    const verified = {valid: true, fingerprint: plan.fingerprint,
      synthetic_scope: false, head_complete: true, evidence_scope: 'checkout'};
    const inventory = {digest: 'c'.repeat(64), files: inventoryFiles};
    const proof = evidence.provenance(plan, inventory, inventory, verified, verified);
    const descriptorContext = {root: path.resolve(__dirname, '../..'),
      artifacts: path.resolve(__dirname, 'fixture-artifacts'), platform: entry.platform,
      python: 'python', plan, targetSource: 'default'};
    descriptorContext.targetDir = path.join(descriptorContext.root, 'rust', 'target');
    const manifest = {schema: 1, head, platform: entry.platform, shard: entry.shard,
      selection_digest: shard.selection_digest, plan_fingerprint: plan.fingerprint,
      execution: {host_platform: {windows: 'win32', linux: 'linux', macos: 'darwin'}[entry.platform],
        host_arch: 'x64', python: 'python',
        cargo_target: {source: 'default', path_sha256: evidence.digest('target')}},
      provenance: proof, commands: [], checks: []};
    const add = (descriptor, output) => {
      const index = manifest.commands.length;
      manifest.commands.push({index, id: descriptor.id,
        aliases: descriptor.aliases || [descriptor.id], program: descriptor.program,
        argv_sha256: evidence.digest(descriptor.args), execution_identity: evidence.digest(descriptor),
        descriptor_sha256: evidence.descriptorDigest(descriptor, descriptorContext),
        status: 'passed', exit: 0, signal: null, error: null, timed_out: false, cancelled: false,
        cleanup_confirmed: true, cleanup_errors: [],
        stdout_sha256: crypto.createHash('sha256').update(output || '').digest('hex'),
        stderr_sha256: evidence.digest(''), ...(output === undefined ? {} : {output})});
      return index;
    };
    for (const [name, args] of [['cargo', ['--version']], ['rustc', ['--version', '--verbose']],
      ['rustup', ['show', 'active-toolchain']]]) {
      add({id: `toolchain:${name}`, program: name, args}, `${toolchain[name]}\n`);
    }
    if (shard.engineering) add({id: 'preflight:engineering-unit', program: 'python', args: []});
    for (const command of shard.commands) add(command);
    for (const id of evidence.executionChecks(shard.checks, entry.platform)) {
      const descriptors = registry.commandsFor(id, descriptorContext);
      const completed = {id, descriptors: [], profiles: []};
      for (const descriptor of descriptors) {
        const kind = descriptor.kind || 'command';
        const descriptor_sha256 = evidence.descriptorDigest(descriptor, descriptorContext);
        completed.descriptors.push(kind === 'transport-graphs' ?
          {kind, passed: true, inputs: completed.descriptors.map(item => item.command), descriptor_sha256} :
          {kind, descriptor_sha256, command: add({...descriptor,
            id: kind === 'prepare-oracle' ? 'oracle:prepare' : descriptor.id || id})});
      }
      if (!descriptors.length) completed.profiles = plan.profiles.filter(profile =>
        profile.checks?.includes(id) && profile.platforms.includes(entry.platform) &&
        (profile.tests || plan.tests.filter(test => profile.packages.includes(test.package))).length)
        .map(profile => profile.id);
      manifest.checks.push(completed);
    }
    executionManifests.push(manifest);
    return {schema: 1, head, platform: entry.platform, shard: entry.shard, stage: 'all',
      selection_digest: shard.selection_digest, obligations: [...shard.obligations],
      source_before: 'c'.repeat(64), source_after: 'c'.repeat(64),
      status: 'passed', complete_plan: false, timed_out: false, cancelled: false,
      commands: manifest.commands.length, plan_fingerprint: plan.fingerprint, toolchain,
      provenance: proof, execution_manifest: {data: manifest, sha256: evidence.digest(manifest)}};
  });
  const engineeringReceipts = entries.filter(entry => entry.engineering).map(entry => ({
    kind: 'engineering-unit-preflight', head, platform: entry.platform, shard: entry.shard,
    passed: true, suites: [{pattern: engineeringInventory.pattern, expected: engineeringInventory.count,
      discovered: engineeringInventory.count, run: engineeringInventory.count,
      passed: true, failures: 0, errors: 0, expected_failures: 0,
      skipped: entry.platform === 'windows' ? engineeringInventory.windows_skips : []}],
  }));
  return {plan, receipts, options: {selectionResult: 'success', nativeResult: 'success',
    engineeringReceipts, executionManifests}};
}

function reseal(receipts, manifests) {
  for (const receipt of receipts) {
    const manifest = manifests.find(item =>
      item.platform === receipt.platform && item.shard === receipt.shard);
    if (manifest) receipt.execution_manifest.sha256 = evidence.digest(manifest);
  }
}

test('dispatch defaults and unknown/missing bases conservatively select full validation', () => {
  for (const environment of [
    {GITHUB_EVENT_NAME: 'workflow_dispatch'},
    {GITHUB_EVENT_NAME: 'workflow_dispatch', IMPACT_SCOPE: 'unknown', IMPACT_DISPATCH_BASE: base},
    {GITHUB_EVENT_NAME: 'workflow_dispatch', IMPACT_SCOPE: 'affected'},
    {GITHUB_EVENT_NAME: 'workflow_dispatch', IMPACT_SCOPE: 'affected', IMPACT_DISPATCH_BASE: '--files-json'},
    {GITHUB_EVENT_NAME: 'schedule', IMPACT_BASE: base},
    {GITHUB_EVENT_NAME: 'push', IMPACT_BASE: '0'.repeat(40)},
  ]) assert.equal(requestFor(head, environment).full, true);
  const affected = requestFor(head, {GITHUB_EVENT_NAME: 'workflow_dispatch',
    IMPACT_SCOPE: 'affected', IMPACT_DISPATCH_BASE: base, IMPACT_BASE: 'c'.repeat(40)});
  assert.equal(affected.full, false);
  assert.equal(affected.base, base);
  assert.equal(requestFor(head, {GITHUB_EVENT_NAME: 'pull_request', IMPACT_BASE: base}).full, false);
});

test('aggregate requires every selected native obligation exactly once', () => {
  const {plan, receipts, options} = fixture();
  const result = aggregate(plan, receipts, options);
  assert.equal(result.status, 'passed');
  assert.equal(result.shards, 3);
  assert.equal(result.selection_digest, selectionDigest(plan));
  assert.equal(result.obligations, receipts.flatMap(receipt => receipt.obligations).length);
  assert.deepEqual(aggregate(plan, [...receipts].reverse(), options), result);
  const reordered = structuredClone(receipts);
  reordered[0].obligations.reverse();
  assert.deepEqual(aggregate(plan, reordered, options), result);
});

test('aggregate requires all original alias obligations assigned to their single execution owner', () => {
  const profiles = ['alpha', 'beta'].map(id => ({id, packages: ['fabric-cli'],
    features: ['fabric-cli/native'], platforms: ['windows', 'linux', 'macos'], checks: [],
    tests: [{package: 'fabric-cli', target: 'lib'}]}));
  const {plan, receipts, options} = fixture({packages: ['fabric-cli'],
    tests: [{package: 'fabric-cli', target: 'lib'}], profiles});
  assert.equal(aggregate(plan, receipts, options).status, 'passed');
  const owner = receipts.find(receipt =>
    receipt.obligations.includes('linux:core:alpha:test:fabric-cli'));
  assert.ok(owner.obligations.includes('linux:core:beta:test:fabric-cli'));
  owner.obligations = owner.obligations.filter(id => id !== 'linux:core:beta:test:fabric-cli');
  assert.throws(() => aggregate(plan, receipts, options), /obligation mismatch/);
});

test('aggregate derives command-backed contracts, profile markers and full preflights from execution', () => {
  for (const override of [
    {checks: ['command-inventory', 'transport-graphs', 'batch-oracle']},
    {mode: 'full', checks: ['oracle-tools', 'frozen-verify']},
    {checks: ['interactive'], profiles: [{id: 'interactive', packages: ['fabric-core'],
      features: ['fabric-core/interactive'], platforms: ['windows', 'linux', 'macos'],
      checks: ['interactive'], tests: [{package: 'fabric-core', target: 'lib'}]}]},
  ]) {
    const {plan, receipts, options} = fixture(override);
    assert.equal(aggregate(plan, receipts, options).status, 'passed');
  }
});

test('real graph runner evidence aggregates oracle and profile dependencies on all three platforms', t => {
  const graph = JSON.parse(fs.readFileSync(path.join(__dirname, '..', 'validation', 'impact-graph.json')));
  const checks = ['selector-tests', 'oracle-tools', 'frozen-verify', 'interactive'];
  const state = fixture({packages: ['fabric-cli'], tests: [{package: 'fabric-cli', target: 'lib'}],
    checks, check_details: graph.checks.filter(check => checks.includes(check.id)),
    profiles: [{id: 'interactive', packages: ['fabric-cli'], features: ['fabric-cli/interactive'],
      checks: ['interactive'], platforms: ['windows', 'linux', 'macos'],
      tests: [{package: 'fabric-cli', target: 'lib'}]}]});
  const baseDirectory = process.env.IMPACT_TEST_ROOT || path.join(__dirname, '..', 'target', 'validation-tests');
  fs.mkdirSync(baseDirectory, {recursive: true});
  const artifacts = fs.mkdtempSync(path.join(baseDirectory, 'real-graph-evidence-'));
  t.after(() => fs.rmSync(artifacts, {recursive: true, force: true}));
  const root = path.resolve(__dirname, '..', '..');
  for (const receipt of state.receipts) {
    const shard = selectShard(state.plan, receipt.platform, receipt.shard);
    if (!shard.checks.length) continue;
    const context = {root, artifacts: path.join(artifacts, `${receipt.platform}-${receipt.shard}`),
      platform: receipt.platform, plan: state.plan, checks: shard.checks, python: 'python',
      results: [], commandTimeout: 1000, deadline: Date.now() + 30000,
      targetDir: path.join(root, 'rust', 'target'), targetSource: 'default'};
    fs.mkdirSync(context.artifacts);
    context.pinnedOutput = path.join(context.artifacts, 'pinned-python');
    const run = command => commandResult(command, context, () => {
      let stdout = '', stderr = '';
      if (command.kind === 'prepare-oracle') {
        const pinnedRust = path.join(context.pinnedOutput, 'source', 'rust');
        fs.mkdirSync(pinnedRust, {recursive: true});
        stdout = JSON.stringify({rust_root: pinnedRust});
      }
      if (command.test === 'unittest') stderr = 'Ran 1 test in 0.001s\n\nOK\n';
      return {status: 0, stdout, stderr, cleanup_confirmed: true, cleanup_errors: []};
    });
    runContracts(context, registry, run, () => ({status: 0}));
    const actual = evidence.createManifest(context, receipt);
    const manifest = receipt.execution_manifest.data;
    const priorIndices = new Set(manifest.checks.flatMap(check =>
      check.descriptors.map(descriptor => descriptor.command).filter(Number.isSafeInteger)));
    const retained = manifest.commands.filter(command => !priorIndices.has(command.index));
    const offset = retained.length;
    for (const command of actual.commands) command.index += offset;
    for (const check of actual.checks) for (const descriptor of check.descriptors) {
      if (Number.isSafeInteger(descriptor.command)) descriptor.command += offset;
      if (descriptor.inputs) descriptor.inputs = descriptor.inputs.map(index => index + offset);
    }
    manifest.commands = [...retained, ...actual.commands];
    manifest.checks = actual.checks;
    receipt.commands = manifest.commands.length;
    receipt.execution_manifest.sha256 = evidence.digest(manifest);
    if (shard.id === 'contracts-oracle' || shard.id === 'profile-interactive') {
      assert.ok(!shard.checks.includes('selector-tests'), 'dependency is not owned by its consumer');
      assert.ok(manifest.checks.some(check => check.id === 'selector-tests'));
      assert.ok(manifest.commands.some(command => command.id === 'selector-tests'));
    }
  }
  assert.equal(aggregate(state.plan, state.receipts, state.options).status, 'passed');
  for (const platform of ['windows', 'linux', 'macos']) {
    for (const failure of ['missing-completion', 'failed-command']) {
      const receipts = structuredClone(state.receipts);
      const receipt = receipts.find(item => item.platform === platform && item.shard === 'contracts-oracle');
      const manifest = receipt.execution_manifest.data;
      if (failure === 'missing-completion') manifest.checks = manifest.checks.filter(check =>
        check.id !== 'selector-tests');
      else manifest.commands.find(command => command.id === 'selector-tests').exit = 1;
      receipt.execution_manifest.sha256 = evidence.digest(manifest);
      assert.throws(() => aggregate(state.plan, receipts, state.options), /Execution evidence/);
    }
  }
});

for (const [description, mutate] of [
  ['missing manifest', ({receipts}) => { delete receipts[0].execution_manifest.data; }],
  ['malformed manifest', ({receipts}) => { receipts[0].execution_manifest.data = []; }],
  ['missing manifest reference', ({receipts}) => { delete receipts[0].execution_manifest; }],
  ['wrong manifest hash', ({receipts}) => { receipts[0].execution_manifest.sha256 = '0'.repeat(64); }],
  ['zero commands', ({receipts, options}) => {
    receipts[0].commands = 0; options.executionManifests[0].commands = [];
  }],
  ['missing command result', ({options}) => { options.executionManifests[0].commands.pop(); }],
  ['wrong command count', ({receipts}) => { receipts[0].commands++; }],
  ['missing core alias', ({options}) => {
    options.executionManifests[0].commands.find(command => command.id.includes(':core:') ||
      command.id.includes(':default:')).aliases = ['unselected'];
  }],
  ['incorrect core argv', ({options}) => {
    options.executionManifests[0].commands.at(-1).argv_sha256 = '0'.repeat(64);
  }],
  ['incorrect core program', ({options}) => {
    options.executionManifests[0].commands.at(-1).program = 'echo';
  }],
  ['failed command marked passed', ({options}) => { options.executionManifests[0].commands[0].exit = 1; }],
  ['failed status', ({options}) => { options.executionManifests[0].commands[0].status = 'failed'; }],
  ['command error', ({options}) => { options.executionManifests[0].commands[0].error = 'spawn failed'; }],
  ['command signal', ({options}) => { options.executionManifests[0].commands[0].signal = 'SIGTERM'; }],
  ['timed out command', ({options}) => { options.executionManifests[0].commands[0].timed_out = true; }],
  ['cancelled command', ({options}) => { options.executionManifests[0].commands[0].cancelled = true; }],
  ['unconfirmed cleanup', ({options}) => { options.executionManifests[0].commands[0].cleanup_confirmed = false; }],
  ['absent cleanup', ({options}) => { delete options.executionManifests[0].commands[0].cleanup_confirmed; }],
  ['cleanup error', ({options}) => { options.executionManifests[0].commands[0].cleanup_errors = ['reap failed']; }],
  ['receipt timeout', ({receipts}) => { receipts[0].timed_out = true; }],
  ['receipt cancellation', ({receipts}) => { receipts[0].cancelled = true; }],
  ['receipt error', ({receipts}) => { receipts[0].error = 'failed'; }],
  ['wrong native host', ({options}) => { options.executionManifests[0].execution.host_platform = 'other'; }],
  ['missing native host', ({options}) => { delete options.executionManifests[0].execution; }],
  ['missing provenance', ({receipts, options}) => {
    delete receipts[0].provenance; delete options.executionManifests[0].provenance;
  }],
  ['unverified plan', ({receipts}) => { receipts[0].provenance.verified_after.valid = false; }],
  ['dirty source', ({receipts}) => { receipts[0].provenance.dirty = true; }],
  ['synthetic source', ({receipts}) => { receipts[0].provenance.synthetic_scope = true; }],
  ['missing checkout head', ({receipts}) => { receipts[0].provenance.head_complete = false; }],
  ['missing plan fingerprint', ({receipts}) => { delete receipts[0].plan_fingerprint; }],
  ['changed plan input source', ({receipts}) => { receipts[0].provenance.plan_source = '0'.repeat(64); }],
  ['changed execution source', ({receipts}) => {
    receipts[0].provenance.execution_source_after = '0'.repeat(64);
  }],
  ['arbitrary equal source hashes', ({receipts}) => {
    receipts[0].source_before = receipts[0].source_after = '0'.repeat(64);
  }],
  ['unbound toolchain versions', ({receipts}) => { receipts[0].toolchain.cargo = 'cargo 1.0.0'; }],
  ['invented command output', ({options}) => { options.executionManifests[0].commands[0].output = 'cargo 1.0.0'; }],
  ['oversized inline evidence', ({receipts}) => {
    receipts[0].execution_manifest.data.extra = 'x'.repeat(evidence.MAX_RECEIPT_BYTES);
  }],
]) test(`aggregate rejects ${description} despite a passed receipt`, () => {
  const state = fixture();
  mutate(state);
  if (description !== 'wrong manifest hash' && description !== 'missing manifest reference') {
    reseal(state.receipts, state.options.executionManifests);
  }
  assert.throws(() => aggregate(state.plan, state.receipts, state.options), /[Ee]xecution/);
});

for (const [description, mutate] of [
  ['missing completion', manifest => { manifest.checks.pop(); }],
  ['empty command-backed check', manifest => { manifest.checks[0].descriptors = []; }],
  ['wrong registry descriptor kind', manifest => { manifest.checks[0].descriptors[0].kind = 'other'; }],
  ['missing command link', manifest => { delete manifest.checks[0].descriptors[0].command; }],
  ['reused command link', manifest => {
    manifest.checks[0].descriptors[1].command = manifest.checks[0].descriptors[0].command;
  }],
]) test(`aggregate rejects contract ${description}`, () => {
  const {plan, receipts, options} = fixture({checks: ['command-inventory']});
  const manifest = options.executionManifests.find(item => item.checks.length);
  mutate(manifest);
  reseal(receipts, options.executionManifests);
  assert.throws(() => aggregate(plan, receipts, options), /Execution evidence/);
});

for (const changed of ['argv', 'cwd', 'env']) {
  for (const replaceExpected of [false, true]) {
    test(`aggregate rejects same-program/id contract with changed ${changed}${replaceExpected ? ' and rewritten completion proof' : ''}`, () => {
      const {plan, receipts, options} = fixture({checks: ['command-inventory']});
      const manifest = options.executionManifests.find(item => item.checks.length);
      const context = {root: path.resolve(__dirname, '../..'),
        artifacts: path.resolve(__dirname, 'fixture-artifacts'), platform: manifest.platform,
        python: 'python', plan, targetSource: 'default'};
      context.targetDir = path.join(context.root, 'rust', 'target');
      const expected = registry.commandsFor('command-inventory', context)[1];
      const altered = structuredClone(expected);
      if (changed === 'argv') altered.args = ['-c', 'pass'];
      else if (changed === 'cwd') altered.cwd = path.join(context.artifacts, 'pinned-python', 'source', 'rust');
      else altered.env.PYTHONDONTWRITEBYTECODE = '0';
      const completion = manifest.checks[0].descriptors[1];
      const command = manifest.commands[completion.command];
      command.argv_sha256 = evidence.digest(altered.args);
      command.descriptor_sha256 = evidence.descriptorDigest(altered, context);
      if (replaceExpected) completion.descriptor_sha256 = command.descriptor_sha256;
      reseal(receipts, options.executionManifests);
      assert.throws(() => aggregate(plan, receipts, options),
        /registry descriptor identity mismatch|contract command or argv\/cwd\/env mismatch/);
    });
  }
}

test('source provenance is bound by content, not host-specific plan fingerprints or file modes', () => {
  const {plan, receipts, options} = fixture();
  plan.fingerprint = 'e'.repeat(64);
  plan.inputs.inventory.files[0].mode = 0o666;
  assert.equal(aggregate(plan, receipts, options).status, 'passed');
  plan.inputs.inventory.files[0].digest = '0'.repeat(64);
  assert.throws(() => aggregate(plan, receipts, options), /source does not match/);
});

test('recorded compiler and active-toolchain outputs must both prove the native host', () => {
  for (const change of ['compiler', 'rustup']) {
    const {plan, receipts, options} = fixture();
    const receipt = receipts.find(item => item.platform === 'linux');
    const manifest = options.executionManifests.find(item =>
      item.platform === receipt.platform && item.shard === receipt.shard);
    const name = change === 'compiler' ? 'rustc' : 'rustup';
    const command = manifest.commands.find(item => item.id === `toolchain:${name}`);
    command.output = command.output.replaceAll('x86_64-unknown-linux-gnu', 'x86_64-pc-windows-msvc');
    command.stdout_sha256 = crypto.createHash('sha256').update(command.output).digest('hex');
    receipt.toolchain[name] = command.output.trim();
    reseal(receipts, options.executionManifests);
    assert.throws(() => aggregate(plan, receipts, options), /toolchain host mismatch/);
  }
});

test('alias coverage is derived from result aliases, not just receipt obligations', () => {
  const profiles = ['alpha', 'beta'].map(id => ({id, packages: ['fabric-cli'],
    features: ['fabric-cli/native'], platforms: ['windows', 'linux', 'macos'], checks: [],
    tests: [{package: 'fabric-cli', target: 'lib'}]}));
  const {plan, receipts, options} = fixture({packages: ['fabric-cli'],
    tests: [{package: 'fabric-cli', target: 'lib'}], profiles});
  const manifest = options.executionManifests.find(item => item.commands.some(command =>
    command.aliases.includes('beta:test:fabric-cli')));
  const command = manifest.commands.find(item => item.aliases.includes('beta:test:fabric-cli'));
  command.aliases = command.aliases.filter(alias => alias !== 'beta:test:fabric-cli');
  reseal(receipts, options.executionManifests);
  assert.throws(() => aggregate(plan, receipts, options), /missing executed core alias/);
});

test('manifest coverage retains package-local forwarded profile test features', () => {
  const {plan, receipts, options} = fixture({
    packages: ['fabric-cli', 'fabric-identity'],
    tests: [{package: 'fabric-cli', target: 'lib'}, {package: 'fabric-identity', target: 'lib'}],
    profiles: [{
      id: 'forwarded', packages: ['fabric-cli', 'fabric-identity'],
      features: ['fabric-cli/native-read', 'fabric-cli/repl'],
      test_features: {
        'fabric-cli': ['fabric-cli/native-read', 'fabric-cli/repl'],
        'fabric-identity': ['fabric-identity/windows-broker'],
      },
      platforms: ['windows', 'linux', 'macos'], checks: [],
      tests: [{package: 'fabric-cli', target: 'lib'}, {package: 'fabric-identity', target: 'lib'}],
    }],
  });
  assert.equal(aggregate(plan, receipts, options).status, 'passed');
  const manifest = options.executionManifests.find(item => item.commands.some(command =>
    command.aliases.includes('forwarded:test:fabric-identity')));
  const command = manifest.commands.find(item =>
    item.aliases.includes('forwarded:test:fabric-identity'));
  command.argv_sha256 = evidence.digest(['test', '--offline', '--locked', '--package',
    'fabric-identity', '--features', 'fabric-cli/native-read,fabric-cli/repl', '--lib']);
  reseal(receipts, options.executionManifests);
  assert.throws(() => aggregate(plan, receipts, options), /core alias or argv mismatch/);
});

for (const [description, mutate, reason] of [
  ['missing', receipts => receipts.pop(), /Missing native/],
  ['duplicate', receipts => receipts.push(structuredClone(receipts[0])), /Duplicate native/],
  ['stale head', receipts => { receipts[0].head = base; }, /Stale or mismatched/],
  ['stale selection', receipts => { receipts[0].selection_digest = 'bad'; }, /Stale or mismatched/],
  ['failed', receipts => { receipts[0].status = 'failed'; }, /did not pass/],
  ['cancelled', receipts => { receipts[0].status = 'cancelled'; }, /did not pass/],
  ['partial', receipts => { receipts[0].status = 'partial_passed'; }, /did not pass/],
  ['legacy complete', receipts => { receipts[0].complete_plan = true; }, /did not pass/],
  ['changed source', receipts => { receipts[0].source_after = 'd'.repeat(64); }, /source-integrity/],
  ['missing source inventory', receipts => { delete receipts[0].source_before; }, /source-integrity/],
  ['unexpected shard', receipts => { receipts[0].shard = 'unselected'; }, /Unexpected native/],
  ['unexpected platform', receipts => { receipts[0].platform = 'other'; }, /Unexpected native/],
  ['missing obligation', receipts => receipts[0].obligations.pop(), /obligation mismatch/],
  ['extra obligation', receipts => receipts[0].obligations.push('unknown'), /obligation mismatch/],
  ['duplicate obligation', receipts => receipts[0].obligations.push(receipts[0].obligations[0]),
    /obligation mismatch/],
]) test(`aggregate rejects ${description} receipts`, () => {
  const {plan, receipts, options} = fixture();
  mutate(receipts);
  assert.throws(() => aggregate(plan, receipts, options), reason);
});

test('aggregate rejects unsuccessful/cancelled upstream jobs even with passing artifacts', () => {
  const {plan, receipts, options} = fixture();
  for (const status of ['failure', 'cancelled', 'skipped', '', undefined]) {
    assert.throws(() => aggregate(plan, receipts, {...options, selectionResult: status}), /selection/);
    assert.throws(() => aggregate(plan, receipts, {...options, nativeResult: status}), /native checks/);
  }
});

test('engineering reports enforce exact current-head inventory and known platform skips', () => {
  for (const mutate of [
    reports => reports.pop(),
    reports => reports.push(structuredClone(reports[0])),
    reports => { reports[0].head = base; },
    reports => { reports[0].passed = false; },
    reports => { reports[0].suites[0].run = engineeringInventory.count - 1; },
    reports => { reports[0].suites[0].expected_failures = 1; },
    reports => { reports.find(report => report.platform === 'windows').suites[0].skipped = []; },
  ]) {
    const {plan, receipts, options} = fixture();
    mutate(options.engineeringReceipts);
    assert.throws(() => aggregate(plan, receipts, options), /engineering|Engineering/);
  }
});

test('a valid no-impact selection skips native; forged skips and unexpected artifacts fail', () => {
  const plan = {schema: 1, head, mode: 'none', files: ['README.md'],
    packages: [], tests: [], profiles: [], checks: []};
  const options = {selectionResult: 'success', nativeResult: 'skipped'};
  assert.equal(aggregate(plan, [], options).no_impact, true);
  assert.throws(() => aggregate(plan, [], {...options, nativeResult: 'success'}), /native checks/);
  assert.throws(() => aggregate(plan, fixture().receipts, options), /Unexpected native/);
  assert.throws(() => aggregate(fixture().plan, [], options), /native checks/);
});

test('canonical selection binds scheduler dependencies but excludes host-specific provenance', () => {
  const {plan} = fixture();
  equalSelection(plan, {...plan, root: 'another host', fingerprint: 'another fingerprint',
    base_metadata_digest: 'host-specific raw old metadata',
    inputs: {base_metadata_digest: 'host-specific raw old metadata', base_metadata_ref: base}});
  const different = {...plan, check_details: [{id: 'unexpected', depends_on: []}]};
  assert.notEqual(selectionDigest(plan), selectionDigest(different));
  assert.throws(() => equalSelection(plan, different), /differs/);
  assert.notEqual(selectionDigest({...plan, check_order: ['first', 'second']}),
    selectionDigest({...plan, check_order: ['second', 'first']}));
});

test('receipt collection accepts only bounded regular JSON artifacts', t => {
  const directory = path.join(__dirname, `impact-receipts-test-${process.pid}`);
  assert.ok(!fs.existsSync(directory));
  fs.mkdirSync(directory);
  t.after(() => fs.rmSync(directory, {recursive: true, force: true}));
  fs.mkdirSync(path.join(directory, 'windows'));
  fs.writeFileSync(path.join(directory, 'windows', 'receipt.json'), '{"schema":1}');
  fs.writeFileSync(path.join(directory, 'windows', 'engineering-receipt.json'), '{"passed":true}');
  fs.writeFileSync(path.join(directory, 'timing.json'), '{"setup_seconds":1}');
  fs.writeFileSync(path.join(directory, 'cache-target.json'), '{"kind":"target","bytes":5}');
  fs.writeFileSync(path.join(directory, 'ignored.log'), 'not JSON');
  assert.deepEqual(readReceipts(directory), {receipts: [{schema: 1}],
    engineeringReceipts: [{passed: true}], timings: [{setup_seconds: 1}],
    cacheMetrics: [{kind: 'target', bytes: 5}]});
  fs.writeFileSync(path.join(directory, 'windows', 'receipt.json'),
    JSON.stringify({padding: 'x'.repeat(64 * 1024)}));
  assert.equal(readReceipts(directory).receipts[0].padding.length, 64 * 1024);
  fs.writeFileSync(path.join(directory, 'windows', 'receipt.json'),
    ' '.repeat(evidence.MAX_RECEIPT_BYTES + 1));
  assert.throws(() => readReceipts(directory), /bounded/);
});

test('workflow preserves source-bound recomputation, bounded matrix, cache settings and exact final gate', () => {
  const workflow = fs.readFileSync(path.resolve(__dirname, '../../.github/workflows/rust-ci.yml'), 'utf8');
  assert.match(workflow, /options: \[full, affected\]/);
  assert.match(workflow, /default: full/);
  assert.ok(!workflow.includes('rust/scripts/validation-process.test.cjs'));
  const supervision = require('../validation/impact-graph.json').checks.find(
    check => check.id === 'process-supervision');
  assert.deepEqual(supervision.platforms, ['windows', 'linux', 'macos']);
  assert.ok(workflow.indexOf('Set up development-only Python oracle') <
    workflow.indexOf('Recompute and verify the same dependency selection on this host'));
  assert.match(workflow, /IMPACT_DISPATCH_BASE: \$\{\{ inputs\.base \}\}/);
  assert.match(workflow, /matrix: \$\{\{ fromJSON\(needs\.changes\.outputs\.matrix\) \}\}/);
  assert.match(workflow, /max-parallel: 12/);
  assert.match(workflow, /timeout-minutes: 15/);
  assert.match(workflow, /rust-native-v4-.*runner\.os.*runner\.arch.*rust-toolchain\.toml.*Cargo\.lock/);
  assert.match(workflow, /jobs2-dev0-test0-incr0-\$\{\{ matrix\.cache_group \}\}/);
  assert.match(workflow, /actions\/cache\/restore@/);
  assert.match(workflow, /actions\/cache\/save@/);
  assert.match(workflow, /matrix\.cache_writer && steps\.cargo-cache\.outputs\.cache-hit != 'true'/);
  assert.ok(!workflow.split('\n').filter(line => line.includes('key:')).some(line =>
    line.includes('github.sha')));
  assert.match(workflow, /name: rust-receipt-\$\{\{ github\.run_attempt \}\}-\$\{\{ matrix\.os \}\}-\$\{\{ matrix\.shard \}\}/);
  assert.match(workflow, /pattern: rust-receipt-\$\{\{ github\.run_attempt \}\}-\*/);
  assert.equal((workflow.match(/name: rust-impact-selection-\$\{\{ github\.run_attempt \}\}/g) || []).length, 3);
  assert.match(workflow, /name: rust-validation-aggregate-\$\{\{ github\.run_attempt \}\}/);
  assert.match(workflow, /node rust\/scripts\/impact-ci\.cjs aggregate/);
  assert.ok(!workflow.includes('--files-json'));
  assert.ok(!workflow.includes('continue-on-error'));
});

test('every plan producer pins Python 3.13 before tests or source-bound plan recomputation', () => {
  const workflow = fs.readFileSync(path.resolve(__dirname, '../../.github/workflows/rust-ci.yml'), 'utf8');
  const changes = workflow.slice(workflow.indexOf('\n  changes:'), workflow.indexOf('\n  native:'));
  const native = workflow.slice(workflow.indexOf('\n  native:'), workflow.indexOf('\n  validation:'));
  const validation = workflow.slice(workflow.indexOf('\n  validation:'));
  for (const [section, setup, command] of [
    [changes, 'Set up source-plan Python', 'node --test rust/scripts/impact-plan.test.cjs'],
    [changes, 'Set up source-plan Python', 'node rust/scripts/impact-ci.cjs select'],
    [native, 'Set up development-only Python oracle', 'node rust/scripts/impact-ci.cjs native'],
    [validation, 'Set up aggregation Python', 'node rust/scripts/impact-ci.cjs aggregate'],
  ]) {
    assert.ok(section.indexOf(setup) >= 0 && section.indexOf(setup) < section.indexOf(command));
    const installed = section.slice(section.indexOf(setup), section.indexOf(command));
    assert.match(installed, /uses: actions\/setup-python@5fda3b95a4ea91299a34e894583c3862153e4b97/);
    assert.match(installed, /python-version: "3\.13"/);
    assert.match(installed, /timeout-minutes: 2/);
  }
});
