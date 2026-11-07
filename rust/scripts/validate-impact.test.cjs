// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const {execFileSync} = require('node:child_process');
const {test} = require('node:test');
const {packageArgs, ownFeatures, testCommands, coreCommands, commandResult,
  oracleVersions, restoreOracleDependencies, runContracts, options, buildEnvironment, sourceDigest, sourceInventory} =
  require('./validate-impact.cjs');
const {selection, equalSelection} = require('./impact-ci.cjs');
const {plan: impactPlan} = require('./impact-plan.cjs');
const validationRegistry = require('./validation-checks.cjs');

function temporary(t) {
  const directory = fs.mkdtempSync(path.join(process.env.IMPACT_TEST_ROOT ||
    process.env.RUNNER_TEMP || os.tmpdir(), 'fabric-impact-runner-'));
  t.after(() => fs.rmSync(directory, {recursive: true, force: true}));
  return directory;
}

function plan() {
  return {
    schema: 1, mode: 'affected', files: ['rust/notebook.rs'],
    packages: ['deploy', 'cli'], checks: [],
    tests: [{package: 'deploy', target: 'test:notebook'}, {package: 'cli', target: 'test:deploy_mount'}],
    profiles: [{
      id: 'native', packages: ['cli', 'identity'],
      features: ['cli/native-read', 'identity/windows-broker'],
      platforms: ['windows', 'linux', 'macos'], checks: ['native-read'],
    }],
  };
}

test('source inventory retains the exact digest and identifies tracked, untracked and deleted input changes', t => {
  const root = temporary(t);
  const git = args => execFileSync('git', args, {cwd: root, encoding: 'utf8'});
  git(['init', '--quiet']);
  fs.writeFileSync(path.join(root, '.gitignore'), 'ignored/\n');
  fs.writeFileSync(path.join(root, 'managed.lock.json'), '{"locked":true}\n');
  git(['add', '.gitignore', 'managed.lock.json']);
  fs.writeFileSync(path.join(root, 'untracked.cs'), 'class Test {}\n');
  fs.mkdirSync(path.join(root, 'ignored'));
  fs.writeFileSync(path.join(root, 'ignored', 'generated.cs'), 'not source\n');
  const before = sourceInventory(root);
  const expected = crypto.createHash('sha256');
  for (const file of before.files) {
    expected.update(file.path);
    expected.update('\0');
    expected.update(fs.readFileSync(path.join(root, file.path)));
    expected.update('\0');
  }
  assert.equal(before.digest, expected.digest('hex'));
  assert.equal(sourceDigest(root), before.digest);
  assert.deepEqual(before.files.map(file => file.path), ['.gitignore', 'managed.lock.json', 'untracked.cs']);
  assert.ok(before.files.every(file => file.sha256.length === 64 && file.state === 'file'));
  assert.ok(!JSON.stringify(before).includes('class Test'));
  fs.writeFileSync(path.join(root, 'managed.lock.json'), '{"locked":false}\n');
  fs.unlinkSync(path.join(root, 'untracked.cs'));
  fs.writeFileSync(path.join(root, 'generated.cs'), 'class Generated {}\n');
  const after = sourceInventory(root);
  assert.notEqual(after.digest, before.digest);
  assert.notEqual(after.files.find(file => file.path === 'managed.lock.json').sha256,
    before.files.find(file => file.path === 'managed.lock.json').sha256);
  assert.ok(after.files.some(file => file.path === 'generated.cs'));
  fs.unlinkSync(path.join(root, 'managed.lock.json'));
  const deleted = sourceInventory(root);
  assert.deepEqual(deleted.files.find(file => file.path === 'managed.lock.json'),
    {path: 'managed.lock.json', state: 'deleted'});
  assert.notEqual(deleted.digest, after.digest);
});

test('Cargo package arguments are sorted, deduplicated and not shell text', () => {
  assert.deepEqual(packageArgs(['z', 'a', 'z']), ['--package', 'a', '--package', 'z']);
});

