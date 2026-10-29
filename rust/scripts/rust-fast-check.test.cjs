// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {sha, lane, runnerLane, laneBranch, metadataTargets, ciDelta, activeRunsArgs, select, plan, execute, options, requireFastPass, parallelCandidate, commandTimeout, verifyRequest015} = require('./rust-fast-check.cjs');

test('publication projects bounded active-run fields before buffering provider history', () => {
  assert.deepEqual(activeRunsArgs(), [
    'api', '--method', 'GET', 'repos/eisber/fabric-cli/actions/runs', '-f', 'per_page=100',
    '--jq', '{workflow_runs: [.workflow_runs[] | select(.status != "completed") | {path, status, head_branch}]}',
  ]);
});

test('only full source IDs and finite lanes are accepted', () => {
  assert.equal(sha('a'.repeat(40)), true);
  for (const value of ['abc123', '0'.repeat(40), '--upload-pack=x', 'a'.repeat(40) + ';echo']) {
    assert.equal(sha(value), false);
  }
  for (const value of ['baseline', 'cli-jobs', 'endpoint', 'native-values', 'sdk-values', 'cli-auth', 'cli-95', 'windows-auth', 'deploy-mount', 'native-endpoint', 'output-audit', 'orchestration-audit', 'request015-audit', 'argv-audit', 'argv-repair', 'windows-wheelhouse']) assert.equal(lane(value), value);
  for (const value of ['__proto__', 'unknown', 'cli-jobs;echo']) assert.throws(() => lane(value));
  assert.throws(() => options(['--source', 'a', '--source', 'b'], ['--source']));
  assert.throws(() => options(['--command', 'echo'], ['--source']));
});

test('argv repair isolates four feature binaries and preserves all actual before/after captures', () => {
  const commands = plan('audit', {lane: 'argv-repair', packages: ['fabric-cli']},
    '/target', '/receipts').slice(1, -1);
  assert.equal(commands.length, 13);
  for (const index of [0, 2]) {
    for (const target of ['argv_contract', 'cli_contract', 'job_cli_contract',
      'job_arguments', 'query_arguments', 'local_mounted']) assert.ok(commands[index].args.includes(target));
  }
  assert.ok(!commands[0].args.includes('native-read'));
  assert.ok(commands[2].args.includes('native-read'));
  const paths = [0, 2, 5, 7].map(index => commands[index].environment.CARGO_TARGET_DIR);
  assert.equal(new Set(paths).size, 4);
  assert.deepEqual([1, 3, 6, 8].map(index => commands[index].args.at(-1)),
    ['new-default', 'new-native', 'old-default', 'old-native']);
  assert.ok(commands[4].args.includes('3fa71a553551b49ceb83671f2f45a5ca5c02df21'));
  assert.equal(commands.slice(9).length, 4);
  assert.ok(commands.slice(9).every(item => item.args.includes('tests/compatibility/argv_review/capture.py')));
  assert.throws(() => requireFastPass({lane: 'argv-repair'}, {}));
  assert.equal(parallelCandidate('argv-repair', 'validation/rust-fast-msit-exact'), true);
  assert.equal(parallelCandidate('native-endpoint', 'validation/rust-fast-argvfix-exact'), true);
});
test('argv baseline keeps independent default and native results without hiding either failure', () => {
  const commands = plan('audit', {lane: 'argv-audit', packages: ['fabric-cli']});
  assert.deepEqual(commands.map(item => item.args), [
    ['test', '--offline', '--locked', '-p', 'fabric-cli',
      '--test', 'argv_contract', '--', '--test-threads=1'],
    ['test', '--offline', '--locked', '-p', 'fabric-cli', '--features', 'native-read',
      '--test', 'argv_contract', '--', '--test-threads=1'],
  ]);
  assert.deepEqual(execute(commands, '.', () => ({status: 101}), true).map(item => item.exit), [101, 101]);
  laneBranch('argv-audit', 'validation/rust-fast-argv-exact');
  assert.throws(() => laneBranch('baseline', 'validation/rust-fast-argv-exact'));
  assert.throws(() => requireFastPass({lane: 'argv-audit'}, {}));
  assert.equal(parallelCandidate('argv-audit', 'validation/rust-fast-deploy-exact'), true);
  assert.equal(parallelCandidate('argv-audit', 'validation/rust-fast-argv-other'), false);
});

test('baseline audits run both finite output targets without granting full compatibility status', () => {
  const manifest = {lane: 'output-audit', packages: ['fabric-cli', 'fabric-cli-output']};
  const commands = plan('audit', manifest);
  assert.equal(commands.length, 2);
  assert.ok(commands[0].args.includes('output_audit'));
  assert.ok(commands[1].args.includes('output_contract_audit'));
  assert.ok(commands[1].args.includes('native-read'));
  const calls = [];
  const results = execute(commands, '.', (program, args) => {
    calls.push({program, args});
    return {status: 101};
  }, true);
  assert.equal(calls.length, 2);
  assert.deepEqual(results.map(item => item.exit), [101, 101]);
  assert.throws(() => plan('audit', {...manifest, lane: 'baseline'}));
  assert.throws(() => requireFastPass({lane: 'output-audit'}, {}));
  assert.throws(() => requireFastPass({lane: 'windows-wheelhouse'}, {}));
  for (const [family, prefix] of [['windows-wheelhouse', 'wheelhouse'],
    ['output-audit', 'output'], ['orchestration-audit', 'orchestration']]) {
    laneBranch(family, `validation/rust-fast-${prefix}-exact`);
    assert.throws(() => laneBranch(family, 'validation/rust-fast-other'));
    assert.throws(() => laneBranch('baseline', `validation/rust-fast-${prefix}-exact`));
  }
});

test('orchestration evidence retains source capture, guards, exact probe build and actual execution', () => {
  const commands = plan('audit', {lane: 'orchestration-audit', source: 'a'.repeat(40), packages: ['fabric-cli']},
    '/target', '/receipts').slice(1, -1);
  assert.equal(commands.length, 4);
  assert.ok(commands[0].args.includes('tests/compatibility/orchestration/capture.py'));
  assert.ok(commands[0].stdout.endsWith('orchestration-python.json'));
  assert.ok(commands[1].args.includes('test_capture.py'));
  assert.ok(commands[2].args.includes('read_session_probe'));
  assert.ok(commands[2].args.includes('--locked') && commands[2].args.includes('--offline'));
  assert.ok(commands[3].args.includes('--rust-probe'));
  assert.ok(commands[3].args.includes('a'.repeat(40)));
  assert.ok(commands[3].stdout.endsWith('orchestration-native.json'));
});

