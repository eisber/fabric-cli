// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const {spawnSync} = require('node:child_process');

const BRANCH = 'dev/marcozo/fab-cli-v2-integration-20261104';
const REPOSITORY = 'eisber/fabric-cli';
const TARGET_REF = `refs/heads/${BRANCH}`;
const API_ROOT = `https://api.github.com/repos/${REPOSITORY}/git`;
const ENDPOINT = `${API_ROOT}/ref/heads/${BRANCH}`;
const FILE = 'integration-target-witness.json';
const SHA = /^(?!0{40}$)[a-f0-9]{40}$/;
const digest = bytes => crypto.createHash('sha256').update(bytes).digest('hex');

function requireWitness(condition, code, message) {
  if (!condition) throw Object.assign(new Error(message), {code});
}

function witnessContext(event, env) {
  const valid = env.GITHUB_EVENT_NAME === 'pull_request' &&
    env.GITHUB_REPOSITORY === REPOSITORY && event.repository?.full_name === REPOSITORY &&
    event.pull_request?.base?.repo?.full_name === REPOSITORY &&
    event.pull_request.base.ref === BRANCH &&
    Number.isSafeInteger(event.number) && event.number > 0 &&
    event.pull_request.number === event.number &&
    env.GITHUB_REF === `refs/pull/${event.number}/merge` &&
    SHA.test(env.GITHUB_SHA || '') && SHA.test(event.pull_request.head?.sha || '') &&
    SHA.test(event.pull_request.base.sha || '') &&
    /^[1-9]\d*$/.test(env.GITHUB_RUN_ID || '') &&
    /^[1-9]\d*$/.test(env.GITHUB_RUN_ATTEMPT || '') && env.GITHUB_JOB === 'smoke' &&
    env.GITHUB_SERVER_URL === 'https://github.com' &&
    env.GITHUB_API_URL === 'https://api.github.com';
  requireWitness(valid, 'TARGET_WITNESS_CONTEXT', 'Invalid exact integration PR/run context');
  return {repository: REPOSITORY, target_ref: TARGET_REF, event_ref: env.GITHUB_REF,
    event_base: event.pull_request.base.sha, source: event.pull_request.head.sha,
    checkout: env.GITHUB_SHA, run_id: env.GITHUB_RUN_ID,
    run_attempt: env.GITHUB_RUN_ATTEMPT, job: env.GITHUB_JOB};
}

function validateCheckout(context, actual, parents) {
  requireWitness(actual === context.checkout && Array.isArray(parents) &&
    parents.length === 2 && parents.every(parent => typeof parent === 'string' && SHA.test(parent)) &&
    parents[1] === context.source,
  'TARGET_WITNESS_CHECKOUT', 'Require exact event checkout and two ordered base/source parents');
}

function validateWitness(witness, context, actual, parents, now = Date.now()) {
  validateCheckout(context, actual, parents);
  const fields = ['schema', 'kind', ...Object.keys(context), 'checkout_base', 'endpoint',
    'requested_at', 'observed_at', 'provider_date'];
  requireWitness(witness && typeof witness === 'object' && !Array.isArray(witness) &&
    JSON.stringify(Object.keys(witness).sort()) === JSON.stringify(fields.sort()) &&
    witness.schema === 1 && witness.kind === 'github-rest-exact-target-ref' &&
    witness.endpoint === ENDPOINT && typeof witness.checkout_base === 'string' &&
    SHA.test(witness.checkout_base),
  'TARGET_WITNESS_SCHEMA', 'Missing or invalid exact-target witness schema');
  for (const [key, value] of Object.entries(context)) {
    requireWitness(witness[key] === value, 'TARGET_WITNESS_BINDING',
      `Target witness does not bind current ${key}`);
  }
  const observed = Date.parse(witness.observed_at);
  const requested = Date.parse(witness.requested_at);
  const providerDate = Date.parse(witness.provider_date);
  requireWitness(typeof witness.observed_at === 'string' && Number.isFinite(observed) &&
    observed > 0 && observed <= now && new Date(observed).toISOString() === witness.observed_at,
  'TARGET_WITNESS_TIME', 'Invalid or future target witness observation');
  requireWitness(typeof witness.requested_at === 'string' && Number.isFinite(requested) &&
    requested > 0 && requested <= observed && observed - requested <= 30000 &&
    new Date(requested).toISOString() === witness.requested_at &&
    typeof witness.provider_date === 'string' && Number.isFinite(providerDate) &&
    new Date(providerDate).toUTCString() === witness.provider_date &&
    providerDate >= requested - 1000 && providerDate <= observed + 1000,
  'TARGET_WITNESS_FRESHNESS', 'Provider date must bind the bounded fresh target request');
  if (witness.checkout_base !== parents[0]) {
    throw Object.assign(new Error('Independently observed target does not equal the checkout first parent'),
      {code: 'TARGET_DRIFT', expected_checkout_base: parents[0], observed_target_base: witness.checkout_base});
  }
  return witness;
}