test('build receipts retain only explicit Cargo resource settings, not unrelated environment values', () => {
  assert.deepEqual(buildEnvironment({
    CARGO_BUILD_JOBS: '2', RUST_TEST_THREADS: '1',
    CARGO_PROFILE_DEV_DEBUG: '0', CARGO_PROFILE_TEST_DEBUG: '0', CARGO_INCREMENTAL: '0',
    GITHUB_TOKEN: 'must-not-leak', PATH: 'private-path',
  }), {
    CARGO_BUILD_JOBS: '2', RUST_TEST_THREADS: '1',
    CARGO_PROFILE_DEV_DEBUG: '0', CARGO_PROFILE_TEST_DEBUG: '0', CARGO_INCREMENTAL: '0',
  });
  assert.deepEqual(buildEnvironment({}), {});
  const workflow = fs.readFileSync(path.join(__dirname, '..', '..', '.github', 'workflows', 'rust-ci.yml'), 'utf8');
  for (const setting of ['CARGO_PROFILE_DEV_DEBUG', 'CARGO_PROFILE_TEST_DEBUG', 'CARGO_INCREMENTAL']) {
    assert.ok(workflow.includes(`${setting}: "0"`));
  }
  assert.ok(!workflow.includes('CARGO_PROFILE_TEST_DEBUG_ASSERTIONS'));
  assert.ok(!workflow.includes('CARGO_PROFILE_DEV_DEBUG_ASSERTIONS'));
});

test('command receipts record the actual allowlisted build overrides', t => {
  const directory = temporary(t);
  const context = {root: directory, artifacts: directory, results: [],
    deadline: Date.now() + 10000, commandTimeout: 1000};
  commandResult({id: 'resource-fixture', program: 'cargo', args: [],
    env: {CARGO_PROFILE_DEV_DEBUG: '0', CARGO_PROFILE_TEST_DEBUG: '0',
      CARGO_INCREMENTAL: '0', GITHUB_TOKEN: 'must-not-leak'}}, context,
  (_, __, options) => {
    assert.equal(options.env.CARGO_PROFILE_TEST_DEBUG, '0');
    return {status: 0, stdout: '', stderr: ''};
  });
  assert.equal(context.results[0].build_environment.CARGO_PROFILE_DEV_DEBUG, '0');
  assert.equal(context.results[0].build_environment.CARGO_PROFILE_TEST_DEBUG, '0');
  assert.equal(context.results[0].build_environment.CARGO_INCREMENTAL, '0');
  assert.ok(!Object.hasOwn(context.results[0].build_environment, 'GITHUB_TOKEN'));
  assert.ok(!fs.readFileSync(path.join(directory, 'commands.json'), 'utf8').includes('must-not-leak'));
});

test('Windows formatting bounds each expanded rustfmt invocation without omitting packages', () => {
  const p = plan();
  p.packages = Array.from({length: 35}, (_, index) => `component-${String(index).padStart(2, '0')}`);
  p.packages.push(p.packages[0]);
  const commands = coreCommands(p, 'windows').filter(command => command.args[0] === 'fmt');
  assert.equal(commands.length, 35);
  assert.deepEqual(commands.map(command => command.args[2]), [...new Set(p.packages)].sort());
  for (const command of commands) {
    assert.deepEqual(command.args, ['fmt', '--package', command.args[2], '--', '--check']);
    assert.equal(command.id, `default:fmt:${command.args[2]}`);
  }
});

test('POSIX formatting retains the combined selected-package check', () => {
  for (const platform of ['linux', 'macos']) {
    const formatting = coreCommands(plan(), platform).filter(command => command.args[0] === 'fmt');
    assert.equal(formatting.length, 1);
    assert.equal(formatting[0].id, 'default:fmt');
    assert.deepEqual(formatting[0].args, ['fmt', '--package', 'cli', '--package', 'deploy', '--', '--check']);
  }
});

