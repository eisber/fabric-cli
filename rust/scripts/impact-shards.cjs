// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const crypto = require('node:crypto');
const {selectionDigest} = require('./impact-ci.cjs');
const TIMINGS = require('./impact-shard-timings.json');

const PLATFORMS = Object.freeze({
  windows: 'windows-2025', linux: 'ubuntu-24.04', macos: 'macos-15',
});
const PROFILE_MARKERS = new Set(['interactive', 'headless', 'certificate', 'updater', 'combined']);
// The executor has 12 minutes inside a separate 15-minute Actions ceiling.
// Queue latency is deliberately not a duration weight. Setup/cleanup reserve
// exceeds the observed ~30-75s overhead; it is not a runner availability promise.
const PROFILE_GROUP_SECONDS = 720;
const SETUP_CLEANUP_SECONDS = 180;
const COLD_RESERVE_SECONDS = Object.freeze({linux: 120, windows: 180, macos: 180});
// Full target sets cost substantially more than the historical serial warm
// profiles. Seven-profile bins exhausted the executor budget in run 43303660409.
// Bound full co-location independently of the unchanged affected-mode weights.
const FULL_PROFILE_GROUP_SECONDS = 600;
const FULL_PROFILE_MAX_DOMAINS = 2;
const FULL_PROFILE_COLD_SECONDS = 180;
const FULL_PROFILE_MIN_SECONDS = 120;
const WINDOWS_DEFAULT_PACKAGES = 8;
const HISTORICAL = new Map(Object.values(TIMINGS.historical));

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

function durationEstimate(commands, platform, fullProfile = false) {
  // An unfamiliar execution environment or descriptor must not inherit a
  // familiar argv's timing. This affects placement only, never execution.
  const knownShape = commands.every(command => Object.keys(command).every(key =>
    ['id', 'program', 'args', 'test'].includes(key)));
  const signature = knownShape ? crypto.createHash('sha256').update(JSON.stringify(
    commands.map(command => [command.program, command.args, command.test || false])))
    .digest('hex').slice(0, 16) : null;
  const observed = TIMINGS.observed[signature];
  const historical = HISTORICAL.get(signature);
  const full = fullProfile ? TIMINGS.full[signature] : undefined;
  if (full && !full[2]) return {seconds: PROFILE_GROUP_SECONDS,
    reserve: 0, basis: 'full-incomplete-isolated', isolated: true};
  if (!observed && !historical && !full) return {seconds: PROFILE_GROUP_SECONDS,
    reserve: 0, basis: 'unmeasured-isolated', isolated: true};
  const index = {linux: 1, windows: 2, macos: 3}[platform];
  let seconds, basis, reserve;
  if (observed) {
    // Missing native measurements use the slower observed native host or twice
    // Linux, explicitly extrapolated rather than labelled native evidence.
    seconds = observed[index] ?? Math.max(observed[2] || 0, observed[3] || 0,
      (observed[1] || 0) * (platform === 'linux' ? 1 : 2));
    basis = observed[index] === null ? 'cross-host-estimate' : 'observed-mixed-cache';
    reserve = 60;
  } else {
    seconds = (historical || full[1]) * (platform === 'linux' ? 1 : 2);
    basis = platform === 'linux' ? 'historical-warm-mix' : 'historical-cross-host-estimate';
    reserve = COLD_RESERVE_SECONDS[platform];
  }
  if (fullProfile) {
    seconds = Math.max(seconds, (full?.[1] || 0) * (platform === 'linux' ? 1 : 2));
    return {seconds: Math.max(FULL_PROFILE_MIN_SECONDS, Math.ceil(seconds * 1.1)),
      reserve: FULL_PROFILE_COLD_SECONDS, isolated: false,
      basis: full ? platform === 'linux' ? 'full-observed-cache-miss' : 'full-cross-host-estimate' :
        `full-conservative-${basis}`};
  }
  return {seconds: Math.ceil(seconds * 1.1), reserve, basis, isolated: false};
}