test('REQ015 repair checks only released stale-root semantics and rejects missing actual evidence', () => {
  const commands = plan('audit', {lane: 'request015-audit', source: 'a'.repeat(40),
    packages: ['fabric-cli']}, '/target', '/receipts').slice(1, -1);
  assert.equal(commands.length, 6);
  assert.ok(commands[0].args.includes('read_session') && commands[0].args.includes('source_operations'));
  assert.ok(commands.at(-1).args.includes('REQ-015'));
  laneBranch('request015-audit', 'validation/rust-fast-request-exact');
  assert.throws(() => requireFastPass({lane: 'request015-audit'}, {}));
  const render = data => JSON.stringify({command: 'ls', status: 'Success', result: {data}});
  const side = {exit: 0, stderr: '', requests: ['https://api.fabric.microsoft.com/v1/workspaces'],
    scopes: [['https://api.fabric.microsoft.com/.default']]};
  const capture = {rust_execution: 'actual-run', observations: [{
    id: 'REQ-015', fixture: {current: '/Gone.Workspace'},
    python: {...side, current_before: '/Gone.Workspace', current_after: '/Gone.Workspace',
      stdout: render([{name: 'Demo.Workspace'}])},
    rust: {...side, stdout: render(['Demo.Workspace'])},
  }]};
  assert.equal(verifyRequest015(capture).exitRequestsScopesAndNames, 'passed');
  assert.equal(verifyRequest015(capture).byteEquality, false);
  for (const field of ['exit', 'stderr', 'requests', 'scopes', 'stdout']) {
    const broken = structuredClone(capture);
    delete broken.observations[0].rust[field];
    assert.throws(() => verifyRequest015(broken));
  }
  const failed = structuredClone(capture);
  failed.observations[0].rust.exit = 1;
  assert.throws(() => verifyRequest015(failed));
});

test('only separately approved disjoint hosted families may publish concurrently', () => {
  assert.equal(parallelCandidate('windows-wheelhouse', 'validation/rust-fast-output-exact'), true);
  assert.equal(parallelCandidate('deploy-mount', 'validation/rust-fast-orchestration-exact'), true);
  for (const [family, branch] of [
    ['output-audit', 'validation/rust-fast-output-other'],
    ['baseline', 'validation/rust-fast-wheelhouse-exact'],
    ['windows-wheelhouse', 'validation/rust-fast-unreviewed-exact'],
    ['native-endpoint', 'validation/rust-fast-msit-other'],
    ['deploy-mount', 'feature/rust'],
  ]) assert.equal(parallelCandidate(family, branch), false);
});
test('native endpoint union checks both actual hosts with first-failure native selectors', () => {
  const ref = 'refs/heads/validation/rust-fast-msit-exact';
  laneBranch('native-endpoint', 'validation/rust-fast-msit-exact');
  for (const target of ['x86_64-pc-windows-msvc', 'x86_64-unknown-linux-gnu']) {
    runnerLane('native-endpoint', target, ref);
  }
  assert.throws(() => runnerLane('native-endpoint', 'aarch64-apple-darwin', ref));
  assert.throws(() => laneBranch('native-endpoint', 'validation/rust-fast-other'));
  const manifest = {lane: 'native-endpoint', target: 'x86_64-pc-windows-msvc',
    packages: ['fabric-cli', 'fabric-sdk', 'fabric-core-json', 'fabric-cli-output']};
  const commands = plan('test', manifest);
  for (const selector of ['native_endpoint_configuration', 'native_endpoint_reads',
    'read_session', 'source_operations']) assert.ok(commands[0].args.includes(selector));
  assert.ok(commands[1].args.includes('fabric-sdk'));
  assert.ok(commands[2].args.includes('native-read'));
  assert.ok(commands[3].args.includes('--doc'));
  const family = plan('family', manifest, 'D:\\target', 'D:\\receipts', () => true);
  assert.ok(family.some(item => item.args.some(value => value.endsWith('read_session_probe.exe'))));
  assert.ok(family.some(item => item.args.includes('tests/compatibility/folders/reference.py')));
  assert.ok(family.some(item => item.args.includes('native_json')));
});

