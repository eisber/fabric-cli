// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const path = require('node:path');
const {coreCommands, deduplicateCoreCommands} = require('./validate-impact.cjs');
const {PLATFORMS, PROFILE_GROUP_SECONDS, SETUP_CLEANUP_SECONDS, WINDOWS_DEFAULT_PACKAGES,
  FULL_PROFILE_GROUP_SECONDS, FULL_PROFILE_MAX_DOMAINS, FULL_PROFILE_COLD_SECONDS,
  shardsForPlan, selectShard, matrixForPlan} =
  require('./impact-shards.cjs');
const {selectionDigest} = require('./impact-ci.cjs');
const TIMINGS = require('./impact-shard-timings.json');

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

function measuredLeaf() {
  const graph = require('../validation/impact-graph.json');
  const cliTargets = ['bin:fab-rs', 'lib', 'test:execute_arguments', 'test:execute_cli_contract',
    'test:execute_host', 'test:job_arguments', 'test:native_tabular', 'test:workspace_contract'];
  const tests = [
    ...cliTargets.map(target => ({package: 'fabric-cli', target})),
    {package: 'fabric-cli-v2', target: 'all'},
    {package: 'fabric-sdk-dataflow', target: 'lib'},
    {package: 'fabric-sdk-dataflow', target: 'test:execute_query'},
  ];
  const profileIds = ['cli-v2-broker', 'cli-v2-combined', 'cli-v2-updater',
    'headless', 'interactive', 'native-read'];
  const checks = ['command-inventory', 'dependency-graph', 'frozen-verify', 'headless',
    'interactive', 'native-read', 'oracle-tools', 'selector-tests'];
  return {schema: 1, mode: 'affected', files: ['rust/engine/data-plane/dataflow/src/lib.rs'],
    packages: ['fabric-cli', 'fabric-cli-v2', 'fabric-identity',
      'fabric-sdk-dataflow', 'fabric-sdk-transport'], tests,
    profiles: graph.profiles.filter(profile => profileIds.includes(profile.id))
      .map(profile => ({...structuredClone(profile),
        tests: tests.filter(test => profile.packages.includes(test.package))})),
    checks, check_details: graph.checks.filter(check => checks.includes(check.id))};
}

// Exact full Cargo domains from run 43303660409 (head 1402dd06497c8bb46e246ad3e5578fa84c6d68a2).
// Every selected package uses its complete all-targets command; native transport
// test features include the CLI-forwarded feature recorded in that selection.
function measuredFullProfiles(ids) {
  const graph = require('../validation/impact-graph.json');
  const profiles = graph.profiles.filter(profile => ids.includes(profile.id))
    .map(profile => ({...structuredClone(profile),
      tests: profile.packages.map(name => ({package: name, target: 'all'})),
      test_features: Object.fromEntries(profile.packages.map(name => {
        const own = profile.features.filter(feature => feature.startsWith(`${name}/`)).sort();
        return [name, name === 'fabric-sdk-transport' && !own.length ?
          ['fabric-sdk-transport/hyper-rustls'] : own];
      })),
    }));
  const packages = [...new Set(profiles.flatMap(profile => profile.packages))].sort();
  return {schema: 1, mode: 'full', files: [], packages,
    tests: packages.map(name => ({package: name, target: 'all'})), profiles, checks: []};
}

