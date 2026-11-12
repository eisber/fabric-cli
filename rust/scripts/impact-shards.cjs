// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const crypto = require('node:crypto');
const {selectionDigest} = require('./impact-ci.cjs');

const PLATFORMS = Object.freeze({
  windows: 'windows-2025', linux: 'ubuntu-24.04', macos: 'macos-15',
});
const PROFILE_MARKERS = new Set(['interactive', 'headless', 'certificate', 'updater', 'combined']);
// Initial seconds are conservative historical weights, not a cold-run promise.
// Historical serial run https://github.com/eisber/fabric-cli/actions/runs/43072057855
// (ci-historical-duration-inputs.json; mixed warm compiles) observed ~231s for the
// entire default sequence and ~239s for the largest combined feature profile.
// Each default family gets 180s; each whole profile gets 240s. Oracle/preflight
// 480s are conservative initial allowances, not measured current baselines.
// The supplied Windows serial baseline measured ~399s default / ~266s combined:
// bound per-package fmt launches to eight and keep combined in its own cohort.
// New command/setup receipts provide the evidence for subsequent tuning.
const INITIAL_WEIGHTS = Object.freeze({default: 180, profile: 240, contracts: 120,
  windowsCombined: 360, oracle: 480, preflight: 480});
const PROFILE_GROUP_SECONDS = 480;
const WINDOWS_DEFAULT_PACKAGES = 8;

function requireThat(condition, message) {
  if (!condition) throw new Error(message);
}

function family(name) {
  if (/^fabric-(core|name-search)/.test(name)) return 'core';
  if (/^fabric-identity/.test(name)) return 'identity';
  if (/^fabric-(deploy|cicd)/.test(name)) return 'deployment';
  if (/^fabric-cli/.test(name)) return 'cli';
  if (/^fabric-(sdk-)?(http|transport|onelake|storage|table|dataflow|tabular|semantic|query)/.test(name)) {
    return 'data-plane';
  }
  return 'control-plane';
}

function profileGroup(profile) {
  return `feature-${crypto.createHash('sha256')
    .update(JSON.stringify([...profile.features].sort())).digest('hex').slice(0, 16)}`;
}

function canonical(value) {
  if (Array.isArray(value)) return value.map(canonical);
  if (value && typeof value === 'object') return Object.fromEntries(
    Object.keys(value).sort().map(key => [key, canonical(value[key])]));
  return value;
}

function colocateEquivalentCommands(shards, platform) {
  const owners = new Map();
  for (const shard of shards) {
    shard.commands = shard.commands.filter(command => {
      const {id, aliases, ...descriptor} = command;
      const key = JSON.stringify(canonical(descriptor));
      const previous = owners.get(key);
      if (!previous) {
        owners.set(key, {command, shard});
        return true;
      }
      const ids = [...new Set([id, ...(aliases || [])])];
      previous.command.aliases = [...new Set([
        previous.command.id, ...(previous.command.aliases || []), ...ids,
      ])].sort();
      for (const name of ids) {
        const obligation = `${platform}:core:${name}`;
        shard.obligations = shard.obligations.filter(value => value !== obligation);
        previous.shard.obligations.push(obligation);
      }
      return false;
    });
  }
  for (const shard of shards) shard.obligations.sort();
  return shards.filter(shard => shard.commands.length || shard.checks.length);
}

