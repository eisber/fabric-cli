// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const {capture, sourcePin, requireUnchanged} = require('./integration-smoke.cjs');
const {sourceInventory, sourceDifferences} = require('./validate-impact.cjs');
const {plan} = require('./impact-plan.cjs');
const update = require('./integration-update-smoke.cjs');
const notebook = require('./integration-notebook-smoke.cjs');
const ocv = require('./integration-ocv-smoke.cjs');

const ROOT = path.resolve(__dirname, '../..');
const CANDIDATE = 'dev/marcozo/integrate-explicit-stage-dataagent-reads-tomcl2';
const BASE = 'ce50f369f2713ed1a2599099458f19bb0750d3e3';
const SOURCE = '6af13a0d3f87077151ea674f32e2da659996b5ab';
const DEFAULT_POLICY = Object.freeze({
  id: 'candidate-qualification-v1', phase: 'candidate-qualification',
  executorMilliseconds: 570000, commandMaximumMilliseconds: 480000,
  workflowStepMinutes: 10, enclosingJobMinutes: 45, cargoWorkInventoryMaximum: null,
});
const KQL_POLICY = Object.freeze({
  id: 'kql-link-full-qualification-v2', phase: 'candidate-qualification',
  executorMilliseconds: 1140000, commandMaximumMilliseconds: 480000,
  workflowStepMinutes: 20, enclosingJobMinutes: 45, cargoWorkInventoryMaximum: 64,
  maximumAnchorAgeMilliseconds: 1200000, estimatedCleanupMarginMilliseconds: 180000,
});
const KQL_POLICY_ENTRY = Object.freeze({
  candidate: 'dev/marcozo/integrate-kql-shortcut-links-pr55-end-to-tomz4t',
  base: '61c8221914152e231e5e64f7bd38418f66264999',
  source: 'e1baaee16dc6eb4917c5310224c27fcd258564ef',
  driver: 'rust/scripts/integration-kql-link-smoke.cjs',
});
const LEAVES = ['dataagent_list', 'v2_dataagent_list_binary'];
const INPUTS = [
  'rust/clients/cli/app/src/v2_dataagent_list_host.rs',
  'rust/clients/cli/app/src/v2_dataagent_list_main.rs',
  'rust/clients/cli/app/src/v2_main.rs',
  ...LEAVES.map(name => `rust/clients/cli/app/tests/${name}.rs`),
  ...['get_backend', 'explicit_get', 'output_contract_audit', 'workspace_v2',
    'ml_endpoint', 'v2_schema_native', 'livy_list_entry', 'create_host',
    'v2_create_report', 'kql_database_create_binary']
    .map(name => `rust/clients/cli/app/tests/${name}.rs`),
];
const digest = value => crypto.createHash('sha256').update(value).digest('hex');
const REPORT_TESTS = [
  ...update.registeredTests('rust/clients/cli/app/tests/v2_create_report.rs'),
  ...[
    ['host.rs', 'host::'],
    ['http.rs', 'host::http::'],
    ['status.rs', 'host::status::'],
    ['process.rs', 'process::'],
  ].flatMap(([file, prefix]) => update.registeredTests(`rust/clients/cli/app/tests/v2_create_report/${file}`)
    .map(name => `${prefix}${name}`)),
];

