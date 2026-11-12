// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const {coreCommands, deduplicateCoreCommands} = require('./validate-impact.cjs');
const {PLATFORMS, PROFILE_GROUP_SECONDS, WINDOWS_DEFAULT_PACKAGES,
  shardsForPlan, selectShard, matrixForPlan} =
  require('./impact-shards.cjs');
const {selectionDigest} = require('./impact-ci.cjs');

function fixture(mode = 'affected') {
  return {schema: 1, head: 'a'.repeat(40), mode, files: ['rust/engine/core/contracts/src/lib.rs'],
    packages: ['fabric-core', 'fabric-cli', 'fabric-sdk-jobs', 'fabric-sdk-transport'],
    tests: [
      {package: 'fabric-core', target: 'lib'},
      {package: 'fabric-cli', target: 'test:argv_contract'},
      {package: 'fabric-sdk-jobs', target: 'all'},
      {package: 'fabric-sdk-transport', target: 'lib'},
    ],
    profiles: [
      {id: 'headless', packages: ['fabric-cli'], features: ['fabric-cli/headless'],
        platforms: ['windows', 'linux', 'macos'], checks: ['headless'],
        tests: [{package: 'fabric-cli', target: 'test:headless_contract', default_available: false}]},
      {id: 'semantic-model-dotnet', packages: ['fabric-cli'],
        features: ['fabric-cli/semantic-model-dotnet'], platforms: ['windows'], checks: [],
        tests: [{package: 'fabric-cli', target: 'lib'}]},
    ],
    checks: ['selector-tests', 'oracle-tools', 'frozen-verify', 'read-parity',
      'headless', 'dependency-graph'],
    check_details: [
      {id: 'selector-tests', depends_on: [], platforms: ['windows', 'linux', 'macos']},
      {id: 'oracle-tools', depends_on: ['selector-tests']},
      {id: 'frozen-verify', depends_on: ['oracle-tools']},
      {id: 'read-parity', depends_on: ['oracle-tools', 'frozen-verify']},
      {id: 'headless', depends_on: ['selector-tests']},
      {id: 'dependency-graph', depends_on: ['selector-tests']},
    ]};
}

test('dynamically selected shards retain all three native platforms', () => {
  const matrix = matrixForPlan(fixture(), coreCommands).include;
  assert.deepEqual([...new Set(matrix.map(entry => entry.os))].sort(),
    Object.values(PLATFORMS).sort());
  for (const [platform, os] of Object.entries(PLATFORMS)) {
    const entries = matrix.filter(entry => entry.platform === platform);
    assert.ok(entries.length > 1);
    assert.ok(entries.every(entry => entry.os === os && entry.estimated_seconds > 0));
    assert.equal(entries.filter(entry => entry.engineering).length, 1);
    assert.equal(entries.filter(entry => entry.dotnet).length, platform === 'windows' ? 1 : 0);
  }
});

test('cache producers are bounded per OS and immutable compilation cohort, not per shard', () => {
  const matrix = matrixForPlan(fixture('full'), coreCommands).include;
  for (const platform of Object.keys(PLATFORMS)) {
    const native = matrix.filter(entry => entry.platform === platform);
    assert.equal(native.filter(entry => entry.dependency_cache_writer).length, 1);
    assert.equal(native.filter(entry => entry.cache_writer).length, 2);
    assert.deepEqual([...new Set(native.map(entry => entry.cache_group))].sort(), ['default', 'features']);
    for (const cohort of ['default', 'features']) assert.equal(native.filter(entry =>
      entry.cache_group === cohort && entry.cache_writer).length, 1);
    assert.ok(native.find(entry => entry.shard === 'preflight-clippy').cache_writer);
  }
});

