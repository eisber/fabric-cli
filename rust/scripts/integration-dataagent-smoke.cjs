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
const LEAVES = ['dataagent_list', 'v2_dataagent_list_binary'];
const INPUTS = [
  'rust/clients/cli/app/src/v2_dataagent_list_host.rs',
  'rust/clients/cli/app/src/v2_dataagent_list_main.rs',
  'rust/clients/cli/app/src/v2_main.rs',
  ...LEAVES.map(name => `rust/clients/cli/app/tests/${name}.rs`),
  ...['get_backend', 'explicit_get', 'output_contract_audit', 'workspace_v2',
    'ml_endpoint', 'v2_schema_native', 'livy_list_entry']
    .map(name => `rust/clients/cli/app/tests/${name}.rs`),
];
const digest = value => crypto.createHash('sha256').update(value).digest('hex');

function selectedCommands(selection, read = file => fs.readFileSync(path.join(ROOT, file), 'utf8')) {
  const groups = new Map(), deferred = [];
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
    const key = test.package;
    if (!groups.has(key)) groups.set(key, {targets: new Set(), features: new Set()});
    const group = groups.get(key);
    group.targets.add(test.target.slice(5));
    for (const feature of [...target.required_features, ...guarded]) group.features.add(feature);
  }
  const cli = groups.get('fabric-cli');
  assert.ok(cli && LEAVES.every(name => cli.targets.has(name)), 'Actual DataAgent leaf selection is required');
  // Exercise the existing ambiguous root help and typed Livy routing together.
  cli.features.add('headless');
  cli.features.add('runtime-v2');
  const commands = [...groups].sort(([a], [b]) => a.localeCompare(b)).map(([pkg, group]) => ({
    args: ['test', '--offline', '--locked', '-p', pkg, '--no-default-features',
      ...(group.features.size ? ['--features', [...group.features].sort().join(',')] : []),
      ...[...group.targets].sort().flatMap(name => ['--test', name])],
  }));
  return {commands, deferred, cli_features: [...cli.features].sort()};
}

function leafCommand(name) {
  return {args: ['test', '--offline', '--locked', '-p', 'fabric-cli', '--no-default-features',
    '--features', 'headless', '--test', name],
  required: update.registeredTests(`rust/clients/cli/app/tests/${name}.rs`), exhaustive: true};
}

function main(output) {
  const root = fs.realpathSync(ROOT);
  assert.ok(output && path.isAbsolute(output), 'An absolute external artifact directory is required');
  const artifacts = path.resolve(output);
  assert.ok(artifacts !== root && !artifacts.startsWith(`${root}${path.sep}`), 'Artifacts must be external');
  fs.mkdirSync(artifacts, {recursive: false});
  const started = Date.now(), deadline = started + 570000;
  const report = {schema: 1, passed: false,
    qualification: 'candidate-only offline Windows DataAgent and metadata-selected consumer proof',
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
  const run = (file, args, options = {}) => {
    const remaining = deadline - Date.now();
    assert.ok(remaining > 0, 'Finite candidate proof budget expired');
    console.log(`${file} ${JSON.stringify(args)}`);
    return capture(file, args, {cwd: root, ...options,
      timeout: Math.min(options.timeout || 30000, remaining)}, report, persist);
  };
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
    report.tree = git(['rev-parse', 'HEAD^{tree}']);
    report.candidate_parents = git(['show', '-s', '--format=%P', event.pull_request.head.sha]).split(' ');
    git(['merge-base', '--is-ancestor', SOURCE, event.pull_request.head.sha]);
    git(['merge-base', '--is-ancestor', BASE, event.pull_request.head.sha]);
    assert.equal(git(['status', '--porcelain']), '');
    before = sourceInventory(root);
    fs.writeFileSync(path.join(artifacts, 'source-before.json'), JSON.stringify(before, null, 2));
    report.lockfile_sha256 = digest(fs.readFileSync(path.join(root, 'rust/Cargo.lock')));
    report.driver_sha256 = digest(fs.readFileSync(__filename));
    const rust = path.join(root, 'rust');
    const metadataResult = run('cargo', ['metadata', '--offline', '--locked', '--no-deps',
      '--format-version', '1'], {cwd: rust});
    assert.equal(metadataResult.status, 0, 'Actual locked Cargo metadata is required');
    fs.writeFileSync(path.join(artifacts, 'metadata.json'), metadataResult.stdout);
    const selection = plan({root, metadata: JSON.parse(metadataResult.stdout),
      graph: JSON.parse(fs.readFileSync(path.join(rust, 'validation/impact-graph.json'))), files: INPUTS});
    fs.writeFileSync(path.join(artifacts, 'selection.json'), JSON.stringify(selection, null, 2));
    assert.equal(selection.mode, 'affected', 'Bounded capability selection must not silently widen');
    const selected = selectedCommands(selection);
    report.selection = {fingerprint: selection.fingerprint, inputs: INPUTS,
      scope: 'explicit DataAgent entry and coupled consumer inputs, not the complete head diff',
      deferred: selected.deferred, qualification_profiles: selection.profiles.map(item => item.id),
      qualification_checks: selection.checks};
    const commands = [
      {args: notebook.COMMANDS[0]},
      ...LEAVES.map(leafCommand),
      ...selected.commands,
      update.COMMANDS[4],
      ...ocv.COMMANDS.slice(3, 8),
      {args: ['check', '--offline', '--locked', '-p', 'fabric-cli', '--all-targets',
        '--no-default-features', '--features', selected.cli_features.join(',')]},
      {args: ['clippy', '--offline', '--locked', '-p', 'fabric-cli', '--all-targets',
        '--no-default-features', '--features', selected.cli_features.join(','), '--', '-D', 'warnings']},
    ];
    for (const command of commands) {
      const result = run('cargo', command.args, {cwd: rust, timeout: 8 * 60 * 1000});
      assert.equal(result.status, 0, `DataAgent command failed: cargo ${command.args.join(' ')}`);
      if (command.args[0] === 'test') {
        report.commands.at(-1).tests = update.testResults(result.stdout, result.stderr, command);
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

module.exports = {CANDIDATE, BASE, SOURCE, INPUTS, LEAVES, selectedCommands, leafCommand};
if (require.main === module) {
  try { assert.equal(process.argv.length, 3); main(process.argv[2]); }
  catch (error) { console.error(error.stack); process.exitCode = 1; }
}