test('timeout rescue changes only native Windows endpoint test bounds and no commands', () => {
  const windows = {lane: 'native-endpoint', target: 'x86_64-pc-windows-msvc'};
  assert.equal(commandTimeout('test', windows), 600000);
  for (const stage of ['fmt', 'fetch', 'check', 'clippy', 'family', 'audit']) {
    assert.equal(commandTimeout(stage, windows), 300000);
  }
  assert.equal(commandTimeout('test', {...windows, target: 'x86_64-unknown-linux-gnu'}), 300000);
  assert.equal(commandTimeout('test', {...windows, lane: 'windows-auth'}), 300000);
});
test('deploy preflight first runs actual library then native root consumer, never live deployment', () => {
  const manifest = {lane: 'deploy-mount', packages: ['fabric-deploy', 'fabric-cli']};
  const commands = plan('test', manifest, '/target', '/receipts',
    file => !['parameter_transforms.rs', 'deploy_full.rs', 'native_deploy_inputs.rs', 'deploy_inputs_debug.rs', 'export_parity_mount.rs']
      .some(name => file.endsWith(name)));
  assert.deepEqual(commands.map(item => item.args), [
    ['test', '--offline', '--locked', '-p', 'fabric-deploy',
      '--test', 'config', '--test', 'repository', '--test', 'parameters', '--test', 'yaml',
      '--', '--list'],
    ['test', '--offline', '--locked', '-p', 'fabric-cli', '--features', 'native-read',
      '--test', 'deploy_parameters', '--test', 'deploy_mount', '--test', 'family_mounts', '--', '--list'],
    ['test', '--offline', '--locked', '-p', 'fabric-deploy', '--', '--test-threads=1'],
    ['test', '--offline', '--locked', '-p', 'fabric-cli', '--features', 'native-read',
      '--test', 'deploy_parameters', '--test', 'deploy_mount', '--test', 'family_mounts', '--', '--test-threads=1'],
  ]);
  assert.ok(plan('clippy', manifest)[1].args.includes('native-read'));
  const family = plan('family', manifest, '/target', '/receipts',
    file => !file.endsWith('transform_program_reference.py'));
  assert.equal(family.length, 29);
  assert.ok(family.some(item => item.program === 'git' &&
    item.args.slice(-4).join(' ') === 'config --local core.autocrlf true'));
  assert.ok(family.at(-2).args.includes('--doc'));
  assert.ok(family.some(item => item.args.includes('capabilities/deployment/fabric-deploy/tests/orchestration_reference.py')));
  assert.ok(family.some(item => item.args.includes('hosts/cli/fabric-cli/tests/deploy_wrapper_reference.py')));
  assert.ok(family.some(item => item.args.includes('capabilities/deployment/fabric-deploy/tests/definition_import_reference_checks.py')));
  assert.ok(family.some(item => item.args.includes('capabilities/deployment/fabric-deploy/tests/operation_result_reference.py')));
  const operation = family.find(item => item.args.includes('capabilities/deployment/fabric-deploy/tests/operation_result_reference.py'));
  assert.ok(operation.args.includes('--cli-reference-root') && operation.args.includes('--cicd-reference-root'));
  assert.ok(!operation.args.includes('--reference-root'));
  const scalar = family.find(item => item.args.includes('capabilities/deployment/fabric-deploy/tests/yaml_scalar_reference.py'));
  assert.deepEqual(scalar.args, ['-B', 'capabilities/deployment/fabric-deploy/tests/yaml_scalar_reference.py']);
  for (const item of [...commands, ...family]) {
    assert.ok(!item.args.includes('run') && !item.args.includes('--config'));
  }
  assert.throws(() => plan('family', manifest, '/target', '/receipts', () => false),
    /Released offline deployment root consumer is missing/);
});

test('released parameter transform target runs first and retains its source comparator', () => {
  const manifest = {lane: 'deploy-mount', packages: ['fabric-deploy', 'fabric-cli']};
  const tests = plan('test', manifest, '/target', '/receipts', () => true);
  assert.equal(tests[0].args[0], 'test');
  assert.ok(tests[0].args.includes('parameter_transforms'));
  assert.ok(tests[1].args.includes('parameter_transforms') && tests[1].args.includes('--list'));
  const family = plan('family', manifest, '/target', '/receipts', () => true);
  assert.ok(family.some(item => item.args.includes('capabilities/deployment/fabric-deploy/tests/transform_program_reference.py')));
});

test('released full deployment consumer and parameter source captures cannot be omitted', () => {
  const manifest = {lane: 'deploy-mount', packages: ['fabric-deploy', 'fabric-cli']};
  const tests = plan('test', manifest, '/target', '/receipts', () => true);
  const full = tests.filter(item => item.args.includes('deploy_full'));
  assert.equal(full.length, 2);
  assert.ok(full.some(item => item.args.includes('--list')));
  assert.ok(full.some(item => item.args.includes('--test-threads=1') && !item.args.includes('--list')));
  for (const target of ['native_deploy_inputs', 'deploy_inputs_debug', 'family_mounts', 'export_parity_mount']) {
    const selected = tests.filter(item => item.args.includes(target));
    assert.equal(selected.length, 2);
    assert.ok(selected.some(item => item.args.includes('--list')));
    assert.ok(selected.some(item => item.args.includes('--test-threads=1') && !item.args.includes('--list')));
  }
  const family = plan('family', manifest, '/target', '/receipts', () => true);
  for (const name of ['configured_flags', 'flag_protocol', 'config_value_edges', 'parameter_context',
    'parameter_pipeline', 'path_filter', 'selected_constants', 'parameter_resolution',
    'parameter_schema_edges', 'config_extensions_execution']) {
    assert.ok(family.some(item => item.args.includes(`capabilities/deployment/fabric-deploy/tests/${name}_reference.py`)));
  }
  const paths = family.find(item => item.args.includes('capabilities/deployment/fabric-deploy/tests/path_filter_reference.py'));
  assert.ok(paths.args.includes('--observations'));
  assert.ok(paths.args.includes(path.join('/receipts', 'path-filter-observed.json')));
});

test('capability layout preserves every released filesystem-selected deployment and auth check', () => {
  const expected = new Set([
    'capabilities/deployment/fabric-deploy/tests/parameter_transforms.rs',
    'hosts/cli/fabric-cli/tests/deploy_mount.rs',
    'hosts/cli/fabric-cli/tests/export_parity_mount.rs',
    'hosts/cli/fabric-cli/tests/deploy_full.rs',
    'hosts/cli/fabric-cli/tests/native_deploy_inputs.rs',
    'hosts/cli/fabric-cli/tests/deploy_inputs_debug.rs',
    'hosts/cli/fabric-cli/tests/auth_commands.rs',
    'hosts/cli/fabric-cli/tests/table_mounted.rs',
    'infrastructure/identity/fabric-identity-windows/src/native.rs',
    ...['transform_program', 'configured_flags', 'flag_protocol', 'config_value_edges',
      'parameter_context', 'parameter_pipeline', 'path_filter', 'selected_constants',
      'parameter_resolution', 'parameter_schema_edges', 'config_extensions_execution']
      .map(name => `capabilities/deployment/fabric-deploy/tests/${name}_reference.py`),
  ]);
  const seen = new Set();
  const exists = file => {
    if (file.startsWith('tests/')) return true;
    assert.ok(expected.has(file), `Unexpected operational path: ${file}`);
    seen.add(file);
    return expected.has(file);
  };
  const deploy = {lane: 'deploy-mount', packages: ['fabric-deploy', 'fabric-cli']};
  const commands = plan('test', deploy, '/target', '/receipts', exists);
  for (const selector of ['parameter_transforms', 'export_parity_mount', 'deploy_full',
    'native_deploy_inputs', 'deploy_inputs_debug']) {
    assert.equal(commands.filter(item => item.args.includes(selector)).length, 2, selector);
  }
  const family = plan('family', deploy, '/target', '/receipts', exists);
  assert.equal(family.length, 30);
  for (const lane of ['windows-auth', 'cli-auth', 'cli-95']) {
    plan('family', {lane, packages: ['fabric-cli']}, '/target', '/receipts', exists);
  }
  assert.deepEqual(seen, expected);
  for (const file of expected) {
    assert.ok(fs.existsSync(path.join(__dirname, '..', file)), `Released input missing: ${file}`);
  }
});