test('default package dependency families retain full Cargo target commands without feature unification', () => {
  const plan = fixture();
  const shards = shardsForPlan(plan, 'linux', coreCommands);
  const defaults = shards.filter(shard => shard.id.startsWith('default-'));
  assert.equal(defaults.length, 4);
  for (const shard of defaults) {
    assert.ok(shard.commands.every(command => !command.args.includes('--features')));
    assert.ok(shard.commands.some(command => command.args[0] === 'check' &&
      command.args.includes('--all-targets')));
    assert.ok(shard.commands.some(command => command.args[0] === 'clippy' &&
      command.args.includes('--all-targets')));
    assert.ok(shard.commands.some(command => command.args.includes('--doc')));
  }
  const cargoPackages = command => command.args.flatMap((arg, index) =>
    arg === '--package' ? [command.args[index + 1]] : []);
  const checked = defaults.flatMap(shard => shard.commands
    .filter(command => command.args[0] === 'check').flatMap(cargoPackages)).sort();
  assert.deepEqual(checked, [...plan.packages].sort());
  const jobs = defaults.flatMap(shard => shard.commands).find(command =>
    command.test && command.args.includes('fabric-sdk-jobs') && !command.args.includes('--doc'));
  assert.ok(jobs.args.includes('--all-targets'));
  const cli = defaults.flatMap(shard => shard.commands).find(command =>
    command.test && command.args.includes('--test'));
  assert.ok(cli.args.includes('argv_contract'));
});

test('Windows bounds default formatting launches while retaining every per-package fmt obligation', () => {
  const packages = Array.from({length: 19}, (_, index) => `fabric-cli-leaf-${index}`);
  const plan = {schema: 1, mode: 'affected', files: [], packages,
    tests: packages.map(name => ({package: name, target: 'all'})), profiles: [], checks: []};
  const windows = shardsForPlan(plan, 'windows', coreCommands);
  assert.equal(windows.length, 3);
  const formatted = [];
  for (const shard of windows) {
    const formatting = shard.commands.filter(command => command.args[0] === 'fmt');
    assert.ok(formatting.length > 0 && formatting.length <= WINDOWS_DEFAULT_PACKAGES);
    for (const command of formatting) {
      assert.equal(command.args.filter(arg => arg === '--package').length, 1);
      formatted.push(command.args[command.args.indexOf('--package') + 1]);
    }
    assert.equal(shard.feature_group, 'default-cli');
  }
  assert.deepEqual(formatted.sort(), [...packages].sort());
  assert.equal(shardsForPlan(plan, 'linux', coreCommands).length, 1);
});

test('complete feature profiles preserve package-local features and platform applicability', () => {
  const plan = fixture();
  const linux = shardsForPlan(plan, 'linux', coreCommands);
  assert.ok(!linux.some(shard => shard.id === 'profile-semantic-model-dotnet'));
  const shard = selectShard(plan, 'linux', 'profile-headless', coreCommands);
  assert.equal(shard.commands.length, 3);
  assert.ok(shard.commands.every(command => command.id.startsWith('headless:')));
  assert.ok(shard.commands.every(command => command.args.includes('fabric-cli/headless')));
  assert.ok(shard.commands.some(command => command.args.includes('headless_contract')));
  assert.deepEqual(shard.checks, ['headless']);
  assert.ok(shard.feature_group.startsWith('feature-'));
  assert.equal(selectShard(plan, 'windows', 'profile-headless', coreCommands).feature_group,
    shard.feature_group);
});

test('legacy profile test fallback retains complete selected targets', () => {
  const plan = fixture();
  delete plan.profiles[0].tests;
  assert.ok(selectShard(plan, 'linux', 'profile-headless', coreCommands).commands.some(command =>
    command.args.includes('argv_contract')));
});

test('duration-bounded profile cohorts reuse dependency packages without unifying features', () => {
  const plan = fixture();
  for (const id of ['other', 'third']) {
    plan.profiles.push({...structuredClone(plan.profiles[0]), id, checks: [],
      features: [`fabric-cli/${id}`]});
  }
  const shards = shardsForPlan(plan, 'linux', coreCommands)
    .filter(shard => shard.id.startsWith('profile-'));
  assert.equal(shards.length, 2);
  assert.ok(shards.every(shard => shard.estimated_seconds <= PROFILE_GROUP_SECONDS));
  const grouped = shards.find(shard => shard.id.startsWith('profile-group-'));
  assert.equal(grouped.commands.length, 6);
  const features = new Set(grouped.commands.map(command =>
    command.args[command.args.indexOf('--features') + 1]));
  assert.deepEqual([...features].sort(), ['fabric-cli/headless', 'fabric-cli/other']);
  assert.ok(grouped.commands.every(command => !command.args.some(arg =>
    arg === 'fabric-cli/headless,fabric-cli/other')));
  const otherHead = {...plan, head: 'b'.repeat(40)};
  assert.deepEqual(matrixForPlan(otherHead, coreCommands), matrixForPlan(plan, coreCommands));
});