/** Partition a verified selection into complete dependency/feature obligations. */
function shardsForPlan(plan, platform, coreCommands) {
  requireThat(Object.hasOwn(PLATFORMS, platform), `Unsupported shard platform: ${platform}`);
  requireThat(plan && Array.isArray(plan.packages) && Array.isArray(plan.tests) &&
    Array.isArray(plan.profiles) && Array.isArray(plan.checks), 'Invalid shard plan');
  requireThat(['none', 'affected', 'full'].includes(plan.mode), 'Unknown shard plan mode');
  if (plan.mode === 'none') {
    requireThat(!plan.packages.length && !plan.tests.length && !plan.profiles.length &&
      !plan.checks.length, 'No-impact plan contains executable obligations');
    return [];
  }
  coreCommands ||= require('./validate-impact.cjs').coreCommands;
  requireThat(typeof coreCommands === 'function', 'Core command factory must be a function');
  const digest = selectionDigest(plan);
  let shards = [];
  const add = (id, commands, checks, featureGroup, weight, extra = {}) => {
    if (!commands.length && !checks.length) return;
    const obligations = [
      ...commands.map(command => `${platform}:core:${command.id}`),
      ...checks.map(check => `${platform}:check:${check}`),
    ].sort();
    requireThat(new Set(obligations).size === obligations.length, `Duplicate obligations in ${id}`);
    shards.push({id, commands, checks, obligations, selection_digest: digest,
      feature_group: featureGroup, estimated_seconds: weight, engineering: false,
      dotnet: commands.some(command => command.args.includes('--all-features') ||
        command.args.some(arg => arg.includes('semantic-model-dotnet'))), ...extra});
  };
  // Default and opt-in builds must never unify their feature graphs. Package
  // families co-locate their shared dependency foundation, not arbitrary argv
  // slices that can turn a complete target into a filtered test invocation.
  const families = new Map();
  for (const name of [...plan.packages].sort()) {
    const key = family(name);
    if (!families.has(key)) families.set(key, []);
    families.get(key).push(name);
  }
  for (const [key, familyPackages] of [...families].sort(([a], [b]) => a.localeCompare(b))) {
    const size = platform === 'windows' ? WINDOWS_DEFAULT_PACKAGES : familyPackages.length;
    for (let offset = 0; offset < familyPackages.length; offset += size) {
      const packages = familyPackages.slice(offset, offset + size);
      const suffix = familyPackages.length > size ? `-${crypto.createHash('sha256')
        .update(JSON.stringify(packages)).digest('hex').slice(0, 12)}` : '';
      const id = `default-${key}${suffix}`;
      const selected = {...plan, packages, tests: plan.tests.filter(test => packages.includes(test.package)),
        profiles: []};
      add(id, coreCommands(selected, platform).map(command =>
        ({...command, id: `${id}:${command.id}`})), [], `default-${key}`,
      INITIAL_WEIGHTS.default);
    }
  }
  const markers = new Set(), profileGroups = new Map();
  for (const profile of [...plan.profiles].sort((a, b) => a.id.localeCompare(b.id))) {
    if (!profile.platforms.includes(platform)) continue;
    requireThat(/^[a-zA-Z0-9][a-zA-Z0-9_-]*$/.test(profile.id), 'Invalid feature profile ID');
    const checks = (profile.checks || []).filter(id =>
      PROFILE_MARKERS.has(id) && plan.checks.includes(id) && !markers.has(id));
    checks.forEach(id => markers.add(id));
    const managed = profile.features.some(feature => feature.includes('semantic-model-dotnet'));
    const cohort = JSON.stringify([[...profile.packages].sort(), managed]);
    if (!profileGroups.has(cohort)) profileGroups.set(cohort, []);
    const groups = profileGroups.get(cohort);
    const weight = platform === 'windows' && profile.id === 'combined' ?
      INITIAL_WEIGHTS.windowsCombined : INITIAL_WEIGHTS.profile;
    let group = groups.at(-1);
    if (!group || group.weight + weight > PROFILE_GROUP_SECONDS) {
      group = {profiles: [], commands: [], checks: [], weight: 0};
      groups.push(group);
    }
    group.profiles.push(profile);
    group.commands.push(...coreCommands({...plan, packages: profile.packages,
      tests: plan.tests, profiles: [profile]}, platform).filter(command => !command.id.startsWith('default:')));
    group.checks.push(...checks);
    group.weight += weight;
  }
  // Two complete profiles sharing the same package/dependency cohort can reuse
  // compilation without merging their feature flags. The weight cap leaves
  // setup/fetch/cleanup headroom inside the separate 15-minute job ceiling.
  for (const groups of profileGroups.values()) for (const group of groups) {
    const ids = group.profiles.map(profile => profile.id);
    const id = ids.length === 1 ? `profile-${ids[0]}` :
      `profile-group-${crypto.createHash('sha256').update(JSON.stringify(ids)).digest('hex').slice(0, 16)}`;
    add(id, group.commands, group.checks, profileGroup({
      features: [...new Set(group.profiles.flatMap(profile => profile.features))],
    }), group.weight);
  }
  const details = new Map((plan.check_details || []).map(check => [check.id, check]));
  const applicable = id => !details.get(id)?.platforms || details.get(id).platforms.includes(platform);
  const checks = plan.checks.filter(id => applicable(id) && !markers.has(id));
  for (const id of checks) {
    requireThat(!PROFILE_MARKERS.has(id), `No native profile satisfies ${id} on ${platform}`);
    for (const dependency of details.get(id)?.depends_on || []) {
      requireThat(plan.checks.includes(dependency), `Missing selected dependency: ${id}/${dependency}`);
    }
  }
  const pinned = new Set(['oracle-tools', 'frozen-verify']);
  let changed = true;
  while (changed) {
    changed = false;
    for (const id of checks) {
      if (!pinned.has(id) && (details.get(id)?.depends_on || []).some(dep => pinned.has(dep))) {
        pinned.add(id);
        changed = true;
      }
    }
  }
  // All consumers of one frozen source stay in its preparation/integrity
  // domain. The executor runs frozen-verify in finally, including on failure.
  add('contracts-oracle', [], checks.filter(id => pinned.has(id)), 'oracle',
    INITIAL_WEIGHTS.oracle);
  add('contracts-native', [], checks.filter(id => !pinned.has(id)), 'contracts',
    INITIAL_WEIGHTS.contracts);
  if (plan.mode === 'full') {
    add('preflight-clippy', [{id: 'preflight:clippy-all-features', program: 'cargo',
      args: ['clippy', '--offline', '--locked', '--workspace', '--all-targets',
        '--all-features', '--', '-D', 'warnings']}], [], 'all-features',
    INITIAL_WEIGHTS.preflight);
    add('preflight-cli', [{id: 'preflight:cli-all-features', program: 'cargo', test: true,
      args: ['test', '--offline', '--locked', '-p', 'fabric-cli', '--all-targets',
        '--all-features', '--no-fail-fast']}], [], 'all-features',
    INITIAL_WEIGHTS.preflight);
  }
  // Equivalent package-local test commands can occur in otherwise different
  // profiles. Execute each descriptor once without joining large profile
  // domains: its owner carries every original obligation and command alias.
  // Effective cwd/environment defaults are shared by all commands in an owner;
  // explicit cwd/env and every other descriptor field participate in equality.
  shards = colocateEquivalentCommands(shards, platform);
  if (shards.length) {
    const engineering = shards.find(shard => shard.id === 'contracts-oracle') || shards[0];
    engineering.engineering = true;
    engineering.obligations.push(`${platform}:preflight:engineering-unit`);
    engineering.obligations.sort();
  }
  requireThat(shards.length > 0, 'Executable impact plan selected no native shards');
  const obligations = shards.flatMap(shard => shard.obligations);
  requireThat(new Set(obligations).size === obligations.length, 'Duplicate native obligations');
  return shards;
}