test('Windows native lane, host and dedicated validation namespace must agree', () => {
  const ref = 'refs/heads/validation/rust-fast-windows-03fab';
  runnerLane('windows-auth', 'x86_64-pc-windows-msvc', ref);
  laneBranch('windows-auth', 'validation/rust-fast-windows-03fab');
  for (const target of ['x86_64-unknown-linux-gnu', 'aarch64-pc-windows-msvc', 'aarch64-apple-darwin']) {
    assert.throws(() => runnerLane('windows-auth', target, ref));
  }
  assert.throws(() => runnerLane('windows-auth', 'x86_64-pc-windows-msvc',
    'refs/heads/validation/rust-fast-linux'));
  assert.throws(() => runnerLane('cli-auth', 'x86_64-unknown-linux-gnu', ref));
  assert.throws(() => laneBranch('windows-auth', 'validation/rust-fast-linux'));
  assert.throws(() => laneBranch('cli-auth', 'validation/rust-fast-windows-03fab'));
  assert.throws(() => laneBranch('windows-auth', 'feature/rust'));
});

test('only Windows auth explicitly selects the native identity crate outside defaults', () => {
  const packages = [
    {id: 'cli', name: 'fabric-cli', dependencies: []},
    {id: 'identity', name: 'fabric-identity', dependencies: []},
    {id: 'native', name: 'fabric-identity-windows', dependencies: []},
  ];
  const metadata = {packages, workspace_default_members: ['cli', 'identity'],
    workspace_members: ['cli', 'identity', 'native']};
  assert.deepEqual(select(metadata, 'windows-auth'),
    ['fabric-cli', 'fabric-identity', 'fabric-identity-windows']);
  assert.deepEqual(select(metadata, 'baseline'), ['fabric-cli', 'fabric-identity']);
});

test('native Windows plan invokes only finite reviewed library and CLI test contracts', () => {
  const manifest = {lane: 'windows-auth', packages: [
    'fabric-cli', 'fabric-identity', 'fabric-identity-windows',
  ], target: 'x86_64-pc-windows-msvc'};
  assert.equal(plan('fetch', manifest).length, 3);
  const checks = plan('check', manifest);
  assert.deepEqual(metadataTargets(manifest.target),
    ['x86_64-unknown-linux-gnu', 'aarch64-apple-darwin']);
  assert.deepEqual(plan('fetch', manifest).slice(1).map(item => item.args.at(-1)),
    metadataTargets(manifest.target));
  assert.ok(checks.some(item => item.args.includes('windows-broker')));
  assert.ok(checks.some(item => item.args.includes('native-read')));
  assert.equal(plan('clippy', manifest).length, 3);
  const tests = plan('test', manifest);
  assert.equal(tests.length, 6);
  assert.deepEqual(tests[0].args, [
    'test', '--offline', '--locked', '-p', 'fabric-identity-windows', '--lib',
    '--', '--test-threads=1',
  ]);
  assert.ok(tests[1].args.includes('--lib'));
  assert.ok(tests[2].args.includes('--bin'));
  for (const item of [...checks, ...plan('clippy', manifest), ...tests,
    ...plan('family', manifest, 'C:\\target', 'C:\\receipts', () => true)]) {
    assert.equal(item.program, 'cargo');
    assert.ok(item.args.includes('--offline') && item.args.includes('--locked'));
    for (const forbidden of ['run', 'login', 'logout', 'status', '--account', '--tenant', '--token']) {
      assert.ok(!item.args.includes(forbidden));
    }
  }
  assert.throws(() => plan('family', manifest, 'C:\\target', 'C:\\receipts', () => false),
    /Reviewed native offline auth consumer is missing/);
});

test('CLI prefetch retains offline architecture checks for all declared metadata targets', () => {
  const target = 'x86_64-unknown-linux-gnu';
  assert.deepEqual(metadataTargets(target),
    ['x86_64-pc-windows-msvc', 'aarch64-apple-darwin']);
  const manifest = {lane: 'cli-auth', packages: ['fabric-cli'], target};
  assert.deepEqual(plan('fetch', manifest).map(item => item.args), [
    ['fetch', '--locked', '--target', target],
    ['fetch', '--locked', '--target', 'x86_64-pc-windows-msvc'],
    ['fetch', '--locked', '--target', 'aarch64-apple-darwin'],
  ]);
  assert.deepEqual(plan('fetch', {...manifest, packages: ['fabric-sdk-http']}).map(item => item.args),
    [['fetch', '--locked', '--target', target]]);
  const workflow = fs.readFileSync(path.resolve(__dirname, '../../.github/workflows/rust-fast-check.yml'), 'utf8');
  assert.ok(!workflow.includes("if: steps.cache.outputs.cache-hit != 'true'"));
});

test('combined CLI lane includes auth/table roots and keeps original default/native regressions', () => {
  const manifest = {lane: 'cli-95', packages: [
    'fabric-cli', 'fabric-cli-tables', 'fabric-sdk-tables', 'fabric-identity',
  ]};
  assert.ok(plan('check', manifest)[1].args.includes('native-read'));
  assert.ok(plan('clippy', manifest)[1].args.includes('native-read'));
  const commands = plan('test', manifest);
  for (const target of ['auth_commands', 'auth_arguments', 'cli_contract', 'workspace_contract',
    'table_mounted', 'family_mounts', 'local_mounted']) {
    assert.ok(commands[0].args.includes(target));
  }
  assert.ok(commands[1].args.includes('fabric-cli-tables'));
  assert.ok(!commands[1].args.includes('--features'));
  assert.ok(commands[2].args.includes('native-read'));
  assert.ok(commands[3].args.includes('--doc'));
  const family = plan('family', manifest, '/target', '/receipts', () => true);
  assert.ok(family.some(item => item.args.includes('tests/compatibility/job_reads/compare.py')));
  assert.ok(family.some(item => item.args.includes('tests/compatibility/read_session/consumer.py')));
  assert.throws(() => plan('family', manifest, '/target', '/receipts', () => false),
    /combined CLI consumer is missing/);
});

