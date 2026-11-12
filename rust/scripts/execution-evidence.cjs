// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');

const MAX_RECEIPT_BYTES = 512 * 1024;
const MAX_MANIFEST_BYTES = MAX_RECEIPT_BYTES;
const MAX_COMMANDS = 4096;
const PLATFORMS = {win32: 'windows', linux: 'linux', darwin: 'macos'};
const TOOLCHAINS = [
  ['cargo', ['--version']], ['rustc', ['--version', '--verbose']],
  ['rustup', ['show', 'active-toolchain']],
];
const sha256 = value => crypto.createHash('sha256').update(value).digest('hex');
const digest = value => sha256(JSON.stringify(value));
const hash = value => typeof value === 'string' && /^[a-f0-9]{64}$/.test(value);
function requireThat(condition, message) {
  if (!condition) throw new Error(`Execution evidence: ${message}`);
}
function same(first, second) {
  return JSON.stringify(first) === JSON.stringify(second);
}
function exactStrings(actual, expected) {
  return Array.isArray(actual) && actual.every(value => typeof value === 'string') &&
    new Set(actual).size === actual.length && same([...actual].sort(), [...expected].sort());
}

/** Resolve the Cargo output domain once; invalid overrides must never fall back. */
function executionContext(context, environment = process.env) {
  const paths = context.host_platform === 'win32' ? path.win32 :
    ['linux', 'darwin'].includes(context.host_platform) ? path.posix : path;
  const root = paths.resolve(context.root), artifacts = paths.resolve(context.artifacts);
  const configured = context.targetDir !== undefined ? context.targetDir :
    context.env?.CARGO_TARGET_DIR !== undefined ? context.env.CARGO_TARGET_DIR :
      environment.CARGO_TARGET_DIR;
  requireThat(configured === undefined || (typeof configured === 'string' &&
    configured.length > 0 && !/[\0\r\n]/.test(configured)), 'invalid CARGO_TARGET_DIR');
  const targetDir = paths.resolve(root, 'rust', configured === undefined ? 'target' : configured);
  return {root, artifacts, targetDir,
    targetSource: context.targetSource || (configured === undefined ? 'default' : 'environment')};
}

function portable(value, context) {
  let result = String(value).replaceAll('\\', '/');
  const prefixes = [[context.root, '<ROOT>'], [context.artifacts, '<ARTIFACTS>'],
    [context.targetDir, '<TARGET>']].map(([prefix, token]) =>
    [prefix.replaceAll('\\', '/').replace(/\/$/, ''), token])
    .sort(([first], [second]) => second.length - first.length);
  for (const [prefix, token] of prefixes) {
    const escaped = prefix.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    result = result.replace(new RegExp(`${escaped}(?=/|$)`, 'g'), token);
  }
  return result;
}

/** Hash actual argv, cwd and effective validation environment without retaining values. */
function descriptorDigest(command, context, actualEnvironment) {
  const native = executionContext(context);
  const defaults = {PYTHONUTF8: '1', PYTHONDONTWRITEBYTECODE: '1',
    FAB_UPDATE_TEST_ROOT: native.artifacts, FAB_CLI_V2_TEST_ROOT: native.artifacts,
    FAB_IDENTITY_TEST_ROOT: native.artifacts, CARGO_TARGET_DIR: native.targetDir};
  const environment = actualEnvironment || {...defaults, ...command.env};
  const names = [...new Set([...Object.keys(defaults), ...Object.keys(command.env || {})])].sort();
  return digest({
    program: portable(command.program, native), args: command.args.map(value => portable(value, native)),
    cwd: portable(command.cwd || path.join(native.root, 'rust'), native),
    env: names.map(name => [name, environment[name] === undefined ? null :
      portable(environment[name], native)]),
    cargo_target_source: native.targetSource,
    kind: command.kind || 'command', test: command.test || false,
    stdout: command.stdout ? portable(command.stdout, native) : null,
  });
}

/** Dependencies execute in each consuming shard; they do not acquire its owned obligations. */
function executionChecks(checks, platform, graph) {
  graph ||= JSON.parse(fs.readFileSync(path.join(__dirname, '..', 'validation', 'impact-graph.json'), 'utf8'));
  const known = new Map(graph.checks.map(check => [check.id, check]));
  const complete = new Set(), active = new Set(), order = [];
  const visit = id => {
    if (complete.has(id)) return;
    const check = known.get(id);
    requireThat(check, `unknown execution dependency: ${id}`);
    if (check.platforms && !check.platforms.includes(platform)) return;
    requireThat(!active.has(id), `cyclic execution dependency: ${id}`);
    active.add(id);
    for (const dependency of check.depends_on || []) visit(dependency);
    active.delete(id);
    complete.add(id);
    order.push(id);
  };
  checks.forEach(visit);
  return order;
}

