// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {execFileSync} = require('node:child_process');
const {LIMITS, measure} = require('./impact-cache.cjs');

test('bounded cache measurements retain exact byte counts and reject oversize saves', t => {
  const root = path.join(__dirname, `cache-test-${process.pid}`);
  assert.ok(!fs.existsSync(root));
  fs.mkdirSync(root);
  t.after(() => fs.rmSync(root, {recursive: true, force: true}));
  fs.mkdirSync(path.join(root, 'nested'));
  fs.writeFileSync(path.join(root, 'one'), '12345');
  fs.writeFileSync(path.join(root, 'nested', 'two'), '123');
  const metric = measure([root], {limit: 8});
  assert.equal(metric.bytes, 8);
  assert.equal(metric.files, 2);
  assert.equal(metric.complete, true);
  assert.equal(metric.eligible, true);
  assert.equal(measure([root], {limit: 7}).reason, 'size_limit');
  assert.equal(measure([root], {limit: 8, maxFiles: 1}).eligible, false);
  assert.equal(measure([root], {limit: 8, maxMs: -1}).reason, 'measurement_budget');
  assert.equal(measure([path.join(root, 'missing')], {limit: 8}).reason, 'empty');
});

test('cache caps limit six measured target cohorts plus three source archives to 9.75 GiB raw', () => {
  assert.equal(LIMITS.target, 1536 * 1024 ** 2);
  assert.equal(LIMITS.dependencies, 256 * 1024 ** 2);
  assert.equal((6 * LIMITS.target + 3 * LIMITS.dependencies) / 1024 ** 3, 9.75);
});

test('cache CLI records raw size and bounded save eligibility without using ambient caches', t => {
  const root = path.join(__dirname, `cache-cli-test-${process.pid}`);
  assert.ok(!fs.existsSync(root));
  fs.mkdirSync(path.join(root, 'registry', 'cache'), {recursive: true});
  fs.mkdirSync(path.join(root, 'registry', 'src'), {recursive: true});
  t.after(() => fs.rmSync(root, {recursive: true, force: true}));
  fs.writeFileSync(path.join(root, 'registry', 'cache', 'fixture'), '12345');
  fs.writeFileSync(path.join(root, 'registry', 'src', 'not-cached'), 'expanded source is regenerated');
  const output = path.join(root, 'cache-dependencies.json');
  execFileSync(process.execPath, [path.join(__dirname, 'impact-cache.cjs'), 'dependencies', output],
    {timeout: 30000, env: {...process.env, CARGO_HOME: root,
      IMPACT_SETUP_STARTED: String(Date.now() / 1000), IMPACT_CACHE_WRITER: 'true',
      IMPACT_PLATFORM: 'windows', IMPACT_SHARD: 'cache-fixture', IMPACT_CACHE_GROUP: 'default',
      GITHUB_OUTPUT: path.join(root, 'outputs'), GITHUB_STEP_SUMMARY: path.join(root, 'summary')}});
  const metric = JSON.parse(fs.readFileSync(output, 'utf8'));
  assert.equal(metric.bytes, 5);
  assert.equal(metric.writer, true);
  assert.equal(metric.save_allowed, true);
  assert.equal(metric.kind, 'dependencies');
  assert.match(fs.readFileSync(path.join(root, 'outputs'), 'utf8'), /eligible=true/);
  assert.match(fs.readFileSync(path.join(root, 'summary'), 'utf8'), /MiB raw/);
});
