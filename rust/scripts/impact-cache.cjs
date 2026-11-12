// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');

const LIMITS = Object.freeze({target: 1024 ** 3, dependencies: 512 * 1024 ** 2});

/** Measure bounded cache candidates without following links or mutating them. */
function measure(roots, {limit, maxFiles = 250000, maxMs = 10000} = {}) {
  const started = Date.now(), pending = [...roots];
  let bytes = 0, files = 0, complete = true, reason = null;
  try {
    while (pending.length) {
      if (files >= maxFiles || Date.now() - started > maxMs) {
        complete = false;
        reason = 'measurement_budget';
        break;
      }
      const current = pending.pop();
      if (!fs.existsSync(current)) continue;
      const stat = fs.lstatSync(current);
      if (stat.isSymbolicLink()) {
        complete = false;
        reason = 'symlink_candidate';
        break;
      }
      if (stat.isDirectory()) {
        pending.push(...fs.readdirSync(current).map(name => path.join(current, name)));
      } else if (stat.isFile()) {
        bytes += stat.size;
        files++;
      } else {
        complete = false;
        reason = 'non_regular_candidate';
        break;
      }
    }
  } catch (error) {
    complete = false;
    reason = `measurement_error:${error.code || 'unknown'}`;
  }
  const eligible = complete && files > 0 && bytes <= limit;
  return {bytes, files, complete, limit_bytes: limit, eligible,
    reason: reason || (files === 0 ? 'empty' : bytes > limit ? 'size_limit' : 'within_limit'),
    measurement_ms: Date.now() - started};
}

function main(argv) {
  const [kind, output] = argv;
  if (!Object.hasOwn(LIMITS, kind) || !output || !path.isAbsolute(output) || argv.length !== 2) {
    throw new Error('Usage: impact-cache.cjs dependencies|target <absolute-metric-file>');
  }
  const root = path.resolve(__dirname, '..', '..');
  const cargo = process.env.CARGO_HOME || path.join(os.homedir(), '.cargo');
  const roots = kind === 'target' ? [path.join(root, 'rust', 'target')] :
    [path.join(cargo, 'registry'), path.join(cargo, 'git')];
  const metric = {...measure(roots, {limit: LIMITS[kind]}), kind,
    platform: process.env.IMPACT_PLATFORM, shard: process.env.IMPACT_SHARD,
    cache_group: process.env.IMPACT_CACHE_GROUP, writer: process.env.IMPACT_CACHE_WRITER === 'true'};
  // Keep artifact time inside the 15-minute ceiling; cache saves are optional.
  const elapsed = Date.now() / 1000 - Number(process.env.IMPACT_SETUP_STARTED);
  const timeAvailable = Number.isFinite(elapsed) && elapsed < 720;
  metric.save_allowed = metric.eligible && timeAvailable;
  if (metric.eligible && !timeAvailable) metric.reason = 'artifact_time_reserved';
  fs.writeFileSync(output, `${JSON.stringify(metric, null, 2)}\n`);
  if (process.env.GITHUB_OUTPUT) fs.appendFileSync(process.env.GITHUB_OUTPUT,
    `eligible=${metric.save_allowed}\n`);
  if (process.env.GITHUB_STEP_SUMMARY) fs.appendFileSync(process.env.GITHUB_STEP_SUMMARY,
    `\nCache ${kind} / ${metric.cache_group}: ${(metric.bytes / 1024 ** 2).toFixed(1)} MiB raw ` +
    `(${metric.complete ? 'complete' : 'partial'}), ${metric.reason}; ` +
    `designated producer: ${metric.writer}.\n`);
  console.log(JSON.stringify(metric));
}

module.exports = {LIMITS, measure};
if (require.main === module) {
  try { main(process.argv.slice(2)); }
  catch (error) { console.error(error.message); process.exitCode = 1; }
}