test('complete target selection keeps Notebook and downstream contracts, not unrelated suites', () => {
  const commands = coreCommands(plan(), 'windows');
  const tests = commands.filter(command => command.test && command.id !== 'default:doc');
  assert.equal(tests.length, 3);
  assert.deepEqual(tests.map(command => command.args.filter(arg => ['notebook', 'deploy_mount'].includes(arg))),
    [['deploy_mount'], ['notebook'], ['deploy_mount']]);
  assert.ok(tests.every(command => !command.args.includes('--all-targets')));
  assert.ok(commands.some(command => command.id === 'default:doc'));
  assert.ok(commands.some(command => command.id === 'native:check' &&
    command.args.includes('--all-targets') && command.args.includes('identity')));
  assert.ok(!tests.some(command => command.args.includes('identity')));
});

test('default full packages are grouped but exact target commands remain separate', () => {
  const commands = testCommands([
    {package: 'a', target: 'all'}, {package: 'b', target: 'all'},
    {package: 'a', target: 'test:duplicate'}, {package: 'c', target: 'lib'},
    {package: 'c', target: 'bin:demo'}, {package: 'c', target: 'example:probe'},
  ]);
  assert.equal(commands.length, 2);
  assert.deepEqual(commands[0].args, ['test', '--offline', '--locked',
    '--package', 'a', '--package', 'b', '--all-targets']);
  assert.deepEqual(commands[1].args.slice(-5), ['--bin', 'demo', '--example', 'probe', '--lib']);
});

test('feature tests use the tested package feature forwarding, never unrelated workspace features', () => {
  const p = plan();
  assert.deepEqual(ownFeatures(p.profiles[0], 'cli'), ['cli/native-read']);
  const command = coreCommands(p, 'linux').find(command => command.id === 'native:test:cli');
  assert.ok(command.args.includes('cli/native-read'));
  assert.ok(!command.args.join(',').includes('identity/windows-broker'));
});

test('platform-only feature profiles are not passed off as native execution elsewhere', () => {
  const p = plan();
  p.profiles[0].platforms = ['windows'];
  assert.ok(!coreCommands(p, 'linux').some(command => command.id.startsWith('native:')));
  assert.throws(() => coreCommands(p, 'freebsd'), /Unsupported host/);
});

test('required-feature targets run in their assigned profile, never a default or unrelated variant', () => {
  const p = plan();
  const gated = {package: 'cli', target: 'test:native_case', default_available: false};
  p.tests.push(gated);
  p.profiles[0].tests = [gated];
  const commands = coreCommands(p, 'windows');
  assert.ok(!commands.some(command => command.id.startsWith('default:test:') &&
    command.args.includes('native_case')));
  const native = commands.find(command => command.id === 'native:test:cli');
  assert.ok(native.args.includes('native_case'));
  assert.ok(!native.args.includes('deploy_mount'));
});

test('no target-name test filters or invalid Cargo selectors can silently run zero tests', () => {
  assert.throws(() => testCommands([{package: 'a', target: 'test:abc', filter: 'missing'}]), /complete Cargo/);
  assert.throws(() => testCommands([{package: 'a', target: 'abc'}]), /Unsupported Cargo/);
  assert.throws(() => testCommands([{package: 'a', target: 'test:a --ignored'}]), /Unsupported Cargo/);
});

test('recorded command execution retains failures, output hashes, counts and literal arguments', t => {
  const root = temporary(t);
  const context = {root, artifacts: root, commandTimeout: 1000, deadline: Date.now() + 10000, results: []};
  const command = {id: 'literal:test', program: 'cargo', args: ['test', 'space ; value']};
  const execute = (program, args, opts) => {
    assert.equal(program, 'cargo');
    assert.deepEqual(args, command.args);
    assert.equal(opts.shell, false);
    for (const name of ['FAB_UPDATE_TEST_ROOT', 'FAB_CLI_V2_TEST_ROOT', 'FAB_IDENTITY_TEST_ROOT']) {
      assert.equal(opts.env[name], root);
      assert.ok(path.isAbsolute(opts.env[name]));
    }
    return {status: 0, stdout: 'test result: ok. 4 passed; 0 failed; 1 ignored;\n', stderr: ''};
  };
  commandResult(command, context, execute);
  assert.equal(context.results[0].tests_passed, 4);
  assert.equal(context.results[0].tests_ignored, 1);
  assert.equal(path.basename(context.results[0].stdout), '000-literal_test.stdout.log');
  assert.match(context.results[0].stdout_sha256, /^[a-f0-9]{64}$/);
  assert.throws(() => commandResult(command, context, () => ({
    status: null, error: new Error('deadline exceeded'), stdout: '', stderr: 'partial',
  })), /Validation failed/);
  assert.equal(context.results.length, 2);
  assert.equal(context.results[1].exit, null);
});