/** Compare file bytes across native checkouts without host-specific mode metadata. */
function sourceFingerprint(files) {
  requireThat(Array.isArray(files), 'missing source inventory');
  const snapshots = files.map(file => {
    requireThat(typeof file.path === 'string' && ['file', 'deleted', 'missing'].includes(file.state),
      'invalid source inventory entry');
    if (file.state !== 'file') return {path: file.path, state: 'missing'};
    const bytes = file.size ?? file.bytes, checksum = file.digest ?? file.sha256;
    requireThat(Number.isSafeInteger(bytes) && bytes >= 0 && hash(checksum),
      'invalid source file hash');
    return {path: file.path, state: 'file', bytes, sha256: checksum};
  }).sort((a, b) => a.path < b.path ? -1 : a.path > b.path ? 1 : 0);
  requireThat(new Set(snapshots.map(file => file.path)).size === snapshots.length,
    'duplicate source inventory entry');
  return digest(snapshots);
}

/** Bind the native plan's verified input inventory to both execution inventories. */
function provenance(plan, before, after, verifiedBefore, verifiedAfter) {
  return {
    head: plan.head, plan_fingerprint: plan.fingerprint,
    inventory_scope: plan.inputs?.inventory?.scope,
    inventory_digest: plan.inputs?.inventory?.digest,
    inventory_files: plan.inputs?.inventory?.files?.length,
    plan_source: sourceFingerprint(plan.inputs?.inventory?.files),
    execution_source_before: sourceFingerprint(before.files),
    execution_source_after: sourceFingerprint(after.files),
    source_before: before.digest, source_after: after.digest,
    dirty: plan.dirty, dirty_files: plan.dirty_files,
    synthetic_scope: plan.synthetic_scope, head_complete: plan.head_complete,
    evidence_scope: plan.evidence_scope, input_evidence_scope: plan.inputs?.evidence_scope,
    verified_before: verifiedBefore, verified_after: verifiedAfter,
  };
}

/** Keep actual results and contract completion links, never command logs or environment secrets. */
function createManifest(context, receipt) {
  const commands = context.results.map((record, index) => ({
    index, id: record.id, aliases: record.aliases, program: record.program,
    argv_sha256: digest(record.args), execution_identity: record.execution_identity,
    descriptor_sha256: record.descriptor_sha256,
    status: record.status, exit: record.exit, signal: record.signal, error: record.error,
    timed_out: record.timed_out, cancelled: record.cancelled,
    cleanup_confirmed: record.cleanup_confirmed, cleanup_errors: record.cleanup_errors,
    stdout_sha256: record.stdout_sha256, stderr_sha256: record.stderr_sha256,
    ...(record.id.startsWith('toolchain:') ?
      {output: fs.readFileSync(record.stdout, 'utf8')} : {}),
  }));
  const manifest = {
    schema: 1, head: receipt.head, platform: receipt.platform, shard: receipt.shard,
    selection_digest: receipt.selection_digest, plan_fingerprint: receipt.plan_fingerprint,
    execution: {host_platform: process.platform, host_arch: process.arch, python: context.python,
      cargo_target: {source: executionContext(context).targetSource,
        path_sha256: sha256(executionContext(context).targetDir.replaceAll('\\', '/'))}},
    provenance: receipt.provenance, commands, checks: context.completedChecks || [],
  };
  requireThat(commands.length <= MAX_COMMANDS &&
    Buffer.byteLength(JSON.stringify(manifest)) <= MAX_MANIFEST_BYTES, 'manifest exceeded bounds');
  return manifest;
}

function verifyProvenance(plan, receipt, manifest) {
  const proof = manifest.provenance;
  requireThat(proof && same(receipt.provenance, proof), 'missing or mismatched native provenance');
  requireThat(hash(receipt.plan_fingerprint) &&
    proof.plan_fingerprint === receipt.plan_fingerprint && proof.head === plan.head &&
    proof.inventory_scope === 'git-tracked-and-untracked' && hash(proof.inventory_digest) &&
    Number.isSafeInteger(proof.inventory_files) && proof.inventory_files > 0 &&
    proof.inventory_files === plan.inputs?.inventory?.files?.length &&
    proof.dirty === false && exactStrings(proof.dirty_files, []) &&
    proof.synthetic_scope === false && proof.head_complete === true &&
    proof.evidence_scope === 'checkout' && proof.input_evidence_scope === 'checkout',
  'native plan is not clean, head-complete checkout provenance');
  const source = sourceFingerprint(plan.inputs?.inventory?.files);
  requireThat(proof.plan_source === source && proof.execution_source_before === source &&
    proof.execution_source_after === source && proof.source_before === receipt.source_before &&
    proof.source_after === receipt.source_after, 'source does not match verified native plan and gate checkout');
  for (const verified of [proof.verified_before, proof.verified_after]) {
    requireThat(verified?.valid === true && verified.fingerprint === receipt.plan_fingerprint &&
      verified.synthetic_scope === false && verified.head_complete === true &&
      verified.evidence_scope === 'checkout', 'missing source-bound plan verification');
  }
}