test('auth lane executes finite synthetic contracts, never a real login or native identity run', () => {
  const manifest = {lane: 'cli-auth', packages: ['fabric-cli', 'fabric-identity']};
  const checks = plan('check', manifest);
  assert.ok(checks[1].args.includes('windows-broker'));
  assert.ok(plan('clippy', manifest)[1].args.includes('windows-broker'));
  const tests = plan('test', manifest);
  assert.equal(tests.length, 3);
  for (const selector of ['auth_commands', 'auth_arguments', 'cli_contract', 'workspace_contract']) {
    assert.ok(tests[0].args.includes(selector));
  }
  assert.ok(tests[1].args.includes('--bin') && tests[1].args.includes('fab-rs'));
  assert.ok(tests[2].args.includes('--no-default-features'));
  const family = plan('family', manifest, '/target', '/receipts', () => true);
  assert.equal(family.length, 1);
  assert.ok(family[0].args.includes('--doc'));
  for (const item of [...checks, ...plan('clippy', manifest), ...tests, ...family]) {
    assert.equal(item.program, 'cargo');
    assert.ok(item.args.includes('--offline') && item.args.includes('--locked'));
    for (const prohibited of ['run', 'login', 'logout', 'status', 'native-read', 'fabric-identity-windows']) {
      assert.ok(!item.args.includes(prohibited));
    }
  }
  assert.throws(() => plan('family', manifest, '/target', '/receipts', () => false),
    /offline auth command consumer is missing/);
});

test('SDK/value lane retains in-boundary consumers without certifying central CLI', () => {
  const pkg = (name, layer, dependencies = []) => ({
    id: name, name, metadata: {fabric: {layer}}, dependencies: dependencies.map(name => ({name})),
  });
  const packages = [
    pkg('fabric-core', 'core'), pkg('fabric-core-json', 'core'),
    pkg('fabric-sdk-http', 'sdk', ['fabric-core']),
    pkg('fabric-sdk', 'sdk', ['fabric-sdk-http']),
    pkg('fabric-sdk-items', 'sdk', ['fabric-core']),
    pkg('fabric-cli-output', 'cli', ['fabric-core-json']),
    pkg('fabric-cli', 'cli', ['fabric-sdk-http']),
    pkg('fabric-identity', 'identity', ['fabric-core']),
    pkg('fabric-deploy', 'deploy', ['fabric-sdk']),
  ];
  assert.deepEqual(select({packages, workspace_default_members: packages.map(pkg => pkg.id)}, 'sdk-values'),
    ['fabric-cli-output', 'fabric-core', 'fabric-core-json', 'fabric-sdk', 'fabric-sdk-http', 'fabric-sdk-items']);
});

test('CI composition cannot modify product, old workflow, lock or oracle', () => {
  ciDelta(['.github/workflows/rust-fast-check.yml', 'rust/scripts/rust-fast-check.cjs',
    'rust/scripts/pinned_python_source.py', 'rust/scripts/test_pinned_python_source.py',
    'rust/tests/performance/startup.py', 'rust/tests/performance/test_startup.py',
    'rust/tests/performance/measurement-request.json', 'rust/docs/performance-baseline.md']);
  for (const file of ['rust/Cargo.lock', '.github/workflows/rust-ci.yml',
    'rust/hosts/cli/fabric-cli/src/main.rs', 'rust/tests/compatibility/jobs/reference.py']) {
    assert.throws(() => ciDelta([file]));
  }
});

test('native CLI compilation precedes lint, unit tests and family consumers', () => {
  const manifest = {lane: 'cli-jobs', packages: ['fabric-cli', 'fabric-cli-jobs']};
  const commands = plan('check', manifest);
  assert.equal(commands.length, 2);
  assert.ok(!commands[0].args.includes('--features'));
  assert.deepEqual(commands[1], {program: 'cargo', args: [
    'check', '--offline', '--locked', '-p', 'fabric-cli',
    '--features', 'native-read', '--all-targets',
  ]});
  assert.equal(plan('check', {packages: ['fabric-sdk-http']}).length, 1);
});

test('SDK/value checks cover endpoint consumers and explicit native transport features', () => {
  const manifest = {lane: 'sdk-values', packages: [
    'fabric-core', 'fabric-sdk-http', 'fabric-sdk-operations', 'fabric-sdk-transport',
  ]};
  const checks = plan('check', manifest);
  assert.deepEqual(checks[1].args, [
    'check', '--offline', '--locked', '-p', 'fabric-sdk-transport', '--no-default-features', '--lib',
  ]);
  const lints = plan('clippy', manifest);
  assert.ok(lints[1].args.includes('hyper-rustls'));
  assert.ok(lints[1].args.includes('--all-targets'));
  const tests = plan('test', manifest);
  assert.deepEqual(tests[0].args, [
    'test', '--offline', '--locked', '-p', 'fabric-sdk-http', '-p', 'fabric-sdk-operations',
    '--test', 'endpoint_context', '--', '--test-threads=1',
  ]);
  assert.ok(tests.some(item => item.args.includes('--doc')));
  assert.ok(tests.some(item => item.args.includes('hyper-rustls')));
  for (const item of [...checks, ...lints, ...tests]) {
    assert.ok(item.args.includes('--offline'));
    assert.ok(item.args.includes('--locked'));
  }
  assert.throws(() => plan('test', {...manifest, packages: ['fabric-sdk-http']}),
    /requires HTTP and operation endpoint consumers/);
});

