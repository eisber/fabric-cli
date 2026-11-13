// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const {execFileSync} = require('node:child_process');

function selection(plan) {
  const sorted = items => [...items].sort((a, b) => {
    const first = JSON.stringify(a), second = JSON.stringify(b);
    return first < second ? -1 : first > second ? 1 : 0;
  });
  const tests = items => sorted(items.map(test => ({package: test.package, target: test.target,
    ...(test.filter ? {filter: test.filter} : {}),
    default_available: test.default_available !== false && test.default !== false &&
      !test.default_skipped && !test.profile_only,
    default_skipped_targets: sorted(test.default_skipped_targets || []),
    profiles: sorted(test.profiles || []),
  })));
  return {
    mode: plan.mode,
    files: sorted(plan.files),
    packages: sorted(plan.packages),
    tests: tests(plan.tests),
    profiles: sorted(plan.profiles.map(profile => ({id: profile.id,
      packages: sorted(profile.packages), features: sorted(profile.features),
      ...(profile.test_features ? {test_features: Object.fromEntries(
        Object.entries(profile.test_features).sort(([a], [b]) => a.localeCompare(b))
          .map(([name, features]) => [name, sorted(features)]))} : {}),
      platforms: sorted(profile.platforms), checks: sorted(profile.checks || []),
      tests: tests(profile.tests || plan.tests.filter(test => profile.packages.includes(test.package))),
    }))),
    checks: sorted(plan.checks),
    check_details: sorted((plan.check_details || []).map(check => ({id: check.id,
      packages: sorted(check.packages || []), components: sorted(check.components || []),
      platforms: sorted(check.platforms || []), depends_on: sorted(check.depends_on || []),
    }))),
    check_order: [...(plan.check_order || [])],
  };
}

function selectionDigest(plan) {
  return crypto.createHash('sha256').update(JSON.stringify(selection(plan))).digest('hex');
}

function equalSelection(expected, actual) {
  if (JSON.stringify(selection(expected)) !== JSON.stringify(selection(actual))) {
    throw new Error('Native impact selection differs from the recorded selection job');
  }
}

function requestFor(head, environment = process.env) {
  const dispatch = environment.GITHUB_EVENT_NAME === 'workflow_dispatch';
  const base = dispatch ? environment.IMPACT_DISPATCH_BASE : environment.IMPACT_BASE;
  const affected = dispatch ? environment.IMPACT_SCOPE === 'affected' :
    environment.GITHUB_EVENT_NAME !== 'schedule';
  const validBase = typeof base === 'string' && /^[a-f0-9]{40}$/i.test(base) &&
    !/^0{40}$/.test(base);
  return {schema: 1, head, base: validBase ? base : null, full: !affected || !validBase,
    reason: !affected ? 'Full validation requested (default or unknown dispatch scope)' :
      !validBase ? 'Missing or invalid base SHA; conservatively validating the full workspace' :
        'Affected validation using the actual Git diff'};
}

function requireThat(condition, message) {
  if (!condition) throw new Error(message);
}

/** Route the unchanged canonical matrix into independent native admission pools. */
function nativePoolsForMatrix(matrix) {
  const {PLATFORMS} = require('./impact-shards.cjs');
  requireThat(matrix && Array.isArray(matrix.include) && matrix.include.length <= 256,
    'Invalid native routing matrix');
  const seen = new Set();
  for (const entry of matrix.include) {
    requireThat(entry && Object.hasOwn(PLATFORMS, entry.platform) &&
      entry.os === PLATFORMS[entry.platform] &&
      typeof entry.shard === 'string' && /^[a-zA-Z0-9][a-zA-Z0-9_-]*$/.test(entry.shard),
    'Native routing matrix contains an invalid platform, image or shard');
    const key = `${entry.platform}/${entry.shard}`;
    requireThat(!seen.has(key), `Duplicate native routing entry: ${key}`);
    seen.add(key);
  }
  const include = Object.keys(PLATFORMS).flatMap(platform => {
    const entries = matrix.include.filter(entry => entry.platform === platform);
    return entries.length ? [{platform, shards: {include: entries}}] : [];
  });
  requireThat(!include.length || include.length === Object.keys(PLATFORMS).length,
    'Native routing must retain every selected native platform');
  return {include};
}