/** Resolve only trusted descriptors from the original, verified plan. */
function selectShard(plan, platform, id, coreCommands) {
  // The executor may already have the full command array. Family partitions
  // still need the canonical factory; never accept caller-supplied argv as a
  // replacement for regenerating commands from the original verified plan.
  const factory = Array.isArray(coreCommands) ? undefined : coreCommands;
  const shard = shardsForPlan(plan, platform, factory).find(candidate => candidate.id === id);
  requireThat(shard, `Unknown native shard: ${platform}/${id}`);
  return shard;
}

/** Produce the bounded native Actions matrix without host-dependent metadata. */
function matrixForPlan(plan, coreCommands) {
  const include = Object.entries(PLATFORMS).flatMap(([platform, os]) =>
    shardsForPlan(plan, platform, coreCommands).map(shard => ({
      os, platform, shard: shard.id, feature_group: shard.feature_group,
      engineering: shard.engineering, dotnet: shard.dotnet,
      estimated_seconds: shard.estimated_seconds,
      cache_group: shard.id.startsWith('default-') ? 'default' : 'features',
      cache_writer: false, dependency_cache_writer: false,
    })));
  requireThat(include.length <= 256, 'Native selection exceeds the Actions matrix bound');
  // Dispatch the longest dependency domains first, interleaving native hosts
  // rather than exhausting the Windows queue before Linux/macOS can start.
  include.sort((a, b) => b.estimated_seconds - a.estimated_seconds ||
    a.shard.localeCompare(b.shard) || a.platform.localeCompare(b.platform));
  // Cargo fingerprints isolate exact feature graphs inside these two stable
  // compilation cohorts. Only one producer per host/cohort saves build output;
  // never create a target cache per shard or source head.
  for (const platform of Object.keys(PLATFORMS)) {
    const native = include.filter(entry => entry.platform === platform);
    for (const cohort of ['default', 'features']) {
      const candidates = native.filter(entry => entry.cache_group === cohort);
      const writer = candidates.find(entry => entry.shard === 'preflight-clippy') ||
        candidates.find(entry => entry.shard.startsWith('default-cli')) || candidates[0];
      if (writer) writer.cache_writer = true;
    }
    const dependencyWriter = native.find(entry => entry.cache_writer &&
      entry.cache_group === 'features') || native[0];
    if (dependencyWriter) dependencyWriter.dependency_cache_writer = true;
  }
  return {include};
}

module.exports = {PLATFORMS, INITIAL_WEIGHTS, PROFILE_GROUP_SECONDS, WINDOWS_DEFAULT_PACKAGES,
  shardsForPlan, selectShard, matrixForPlan};