test('selection retains transitive consumers and safe default scope', () => {
  const metadata = {
    workspace_default_members: ['core', 'http', 'items', 'cli'],
    packages: [
      {id: 'core', name: 'fabric-core', dependencies: []},
      {id: 'http', name: 'fabric-sdk-http', dependencies: [{name: 'fabric-core'}]},
      {id: 'items', name: 'fabric-sdk-items', dependencies: [{name: 'fabric-core'}]},
      {id: 'cli', name: 'fabric-cli', dependencies: [{name: 'fabric-sdk-http'}]},
      {id: 'windows', name: 'fabric-identity-windows', dependencies: []},
    ],
  };
  const expected = ['fabric-cli', 'fabric-core', 'fabric-sdk-http', 'fabric-sdk-items'];
  assert.deepEqual(select(metadata, 'baseline'), expected);
  assert.deepEqual(select(metadata, 'endpoint'), expected);
  assert.throws(() => select(metadata, 'cli-jobs'), /Missing lane package/);
});

test('mandatory stages use locked offline all-targets and actual single-thread tests', () => {
  const manifest = {lane: 'endpoint', packages: ['fabric-core'], target: 'x86_64-unknown-linux-gnu'};
  assert.deepEqual(plan('fmt', manifest)[0].args, ['fmt', '--all', '--', '--check']);
  for (const stage of ['check', 'clippy']) {
    const args = plan(stage, manifest)[0].args;
    for (const expected of ['--offline', '--locked', '--all-targets', 'fabric-core']) assert.ok(args.includes(expected));
  }
  for (const item of plan('test', manifest)) assert.ok(item.args.includes('--test-threads=1'));
  assert.throws(() => plan('arbitrary shell', manifest));
});

test('job lane executes actual consumer and requires released comparison hook', () => {
  const manifest = {lane: 'cli-jobs', packages: ['fabric-cli', 'fabric-cli-jobs']};
  const commands = plan('family', manifest, '/private/target', '/private/receipts', () => true);
  assert.ok(commands.some(item => item.args.includes('offline_job')));
  assert.ok(commands.some(item => item.args.includes('tests/compatibility/job_reads/compare.py')));
  assert.ok(commands.some(item => item.args.includes('native-read')));
  assert.throws(() => plan('family', manifest, '/target', '/receipts', () => false), /comparator is missing/);
});

test('job and import source receipts remain required for their command consumers', () => {
  for (const packages of [['fabric-cli'], ['fabric-cli-jobs'], ['fabric-sdk-jobs']]) {
    const commands = plan('family', {lane: 'baseline', packages}, '/target', '/receipts', () => true);
    for (const name of ['command_source', 'poll_source', 'numeric_source', 'endpoint_reference']) {
      assert.ok(commands.some(item => item.program === 'python' &&
        item.args.includes(`tests/compatibility/jobs/${name}.py`)));
    }
  }
  for (const packages of [['fabric-cli'], ['fabric-cli-transfer'], ['fabric-deploy']]) {
    const commands = plan('family', {lane: 'baseline', packages}, '/target', '/receipts', () => true);
    assert.ok(commands.some(item => item.program === 'python' &&
      item.args.includes('capabilities/deployment/fabric-deploy/tests/definition_import_reference_checks.py')));
    assert.ok(commands.some(item => item.program === 'python' &&
      item.args.includes('tests/compatibility/run_export_source_parity.py')));
  }
});

test('source plans isolate frozen Python while retaining candidate native binaries and cicd inputs', () => {
  const repository = path.resolve(__dirname, '../..');
  const output = path.resolve('private-receipts');
  const target = path.resolve('private-target');
  const artifact = path.join(output, 'pinned-python-source');
  const checkout = path.join(artifact, 'source');
  for (const [stage, manifest] of [
    ['family', {lane: 'baseline', packages: ['fabric-cli', 'fabric-cli-mutations']}],
    ['family', {lane: 'deploy-mount', packages: ['fabric-deploy', 'fabric-cli']}],
    ['audit', {lane: 'argv-repair', packages: ['fabric-cli']}],
    ['audit', {lane: 'orchestration-audit', packages: ['fabric-cli'], source: 'a'.repeat(40)}],
  ]) {
    const commands = plan(stage, manifest, target, output, () => true);
    assert.deepEqual(commands[0], {program: 'python', cwd: path.join(repository, 'rust'),
      failFast: true, args: ['-B', 'scripts/pinned_python_source.py', 'prepare',
        '--candidate-root', repository, '--output', artifact]});
    assert.deepEqual(commands.at(-1), {program: 'python', cwd: path.join(repository, 'rust'),
      failFast: true, args: ['-B', 'scripts/pinned_python_source.py', 'verify', '--output', artifact]});
    for (const item of commands.slice(1, -1)) {
      if (item.program !== 'python') {
        assert.equal(item.cwd, undefined, 'Native commands must keep the candidate working directory');
        continue;
      }
      assert.equal(item.cwd, path.join(checkout, 'rust'));
      for (const flag of ['--read-probe', '--query-probe', '--folder-probe', '--rust-probe',
        '--probe', '--probes', '--rust-binary']) {
        if (!item.args.includes(flag)) continue;
        const probe = item.args[item.args.indexOf(flag) + 1];
        assert.ok(path.isAbsolute(probe));
        assert.ok(probe.startsWith(target + path.sep), `Probe left candidate target: ${probe}`);
      }
      if (item.args.includes('--cli-reference-root')) {
        assert.equal(item.args[item.args.indexOf('--cli-reference-root') + 1], checkout);
        assert.equal(item.args[item.args.indexOf('--cicd-reference-root') + 1], path.join(output, 'cicd-source'));
      }
      if (item.args.includes('hosts/cli/fabric-cli/tests/deploy_wrapper_reference.py')) {
        assert.equal(item.args[item.args.indexOf('--reference-root') + 1], checkout);
      }
      if (item.args.includes('tests/compatibility/run_export_source_parity.py')) {
        assert.equal(item.args[item.args.indexOf('--artifacts') + 1],
          path.join(checkout, '.oracle-artifacts', 'export-source-receipts'));
      }
    }
  }
});

test('isolated Python setup failures stop even audit collection and execution records actual cwd', () => {
  const commands = plan('audit', {lane: 'orchestration-audit', source: 'a'.repeat(40),
    packages: ['fabric-cli']}, path.resolve('target'), path.resolve('receipts'));
  const calls = [];
  const results = execute(commands, 'candidate-rust', (program, args, options) => {
    calls.push({program, args, options});
    return {status: 1};
  }, true);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].options.cwd, commands[0].cwd);
  assert.equal(results[0].cwd, commands[0].cwd);
  const source = {...commands[1], stdout: undefined};
  const passed = execute([source], 'candidate-rust', (program, args, options) => {
    assert.equal(options.cwd, source.cwd);
    return {status: 0};
  });
  assert.equal(passed[0].cwd, source.cwd);
});