function loadWitness(env = process.env) {
  requireWitness(env.RUNNER_TEMP && path.isAbsolute(env.RUNNER_TEMP) &&
    /^[a-f0-9]{64}$/.test(env.INTEGRATION_TARGET_WITNESS_SHA256 || ''),
  'TARGET_WITNESS_MISSING', 'Require this run attempt target witness and workflow digest');
  let bytes;
  try {
    const file = path.join(env.RUNNER_TEMP, FILE);
    requireWitness(fs.statSync(file).size <= 16384, 'TARGET_WITNESS_SCHEMA', 'Oversized target witness');
    bytes = fs.readFileSync(file);
  } catch (error) {
    if (error.code === 'TARGET_WITNESS_SCHEMA') throw error;
    throw Object.assign(new Error('Target witness file is unavailable'), {code: 'TARGET_WITNESS_MISSING'});
  }
  requireWitness(digest(bytes) === env.INTEGRATION_TARGET_WITNESS_SHA256,
    'TARGET_WITNESS_DIGEST', 'Target witness bytes differ from the workflow digest');
  try { return JSON.parse(bytes.toString('utf8')); }
  catch { throw Object.assign(new Error('Invalid target witness JSON'), {code: 'TARGET_WITNESS_SCHEMA'}); }
}

async function acquireWitness(event, env, actual, parents, fetcher = fetch, clock = Date.now) {
  const context = witnessContext(event, env);
  validateCheckout(context, actual, parents);
  requireWitness(typeof env.GH_TOKEN === 'string' && env.GH_TOKEN.length > 0,
    'TARGET_WITNESS_AUTH', 'Require the step-scoped read-only workflow token');
  const requested = clock();
  let response;
  try {
    response = await fetcher(ENDPOINT, {method: 'GET', redirect: 'error', cache: 'no-store',
      signal: AbortSignal.timeout(15000),
      headers: {Accept: 'application/vnd.github+json', Authorization: `Bearer ${env.GH_TOKEN}`,
        'X-GitHub-Api-Version': '2022-11-28', 'Cache-Control': 'no-cache, no-store',
        'User-Agent': 'fabric-cli-integration-target-witness'}});
  } catch {
    throw Object.assign(new Error('Exact-target provider read failed; no retry or fallback'),
      {code: 'TARGET_WITNESS_PROVIDER'});
  }
  if (response.status !== 200) {
    const error = Object.assign(new Error('Exact-target provider read failed; no retry or fallback'),
      {code: 'TARGET_WITNESS_PROVIDER', provider_status: response.status});
    for (const [header, field] of [
      ['retry-after', 'retry_after_seconds'], ['x-ratelimit-reset', 'rate_limit_reset_epoch_seconds'],
    ]) {
      const value = response.headers.get(header);
      if (/^\d{1,15}$/.test(value || '') && Number.isSafeInteger(Number(value))) {
        error[field] = Number(value);
      }
    }
    throw error;
  }
  let value;
  try {
    const chunks = [];
    let size = 0;
    for await (const chunk of response.body) {
      size += chunk.length;
      requireWitness(size <= 16384, 'TARGET_WITNESS_PROVIDER', 'Oversized exact-target response');
      chunks.push(chunk);
    }
    value = JSON.parse(Buffer.concat(chunks).toString('utf8'));
  } catch {
    throw Object.assign(new Error('Invalid or incomplete exact-target provider response'),
      {code: 'TARGET_WITNESS_PROVIDER'});
  }
  requireWitness(value?.ref === TARGET_REF &&
    value.url === `${API_ROOT}/refs/heads/${BRANCH}` &&
    value.object?.type === 'commit' && typeof value.object.sha === 'string' &&
    SHA.test(value.object.sha) && value.object.url === `${API_ROOT}/commits/${value.object.sha}`,
  'TARGET_WITNESS_PROVIDER_SCHEMA', 'Provider response is not the exact repository/target commit ref');
  const age = response.headers.get('age');
  requireWitness(age === null || age === '0', 'TARGET_WITNESS_FRESHNESS',
    'Cached target responses cannot establish a fresh target witness');
  const witness = {schema: 1, kind: 'github-rest-exact-target-ref', ...context,
    checkout_base: value.object.sha, endpoint: ENDPOINT, requested_at: new Date(requested).toISOString(),
    observed_at: new Date(clock()).toISOString(), provider_date: response.headers.get('date')};
  return validateWitness(witness, context, actual, parents, clock());
}

