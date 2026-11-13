// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {capture, sourcePin, requireUnchanged} = require('./integration-smoke.cjs');
const {sourceInventory, sourceDifferences} = require('./validate-impact.cjs');

const CANDIDATE = 'dev/marcozo/integrate-native-lakehouse-schema-discov-tofmij';
const BASE = '69dca48d62aeb74ac11519fa0f1f994173d92e20';
const SOURCE = 'e8a0400836ea5cdb8968fe71126a395cee400eb8';
const COMMANDS = [
  ['fetch', '--locked', '--target', 'x86_64-pc-windows-msvc'],
  ['test', '--offline', '--locked', '-p', 'fabric-sdk-tables', '--test', 'schema_document'],
  ['test', '--offline', '--locked', '-p', 'fabric-cli', '--no-default-features',
    '--features', 'v2-discovery,headless', '--test', 'v2_schema_native', '--test', 'v2_help',
    '--test', 'v2_discovery', '--test', 'v2_discovery_host'],
  ['test', '--offline', '--locked', '-p', 'fabric-cli', '--no-default-features',
    '--features', 'v2-discovery,headless', '--bin', 'fab-cli-discovery', 'schema::tests::',
    '--', '--test-threads=1'],
  ['test', '--offline', '--locked', '-p', 'fabric-cli', '--no-default-features',
    '--features', 'v2-discovery,headless,native-read', '--test', 'table_mounted'],
];

function testResults(stdout) {
  const results = [...stdout.matchAll(
    /test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out/g,
  )].map(match => ({
    passed: Number(match[1]), failed: Number(match[2]), ignored: Number(match[3]),
    measured: Number(match[4]), filtered: Number(match[5]),
  }));
  assert.ok(results.length > 0, 'Missing real test results');
  for (const result of results) {
    assert.ok(result.passed > 0, 'An empty test target is not schema proof');
    assert.equal(result.failed, 0);
    assert.equal(result.ignored, 0, 'Ignored schema tests are not proof');
  }
  return results;
}

function main(output) {
  const root = fs.realpathSync(path.resolve(__dirname, '../..'));
  assert.ok(output && path.isAbsolute(output), 'An absolute external artifact directory is required');
  const artifacts = path.resolve(output);
  assert.ok(artifacts !== root && !artifacts.startsWith(`${root}${path.sep}`), 'Artifacts must be external');
  fs.mkdirSync(artifacts, {recursive: false});
  const started = Date.now();
  const report = {schema: 1, passed: false, qualification: 'candidate-only offline Windows schema proof',
    original_source: SOURCE, integration_base: BASE, commands: [],
    run_id: process.env.GITHUB_RUN_ID, run_attempt: process.env.GITHUB_RUN_ATTEMPT};
  const save = () => fs.writeFileSync(path.join(artifacts, 'receipt.json'), `${JSON.stringify(report, null, 2)}\n`);
  const persist = (_, result) => {
    const prefix = String(report.commands.length).padStart(2, '0');
    for (const stream of ['stdout', 'stderr']) {
      fs.writeFileSync(path.join(artifacts, `${prefix}-${stream}.log`), result[stream] || '');
    }
    save();
  };
  const run = (file, args, options = {}) => capture(file, args,
    {cwd: root, timeout: 30000, ...options}, report, persist);
  const git = args => {
    const result = run('git', args);
    assert.equal(result.status, 0, `Git failed: ${args.join(' ')}`);
    return result.stdout.trim();
  };
  let before;
  try {
    save();
    assert.equal(process.platform, 'win32', 'Hosted Windows only');
    assert.equal(process.env.RUNNER_ENVIRONMENT, 'github-hosted', 'No shared local native execution');
    assert.equal(process.env.RUNNER_OS, 'Windows');
    assert.equal(process.env.GITHUB_EVENT_NAME, 'pull_request');
    const event = JSON.parse(fs.readFileSync(process.env.GITHUB_EVENT_PATH, 'utf8'));
    assert.equal(event.pull_request.head.ref, CANDIDATE);
    assert.equal(event.pull_request.base.sha, BASE, 'Integration base moved');
    const actual = git(['rev-parse', 'HEAD']);
    report.source = sourcePin('pull_request', event, process.env.GITHUB_REF,
      process.env.GITHUB_SHA, actual, git(['show', '-s', '--format=%P', 'HEAD']).split(' '), false);
    git(['merge-base', '--is-ancestor', SOURCE, event.pull_request.head.sha]);
    assert.equal(git(['status', '--porcelain']), '');
    before = sourceInventory(root);
    fs.writeFileSync(path.join(artifacts, 'source-before.json'), JSON.stringify(before, null, 2));
    for (const args of COMMANDS) {
      const result = run('cargo', args, {cwd: path.join(root, 'rust'), timeout: 8 * 60 * 1000});
      assert.equal(result.status, 0, `Schema command failed: cargo ${args.join(' ')}`);
      if (args[0] === 'test') {
        report.commands.at(-1).tests = testResults(result.stdout);
        if (args.includes('--bin')) {
          for (const name of ['shared_pipe_driver',
            'shared_undrained_pipe_cannot_block_expiry_or_cancellation_diagnostics']) {
            assert.ok(result.stdout.includes(`test schema::tests::${name} ... ok`),
              `Missing registered process test: ${name}`);
          }
        }
      }
      save();
    }
    report.passed = true;
  } catch (error) {
    report.error = error.message;
    throw error;
  } finally {
    try {
      if (before) {
        const after = sourceInventory(root);
        fs.writeFileSync(path.join(artifacts, 'source-after.json'), JSON.stringify(after, null, 2));
        fs.writeFileSync(path.join(artifacts, 'source-differences.json'),
          JSON.stringify(sourceDifferences(before, after), null, 2));
        requireUnchanged(before, after);
      }
    } catch (error) {
      report.passed = false;
      report.integrity_error = error.message;
      throw error;
    } finally {
      report.elapsed_seconds = (Date.now() - started) / 1000;
      save();
    }
  }
}

module.exports = {CANDIDATE, BASE, SOURCE, COMMANDS, testResults};
if (require.main === module) {
  try { assert.equal(process.argv.length, 3); main(process.argv[2]); }
  catch (error) { console.error(error.stack); process.exitCode = 1; }
}