test('full-workspace command ids use bounded diagnostic filenames without losing receipt identity', t => {
  const root = temporary(t);
  const context = {root, artifacts: root, commandTimeout: 1000, deadline: Date.now() + 10000, results: []};
  const id = `default:test:${Array.from({length: 35}, (_, index) =>
    `fabric-workspace-component-${index}`).join(',')}`;
  const execute = () => ({status: 0, stdout: 'recorded stdout', stderr: 'recorded stderr'});
  for (const suffix of ['', ',different-final-component']) {
    commandResult({id: id + suffix, program: 'cargo', args: ['test', '--all-targets']},
      context, execute);
  }
  assert.deepEqual(context.results.map(record => record.id), [id, `${id},different-final-component`]);
  for (const record of context.results) {
    assert.ok(Buffer.byteLength(path.basename(record.stdout)) <= 80);
    assert.ok(Buffer.byteLength(path.basename(record.stderr)) <= 80);
    assert.equal(fs.readFileSync(record.stdout, 'utf8'), 'recorded stdout');
    assert.equal(fs.readFileSync(record.stderr, 'utf8'), 'recorded stderr');
    assert.deepEqual(record.args, ['test', '--all-targets']);
  }
  assert.notEqual(path.basename(context.results[0].stdout).slice(4),
    path.basename(context.results[1].stdout).slice(4));
});

test('full-workspace command log names stay bounded without losing command identity', t => {
  const root = temporary(t);
  const context = {root, artifacts: root, commandTimeout: 1000, deadline: Date.now() + 10000, results: []};
  const common = `default:test:${Array.from({length: 40}, (_, i) => `fabric-long-package-${i}`).join(',')}`;
  for (const suffix of ['a', 'b']) {
    const id = `${common}:${suffix}`;
    commandResult({id, program: 'cargo', args: ['test', '--all-targets']}, context,
      () => ({status: 0, stdout: 'test result: ok. 1 passed; 0 failed; 0 ignored;\n', stderr: ''}));
    const record = context.results.at(-1);
    assert.equal(record.id, id);
    assert.ok(path.basename(record.stdout).length < 128);
    assert.ok(path.basename(record.stderr).length < 128);
    assert.equal(fs.readFileSync(record.stdout, 'utf8'),
      'test result: ok. 1 passed; 0 failed; 0 ignored;\n');
    assert.ok(fs.existsSync(record.stderr));
  }
  assert.notEqual(context.results[0].stdout, context.results[1].stdout);
});

test('full-workspace command log names stay bounded without changing scope or receipts', t => {
  const root = temporary(t);
  const packages = Array.from({length: 40}, (_, index) =>
    `fabric-package-with-a-long-name-${String(index).padStart(2, '0')}`);
  const [command] = testCommands(packages.map(name => ({package: name, target: 'all'})));
  const run = id => {
    const context = {root, artifacts: root, commandTimeout: 1000,
      deadline: Date.now() + 10000, results: []};
    commandResult({...command, id}, context, (program, args) => {
      assert.equal(program, command.program);
      assert.deepEqual(args, command.args);
      return {status: 0, stdout: 'test result: ok. 1 passed; 0 failed; 0 ignored;\n', stderr: ''};
    });
    const [record] = context.results;
    assert.equal(record.id, id);
    assert.deepEqual(record.args, command.args);
    for (const log of [record.stdout, record.stderr]) {
      assert.ok(path.basename(log).length <= 111);
      assert.ok(fs.existsSync(log));
    }
    return path.basename(record.stdout);
  };
  assert.ok(command.id.length > 255);
  const first = run(command.id);
  assert.equal(first, run(command.id), 'the same command has a stable log basename');
  assert.notEqual(first, run(`${command.id}-distinct-tail`));
});

