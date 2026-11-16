// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {capture, sourcePin, requireUnchanged} = require('./integration-smoke.cjs');
const {sourceInventory, sourceDifferences} = require('./validate-impact.cjs');
const notebook = require('./integration-notebook-smoke.cjs');
const schema = require('./integration-schema-smoke.cjs');

const CANDIDATE = 'dev/marcozo/integrate-authenticated-ocv-feedback-pr9-tom97x';
const BASE = '1c3c819c3c4162af13d23b4dd3aa475b06b16526';
const SOURCE = 'e6360a417d6d56c84e1e92fb338519ea34877a03';
const HOST_TESTS = [
  'preparation_generates_one_v4_id_or_preserves_override_without_entropy',
  'accepted_output_failure_is_an_error_and_uncertain_outcomes_stop_batches',
];
const NATIVE_HOST_TESTS = [
  ...HOST_TESTS,
  'successful_attempt_preserves_pending_correlation_before_completion',
  'pending_output_failure_prevents_operation_polling',
  'cancellation_deadline_and_signal_errors_drop_work_and_retain_uncertainty',
];
const COMMANDS = [
  {args: notebook.COMMANDS[0]},
  {args: ['test', '--offline', '--locked', '-p', 'fabric-identity',
    '--no-default-features', '--features', 'windows-broker', '--lib'], minimum: 48,
  required: ['windows::tests::ocv_connect_waits_for_confirmed_teardown_before_immediate_refresh',
    'windows::tests::cancelled_ocv_teardown_wait_retains_reservation_until_native_exit',
    'windows::tests::logout_during_ocv_teardown_wait_prevents_late_credential_publication',
    'windows::tests::ocv_connect_rejects_unconfirmed_teardown_and_keeps_replacement_blocked',
    'windows::tests::ocv_teardown_wait_timeout_cancels_without_releasing_native_reservation']},
  {args: ['test', '--offline', '--locked', '-p', 'fabric-identity-windows', '--lib'], minimum: 48,
    required: ['entra_contract::tests::ocv_audiences_use_distinct_exact_delegated_scopes']},
  {args: ['test', '--offline', '--locked', '-p', 'fabric-sdk-http', '--lib'], minimum: 50,
    required: ['tests::feedback_statuses_are_single_posts_without_retry_waits',
      'tests::feedback_transport_and_stream_failures_are_sanitized_without_replay']},
  {args: ['test', '--offline', '--locked', '-p', 'fabric-cli', '--no-default-features',
    '--test', 'feedback'], minimum: 12},
  {args: ['test', '--offline', '--locked', '-p', 'fabric-cli', '--no-default-features',
    '--bin', 'fab-rs', 'feedback_host::tests::'], minimum: 2,
  required: HOST_TESTS.map(name => `feedback_host::tests::${name}`)},
  {args: ['test', '--offline', '--locked', '-p', 'fabric-cli', '--no-default-features',
    '--features', 'native-read', '--test', 'feedback'], minimum: 16},
  {args: ['test', '--offline', '--locked', '-p', 'fabric-cli', '--no-default-features',
    '--features', 'native-read', '--bin', 'fab-rs', 'feedback_host::tests::'], minimum: 5,
  required: NATIVE_HOST_TESTS.map(name => `feedback_host::tests::${name}`)},
  ...notebook.COMMANDS.slice(1).map(args => ({args})),
  {args: schema.COMMANDS.at(-1)},
];

function testResults(stdout, stderr, command) {
  const {args, minimum = 1, required = []} = command;
  let results;
  if (args.includes('--test')) {
    results = notebook.testResults(stdout, stderr, args);
  } else {
    const target = args.includes('--lib') ? 'src[\\\\/]lib\\.rs' : 'src[\\\\/]main\\.rs';
    assert.match(stderr, new RegExp(`Running unittests ${target} \\(`), 'Selected unit target must run');
    results = schema.testResults(stdout);
    assert.equal(results.length, 1, 'Exactly one selected unit target must run');
    if (!args.includes('feedback_host::tests::')) {
      assert.equal(results[0].filtered, 0, 'Complete library suites must not filter tests');
    }
  }
  assert.ok(results.every(result => result.passed >= minimum), 'Required test count was not executed');
  for (const name of required) {
    assert.ok(stdout.includes(`test ${name} ... ok`), `Missing registered test: ${name}`);
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
  const report = {schema: 1, passed: false,
    qualification: 'candidate-only offline Windows OCV/auth/HTTP and shared-consumer proof',
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
      assert.equal(result.status, 0, `OCV command failed: cargo ${command.args.join(' ')}`);
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

module.exports = {CANDIDATE, BASE, SOURCE, COMMANDS, HOST_TESTS, NATIVE_HOST_TESTS, testResults};
if (require.main === module) {
  try { assert.equal(process.argv.length, 3); main(process.argv[2]); }
  catch (error) { console.error(error.stack); process.exitCode = 1; }
}