test('Windows measured combined profile receives cold-build headroom instead of a second profile', () => {
  const plan = fixture();
  plan.profiles.push({...structuredClone(plan.profiles[0]), id: 'combined',
    features: ['fabric-cli/combined'], checks: []});
  const windows = selectShard(plan, 'windows', 'profile-combined', coreCommands);
  assert.equal(windows.estimated_seconds, 360);
  assert.ok(windows.commands.every(command => command.id.startsWith('combined:')));
  const linux = shardsForPlan(plan, 'linux', coreCommands);
  assert.ok(linux.some(shard => shard.commands.some(command => command.id.startsWith('combined:')) &&
    shard.commands.some(command => command.id.startsWith('headless:'))));
});

test('equivalent cross-profile commands share one owner with every original obligation alias', () => {
  const plan = fixture();
  plan.profiles.push({...structuredClone(plan.profiles[0]), id: 'mirror',
    packages: ['fabric-cli', 'fabric-core'], checks: []});
  const before = JSON.stringify(plan);
  const shards = shardsForPlan(plan, 'linux', coreCommands);
  const commands = shards.flatMap(shard => shard.commands).filter(command =>
    command.args.includes('headless_contract'));
  assert.equal(commands.length, 1);
  assert.deepEqual(commands[0].aliases, [
    'headless:test:fabric-cli', 'mirror:test:fabric-cli',
  ]);
  const owner = shards.find(shard => shard.commands.includes(commands[0]));
  for (const id of commands[0].aliases) {
    assert.ok(owner.obligations.includes(`linux:core:${id}`));
    assert.equal(shards.flatMap(shard => shard.obligations)
      .filter(obligation => obligation === `linux:core:${id}`).length, 1);
  }
  const executed = deduplicateCoreCommands(owner.commands, {
    root: process.cwd(), artifacts: path.join(process.cwd(), 'rust', 'target', 'alias-proof'),
  });
  const loggedAliases = new Set(executed.flatMap(command => command.aliases));
  assert.deepEqual([...loggedAliases].sort(), owner.obligations
    .filter(id => id.startsWith('linux:core:')).map(id => id.slice('linux:core:'.length)).sort());
  assert.equal(JSON.stringify(plan), before);
  const environmentalCommands = selected => coreCommands(selected, 'linux').map(command =>
    ({...command, env: {PROFILE_ENV: selected.profiles[0]?.id === 'mirror' ? 'other' : 'base'}}));
  assert.equal(shardsForPlan(plan, 'linux', environmentalCommands)
    .flatMap(shard => shard.commands).filter(command =>
      command.args.includes('headless_contract')).length, 2);
});

test('pinned dependencies share a preparation/integrity domain and marker checks stay with profiles', () => {
  const plan = fixture();
  const oracle = selectShard(plan, 'linux', 'contracts-oracle', coreCommands);
  assert.deepEqual([...oracle.checks].sort(), ['frozen-verify', 'oracle-tools', 'read-parity']);
  assert.equal(oracle.engineering, true);
  assert.ok(!oracle.commands.length);
  assert.ok(!selectShard(plan, 'linux', 'contracts-native', coreCommands).checks.includes('headless'));
  const obligations = shardsForPlan(plan, 'linux', coreCommands).flatMap(shard => shard.obligations);
  assert.equal(new Set(obligations).size, obligations.length);
  for (const id of plan.checks) assert.equal(obligations.filter(value =>
    value === `linux:check:${id}`).length, 1);
});