function packDomains(domains, platform, mode) {
  const cohorts = new Map(), bins = [];
  for (const domain of domains) {
    const key = domain.duration.isolated ? domain.id : JSON.stringify([domain.cohort, domain.dotnet]);
    if (!cohorts.has(key)) cohorts.set(key, []);
    cohorts.get(key).push(domain);
  }
  for (const cohort of cohorts.values()) {
    const first = cohort[0];
    const fullProfiles = mode === 'full' && first.cohort === 'profile' && !first.duration.isolated;
    const limit = fullProfiles ? FULL_PROFILE_GROUP_SECONDS : PROFILE_GROUP_SECONDS;
    const domainLimit = fullProfiles ? FULL_PROFILE_MAX_DOMAINS : Infinity;
    const total = cohort.reduce((sum, domain) => sum + domain.duration.seconds, 0);
    const reserve = Math.max(...cohort.map(domain => domain.duration.reserve));
    // Do not turn a leaf into one nearly twelve-minute feature job merely to
    // minimize runner count. Two balanced feature cohorts still fit alongside
    // defaults and the isolated oracle in twelve jobs across three hosts.
    const count = Math.min(cohort.length, Math.max(
      Math.ceil(total / (limit - reserve)), Math.ceil(cohort.length / domainLimit),
      first.cohort === 'profile' && total > PROFILE_GROUP_SECONDS / 2 ? 2 : 1));
    const empty = () => ({domains: [], seconds: 0, reserve: 0, cohort: first.cohort,
      dotnet: first.dotnet, isolated: first.duration.isolated, formatting: 0});
    const native = Array.from({length: count}, empty);
    for (const domain of [...cohort].sort((a, b) => b.duration.seconds - a.duration.seconds ||
      a.id.localeCompare(b.id))) {
      const compatible = bin => !domain.duration.isolated && !bin.isolated &&
        bin.cohort === domain.cohort && bin.dotnet === domain.dotnet &&
        bin.domains.length < domainLimit &&
        (platform !== 'windows' || bin.formatting + domain.commands.filter(command =>
          command.args[0] === 'fmt').length <= WINDOWS_DEFAULT_PACKAGES) &&
        bin.seconds + domain.duration.seconds + Math.max(bin.reserve, domain.duration.reserve) <=
          limit;
      // Longest-first, least-loaded placement balances the critical path. A
      // bounded spill bin handles heterogeneous reserves/formatting limits.
      let bin = [...native].sort((a, b) => a.seconds - b.seconds)
        .find(candidate => !candidate.domains.length || compatible(candidate));
      if (!bin) {
        bin = empty();
        native.push(bin);
      }
      bin.domains.push(domain);
      bin.seconds += domain.duration.seconds;
      bin.reserve = Math.max(bin.reserve, domain.duration.reserve);
      bin.formatting += domain.commands.filter(command => command.args[0] === 'fmt').length;
    }
    bins.push(...native.filter(bin => bin.domains.length));
  }
  return bins.map(bin => {
    // Preserve generation order (and thus alias ownership), not size order.
    const members = bin.domains.sort((a, b) => a.order - b.order);
    const first = members[0], ids = members.map(domain => domain.id);
    const hash = crypto.createHash('sha256').update(JSON.stringify(ids)).digest('hex').slice(0, 16);
    const id = members.length === 1 ? first.id : `${bin.cohort}-group-${hash}`;
    requireThat(bin.seconds + bin.reserve <= PROFILE_GROUP_SECONDS,
      `Estimated native domain exceeds the execution budget: ${id}`);
    const {duration, cohort, order, ...descriptor} = first;
    return {...descriptor, id,
      commands: members.flatMap(domain => domain.commands),
      checks: members.flatMap(domain => domain.checks),
      obligations: members.flatMap(domain => domain.obligations).sort(),
      feature_group: members.length === 1 ? first.feature_group : `${bin.cohort}-${hash}`,
      estimated_seconds: Math.ceil(bin.seconds + bin.reserve),
      cold_reserve_seconds: bin.reserve,
      setup_cleanup_seconds: SETUP_CLEANUP_SECONDS,
      estimated_job_seconds: Math.ceil(bin.seconds + bin.reserve) + SETUP_CLEANUP_SECONDS,
      estimate_basis: [...new Set(members.map(domain => domain.duration.basis))].sort().join('+'),
      domains: ids,
    };
  });
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
  const add = (id, commands, checks, featureGroup, extra = {}) => {
    if (!commands.length && !checks.length) return;
    const obligations = [
      ...commands.map(command => `${platform}:core:${command.id}`),
      ...checks.map(check => `${platform}:check:${check}`),
    ].sort();
    requireThat(new Set(obligations).size === obligations.length, `Duplicate obligations in ${id}`);
    shards.push({id, commands, checks, obligations, selection_digest: digest,
      feature_group: featureGroup, engineering: false,
      order: shards.length, cohort: id.startsWith('default-') ? 'default' : 'profile',
      duration: durationEstimate(commands, platform,
        plan.mode === 'full' && id.startsWith('profile-')),
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
        ({...command, id: `${id}:${command.id}`})), [], `default-${key}`);
    }
  }
  const markers = new Set();
  for (const profile of [...plan.profiles].sort((a, b) => a.id.localeCompare(b.id))) {
    if (!profile.platforms.includes(platform)) continue;
    requireThat(/^[a-zA-Z0-9][a-zA-Z0-9_-]*$/.test(profile.id), 'Invalid feature profile ID');
    const checks = (profile.checks || []).filter(id =>
      PROFILE_MARKERS.has(id) && plan.checks.includes(id) && !markers.has(id));
    checks.forEach(id => markers.add(id));
    add(`profile-${profile.id}`, coreCommands({...plan, packages: profile.packages,
      tests: plan.tests, profiles: [profile]}, platform)
      .filter(command => !command.id.startsWith('default:')), checks, profileGroup(profile));
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
    {duration: {seconds: 480, reserve: 0,
      basis: 'isolated-oracle', isolated: true}});
  add('contracts-native', [], checks.filter(id => !pinned.has(id)), 'contracts',
    {cohort: 'default', duration: {
      seconds: checks.filter(id => !pinned.has(id)).every(id =>
        ['selector-tests', 'command-inventory', 'dependency-graph'].includes(id)) ? 30 :
        120, reserve: 0, basis: 'contracts-allowance', isolated: false}});
  if (plan.mode === 'full') {
    add('preflight-clippy', [{id: 'preflight:clippy-all-features', program: 'cargo',
      args: ['clippy', '--offline', '--locked', '--workspace', '--all-targets',
        '--all-features', '--', '-D', 'warnings']}], [], 'all-features',
    {duration: {seconds: 600, reserve: 0,
      basis: 'isolated-compilation-budget', isolated: true}});
    add('preflight-cli', [{
      id: 'preflight:cli-build-all-features', program: 'cargo',
      args: ['test', '--offline', '--locked', '-p', 'fabric-cli', '--all-targets',
        '--all-features', '--no-fail-fast', '--no-run'],
    }, {id: 'preflight:cli-all-features', program: 'cargo', test: true,
      args: ['test', '--offline', '--locked', '-p', 'fabric-cli', '--all-targets',
        '--all-features', '--no-fail-fast']}], [], 'all-features',
    {duration: {seconds: PROFILE_GROUP_SECONDS, reserve: 0,
      basis: 'isolated-compile-and-test-budget', isolated: true}});
  }
  // Equivalent package-local test commands can occur in otherwise different
  // profiles. Execute each descriptor once without joining large profile
  // domains: its owner carries every original obligation and command alias.
  // Effective cwd/environment defaults are shared by all commands in an owner;
  // explicit cwd/env and every other descriptor field participate in equality.
  shards = colocateEquivalentCommands(shards, platform);
  shards = packDomains(shards, platform, plan.mode);
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
      estimated_job_seconds: shard.estimated_job_seconds,
      setup_cleanup_seconds: shard.setup_cleanup_seconds,
      estimate_basis: shard.estimate_basis,
      domain_count: shard.domains.length,
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
        candidates.find(entry => entry.shard.startsWith('profile-')) ||
        candidates.find(entry => entry.shard.startsWith('default-cli')) || candidates[0];
      if (writer) writer.cache_writer = true;
    }
    const dependencyWriter = native.find(entry => entry.cache_writer &&
      entry.cache_group === 'features') || native[0];
    if (dependencyWriter) dependencyWriter.dependency_cache_writer = true;
  }
  return {include};
}

module.exports = {PLATFORMS, PROFILE_GROUP_SECONDS, SETUP_CLEANUP_SECONDS,
  FULL_PROFILE_GROUP_SECONDS, FULL_PROFILE_MAX_DOMAINS, FULL_PROFILE_COLD_SECONDS,
  WINDOWS_DEFAULT_PACKAGES,
  shardsForPlan, selectShard, matrixForPlan};