function selectedCommands(selection, read = file => fs.readFileSync(path.join(ROOT, file), 'utf8'),
  graph = JSON.parse(fs.readFileSync(path.join(ROOT, 'rust/validation/impact-graph.json'))),
  leaves = LEAVES) {
  const groups = new Map(), deferred = [], cliFeatures = new Set(), cliTargets = new Set();
  for (const test of selection.tests) {
    if (!test.target.startsWith('test:')) {
      deferred.push({...test, disposition: 'broader qualification; not claimed by provisional named-target proof'});
      continue;
    }
    const target = selection.target_catalog.find(item =>
      item.package === test.package && item.target === test.target);
    assert.ok(target, `Missing metadata target ${test.package}/${test.target}`);
    const guarded = [...read(target.source).matchAll(/^#!\[cfg\(([^\n]+)\)\]/gm)]
      .flatMap(match => [...match[1].matchAll(/feature\s*=\s*"([^"]+)"/g)].map(value => value[1]));
    const features = new Set([...target.required_features, ...guarded]);
    for (const reason of test.reasons || []) {
      const component = graph.components.find(item => reason === `Component closure: ${item.id}`);
      for (const id of component?.profiles || []) {
        const profile = selection.profiles.find(item => item.id === id);
        assert.ok(profile, `Missing selected component profile ${id}`);
        for (const feature of profile.features) {
          if (feature.startsWith(`${test.package}/`)) features.add(feature.slice(test.package.length + 1));
        }
      }
    }
    if (test.package === 'fabric-cli') {
      features.add('headless');
      features.add('runtime-v2');
      cliTargets.add(test.target.slice(5));
      for (const feature of features) cliFeatures.add(feature);
    }
    // Keep isolated owner profiles distinct: enabling another create owner
    // changes wrong-owner admission before input I/O in its process fixtures.
    const orderedFeatures = [...features].sort();
    const key = JSON.stringify([test.package, orderedFeatures]);
    if (!groups.has(key)) groups.set(key, {package: test.package, targets: new Set(), features: orderedFeatures});
    const group = groups.get(key);
    group.targets.add(test.target.slice(5));
  }
  assert.ok(leaves.every(name => cliTargets.has(name)), 'Actual DataAgent leaf selection is required');
  const commands = [...groups].sort(([a], [b]) => a.localeCompare(b)).map(([, group]) => ({
    args: ['test', '--offline', '--locked', '-p', group.package, '--no-default-features',
      ...(group.features.length ? ['--features', group.features.join(',')] : []),
      ...[...group.targets].sort().flatMap(name => ['--test', name])],
  }));
  return {commands, deferred, cli_features: [...cliFeatures].sort()};
}

function leafCommand(name) {
  return {args: ['test', '--offline', '--locked', '-p', 'fabric-cli', '--no-default-features',
    '--features', 'headless', '--test', name],
  required: update.registeredTests(`rust/clients/cli/app/tests/${name}.rs`), exhaustive: true};
}

function testObservations(stdout) {
  return [...stdout.matchAll(
    /test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out/g,
  )].map(match => ({status: match[1], passed: Number(match[2]), failed: Number(match[3]),
    ignored: Number(match[4]), measured: Number(match[5]), filtered: Number(match[6])}));
}

function testResults(stdout, stderr, command) {
  const results = update.testResults(stdout, stderr, command);
  if (command.args.includes('--test') && command.args.includes('v2_create_report')) {
    const report = results.find(item => item.target === 'v2_create_report');
    assert.ok(report, 'The actual Report target must run');
    assert.equal(report.passed, REPORT_TESTS.length, 'Every registered Report host/process case must run');
    for (const name of REPORT_TESTS) {
      assert.ok(stdout.includes(`test ${name} ... ok`), `Missing registered Report test: ${name}`);
    }
  }
  return results;
}

function qualificationPolicy(entry) {
  if (!Object.hasOwn(entry, 'policy')) return DEFAULT_POLICY;
  assert.deepEqual(entry.policy, KQL_POLICY, 'Unrecognized qualification policy, phase or budget');
  for (const [key, value] of Object.entries(KQL_POLICY_ENTRY)) {
    assert.equal(entry[key], value, `Qualification policy is not admitted for this entry: ${key}`);
  }
  return KQL_POLICY;
}

function phaseBudget(entry, now = Date.now, started = now()) {
  const policy = qualificationPolicy(entry);
  const deadline = started + policy.executorMilliseconds;
  let observed = started;
  const remaining = () => {
    const current = now();
    if (policy === KQL_POLICY) {
      assert.ok(current >= observed, 'Candidate proof clock moved backwards');
    }
    observed = current;
    const value = deadline - current;
    assert.ok(value > 0, 'Finite candidate proof budget expired');
    return value;
  };
  return {started, deadline, policy, remaining,
    commandTimeout(requested = 30000) {
      assert.ok(Number.isSafeInteger(requested) && requested > 0 &&
        requested <= policy.commandMaximumMilliseconds, 'Invalid command timeout');
      return Math.min(requested, remaining());
    }};
}

function estimatedAnchorAdmission(phase, anchor, context, now = Date.now()) {
  assert.ok(now >= phase.started, 'Candidate admission clock moved backwards');
  assert.ok(anchor && typeof anchor === 'object' && !Array.isArray(anchor),
    'The bound first-step anchor is required');
  assert.equal(anchor.schema, 1, 'Invalid first-step anchor schema');
  assert.equal(anchor.kind, 'estimated-first-workflow-step', 'Invalid first-step anchor kind');
  assert.equal(context.repository, 'eisber/fabric-cli');
  for (const key of ['run_id', 'run_attempt']) assert.match(context[key] || '', /^[1-9]\d*$/);
  for (const key of ['source', 'base', 'checkout', 'original_source']) {
    assert.match(context[key] || '', /^[a-f0-9]{40}$/);
  }
  assert.deepEqual(Object.keys(anchor).sort(),
    ['schema', 'kind', 'recorded_unix_ms', ...Object.keys(context)].sort(),
    'Invalid first-step anchor fields');
  for (const [key, value] of Object.entries(context)) {
    assert.equal(anchor[key], value, `First-step anchor binding mismatch: ${key}`);
  }
  const recorded = anchor.recorded_unix_ms;
  assert.ok(Number.isSafeInteger(recorded) && recorded > 0 && recorded <= phase.started,
    'Invalid or future first-step timestamp');
  const age = phase.started - recorded;
  assert.ok(age <= phase.policy.maximumAnchorAgeMilliseconds, 'First-step anchor is stale');
  const remaining = recorded + phase.policy.enclosingJobMinutes * 60000 - now;
  assert.ok(remaining >= phase.remaining() + phase.policy.estimatedCleanupMarginMilliseconds,
    'Insufficient estimated time for qualification and cleanup');
  return {accounting_policy: 'estimated-first-step-v1', anchor, estimated_admission: true,
    age_at_phase_entry_milliseconds: age,
    observed_unix_ms: now, age_at_admission_milliseconds: now - recorded,
    estimated_remaining_milliseconds: remaining,
    estimated_cleanup_margin_milliseconds: phase.policy.estimatedCleanupMarginMilliseconds,
    provider_job_start_unix_ms: null, pre_anchor_elapsed_milliseconds: null,
    true_provider_headroom_milliseconds: null, provider_job_deadline_verified: false,
    censor_risk: 'Unknown pre-anchor overhead may exhaust the hard 45-minute provider limit; retention is best effort'};
}

function requireUnchangedAnchor(expectedHash, bytes) {
  assert.equal(digest(bytes), expectedHash, 'First-step anchor changed after phase admission');
}

function proofCommands(entry, selected) {
  const commands = [
    {args: notebook.COMMANDS[0]},
    ...entry.leaves.map(entry.leafCommand || leafCommand),
    ...(entry.commands || []),
    ...selected.commands,
    update.COMMANDS[4],
    ...ocv.COMMANDS.slice(3, 8),
    {args: ['check', '--offline', '--locked', '-p', 'fabric-cli', '--all-targets',
      '--no-default-features', '--features', selected.cli_features.join(',')]},
    {args: ['clippy', '--offline', '--locked', '-p', 'fabric-cli', '--all-targets',
      '--no-default-features', '--features', selected.cli_features.join(','), '--', '-D', 'warnings']},
  ];
  const maximum = qualificationPolicy(entry).cargoWorkInventoryMaximum;
  assert.ok(maximum === null || commands.length <= maximum, 'Cargo work inventory exceeds admitted cap');
  return commands;
}

function main(output, entry = {candidate: CANDIDATE, base: BASE, source: SOURCE,
  inputs: INPUTS, leaves: LEAVES}) {
  const root = fs.realpathSync(ROOT);
  assert.ok(output && path.isAbsolute(output), 'An absolute external artifact directory is required');
  const artifacts = path.resolve(output);
  assert.ok(artifacts !== root && !artifacts.startsWith(`${root}${path.sep}`), 'Artifacts must be external');
  fs.mkdirSync(artifacts, {recursive: false});
  const started = Date.now();
  const report = {schema: 1, passed: false,
    qualification: entry.qualification || 'candidate-only offline Windows DataAgent and metadata-selected consumer proof',
    original_source: entry.source, integration_base: entry.base, commands: [],
    run_id: process.env.GITHUB_RUN_ID, run_attempt: process.env.GITHUB_RUN_ATTEMPT};
  const save = () => fs.writeFileSync(path.join(artifacts, 'receipt.json'), `${JSON.stringify(report, null, 2)}\n`);
  const persist = (_, result) => {
    const prefix = String(report.commands.length).padStart(2, '0');
    for (const stream of ['stdout', 'stderr']) {
      fs.writeFileSync(path.join(artifacts, `${prefix}-${stream}.log`), result[stream] || '');
    }
    save();
  };
  let phase, anchorFile, anchorHash;
  const verifyAnchor = () => {
    if (anchorHash) requireUnchangedAnchor(anchorHash, fs.readFileSync(anchorFile));
  };
  const run = (file, args, options = {}) => {
    verifyAnchor();
    const timeout = phase.commandTimeout(options.timeout);
    const remaining = phase.remaining();
    console.log(`${file} ${JSON.stringify(args)}`);
    return capture(file, args, {cwd: root, ...options,
      timeout}, report, (command, result) => {
      command.timeout_milliseconds = timeout;
      command.phase_remaining_before_milliseconds = remaining;
      command.phase_remaining_after_milliseconds = phase.deadline - Date.now();
      persist(command, result);
    });
  };
  const git = args => {
    const result = run('git', args);
    assert.equal(result.status, 0, `Git failed: ${args.join(' ')}`);
    return result.stdout.trim();
  };
  let before;
  try {
    save();
    phase = phaseBudget(entry, Date.now, started);
    report.phase = {policy: phase.policy, started_unix_ms: phase.started,
      deadline_unix_ms: phase.deadline};
    assert.equal(process.platform, 'win32', 'Hosted Windows only');
    assert.equal(process.env.RUNNER_ENVIRONMENT, 'github-hosted', 'No shared local native execution');
    assert.equal(process.env.RUNNER_OS, 'Windows');
    assert.equal(process.env.GITHUB_EVENT_NAME, 'pull_request');
    const event = JSON.parse(fs.readFileSync(process.env.GITHUB_EVENT_PATH, 'utf8'));
    assert.equal(event.pull_request.head.ref, entry.candidate);
    assert.equal(event.pull_request.base.sha, entry.base, 'Integration base moved');
    if (phase.policy === KQL_POLICY) {
      assert.equal(event.repository?.full_name, 'eisber/fabric-cli');
      assert.equal(event.number, 106, 'Qualification is admitted only for candidate PR106');
      assert.ok(process.env.RUNNER_TEMP && path.isAbsolute(process.env.RUNNER_TEMP),
        'An absolute hosted runner temporary directory is required');
      anchorFile = path.join(process.env.RUNNER_TEMP, 'kql-link-first-step-anchor.json');
      const bytes = fs.readFileSync(anchorFile);
      anchorHash = digest(bytes);
      report.enclosing_job = estimatedAnchorAdmission(phase, JSON.parse(bytes.toString('utf8')), {
        repository: event.repository.full_name, run_id: process.env.GITHUB_RUN_ID,
        run_attempt: process.env.GITHUB_RUN_ATTEMPT, source: event.pull_request.head.sha,
        base: entry.base, candidate: entry.candidate, checkout: process.env.GITHUB_SHA,
        original_source: entry.source,
      });
      report.enclosing_job.anchor_sha256 = anchorHash;
      fs.writeFileSync(path.join(artifacts, 'first-step-anchor.json'), bytes);
      save();
    }
    const actual = git(['rev-parse', 'HEAD']);
    report.source = sourcePin('pull_request', event, process.env.GITHUB_REF,
      process.env.GITHUB_SHA, actual, git(['show', '-s', '--format=%P', 'HEAD']).split(' '), false);
    report.tree = git(['rev-parse', 'HEAD^{tree}']);
    report.candidate_parents = git(['show', '-s', '--format=%P', event.pull_request.head.sha]).split(' ');
    git(['merge-base', '--is-ancestor', entry.source, event.pull_request.head.sha]);
    git(['merge-base', '--is-ancestor', entry.base, event.pull_request.head.sha]);
    assert.equal(git(['status', '--porcelain']), '');
    before = sourceInventory(root);
    fs.writeFileSync(path.join(artifacts, 'source-before.json'), JSON.stringify(before, null, 2));
    report.lockfile_sha256 = digest(fs.readFileSync(path.join(root, 'rust/Cargo.lock')));
    report.driver_sha256 = digest(fs.readFileSync(__filename));
    if (entry.driver) report.entry_driver_sha256 = digest(fs.readFileSync(path.join(root, entry.driver)));
    const rust = path.join(root, 'rust');
    const metadataResult = run('cargo', ['metadata', '--offline', '--locked', '--no-deps',
      '--format-version', '1'], {cwd: rust});
    assert.equal(metadataResult.status, 0, 'Actual locked Cargo metadata is required');
    fs.writeFileSync(path.join(artifacts, 'metadata.json'), metadataResult.stdout);
    const selection = plan({root, metadata: JSON.parse(metadataResult.stdout),
      graph: JSON.parse(fs.readFileSync(path.join(rust, 'validation/impact-graph.json'))), files: entry.inputs});
    fs.writeFileSync(path.join(artifacts, 'selection.json'), JSON.stringify(selection, null, 2));
    assert.equal(selection.mode, 'affected', 'Bounded capability selection must not silently widen');
    const graph = JSON.parse(fs.readFileSync(path.join(rust, 'validation/impact-graph.json')));
    const selected = selectedCommands(selection, undefined, graph, entry.leaves);
    report.selection = {fingerprint: selection.fingerprint, inputs: entry.inputs,
      scope: entry.scope || 'explicit DataAgent entry and coupled consumer inputs, not the complete head diff',
      deferred: selected.deferred, qualification_profiles: selection.profiles.map(item => item.id),
      qualification_checks: selection.checks};
    const commands = proofCommands(entry, selected);
    const commandPlan = `${JSON.stringify(commands, null, 2)}\n`;
    fs.writeFileSync(path.join(artifacts, 'command-plan.json'), commandPlan);
    report.command_plan = {cargo_commands: commands.length, sha256: digest(commandPlan)};
    verifyAnchor();
    phase.remaining();
    save();
    for (const command of commands) {
      const result = run('cargo', command.args, {cwd: rust, timeout: 8 * 60 * 1000});
      if (command.args[0] === 'test') {
        report.commands.at(-1).test_observations = testObservations(result.stdout);
        report.commands.at(-1).case_names = [...result.stdout.matchAll(/^test ([^\r\n]+) \.\.\. ok\r?$/gm)]
          .map(match => match[1]);
        save();
      }
      assert.equal(result.status, 0, `Candidate command failed: cargo ${command.args.join(' ')}`);
      if (command.args[0] === 'test') {
        report.commands.at(-1).tests = testResults(result.stdout, result.stderr, command);
      }
      save();
    }
    phase.remaining();
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
      try {
        verifyAnchor();
        if (phase) phase.remaining();
      } catch (error) {
        report.passed = false;
        report.finalization_error = error.message;
        throw error;
      } finally {
        report.elapsed_seconds = (Date.now() - started) / 1000;
        if (phase) report.phase.remaining_after_finalization_milliseconds = phase.deadline - Date.now();
        save();
      }
    }
  }
}

module.exports = {CANDIDATE, BASE, SOURCE, INPUTS, LEAVES, REPORT_TESTS,
  DEFAULT_POLICY, KQL_POLICY, qualificationPolicy, phaseBudget, estimatedAnchorAdmission,
  requireUnchangedAnchor, proofCommands,
  selectedCommands, leafCommand, testObservations, testResults, main};
if (require.main === module) {
  try { assert.equal(process.argv.length, 3); main(process.argv[2]); }
  catch (error) { console.error(error.stack); process.exitCode = 1; }
}