test('full-workspace command log names stay bounded while receipts retain exact IDs', t => {
  const root = temporary(t);
  const context = {root, artifacts: root, commandTimeout: 1000, deadline: Date.now() + 10000, results: []};
  const id = `default:test:${Array.from({length: 40}, (_, i) => `fabric-package-${i}`).join(',')}`;
  const command = {id, program: 'cargo', args: ['test', '--workspace']};
  commandResult(command, context, () => ({status: 0, stdout: 'complete\n', stderr: ''}));
  const record = context.results[0];
  assert.equal(record.id, id);
  assert.deepEqual(record.args, command.args);
  for (const [file, contents] of [[record.stdout, 'complete\n'], [record.stderr, '']]) {
    assert.ok(path.basename(file).length <= 131);
    assert.match(path.basename(file), /-[a-f0-9]{16}\.(stdout|stderr)\.log$/);
    assert.equal(fs.readFileSync(file, 'utf8'), contents);
  }
  assert.equal(JSON.parse(fs.readFileSync(path.join(root, 'commands.json'), 'utf8'))[0].id, id);
});

test('missing full-gate marker profiles fail, rather than passing an empty contract', t => {
  const root = temporary(t);
  fs.mkdirSync(path.join(root, 'rust', 'validation'), {recursive: true});
  fs.writeFileSync(path.join(root, 'rust', 'validation', 'impact-graph.json'),
    JSON.stringify({checks: [{id: 'headless'}]}));
  const context = {root, artifacts: root, platform: 'linux',
    plan: {checks: ['headless'], profiles: []}};
  const registry = {validateRegistry: () => {}, commandsFor: () => []};
  assert.throws(() => runContracts(context, registry, () => {}), /No execution profile/);
  context.plan.tests = [];
  context.plan.profiles = [{checks: ['headless'], platforms: ['linux'], packages: ['identity']}];
  assert.throws(() => runContracts(context, registry, () => {}), /with test targets/);
});

test('oracle dependencies are restored only for missing modules, before source preparation', t => {
  const root = path.resolve(__dirname, '..', '..');
  const context = {root, python: 'python', restoreOracleDeps: true,
    plan: {checks: ['parameters-oracle']}};
  const restored = [];
  restoreOracleDependencies(context, command => restored.push(command), () => ({
    status: 1, stderr: 'ModuleNotFoundError: missing test-only dependency',
  }));
  assert.equal(restored.length, 2);
  assert.ok(restored[1].args.includes(path.join('scripts', 'rust-fast-requirements.txt')));
  assert.ok(restored.every(command => command.args.includes('install')));
  assert.throws(() => restoreOracleDependencies(context, () => assert.fail('must not install'), () => ({
    status: 1, stderr: 'ImportError: broken installed module',
  })), /probe failed/);
  assert.throws(() => restoreOracleDependencies({...context, restoreOracleDeps: false},
    () => assert.fail('must not install'), () => ({status: 1, stderr: 'ModuleNotFoundError'})),
  /Missing oracle dependencies/);
  const mismatched = [];
  restoreOracleDependencies(context, command => mismatched.push(command),
    () => ({status: 42, stdout: '{"jmespath":"0.0.0"}'}));
  assert.equal(mismatched.length, 2);
});

test('oracle versions come from actual pinned manifests, with unsafe or ambiguous includes rejected', t => {
  const root = temporary(t);
  const manifest = path.join(root, 'requirements.txt');
  fs.writeFileSync(manifest, '# example\njmespath==1.0.1\n-r child.txt\n');
  fs.writeFileSync(path.join(root, 'child.txt'), 'cachetools==5.5.2\n');
  assert.deepEqual(oracleVersions(root, 'requirements.txt'), {jmespath: '1.0.1', cachetools: '5.5.2'});
  fs.writeFileSync(path.join(root, 'child.txt'), 'jmespath==0.0.1\n');
  assert.throws(() => oracleVersions(root, 'requirements.txt'), /Conflicting/);
  fs.writeFileSync(path.join(root, 'child.txt'), '-r requirements.txt\n');
  assert.throws(() => oracleVersions(root, 'requirements.txt'), /inclusion cycle/);
  fs.writeFileSync(manifest, `-r ${path.join('..', 'outside.txt')}\n`);
  assert.throws(() => oracleVersions(root, 'requirements.txt'), /escaped/);
  fs.writeFileSync(manifest, 'jmespath>=1\n');
  assert.throws(() => oracleVersions(root, 'requirements.txt'), /exactly pinned/);
});