async function recordWitness(output, acquire, environmentFile) {
  // Exclusive creation plus run/attempt/checkout binding forbids reusing an old file as a fresh read.
  const fd = fs.openSync(output, 'wx');
  try {
    let witness;
    try { witness = await acquire(); }
    catch (error) {
      const code = /^TARGET_[A-Z_]+$/.test(error.code || '') ? error.code : 'TARGET_WITNESS_FAILED';
      const failure = {schema: 1, passed: false, error_code: code};
      if (Number.isInteger(error.provider_status) && error.provider_status >= 100 &&
        error.provider_status <= 599) failure.provider_status = error.provider_status;
      for (const field of ['retry_after_seconds', 'rate_limit_reset_epoch_seconds']) {
        if (Number.isSafeInteger(error[field]) && error[field] >= 0) failure[field] = error[field];
      }
      if (code === 'TARGET_DRIFT' && SHA.test(error.expected_checkout_base || '') &&
        SHA.test(error.observed_target_base || '')) {
        failure.expected_checkout_base = error.expected_checkout_base;
        failure.observed_target_base = error.observed_target_base;
      }
      fs.writeFileSync(fd, `${JSON.stringify(failure)}\n`);
      throw Object.assign(new Error(`Target witness acquisition failed: ${JSON.stringify(failure)}`), {code});
    }
    const bytes = `${JSON.stringify(witness, null, 2)}\n`;
    fs.writeFileSync(fd, bytes);
    fs.appendFileSync(environmentFile, `INTEGRATION_TARGET_WITNESS_SHA256=${digest(bytes)}\n`);
  } finally {
    fs.closeSync(fd);
  }
}

async function main(output) {
  const env = process.env;
  assert.equal(env.RUNNER_ENVIRONMENT, 'github-hosted', 'Target witness acquisition is hosted only');
  assert.ok(env.RUNNER_TEMP && path.isAbsolute(env.RUNNER_TEMP));
  assert.ok(output && path.isAbsolute(output), 'An absolute witness output path is required');
  assert.equal(path.resolve(output), path.resolve(env.RUNNER_TEMP, FILE));
  assert.ok(env.GITHUB_ENV && path.isAbsolute(env.GITHUB_ENV));
  const event = JSON.parse(fs.readFileSync(env.GITHUB_EVENT_PATH, 'utf8'));
  const root = path.resolve(__dirname, '../..');
  const git = args => {
    const result = spawnSync('git', args, {cwd: root, encoding: 'utf8', shell: false,
      windowsHide: true, timeout: 10000, maxBuffer: 1024 * 1024});
    assert.equal(result.error, undefined, 'Local Git inspection must finish');
    assert.equal(result.status, 0, 'Local Git inspection must succeed');
    return result.stdout.trim();
  };
  await recordWitness(output, () => acquireWitness(event, env, git(['rev-parse', 'HEAD']),
    git(['show', '-s', '--format=%P', 'HEAD']).split(' ')), env.GITHUB_ENV);
}

module.exports = {BRANCH, REPOSITORY, TARGET_REF, ENDPOINT, FILE, digest, witnessContext,
  validateWitness, loadWitness, acquireWitness, recordWitness};
if (require.main === module) {
  main(process.argv[2]).catch(error => {
    console.error(error.message);
    process.exitCode = 1;
  });
}