/** Reject a misrouted row before source-plan recomputation or shard execution. */
function verifyNativePool(platform, entry, hostPlatform = process.platform) {
  const {PLATFORMS} = require('./impact-shards.cjs');
  requireThat(Object.hasOwn(PLATFORMS, platform) && entry &&
    entry.platform === platform && entry.os === PLATFORMS[platform],
  'Native shard does not belong to its platform admission pool');
  requireThat({win32: 'windows', linux: 'linux', darwin: 'macos'}[hostPlatform] === platform,
    'Native admission pool does not match the execution host');
}

/** Require exactly the selected head/platform/shard/obligation receipts. */
function aggregate(plan, receipts, options = {}) {
  const {matrixForPlan, selectShard} = require('./impact-shards.cjs');
  const {verifyManifest} = require('./execution-evidence.cjs');
  requireThat(typeof plan.head === 'string' && /^[a-f0-9]{40}$/i.test(plan.head),
    'Aggregate requires the exact source head');
  requireThat(options.selectionResult === 'success', 'Dependency impact selection did not succeed');
  const matrix = matrixForPlan(plan);
  const expected = new Map(matrix.include.map(entry => [
    `${entry.platform}/${entry.shard}`, selectShard(plan, entry.platform, entry.shard),
  ]));
  requireThat(options.nativeResult === (expected.size ? 'success' : 'skipped'),
    'Selected native checks failed, were cancelled, or did not complete successfully');
  requireThat(Array.isArray(receipts), 'Native receipts must be an array');
  const seen = new Set(), engineering = options.engineeringReceipts || [];
  for (const receipt of receipts) {
    const key = `${receipt.platform}/${receipt.shard}`;
    requireThat(expected.has(key), `Unexpected native receipt: ${key}`);
    requireThat(!seen.has(key), `Duplicate native receipt: ${key}`);
    seen.add(key);
    const shard = expected.get(key);
    requireThat(receipt.schema === 1 && receipt.head === plan.head &&
      receipt.selection_digest === shard.selection_digest,
    `Stale or mismatched native receipt: ${key}`);
    requireThat(receipt.status === 'passed' && receipt.complete_plan === false,
      `Native shard did not pass: ${key}`);
    requireThat(typeof receipt.source_before === 'string' &&
      /^[a-f0-9]{64}$/.test(receipt.source_before) && receipt.source_before === receipt.source_after,
    `Native source-integrity evidence mismatch: ${key}`);
    requireThat(Array.isArray(receipt.obligations) &&
      receipt.obligations.every(id => typeof id === 'string') &&
      new Set(receipt.obligations).size === receipt.obligations.length &&
      JSON.stringify([...receipt.obligations].sort()) === JSON.stringify(shard.obligations),
    `Native obligation mismatch: ${key}`);
    verifyManifest(plan, shard, receipt);
    if (shard.engineering) {
      const reports = engineering.filter(report =>
        report.platform === receipt.platform && report.shard === receipt.shard);
      requireThat(reports.length === 1, `Missing or duplicate engineering preflight: ${key}`);
      require('./validate-impact.cjs').engineeringReceipt({report: reports[0],
        head: plan.head, platform: receipt.platform, shard: receipt.shard});
    }
  }
  requireThat(seen.size === expected.size,
    `Missing native receipts: ${[...expected.keys()].filter(key => !seen.has(key)).join(', ')}`);
  requireThat(engineering.length === matrix.include.filter(entry => entry.engineering).length,
    'Unexpected engineering preflight receipts');
  return {schema: 1, head: plan.head, selection_digest: selectionDigest(plan), status: 'passed',
    shards: expected.size, obligations: [...expected.values()].reduce(
      (count, shard) => count + shard.obligations.length, 0), no_impact: !expected.size};
}

function readReceipts(directory) {
  const receipts = [], engineeringReceipts = [], timings = [], cacheMetrics = [];
  if (!fs.existsSync(directory)) return {receipts, engineeringReceipts, timings, cacheMetrics};
  const walk = current => {
    for (const item of fs.readdirSync(current, {withFileTypes: true})) {
      const file = path.join(current, item.name);
      requireThat(!item.isSymbolicLink(), 'Receipt artifacts must not contain symlinks');
      if (item.isDirectory()) walk(file);
      else if (['receipt.json', 'engineering-receipt.json', 'timing.json',
        'cache-target.json', 'cache-dependencies.json'].includes(item.name)) {
        const limit = item.name === 'receipt.json' ?
          require('./execution-evidence.cjs').MAX_RECEIPT_BYTES : 64 * 1024;
        requireThat(item.isFile() && fs.statSync(file).size <= limit, 'Receipt is not bounded');
        const value = JSON.parse(fs.readFileSync(file, 'utf8'));
        (item.name === 'receipt.json' ? receipts : item.name === 'engineering-receipt.json' ?
          engineeringReceipts : item.name === 'timing.json' ? timings : cacheMetrics).push(value);
      }
    }
  };
  walk(directory);
  return {receipts, engineeringReceipts, timings, cacheMetrics};
}