test('a failed source comparison still verifies frozen source and overlays exactly once', t => {
  const root = temporary(t);
  const frozen = path.join(root, 'frozen');
  fs.mkdirSync(frozen);
  fs.mkdirSync(path.join(root, 'rust', 'validation'), {recursive: true});
  fs.mkdirSync(path.join(root, 'rust', 'tests', 'compatibility'), {recursive: true});
  fs.writeFileSync(path.join(root, 'rust', 'tests', 'compatibility', 'requirements.txt'), 'jmespath==1.0.1\n');
  const checks = [{id: 'oracle-tools'}, {id: 'comparison', depends_on: ['oracle-tools']},
    {id: 'frozen-verify'}];
  fs.writeFileSync(path.join(root, 'rust', 'validation', 'impact-graph.json'), JSON.stringify({checks}));
  const context = {root, artifacts: root, platform: 'linux', python: 'python',
    plan: {checks: ['oracle-tools', 'comparison', 'frozen-verify'], profiles: []}};
  const registry = {
    validateRegistry: () => {},
    commandsFor: id => [{kind: id === 'oracle-tools' ? 'prepare-oracle' : 'command',
      program: 'python', args: [id]}],
  };
  const calls = [];
  const run = command => {
    calls.push(command.args[0]);
    if (command.args[0] === 'oracle-tools') return JSON.stringify({rust_root: frozen});
    if (command.args[0] === 'comparison') throw new Error('comparison rejected mismatch');
    return '';
  };
  assert.throws(() => runContracts(context, registry, run,
    () => ({status: 0})), /comparison rejected mismatch/);
  assert.deepEqual(calls, ['oracle-tools', 'comparison', 'frozen-verify']);
});

test('upstream-helper-only changes execute tooling and its frozen integrity gate', t => {
  const root = path.resolve(__dirname, '..', '..');
  const artifacts = temporary(t);
  const frozen = path.join(artifacts, 'frozen');
  fs.mkdirSync(frozen);
  const actual = JSON.parse(fs.readFileSync(
    path.join(root, 'rust', 'validation', 'impact-graph.json'), 'utf8'));
  const graph = {schema: 1, components: [], profiles: [],
    checks: actual.checks.filter(check =>
      ['selector-tests', 'oracle-tools', 'frozen-verify'].includes(check.id))};
  const packageId = 'path+fabric-core#0.1.0';
  const metadata = {version: 1, packages: [{
    id: packageId, name: 'fabric-core',
    manifest_path: path.join(root, 'rust', 'infrastructure', 'core', 'fabric-core', 'Cargo.toml'),
    dependencies: [], features: {default: []}, targets: [],
  }], workspace_members: [packageId], workspace_default_members: [packageId]};
  for (const file of ['rust/scripts/v2_upstream.py', 'rust/scripts/test_v2_upstream.py']) {
    const selected = impactPlan({root, graph, metadata, files: [file]});
    assert.equal(selected.mode, 'affected', file);
    assert.deepEqual(selected.packages, [], file);
    assert.deepEqual([...selected.checks].sort(),
      ['frozen-verify', 'oracle-tools', 'selector-tests'], file);
    const context = {root, artifacts, platform: 'linux', python: 'python',
      plan: selected};
    const calls = [];
    runContracts(context, validationRegistry, command => {
      calls.push(command);
      return command.id === 'oracle:prepare' ? JSON.stringify({rust_root: frozen}) : '';
    }, () => ({status: 0}));
    assert.equal(calls.filter(command => command.args.includes('test_v2_upstream.py')).length, 1);
    assert.equal(calls.filter(command => command.id === 'oracle:prepare').length, 1);
    assert.equal(calls.filter(command => command.id === 'frozen-verify').length, 1);
    assert.equal(calls.at(-1).id, 'frozen-verify');
  }
});