function commandSignature(commands) {
  return crypto.createHash('sha256').update(JSON.stringify(commands.map(command =>
    [command.program, command.args, command.test || false]))).digest('hex').slice(0, 16);
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

test('feature cache producer falls back to the oracle only when no profile is selected', () => {
  const plan = fixture();
  plan.profiles = [];
  plan.checks = ['oracle-tools', 'frozen-verify'];
  plan.check_details = [];
  for (const platform of Object.keys(PLATFORMS)) {
    const writer = matrixForPlan(plan, coreCommands).include.find(entry =>
      entry.platform === platform && entry.cache_group === 'features' && entry.cache_writer);
    assert.equal(writer.shard, 'contracts-oracle');
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

test('new and unmeasured profiles remain isolated instead of borrowing optimistic weights', () => {
  const plan = fixture();
  for (const id of ['other', 'third']) {
    plan.profiles.push({...structuredClone(plan.profiles[0]), id, checks: [],
      features: [`fabric-cli/${id}`]});
  }
  const shards = shardsForPlan(plan, 'linux', coreCommands)
    .filter(shard => shard.id.startsWith('profile-'));
  assert.equal(shards.length, 3);
  assert.ok(shards.every(shard => shard.estimated_seconds === PROFILE_GROUP_SECONDS &&
    shard.estimate_basis === 'unmeasured-isolated' && shard.domains.length === 1));
  assert.ok(shards.every(shard => shard.commands.length === 3));
  const otherHead = {...plan, head: 'b'.repeat(40)};
  assert.deepEqual(matrixForPlan(otherHead, coreCommands), matrixForPlan(plan, coreCommands));
});

test('a known profile name with changed argv cannot reuse its historical weight', () => {
  const plan = fixture();
  plan.profiles.push({...structuredClone(plan.profiles[0]), id: 'combined',
    features: ['fabric-cli/combined'], checks: []});
  const windows = selectShard(plan, 'windows', 'profile-combined', coreCommands);
  assert.equal(windows.estimated_seconds, PROFILE_GROUP_SECONDS);
  assert.equal(windows.estimate_basis, 'unmeasured-isolated');
  assert.ok(windows.commands.every(command => command.id.startsWith('combined:')));
  const linux = shardsForPlan(plan, 'linux', coreCommands);
  assert.ok(!linux.some(shard => shard.commands.some(command => command.id.startsWith('combined:')) &&
    shard.commands.some(command => command.id.startsWith('headless:'))));
});

test('measured Dataflow domains use twelve balanced native jobs rather than twenty-seven', () => {
  const plan = measuredLeaf();
  const matrix = matrixForPlan(plan, coreCommands).include;
  assert.equal(matrix.length, 12);
  for (const platform of Object.keys(PLATFORMS)) {
    const shards = shardsForPlan(plan, platform, coreCommands);
    assert.equal(shards.length, 4);
    assert.equal(shards.filter(shard => shard.engineering).length, 1);
    assert.ok(shards.every(shard => shard.estimated_seconds <= PROFILE_GROUP_SECONDS &&
      shard.estimated_job_seconds === shard.estimated_seconds + SETUP_CLEANUP_SECONDS &&
      shard.estimated_job_seconds <= 900));
    assert.deepEqual(shards.find(shard => shard.engineering).domains, ['contracts-oracle']);
    const defaults = shards.filter(shard => shard.id.startsWith('default-'));
    assert.equal(defaults.length, 1);
    assert.deepEqual(defaults[0].domains, [
      'default-cli', 'default-data-plane', 'default-identity', 'contracts-native',
    ]);
    assert.ok(defaults[0].commands.every(command => !command.args.includes('--features')));
    const features = shards.filter(shard => shard.id.startsWith('profile-'));
    assert.equal(features.length, 2);
    assert.ok(Math.max(...features.map(shard => shard.estimated_seconds)) <=
      {linux: 400, windows: 680, macos: 680}[platform]);
    const native = matrix.filter(entry => entry.platform === platform);
    assert.equal(native.filter(entry => entry.cache_writer).length, 2);
    assert.equal(native.filter(entry => entry.dependency_cache_writer).length, 1);
    const featureWriter = native.find(entry => entry.cache_group === 'features' && entry.cache_writer);
    assert.ok(featureWriter.shard.startsWith('profile-'),
      'Seed the immutable feature cache with a profile, not the oracle-only build');
    assert.equal(native.find(entry => entry.cache_group === 'default' && entry.cache_writer).shard,
      defaults[0].id);
  }
  assert.ok(matrix.some(entry => entry.platform === 'macos' &&
    entry.estimate_basis.includes('cross-host-estimate')));
  assert.deepEqual(matrixForPlan({...plan, head: 'f'.repeat(40)}, coreCommands).include, matrix);
  const reordered = structuredClone(plan);
  reordered.profiles.reverse();
  reordered.packages.reverse();
  reordered.tests.reverse();
  reordered.checks.reverse();
  assert.deepEqual(matrixForPlan(reordered, coreCommands).include, matrix);
});

test('packing preserves exact default IDs, profile argv, aliases, checks and target obligations', () => {
  const plan = measuredLeaf();
  const families = {
    cli: ['fabric-cli', 'fabric-cli-v2'],
    'data-plane': ['fabric-sdk-dataflow', 'fabric-sdk-transport'],
    identity: ['fabric-identity'],
  };
  for (const platform of Object.keys(PLATFORMS)) {
    const original = Object.entries(families).flatMap(([family, packages]) =>
      coreCommands({...plan, packages, tests: plan.tests.filter(test =>
        packages.includes(test.package)), profiles: []}, platform)
        .map(command => ({...command, id: `default-${family}:${command.id}`})));
    original.push(...coreCommands(plan, platform).filter(command => !command.id.startsWith('default:')));
    const shards = shardsForPlan(plan, platform, coreCommands);
    const actual = shards.flatMap(shard => shard.commands);
    const expected = new Map(original.map(command => [command.id, command]));
    for (const command of actual) for (const id of command.aliases || [command.id]) {
      assert.deepEqual(command.args, expected.get(id).args);
      assert.equal(command.program, expected.get(id).program);
      assert.equal(command.test, expected.get(id).test);
      expected.delete(id);
    }
    assert.equal(expected.size, 0);
    assert.deepEqual(shards.flatMap(shard => shard.obligations).sort(), [
      ...original.map(command => `${platform}:core:${command.id}`),
      ...plan.checks.map(id => `${platform}:check:${id}`),
      `${platform}:preflight:engineering-unit`,
    ].sort());
    for (const marker of ['headless', 'interactive']) {
      const owner = shards.find(shard => shard.checks.includes(marker));
      assert.ok(owner.commands.some(command => command.id.startsWith(`${marker}:`)));
    }
  }
});

for (const regression of [
  {shard: '468264fc60318498', estimate: 703,
    profiles: ['livy-arrow-ipc', 'native-read', 'native-read-repl', 'semantic-model-arguments',
      'v2-cancel-combined', 'v2-copy', 'v2-graph-refresh-headless'],
    censored: ['v2-graph-refresh-headless']},
  {shard: '94b09b6fab6c103f', estimate: 709,
    profiles: ['certificate-macos', 'headless', 'interactive', 'notebook-websocket',
      'repl', 'v2-preview', 'workspace-read-v2-headless'],
    censored: ['repl', 'v2-preview', 'workspace-read-v2-headless']},
]) {
  test(`full group ${regression.shard} cannot recombine its measured budget-exhausting prefix`, () => {
    const plan = measuredFullProfiles(regression.profiles);
    const samples = new Map();
    for (const profile of plan.profiles) {
      const commands = coreCommands({...plan, packages: profile.packages, profiles: [profile]}, 'linux')
        .filter(command => !command.id.startsWith('default:'));
      const sample = TIMINGS.full[commandSignature(commands)];
      assert.ok(sample, `Changed recorded full command descriptors: ${profile.id}`);
      assert.equal(sample[0], profile.id);
      samples.set(profile.id, sample);
    }
    assert.ok(regression.estimate < PROFILE_GROUP_SECONDS);
    const oldPrefix = [...samples.values()].reduce((sum, sample) => sum + sample[1], 0);
    assert.ok(oldPrefix + FULL_PROFILE_COLD_SECONDS > PROFILE_GROUP_SECONDS,
      'The real command prefix contradicts the old warm-mix estimate');
    for (const platform of Object.keys(PLATFORMS)) {
      const shards = shardsForPlan(plan, platform, coreCommands);
      const profiles = shards.filter(shard => shard.domains.every(id => id.startsWith('profile-')));
      assert.ok(profiles.length > 1);
      for (const shard of profiles) {
        assert.ok(shard.domains.length <= FULL_PROFILE_MAX_DOMAINS);
        assert.ok(shard.estimated_seconds <= PROFILE_GROUP_SECONDS);
        if (shard.domains.length > 1) {
          assert.ok(shard.estimated_seconds <= FULL_PROFILE_GROUP_SECONDS);
          assert.equal(shard.cold_reserve_seconds, FULL_PROFILE_COLD_SECONDS);
        }
        if (platform === 'linux') {
          const prefix = shard.domains.reduce((sum, id) =>
            sum + samples.get(id.slice('profile-'.length))[1], 0);
          assert.ok(prefix + FULL_PROFILE_COLD_SECONDS < PROFILE_GROUP_SECONDS);
          if (shard.estimate_basis !== 'full-incomplete-isolated') {
            assert.ok(prefix + FULL_PROFILE_COLD_SECONDS <= shard.estimated_seconds);
          }
        }
      }
      for (const id of regression.censored) {
        const isolated = selectShard(plan, platform, `profile-${id}`, coreCommands);
        assert.deepEqual(isolated.domains, [`profile-${id}`]);
        const profile = plan.profiles.find(candidate => candidate.id === id);
        const nativeCommands = coreCommands({...plan, packages: profile.packages,
          profiles: [profile]}, platform).filter(command => !command.id.startsWith('default:'));
        const recorded = TIMINGS.full[commandSignature(nativeCommands)];
        assert.equal(isolated.estimate_basis, recorded ?
          'full-incomplete-isolated' : 'unmeasured-isolated');
        assert.equal(isolated.estimated_seconds, PROFILE_GROUP_SECONDS,
          'An interrupted or never-started domain must not become a cheap timing input');
        assert.ok(isolated.commands.some(command => command.id === `${id}:clippy`));
        assert.ok(isolated.commands.some(command => command.id === `${id}:test:fabric-cli`),
          'Keep the interrupted or never-started last test command');
      }
      const expected = new Map(coreCommands(plan, platform)
        .filter(command => !command.id.startsWith('default:')).map(command => [command.id, command]));
      for (const shard of shards) for (const command of shard.commands) {
        for (const id of command.aliases || [command.id]) {
          if (!expected.has(id)) continue;
          const original = expected.get(id);
          assert.deepEqual(command.args, original.args);
          assert.equal(command.program, original.program);
          assert.equal(command.test, original.test);
          assert.ok(shard.obligations.includes(`${platform}:core:${id}`));
          expected.delete(id);
        }
      }
      assert.equal(expected.size, 0, 'Retain every complete original profile command and alias');
    }
    const before = matrixForPlan(plan, coreCommands);
    plan.profiles.reverse();
    plan.packages.reverse();
    plan.tests.reverse();
    assert.deepEqual(matrixForPlan(plan, coreCommands), before);
  });
}

test('full-mode cold guards constrain all sampled profiles, without changing affected packing', () => {
  const plan = measuredFullProfiles(Object.values(TIMINGS.full).map(sample => sample[0]));
  for (const platform of Object.keys(PLATFORMS)) {
    for (const shard of shardsForPlan(plan, platform, coreCommands)) {
      if (!shard.id.startsWith('profile-')) continue;
      assert.ok(shard.domains.length <= FULL_PROFILE_MAX_DOMAINS);
      if (shard.domains.length > 1) assert.ok(shard.estimated_seconds <= FULL_PROFILE_GROUP_SECONDS);
    }
  }
  const leaf = measuredLeaf();
  assert.equal(matrixForPlan(leaf, coreCommands).include.length, 12);
  assert.ok(matrixForPlan(leaf, coreCommands).include.every(entry =>
    !entry.estimate_basis.includes('full-')));
});

test('changed targets and explicit execution environments invalidate timing signatures', () => {
  const plan = measuredLeaf();
  const before = matrixForPlan(plan, coreCommands);
  plan.profiles.find(profile => profile.id === 'cli-v2-updater').tests.push(
    {package: 'fabric-cli-v2', target: 'test:new-contract'});
  // An all-targets selection already contains this target; changing to an
  // explicit selector is what changes the actual command, not incidental metadata.
  assert.deepEqual(matrixForPlan(plan, coreCommands), before);
  plan.profiles.find(profile => profile.id === 'cli-v2-updater').tests =
    [{package: 'fabric-cli-v2', target: 'test:new-contract'}];
  assert.equal(selectShard(plan, 'linux', 'profile-cli-v2-updater', coreCommands)
    .estimate_basis, 'unmeasured-isolated');
  const commands = (selection, platform) => coreCommands(selection, platform)
    .map(command => ({...command, env: {NEW_COMPILER_FLAGS: '1'}}));
  assert.ok(shardsForPlan(measuredLeaf(), 'linux', commands)
    .filter(shard => shard.commands.length).every(shard => shard.domains.length === 1 &&
      shard.estimate_basis === 'unmeasured-isolated'));
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