/** Derive exact obligation coverage from successful executions, not a passed label. */
function verifyManifest(plan, shard, receipt) {
  const manifest = receipt.execution_manifest?.data;
  requireThat(Buffer.byteLength(JSON.stringify(receipt)) + 1 <= MAX_RECEIPT_BYTES,
    'receipt exceeded 512 KiB');
  requireThat(manifest?.schema === 1 && manifest.head === receipt.head &&
    manifest.platform === receipt.platform && manifest.shard === receipt.shard &&
    manifest.selection_digest === receipt.selection_digest &&
    manifest.plan_fingerprint === receipt.plan_fingerprint, 'missing or mismatched manifest');
  requireThat(receipt.execution_manifest.sha256 === digest(manifest), 'manifest digest mismatch');
  requireThat(receipt.stage === 'all' && receipt.timed_out === false && receipt.cancelled === false &&
    !receipt.error && !receipt.source_integrity_error, 'contradictory execution receipt');
  const commands = manifest.commands;
  requireThat(Array.isArray(commands) && commands.length > 0 && commands.length <= MAX_COMMANDS &&
    receipt.commands === commands.length &&
    Buffer.byteLength(JSON.stringify(manifest)) <= MAX_MANIFEST_BYTES, 'invalid command count or manifest bounds');
  for (const [index, command] of commands.entries()) {
    requireThat(command.index === index && typeof command.id === 'string' && command.id.length > 0 &&
      exactStrings(command.aliases, [...new Set(command.aliases || [])]) &&
      command.aliases.includes(command.id) && typeof command.program === 'string' &&
      command.program.length > 0 && hash(command.argv_sha256) && hash(command.execution_identity) &&
      hash(command.descriptor_sha256) &&
      hash(command.stdout_sha256) && hash(command.stderr_sha256) &&
      command.status === 'passed' && command.exit === 0 && command.signal === null &&
      command.error === null && command.timed_out === false && command.cancelled === false &&
      command.cleanup_confirmed === true && exactStrings(command.cleanup_errors, []),
    `unsuccessful or contradictory command result: ${index}`);
  }
  verifyProvenance(plan, receipt, manifest);
  const host = manifest.execution;
  requireThat(host && PLATFORMS[host.host_platform] === receipt.platform &&
    ['x64', 'arm64'].includes(host.host_arch) &&
    typeof host.python === 'string' && host.python.length > 0 &&
    ['default', 'environment'].includes(host.cargo_target?.source) &&
    hash(host.cargo_target.path_sha256), 'missing native host provenance');
  const used = new Set(), core = new Map(), covered = new Set();
  for (const command of shard.commands) {
    for (const alias of [command.id, ...(command.aliases || [])]) {
      requireThat(!core.has(alias) || same(core.get(alias), command), 'ambiguous core descriptor');
      core.set(alias, command);
    }
  }
  for (const command of commands) {
    if (!command.aliases.some(alias => core.has(alias))) continue;
    for (const alias of command.aliases) {
      const expected = core.get(alias);
      requireThat(expected && !covered.has(alias) && command.program === expected.program &&
        command.argv_sha256 === digest(expected.args), `core alias or argv mismatch: ${alias}`);
      covered.add(alias);
    }
    used.add(command.index);
  }
  requireThat(covered.size === core.size, 'missing executed core alias');
  const take = (index, label) => {
    requireThat(Number.isSafeInteger(index) && index >= 0 && index < commands.length &&
      !used.has(index), `missing or reused command for ${label}`);
    used.add(index);
    return commands[index];
  };
  for (const [name, args] of TOOLCHAINS) {
    const matches = commands.filter(command => command.id === `toolchain:${name}`);
    requireThat(matches.length === 1, `missing toolchain command: ${name}`);
    const command = take(matches[0].index, `toolchain:${name}`);
    requireThat(command.program === name && command.argv_sha256 === digest(args) &&
      exactStrings(command.aliases, [`toolchain:${name}`]) &&
      typeof command.output === 'string' && command.output.length <= 16384 &&
      sha256(command.output) === command.stdout_sha256 &&
      receipt.toolchain?.[name] === command.output.trim(), `unbound toolchain output: ${name}`);
  }
  requireThat(/^cargo \d+\.\d+\.\d+\b/.test(receipt.toolchain.cargo) &&
    /^rustc \d+\.\d+\.\d+\b/.test(receipt.toolchain.rustc), 'missing actual toolchain versions');
  const target = /(?:^|\n)host: ([a-z0-9_-]+)(?:\r?\n|$)/.exec(receipt.toolchain.rustc)?.[1];
  const expectedHost = {windows: /^[^-]+-pc-windows-/, linux: /^[^-]+-unknown-linux-/,
    macos: /^[^-]+-apple-darwin/}[receipt.platform];
  requireThat(target && expectedHost.test(target) &&
    target.startsWith(host.host_arch === 'x64' ? 'x86_64-' : 'aarch64-') &&
    receipt.toolchain.rustup.includes(target), 'toolchain host mismatch');
  const registry = require('./validation-checks.cjs');
  const descriptorContext = {
    root: path.resolve(path.parse(__dirname).root, 'fabric-evidence-source'),
    artifacts: path.resolve(path.parse(__dirname).root, 'fabric-evidence-artifacts'),
    platform: receipt.platform, python: host.python, plan,
    targetSource: host.cargo_target.source,
  };
  descriptorContext.targetDir = host.cargo_target.source === 'default' ?
    path.join(descriptorContext.root, 'rust', 'target') :
    path.join(descriptorContext.artifacts, 'cargo-target');
  const completed = manifest.checks;
  const expectedChecks = executionChecks(shard.checks, receipt.platform);
  requireThat(Array.isArray(completed) &&
    exactStrings(completed.map(check => check.id), expectedChecks), 'missing or unexpected completed check');
  for (const check of completed) {
    const expected = registry.commandsFor(check.id, descriptorContext);
    requireThat(Array.isArray(check.descriptors) && check.descriptors.length === expected.length,
      `registry descriptor count mismatch: ${check.id}`);
    if (!expected.length) {
      const profiles = plan.profiles.filter(profile => profile.checks?.includes(check.id) &&
        profile.platforms.includes(receipt.platform) &&
        (profile.tests || plan.tests.filter(test => profile.packages.includes(test.package))).length > 0);
      requireThat(profiles.length > 0 &&
        exactStrings(check.profiles, profiles.map(profile => profile.id)),
      `missing executed marker profile: ${check.id}`);
    } else {
      requireThat(exactStrings(check.profiles, []), `unexpected marker profile: ${check.id}`);
    }
    const indices = [];
    for (const [offset, descriptor] of expected.entries()) {
      const evidence = check.descriptors[offset], kind = descriptor.kind || 'command';
      const expectedDigest = descriptorDigest(descriptor, descriptorContext);
      requireThat(evidence?.kind === kind, `registry descriptor kind mismatch: ${check.id}`);
      requireThat(evidence.descriptor_sha256 === expectedDigest,
        `registry descriptor identity mismatch: ${check.id}`);
      if (kind === 'transport-graphs') {
        requireThat(evidence.passed === true && same(evidence.inputs, indices) && indices.length === 3,
          'transport graph assertion lacks executed inputs');
        continue;
      }
      const command = take(evidence.command, check.id);
      const id = kind === 'prepare-oracle' ? 'oracle:prepare' : descriptor.id || check.id;
      requireThat(command.id === id && exactStrings(command.aliases, [id]) &&
        command.program === descriptor.program && command.descriptor_sha256 === expectedDigest,
      `contract command or argv/cwd/env mismatch: ${check.id}`);
      indices.push(command.index);
    }
  }
  if (shard.engineering) {
    const matches = commands.filter(command => command.id === 'preflight:engineering-unit');
    requireThat(matches.length === 1, 'missing engineering execution');
    const command = take(matches[0].index, 'engineering');
    requireThat(command.program === host.python &&
      exactStrings(command.aliases, ['preflight:engineering-unit']), 'engineering command mismatch');
  }
  for (const command of commands) {
    if (used.has(command.index)) continue;
    requireThat(expectedChecks.includes('oracle-tools') && command.id === 'oracle:restore' &&
      exactStrings(command.aliases, ['oracle:restore']) && command.program === host.python,
    `unexpected unclaimed command: ${command.id}`);
  }
  return true;
}

module.exports = {MAX_RECEIPT_BYTES, MAX_MANIFEST_BYTES, MAX_COMMANDS, digest, sourceFingerprint, provenance,
  executionContext, descriptorDigest, executionChecks, createManifest, verifyManifest};