test('CI comparison binds scopes despite host-specific metadata, and rejects missing consumers', () => {
  const expected = plan();
  const actual = structuredClone(expected);
  actual.packages.reverse();
  actual.provenance = {root: 'another platform checkout'};
  assert.deepEqual(selection(expected), selection(actual));
  equalSelection(expected, actual);
  actual.tests.pop();
  assert.throws(() => equalSelection(expected, actual), /differs/);
});

test('the executor refuses unbounded timeouts, in-checkout artifacts and execution platform impersonation', t => {
  const artifact = temporary(t);
  const root = path.resolve(__dirname, '..', '..');
  const args = ['--plan', path.join(artifact, 'plan.json'), '--artifacts', path.join(artifact, 'logs')];
  assert.throws(() => options([...args, '--budget-seconds', 'Infinity']), /Invalid timeout/);
  assert.throws(() => options(['--plan', path.join(artifact, 'plan.json'),
    '--artifacts', root]), /outside the checkout/);
  assert.throws(() => options([...args, '--budget-seconds', '10', '--budget-seconds', '20']), /Duplicate option/);
  assert.throws(() => options([...args, '--stage', 'unknown']), /Stage must/);
  const other = process.platform === 'win32' ? 'linux' : 'windows';
  assert.throws(() => options([...args, '--platform', other, '--execute']), /actual native host/);
});

test('workflow always tests the planner, covers merge groups and periodic full runs, and has a stable fail-closed gate', () => {
  const workflow = fs.readFileSync(path.resolve(__dirname, '..', '..', '.github', 'workflows', 'rust-ci.yml'), 'utf8');
  assert.match(workflow, /merge_group:/);
  assert.match(workflow, /schedule:/);
  assert.match(workflow, /node --test rust\/scripts\/impact-plan\.test\.cjs/);
  assert.match(workflow, /node rust\/scripts\/impact-ci\.cjs select/);
  assert.match(workflow, /node rust\/scripts\/impact-ci\.cjs native/);
  assert.match(workflow, /node rust\/scripts\/validate-impact\.cjs/);
  assert.match(workflow, /name: Rust validation/);
  assert.match(workflow, /if: always\(\)/);
  assert.match(workflow, /needs: \[changes, native\]/);
  assert.match(workflow, /"\$RUN_NATIVE" == true && "\$NATIVE_RESULT" == success/);
  assert.match(workflow, /"\$RUN_NATIVE" == false && "\$NATIVE_RESULT" == skipped/);
  assert.ok(!workflow.includes('function select(files'));
});

test('the common v2 branch runs the existing combined-candidate gate without privileged merge automation', () => {
  const workflow = fs.readFileSync(path.resolve(__dirname, '..', '..', '.github', 'workflows', 'rust-ci.yml'), 'utf8');
  const branch = 'dev/marcozo/fab-cli-v2-integration-20261104';
  const push = workflow.slice(workflow.indexOf('\n  push:'), workflow.indexOf('\n  merge_group:'));
  assert.ok(push.includes(branch));
  assert.match(push, /\bmain\b/);
  assert.ok(push.includes('feature/rust'));
  const concurrency = workflow.slice(workflow.indexOf('\nconcurrency:'), workflow.indexOf('\njobs:'));
  assert.ok(concurrency.includes(`github.event_name == 'push' && github.ref == 'refs/heads/${branch}' && github.ref`));
  assert.ok(concurrency.includes('github.event.pull_request.number ||'));
  assert.ok(concurrency.includes('|| github.run_id'));
  assert.ok(concurrency.includes("cancel-in-progress: ${{ github.event_name == 'pull_request' }}"));
  assert.match(workflow, /permissions:\n  contents: read/);
  assert.ok(!workflow.includes('pull_request_target:'));
  assert.ok(!workflow.includes('contents: write'));
  assert.ok(workflow.includes('os: [windows-2025, ubuntu-24.04, macos-15]'));
  assert.ok(workflow.includes('github.event.pull_request.base.sha || github.event.merge_group.base_sha || github.event.before'));
  assert.ok(workflow.includes('persist-credentials: false'));
});