test('full mode adds every preflight exactly once per OS, never to affected shards', () => {
  for (const platform of Object.keys(PLATFORMS)) {
    const full = shardsForPlan(fixture('full'), platform, coreCommands);
    const affected = shardsForPlan(fixture(), platform, coreCommands);
    const preflights = full.filter(shard => shard.id.startsWith('preflight-'));
    assert.equal(preflights.length, 2);
    assert.ok(preflights.every(shard => shard.dotnet));
    const cli = preflights.find(shard => shard.id === 'preflight-cli');
    assert.equal(cli.commands.length, 2);
    assert.ok(cli.commands[0].args.includes('--no-run'));
    assert.equal(cli.commands[1].test, true);
    assert.deepEqual(cli.commands[0].args.filter(arg => arg !== '--no-run'), cli.commands[1].args);
    assert.ok(!cli.commands[1].args.includes('--no-run'));
    assert.equal(preflights.find(shard => shard.id === 'preflight-clippy').commands.length, 1);
    assert.ok(preflights.every(shard => shard.commands[0].args.includes('--all-features')));
    assert.ok(!affected.some(shard => shard.commands.some(command =>
      command.args.includes('--all-features'))));
    assert.equal(full.length, affected.length + 2);
  }
});

test('deterministic source-independent sharding uses the exact canonical selection digest', () => {
  const plan = fixture(), before = JSON.stringify(plan);
  const first = shardsForPlan(plan, 'linux', coreCommands);
  assert.deepEqual(shardsForPlan(plan, 'linux', coreCommands), first);
  assert.equal(JSON.stringify(plan), before);
  const otherHost = {...plan, root: 'another checkout', fingerprint: 'host-specific',
    inputs: {root: 'another checkout'}, metadata_digest: 'platform-specific'};
  assert.deepEqual(shardsForPlan(otherHost, 'linux', coreCommands), first);
  assert.ok(first.every(shard => shard.selection_digest === selectionDigest(plan)));
  const reordered = structuredClone(plan);
  reordered.profiles.reverse();
  reordered.packages.reverse();
  reordered.checks.reverse();
  reordered.tests.reverse();
  assert.deepEqual(matrixForPlan(reordered, coreCommands), matrixForPlan(plan, coreCommands));
});

test('selectShard accepts precomputed core commands while regenerating trusted family descriptors', () => {
  const plan = fixture();
  const expected = selectShard(plan, 'linux', 'default-cli', coreCommands);
  assert.deepEqual(selectShard(plan, 'linux', 'default-cli', coreCommands(plan, 'linux')), expected);
  assert.deepEqual(selectShard(plan, 'linux', 'default-cli',
    [{id: 'untrusted', program: 'untrusted-provider', args: ['must-not-execute']}]), expected);
  assert.throws(() => shardsForPlan(plan, 'linux', {}), /factory must be a function/);
});

test('no-impact skip is empty and malformed or unsupported selections fail closed', () => {
  const none = {mode: 'none', files: [], packages: [], tests: [], profiles: [], checks: []};
  assert.deepEqual(matrixForPlan(none, coreCommands), {include: []});
  assert.throws(() => shardsForPlan({...none, checks: ['selector-tests']}, 'linux', coreCommands),
    /No-impact/);
  assert.throws(() => selectShard(fixture(), 'linux', 'missing', coreCommands), /Unknown native shard/);
  assert.throws(() => shardsForPlan(fixture(), 'unsupported', coreCommands), /Unsupported/);
  assert.throws(() => shardsForPlan({...fixture(), mode: 'unknown'}, 'linux', coreCommands), /Unknown/);
  const missing = fixture();
  missing.checks = missing.checks.filter(id => id !== 'oracle-tools');
  assert.throws(() => shardsForPlan(missing, 'linux', coreCommands), /Missing selected dependency/);
  const marker = fixture();
  marker.profiles = [];
  assert.throws(() => shardsForPlan(marker, 'linux', coreCommands), /No native profile/);
});

module.exports = {fixture};