test('definition examples use unique native filenames and retain both unchanged byte comparators', () => {
  const manifest = {lane: 'native-values', packages: ['fabric-core-json', 'fabric-cli-output']};
  const commands = plan('family', manifest, '/target', '/receipts', () => true);
  const definitions = commands.filter(item =>
    item.args.includes('tests/compatibility/output/definition_reference.py') ||
    item.args.includes('core_definition_json') || item.args.includes('definition_json'));
  assert.equal(definitions.length, 4);
  for (const [index, pkg, example] of [
    [0, 'fabric-core-json', 'core_definition_json'],
    [2, 'fabric-cli-output', 'definition_json'],
  ]) {
    assert.deepEqual(definitions[index].args, [
      'run', '--quiet', '--offline', '--locked', '-p', pkg, '--example', example,
    ]);
    assert.equal(definitions[index].stdout, path.join('/receipts', `${example}-${pkg}.json`));
    assert.deepEqual(definitions[index + 1].args, [
      '-B', 'tests/compatibility/output/definition_reference.py', '--observed',
      definitions[index].stdout,
    ]);
  }
  const workflow = fs.readFileSync(path.resolve(__dirname, '../../.github/workflows/rust-ci.yml'), 'utf8');
  assert.ok(workflow.includes('-p fabric-core-json --example core_definition_json > "$RUNNER_TEMP/core-definition-json.json"'));
  assert.ok(workflow.includes('-p fabric-cli-output --example definition_json > "$RUNNER_TEMP/cli-definition-json.json"'));
  assert.ok(!workflow.includes('-p fabric-core-json --example definition_json'));
  const coreExamples = path.resolve(__dirname, '../infrastructure/core/fabric-core-json/examples');
  assert.ok(fs.existsSync(path.join(coreExamples, 'core_definition_json.rs')));
  assert.ok(!fs.existsSync(path.join(coreExamples, 'definition_json.rs')));
  assert.ok(fs.existsSync(path.resolve(__dirname, '../hosts/cli/fabric-cli-output/examples/definition_json.rs')));
});

test('native-values requires native and text observations without weakening oracle', () => {
  const manifest = {lane: 'native-values', packages: ['fabric-core-json', 'fabric-cli-output']};
  const commands = plan('family', manifest, '/target', '/receipts', () => true);
  assert.ok(commands.some(item => item.args.includes('native_json') && item.stdout));
  assert.ok(commands.some(item => item.args.includes('--encoding-only')));
  assert.ok(commands.some(item => item.args.includes('tests/compatibility/output/text_reference.py')));
  assert.throws(() => plan('family', manifest, '/target', '/receipts', () => false), /comparator is missing/);
  assert.throws(() => plan('family', {...manifest, lane: 'sdk-values'},
    '/target', '/receipts', () => false), /comparator is missing/);
});

test('SDK/value family cannot pull deferred CLI consumers back into compilation', () => {
  const manifest = {lane: 'sdk-values', packages: [
    'fabric-core', 'fabric-core-json', 'fabric-sdk', 'fabric-sdk-http',
    'fabric-sdk-jobs', 'fabric-sdk-operations', 'fabric-sdk-onelake', 'fabric-cli-output',
    'fabric-sdk-transport',
  ]};
  const commands = plan('family', manifest, '/target', '/receipts', () => true);
  for (const item of commands) {
    for (const pkg of ['fabric-cli', 'fabric-cli-jobs', 'fabric-cli-mutations', 'fabric-deploy']) {
      assert.ok(!item.args.includes(pkg));
    }
  }
  assert.ok(commands.some(item => item.args.includes('tests/compatibility/jobs/reference.py')));
  assert.ok(commands.some(item => item.args.includes('native_json')));
  assert.ok(commands.some(item => item.args.includes('injected_credentials')));
  assert.ok(commands.some(item => item.args.includes('sdk_consumer') && item.args.includes('hyper-rustls')));
  assert.ok(commands.some(item => item.args.includes('tests/compatibility/items/reference.py') &&
    item.args.includes('--include-python-only')));
  assert.ok(commands.some(item => item.args.includes('tests/compatibility/http/reference.py')));
  assert.ok(commands.some(item => item.args.includes('test_text_reference.py')));
});

test('HTTP source oracle uses its documented pinned dependencies without altering shared requirements', () => {
  const requirements = fs.readFileSync(path.join(__dirname, 'rust-fast-requirements.txt'), 'utf8');
  assert.ok(requirements.includes('-r ../tests/compatibility/requirements.txt'));
  assert.ok(requirements.includes('requests==2.34.2'));
  assert.ok(requirements.includes('urllib3==2.8.0'));
  ciDelta(['rust/scripts/rust-fast-requirements.txt']);
});

test('command spy proves nonzero exits, spawn errors and signals stop subsequent work', () => {
  const commands = [{program: 'cargo', args: ['test']}, {program: 'cargo', args: ['run']}];
  for (const failure of [{status: 101}, {status: null, error: new Error('not installed')},
    {status: null, signal: 'SIGTERM'}]) {
    const calls = [];
    const results = execute(commands, '.', (program, args, opts) => {
      calls.push({program, args, opts});
      return failure;
    });
    assert.equal(calls.length, 1);
    assert.equal(calls[0].opts.shell, false);
    assert.equal(calls[0].opts.timeout, 300000);
    assert.equal(results.length, 1);
    assert.notEqual(results[0].exit, 0);
  }
  assert.equal(execute(commands, '.', () => ({status: 0})).length, 2);
  let actualTimeout;
  execute([{program: 'cargo', args: ['test'], timeout: 600000}], '.', (program, args, opts) => {
    actualTimeout = opts.timeout;
    return {status: 0};
  });
  assert.equal(actualTimeout, 600000);
});

