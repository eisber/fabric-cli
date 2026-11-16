// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {capture, sourcePin, requireUnchanged} = require('./integration-smoke.cjs');
const {sourceInventory, sourceDifferences} = require('./validate-impact.cjs');
const notebook = require('./integration-notebook-smoke.cjs');
const ocv = require('./integration-ocv-smoke.cjs');
const schema = require('./integration-schema-smoke.cjs');

const CANDIDATE = 'dev/marcozo/integrate-approved-notebook-metadata-upd-toman9';
const BASE = '92ebf4a34ef319c291c8344eafdb4c0ab2242ea6';
const SOURCE = '3f0f3a83d4b926c2b20dd0e168d3fab576d6112c';
const ROOT = path.resolve(__dirname, '../..');

function registeredTests(file) {
  const text = fs.readFileSync(path.join(ROOT, file), 'utf8');
  const names = [...text.matchAll(/#\[(?:tokio::)?test(?:\([^\]]*\))?\]\s*(?:async\s+)?fn\s+(\w+)/g)]
    .map(match => match[1]);
  assert.ok(names.length > 0, `No registered tests in ${file}`);
  return names;
}

const HOST_TESTS = registeredTests('rust/clients/cli/app/src/v2_update_notebook_host_tests.rs')
  .map(name => `v2_update_notebook_host::tests::${name}`);
const UPDATE_TARGETS = [
  {package: 'fabric-sdk-items', target: 'update',
    source: 'rust/engine/control-plane/resources/items/tests/update.rs'},
  {package: 'fabric-cli-mutations', target: 'v2_update',
    source: 'rust/clients/cli/commands/mutations/tests/v2_update.rs'},
  {package: 'fabric-cli', target: 'update_v2', features: 'headless',
    source: 'rust/clients/cli/app/tests/update_v2.rs'},
];
const COMMANDS = [
  {args: notebook.COMMANDS[0]},
  ...UPDATE_TARGETS.map(item => ({
    args: ['test', '--offline', '--locked', '-p', item.package, '--no-default-features',
      ...(item.features ? ['--features', item.features] : []), '--test', item.target],
    required: registeredTests(item.source),
    exhaustive: true,
  })),
  {args: ['test', '--offline', '--locked', '-p', 'fabric-cli', '--no-default-features',
    '--features', 'headless', '--bin', 'fab-cli-v2', 'v2_update_notebook_host::tests::'],
  required: HOST_TESTS, exhaustive: true},
  {args: ['test', '--offline', '--locked', '-p', 'fabric-cli', '--no-default-features',
    '--features', 'headless', '--test', 'v2_get_binary', '--test', 'v2_exists_binary',
    '--test', 'v2_search_binary']},
  ...notebook.COMMANDS.slice(1).map(args => ({args})),
  ...ocv.COMMANDS.slice(3, 8),
];

function testResults(stdout, stderr, command) {
  let results;
  if (command.args.includes('--bin') && command.args.includes('fab-cli-v2')) {
    assert.match(stderr, /Running unittests src[\\/]v2_main\.rs \(/,
      'The real headless binary unit target must run');
    results = schema.testResults(stdout);
    assert.equal(results.length, 1, 'Exactly one binary unit target is required');
  } else {
    results = ocv.testResults(stdout, stderr, command);
  }
  for (const name of command.required || []) {
    assert.ok(stdout.includes(`test ${name} ... ok`), `Missing registered test: ${name}`);
  }
  if (command.exhaustive) {
    assert.equal(results.length, 1);
    assert.equal(results[0].passed, command.required.length, 'Every registered selected test must run');
  }
  return results;
}

function main(output) {
  const root = fs.realpathSync(ROOT);
  assert.ok(output && path.isAbsolute(output), 'An absolute external artifact directory is required');
  const artifacts = path.resolve(output);
  assert.ok(artifacts !== root && !artifacts.startsWith(`${root}${path.sep}`), 'Artifacts must be external');
  fs.mkdirSync(artifacts, {recursive: false});
  const started = Date.now();
  const report = {schema: 1, passed: false,
    qualification: 'candidate-only offline Windows Notebook UPDATE and shared-consumer proof',
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
    git(['merge-base', '--is-ancestor', BASE, event.pull_request.head.sha]);
    assert.equal(git(['status', '--porcelain']), '');
    before = sourceInventory(root);
    fs.writeFileSync(path.join(artifacts, 'source-before.json'), JSON.stringify(before, null, 2));
    for (const command of COMMANDS) {
      const result = run('cargo', command.args, {cwd: path.join(root, 'rust'), timeout: 8 * 60 * 1000});
      assert.equal(result.status, 0, `UPDATE command failed: cargo ${command.args.join(' ')}`);
      if (command.args[0] === 'test') {
        report.commands.at(-1).tests = testResults(result.stdout, result.stderr, command);
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

module.exports = {CANDIDATE, BASE, SOURCE, HOST_TESTS, UPDATE_TARGETS, COMMANDS, registeredTests, testResults};
if (require.main === module) {
  try { assert.equal(process.argv.length, 3); main(process.argv[2]); }
  catch (error) { console.error(error.stack); process.exitCode = 1; }
}