function main(argv) {
  const [mode, directory, receiptsDirectory] = argv;
  if (!['select', 'native', 'aggregate'].includes(mode) || !directory ||
      !path.isAbsolute(directory) || argv.length !== (mode === 'aggregate' ? 3 : 2) ||
      (mode === 'aggregate' && !path.isAbsolute(receiptsDirectory || ''))) {
    throw new Error('Usage: impact-ci.cjs select|native <absolute-artifact-directory> | aggregate <absolute-selection-directory> <absolute-receipts-directory>');
  }
  if (mode === 'native' && process.env.IMPACT_POOL_PLATFORM !== undefined) {
    verifyNativePool(process.env.IMPACT_POOL_PLATFORM, {
      platform: process.env.IMPACT_PLATFORM, os: process.env.IMPACT_RUNNER_IMAGE,
    });
  }
  const root = path.resolve(__dirname, '..', '..');
  const head = execFileSync('git', ['rev-parse', 'HEAD'], {cwd: root, encoding: 'utf8'}).trim();
  const requestFile = path.join(directory, 'request.json');
  const selectedFile = path.join(directory, 'selected-plan.json');
  fs.mkdirSync(directory, {recursive: true});
  const request = mode === 'select' ? requestFor(head) : JSON.parse(fs.readFileSync(requestFile, 'utf8'));
  if (request.schema !== 1 || request.head !== head ||
      typeof request.full !== 'boolean' || (request.base !== null && typeof request.base !== 'string')) {
    throw new Error('Selection request does not match the checked-out head');
  }
  const output = mode === 'select' ? selectedFile : path.join(directory, 'native-plan.json');
  execFileSync(process.execPath, [path.join(__dirname, 'impact-plan.cjs'),
    '--root', root, '--head', head,
    ...(request.full ? ['--full'] : request.base ? ['--base', request.base] : ['--full']),
    '--output', output], {cwd: root, stdio: 'inherit', timeout: 120000});
  const plan = JSON.parse(fs.readFileSync(output, 'utf8'));
  requireThat(plan.head === head && plan.inputs?.head === head,
    'Generated impact plan does not match the checked-out head');
  const {matrixForPlan, WINDOWS_DEFAULT_PACKAGES} = require('./impact-shards.cjs');
  if (mode !== 'select') {
    equalSelection(JSON.parse(fs.readFileSync(selectedFile, 'utf8')), plan);
    if (mode === 'aggregate') {
      const collected = readReceipts(receiptsDirectory);
      const result = aggregate(plan, collected.receipts, {...collected,
        selectionResult: process.env.SELECTION_RESULT, nativeResult: process.env.NATIVE_RESULT});
      fs.writeFileSync(path.join(directory, 'aggregate.json'), `${JSON.stringify(result, null, 2)}\n`);
      if (process.env.GITHUB_STEP_SUMMARY) fs.appendFileSync(process.env.GITHUB_STEP_SUMMARY,
        `## Rust validation aggregation\n\nHead: \`${head}\`\n\n` +
        `Selection: \`${result.selection_digest}\`\n\n` +
        `${result.shards} native shards; ${result.obligations} exact obligations passed.\n\n` +
        `Selection reasons: ${(plan.reasons || []).join('; ')}\n\n` +
        'Cache policy: at most six immutable target-cohort entries (3 GiB raw each) and three ' +
        'shared source-archive entries (256 MiB raw each) per manifest/toolchain generation. ' +
        'Oversize/late candidates skip cache saving, never validation obligations. ' +
        'Raw limits are not compressed archive sizes; entries and old generations remain subject ' +
        'to the unchanged provider quota/eviction policy.\n\n' +
        '| Platform / shard | Commands (s) | Setup (s) | Restore (s) | Target/deps hit | Target raw MiB |\n' +
        '| --- | ---: | ---: | ---: | --- | ---: |\n' +
        collected.receipts.map(receipt => {
          const timing = collected.timings.find(item =>
            item.platform === receipt.platform && item.shard === receipt.shard);
          const elapsed = Date.parse(receipt.finished_at) - Date.parse(receipt.started_at);
          const cache = collected.cacheMetrics.find(item => item.kind === 'target' &&
            item.platform === receipt.platform && item.shard === receipt.shard);
          return `| ${receipt.platform} / ${receipt.shard} | ` +
            `${Number.isFinite(elapsed) ? (elapsed / 1000).toFixed(1) : 'unavailable'} | ` +
            `${timing?.setup_seconds ?? 'unavailable'} | ${timing?.cache_seconds ?? 'unavailable'} | ` +
            `${timing ? `${timing.cache_hit}/${timing.dependency_cache_hit}` : 'unavailable'} | ` +
            `${cache ? `${(cache.bytes / 1024 ** 2).toFixed(1)}${cache.complete ? '' : ' (partial)'}` :
              'unavailable'} |\n`;
        }).join('') +
        '\nShared dependency cache candidates (raw sizes, not compressed archive sizes):\n\n' +
        collected.cacheMetrics.filter(item => item.kind === 'dependencies').map(item =>
          `- ${item.platform}: ${(item.bytes / 1024 ** 2).toFixed(1)} MiB, ` +
          `${item.complete ? 'complete' : 'partial'}, ${item.reason}.\n`).join(''));
      console.log(JSON.stringify(result));
    }
  } else {
    fs.writeFileSync(requestFile, `${JSON.stringify(request, null, 2)}\n`);
    const matrix = matrixForPlan(plan);
    const pools = nativePoolsForMatrix(matrix);
    const native = matrix.include.length > 0;
    fs.writeFileSync(path.join(directory, 'matrix.json'), `${JSON.stringify(matrix, null, 2)}\n`);
    fs.writeFileSync(path.join(directory, 'native-pools.json'), `${JSON.stringify(pools, null, 2)}\n`);
    if (process.env.GITHUB_OUTPUT) {
      fs.appendFileSync(process.env.GITHUB_OUTPUT,
        `run_native=${native}\nmatrix=${JSON.stringify(matrix)}\n` +
        `native_pools=${JSON.stringify(pools)}\n`);
    }
    if (process.env.GITHUB_STEP_SUMMARY) {
      fs.appendFileSync(process.env.GITHUB_STEP_SUMMARY,
        `## Rust dependency impact\n\nHead: \`${head}\`\n\n` +
        `Base: \`${request.base || 'full validation'}\`\n\n` +
        `Mode: **${plan.mode}**\n\n` +
        `Dispatch/request: ${request.reason}\n\n` +
        `Selection digest: \`${selectionDigest(plan)}\`\n\n` +
        `${matrix.include.length} selected native shards; three independent native host pools ` +
        `admit at most four jobs each (12 total), so a queued host cannot occupy another host's pool; ` +
        `each job is bounded to 15 minutes. Historical serial duration weights (mixed warm compiles) are scheduling priors, ` +
        `not a measured current baseline or a cold-run guarantee.\n\n` +
        `Windows default shards retain every per-package fmt check, with at most ` +
        `${WINDOWS_DEFAULT_PACKAGES} packages per dependency-family batch.\n\n` +
        'Cache writes are limited to one producer per OS/default-or-feature cohort, with no ' +
        'per-shard or per-head build-output caches. Dependency cache writers are shared per OS.\n\n' +
        `Packages: ${plan.packages.map(name => `\`${name}\``).join(', ') || 'none'}\n\n` +
        `${plan.tests.length} target selections; ${plan.profiles.length} feature profiles; ` +
        `${plan.checks.length} contract checks. The uploaded plan records reasons and inputs.\n\n` +
        `Selection reasons: ${(plan.reasons || []).join('; ')}\n`);
    }
  }
}

module.exports = {selection, selectionDigest, equalSelection, requestFor, aggregate, readReceipts,
  nativePoolsForMatrix, verifyNativePool};
if (require.main === module) {
  try { main(process.argv.slice(2)); }
  catch (error) { console.error(error.message); process.exitCode = 1; }
}