test('full dispatch requires real fast success at the combined immutable head', () => {
  const receipt = {head: 'a'.repeat(40), source: 'b'.repeat(40)};
  const run = {head_sha: receipt.head, path: '.github/workflows/rust-fast-check.yml',
    status: 'completed', conclusion: 'success'};
  requireFastPass(receipt, run);
  for (const patch of [{head_sha: receipt.source}, {status: 'in_progress'},
    {conclusion: 'failure'}, {conclusion: 'skipped'}, {path: '.github/workflows/rust-ci.yml'}]) {
    assert.throws(() => requireFastPass(receipt, {...run, ...patch}));
  }
});

test('full native matrix restores the pinned deployment oracle before comparisons', () => {
  const workflow = fs.readFileSync(path.resolve(__dirname, '../../.github/workflows/rust-ci.yml'), 'utf8');
  const restore = workflow.indexOf('      - name: Restore pinned deployment oracle dependencies');
  const compare = workflow.indexOf('      - name: Require immutable configuration and scoped parameter source receipts');
  assert.ok(restore >= 0 && compare > restore);
  const step = workflow.slice(restore, workflow.indexOf('      - name:', restore + 1));
  assert.ok(step.includes("contains(fromJSON(needs.changes.outputs.packages), 'fabric-deploy')"));
  assert.ok(step.includes('-r scripts/rust-fast-requirements.txt'));
});

test('full native workflow routes every oracle through isolated source and absolute candidate probes', () => {
  const workflow = fs.readFileSync(path.resolve(__dirname, '../../.github/workflows/rust-ci.yml'), 'utf8');
  assert.ok(workflow.includes('scripts/pinned_python_source.py prepare'));
  assert.ok(workflow.includes('--candidate-root "$GITHUB_WORKSPACE"'));
  assert.ok(workflow.includes('scripts/pinned_python_source.py verify'));
  assert.ok(workflow.includes('unittest discover -s scripts -p test_pinned_python_source.py -v'));
  assert.ok(workflow.includes("if: always() && steps.pinned-python.outcome == 'success'"));
  const candidateChecks = [
    'python -B -m unittest discover -s scripts -p test_release_verify.py -v',
    'python -B -m unittest discover -s tests/acceptance/identity -p test_check_report.py -v',
  ];
  for (const command of candidateChecks) {
    assert.equal(workflow.split(command).length - 1, 1);
    assert.ok(workflow.indexOf(command) < workflow.indexOf('scripts/pinned_python_source.py prepare'));
  }
  const calls = workflow.split('\n').filter(line => line.includes('python -B') &&
    !line.includes('scripts/pinned_python_source.py') && !line.includes('test_pinned_python_source.py') &&
    !candidateChecks.some(command => line.trim() === command));
  assert.equal(calls.length, 33);
  const batch = workflow.slice(workflow.indexOf('      - name: Verify independent pinned batch observations'),
    workflow.indexOf('      - name: Use repository-pinned toolchain'));
  assert.ok(batch.includes("contains(fromJSON(needs.changes.outputs.packages), 'fabric-cli')"));
  assert.ok(batch.includes('--check hosts/cli/fabric-cli/tests/batch_source.json'));
  assert.ok(calls.every(line => line.includes('(cd "$PINNED_RUST_ROOT" && python -B ')));
  assert.ok(!workflow.includes('--probes target/'));
  assert.ok(!/--(?:read-probe|query-probe|folder-probe|rust-probe|probe) "target\//.test(workflow));
  assert.equal((workflow.match(/\$GITHUB_WORKSPACE\/rust\/target\/debug\/examples/g) || []).length, 9);
  const assertions = workflow.slice(workflow.indexOf("            const assert = require("),
    workflow.indexOf('            const root = process.env.GITHUB_WORKSPACE;'));
  const info = [];
  new Function('require', 'core', assertions)(require, {info: value => info.push(value)});
  assert.deepEqual(info, ['Affected-crate selection assertions passed']);
});

test('workflow limits runner context to steps and retains pinned bounded push gate', () => {
  const workflow = fs.readFileSync(path.resolve(__dirname, '../../.github/workflows/rust-fast-check.yml'), 'utf8');
  const beforeSteps = workflow.split('    steps:')[0];
  assert.ok(!beforeSteps.includes('runner.temp'));
  assert.ok(workflow.includes('branches: ["validation/rust-fast-*"]'));
  assert.ok(workflow.includes("'refs/heads/validation/rust-fast-windows-'"));
  assert.ok(workflow.includes('["windows-2025"]'));
  assert.ok(workflow.includes('["ubuntu-24.04","windows-2025"]'));
  assert.ok(workflow.includes("matrix.os == 'windows-2025' && 25 || 15"));
  assert.ok(workflow.includes('ref: ${{ github.sha }}'));
  assert.ok(workflow.includes('persist-credentials: false'));
  assert.ok(workflow.includes('"$GITHUB_ENV"'));
  assert.ok(workflow.indexOf('run check') < workflow.indexOf('run clippy'));
  assert.ok(workflow.indexOf('run check') < workflow.indexOf('run test'));
  assert.ok(workflow.indexOf('tests/performance') < workflow.indexOf('run check'));
  assert.ok(workflow.indexOf('test_pinned_python_source.py') < workflow.indexOf('run check'));
  const audit = workflow.slice(workflow.indexOf('\n  audit:'), workflow.indexOf('\n  wheelhouse:'));
  assert.ok(audit.includes('unittest discover -s scripts -p test_pinned_python_source.py -v'));
  assert.ok(audit.indexOf('test_pinned_python_source.py') < audit.indexOf('run audit'));
  const wheelhouse = workflow.slice(workflow.indexOf('\n  wheelhouse:'));
  assert.ok(wheelhouse.includes('unittest discover -s rust/scripts -p test_pinned_python_source.py -v'));
  assert.ok(wheelhouse.indexOf('test_pinned_python_source.py') < wheelhouse.indexOf('rust-fast-wheelhouse.py --output'));
  assert.ok(wheelhouse.includes('test_rust_fast_wheelhouse.py -v\n          if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }'));
  for (const action of workflow.matchAll(/uses: ([^\s]+)/g)) {
    assert.match(action[1], /@[a-f0-9]{40}$/);
  }
});
