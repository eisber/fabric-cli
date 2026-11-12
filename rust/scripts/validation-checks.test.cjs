// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const fs = require('node:fs');
const childProcess = require('node:child_process');
const {CHECK_IDS, commandsFor, validateRegistry, assertTransportGraphs} =
  require('./validation-checks.cjs');

const EXPECTED_IDS = [
  'selector-tests', 'process-supervision', 'command-inventory', 'oracle-tools', 'batch-oracle', 'transport', 'transport-graphs',
  'read-parity', 'json-parity', 'broker', 'repl', 'interactive', 'headless', 'certificate', 'updater',
  'combined', 'jobs-oracle', 'definitions-oracle', 'import-oracle', 'parameters-oracle',
  'export-oracle', 'native-read', 'context-output', 'broker-read-parity', 'sdk-example',
  'resource-oracle', 'cli-smoke', 'sdk-graph', 'dependency-graph', 'frozen-verify',
];
const root = path.resolve('contract fixtures', 'candidate source & not-a-shell');
const artifacts = path.resolve('contract fixtures', 'owned receipts with whitespace');
const rust = path.join(root, 'rust');
const pinnedOutput = path.join(artifacts, 'pinned-python');
const pinnedRust = path.join(pinnedOutput, 'source', 'rust');
const parameterRoot = path.join(artifacts, 'fabric-cicd-parameter-source');
const targetDir = path.join(artifacts, 'separate cargo target');
const plan = Object.freeze({packages: Object.freeze(['fabric-cli', 'fabric-sdk-transport'])});
const baseContext = Object.freeze({
  root, artifacts, platform: 'linux', pinnedOutput, pinnedRust, parameterRoot,
  targetDir, plan,
});
const commands = (id, overrides = {}) => commandsFor(id, {...baseContext, ...overrides});
const valueAfter = (command, flag) => {
  const index = command.args.indexOf(flag);
  assert.notEqual(index, -1, `Missing ${flag} in ${JSON.stringify(command.args)}`);
  return command.args[index + 1];
};
const script = command => command.args[1];
const graph = ids => ({checks: ids.map(id => ({id}))});
const inside = (parent, child) => {
  assert.ok(path.isAbsolute(child));
  const relative = path.relative(parent, child);
  assert.ok(relative && relative !== '..' && !relative.startsWith(`..${path.sep}`) &&
    !path.isAbsolute(relative), `${child} is not contained in ${parent}`);
};

test('native preflights preserve exact engineering inventory and scheduled full obligations', () => {
  const workflow = ['rust-ci.yml', 'rust-native-shard.yml'].map(name =>
    fs.readFileSync(path.join(__dirname, '../../.github/workflows', name), 'utf8')).join('\n');
  const engineering = fs.readFileSync(path.join(__dirname, 'engineering_preflight.py'), 'utf8');
  const executor = fs.readFileSync(path.join(__dirname, 'validate-impact.cjs'), 'utf8');
  const {matrixForPlan, selectShard} = require('./impact-shards.cjs');
  const {coreCommands} = require('./validate-impact.cjs');
  const full = {mode: 'full', files: [], packages: ['fabric-cli'], tests: [],
    profiles: [], checks: []};
  const matrix = matrixForPlan(full, coreCommands).include;
  assert.ok(executor.includes("'engineering_preflight.py'"));
  assert.match(workflow, /test_engineering_preflight\.py/);
  const inventory = require('./engineering-inventory.json');
  assert.equal(inventory.pattern, 'test_native_acceptance*.py');
  assert.ok(Number.isSafeInteger(inventory.count) && inventory.count > 0);
  assert.match(engineering, /_PATTERN, _COUNT = _INVENTORY\["pattern"\], _INVENTORY\["count"\]/);
  assert.match(engineering, /result\.testsRun == _COUNT[\s\S]*not result\.expectedFailures[\s\S]*skips == expected/);
  assert.ok(inventory.windows_skips.every(([, reason]) => reason === 'requires_native_POSIX_PTY'));
  assert.ok(engineering.includes('_WINDOWS_SKIPS'));
  assert.ok(engineering.includes('(artifacts / "native-acceptance.log").open("w", encoding="utf-8")'));
  assert.ok(!engineering.includes('with open(pattern + ".log"'));
  for (const platform of ['windows', 'linux', 'macos']) {
    const entries = matrix.filter(entry => entry.platform === platform);
    assert.equal(entries.filter(entry => entry.engineering).length, 1);
    const commands = entries.flatMap(entry =>
      selectShard(full, platform, entry.shard, coreCommands).commands);
    const clippy = commands.filter(command => command.id === 'preflight:clippy-all-features');
    const cli = commands.filter(command => command.id === 'preflight:cli-all-features');
    assert.equal(clippy.length, 1);
    assert.equal(cli.length, 1);
    assert.deepEqual(clippy[0].args, ['clippy', '--offline', '--locked', '--workspace',
      '--all-targets', '--all-features', '--', '-D', 'warnings']);
    assert.deepEqual(cli[0].args, ['test', '--offline', '--locked', '-p', 'fabric-cli',
      '--all-targets', '--all-features', '--no-fail-fast']);
    assert.equal(cli[0].test, true);
  }
  assert.ok(executor.indexOf('if (shard?.engineering)') <
    executor.indexOf('const cargoCommands = commands.filter('));
  assert.match(executor, /if \(shard\?\.engineering\)/);
  assert.equal((executor.match(/args: \['-I', '-B', path\.join\(opts\.root, 'rust', 'scripts', 'engineering_preflight\.py'\)/g) || []).length, 1);
  assert.ok(!workflow.includes('python -I -B rust/scripts/engineering_preflight.py'));
  assert.match(workflow, /if: matrix\.dotnet/);
  const retainedArtifacts = workflow.slice(
    workflow.indexOf('      - name: Retain plan and actual command exits, hashes and coverage'));
  assert.ok(retainedArtifacts.includes('rust-engineering-unit/engineering-receipt.json'));
  assert.ok(workflow.includes('--restore-oracle-deps --execute'));
  assert.deepEqual([...new Set(matrix.map(entry => entry.os))].sort(),
    ['macos-15', 'ubuntu-24.04', 'windows-2025']);
  assert.ok(workflow.includes('contents: read'));
  assert.ok(!workflow.includes('continue-on-error'));
});

test('native shards have a 15-minute ceiling and bounded concurrency on every OS', () => {
  const workflow = fs.readFileSync(path.join(__dirname, '../../.github/workflows/rust-ci.yml'), 'utf8');
  const start = workflow.indexOf('\n  native:\n'), end = workflow.indexOf('\n  validation:\n');
  assert.ok(start >= 0 && end > start, 'Native job must precede the unchanged stable gate');
  const caller = workflow.slice(start, end);
  const native = fs.readFileSync(path.join(__dirname, '../../.github/workflows/rust-native-shard.yml'), 'utf8');
  assert.match(caller, /^      max-parallel: 3$/m);
  assert.match(caller, /matrix: \$\{\{ fromJSON\(needs\.changes\.outputs\.native_pools\) \}\}/);
  assert.match(caller, /uses: \.\/\.github\/workflows\/rust-native-shard\.yml/);
  assert.match(native, /^    timeout-minutes: 15$/m);
  assert.match(native, /^      max-parallel: 4$/m);
  assert.match(native, /matrix: \$\{\{ fromJSON\(inputs\.matrix\) \}\}/);
  assert.match(native, /IMPACT_POOL_PLATFORM: \$\{\{ inputs\.platform \}\}/);
  assert.match(native, /--shard "\$\{\{ matrix\.shard \}\}"/);
  assert.match(native, /--budget-seconds "\$\{\{ steps\.setup\.outputs\.budget_seconds \}\}"/);
  assert.match(native, /Math\.min\(720,840-Math\.ceil\(elapsed\)\)/);
  assert.match(native, /--restore-oracle-deps --execute/);
  assert.ok(!native.includes('continue-on-error'));
});

function withTargetEnvironment(value, action) {
  const previous = process.env.CARGO_TARGET_DIR;
  if (value === undefined) delete process.env.CARGO_TARGET_DIR;
  else process.env.CARGO_TARGET_DIR = value;
  try {
    return action();
  } finally {
    if (previous === undefined) delete process.env.CARGO_TARGET_DIR;
    else process.env.CARGO_TARGET_DIR = previous;
  }
}

test('all canonical IDs are registered exactly once, including core-profile markers', () => {
  assert.deepEqual([...CHECK_IDS].sort(), [...EXPECTED_IDS].sort());
  assert.equal(CHECK_IDS.length, 30);
  assert.equal(new Set(CHECK_IDS).size, CHECK_IDS.length);
  assert.ok(Object.isFrozen(CHECK_IDS));
  assert.equal(validateRegistry(graph([...EXPECTED_IDS].reverse())), true);
  for (const id of ['interactive', 'headless', 'certificate', 'updater', 'combined']) {
    assert.deepEqual(commands(id), [], id);
    assert.throws(() => validateRegistry(graph(EXPECTED_IDS.filter(value => value !== id))),
      /missing:/, id);
  }
});

test('registry rejects unknown, missing, duplicate and malformed graph IDs', () => {
  assert.throws(() => validateRegistry(null), /checks must be an array/);
  assert.throws(() => validateRegistry({checks: {}}), /checks must be an array/);
  assert.throws(() => validateRegistry({checks: [{}]}), /require IDs/);
  assert.throws(() => validateRegistry({checks: [null]}), /require IDs/);
  assert.throws(() => validateRegistry(graph([...EXPECTED_IDS, EXPECTED_IDS[0]])), /duplicate/);
  assert.throws(() => validateRegistry(graph([...EXPECTED_IDS, 'untrusted-command'])),
    /unknown: untrusted-command; missing: none/);
  assert.throws(() => validateRegistry(graph(EXPECTED_IDS.slice(1))),
    /unknown: none; missing: selector-tests/);
  assert.throws(() => validateRegistry(graph(['not-a-check'])),
    /unknown: not-a-check; missing: selector-tests/);
  for (const id of ['unknown', '__proto__', 'constructor', 'cargo test']) {
    assert.throws(() => commands(id), /Unknown validation check/);
  }
});

for (const id of EXPECTED_IDS) {
  test(`${id} emits bounded executable descriptors, not shell text`, () => {
    for (const command of commands(id)) {
      assert.ok(['cargo', 'python', 'git', 'node'].includes(command.program));
      assert.ok(Array.isArray(command.args));
      assert.ok(command.args.every(arg => typeof arg === 'string' && !/[\0\r\n]/.test(arg)));
      assert.ok(['command', 'prepare-oracle', 'verify-oracle', 'transport-graphs']
        .includes(command.kind));
      assert.ok(path.isAbsolute(command.cwd));
      assert.ok([rust, pinnedRust, artifacts].includes(command.cwd));
      assert.equal(command.shell, undefined);
      if (command.stdout) inside(artifacts, command.stdout);
      if (command.program === 'python') {
        assert.equal(command.args[0], '-B');
        assert.deepEqual(command.env, {PYTHONUTF8: '1', PYTHONDONTWRITEBYTECODE: '1',
          ...(command.cwd === artifacts ? {TMPDIR: artifacts, TEMP: artifacts, TMP: artifacts} : {})});
      }
      if (command.program === 'cargo') {
        assert.equal(command.cwd, rust);
        assert.equal(command.env.CARGO_TARGET_DIR, targetDir);
      }
    }
  });
}

test('command planning invokes neither providers nor filesystem operations and mutates no input', () => {
  const replacements = [
    [childProcess, ['spawn', 'spawnSync', 'exec', 'execSync', 'execFile', 'execFileSync', 'fork']],
    [fs, ['readFileSync', 'writeFileSync', 'appendFileSync', 'mkdirSync', 'rmSync',
      'readdirSync', 'statSync', 'existsSync', 'realpathSync']],
  ];
  const originals = [];
  const before = JSON.stringify(baseContext);
  try {
    for (const [object, names] of replacements) for (const name of names) {
      originals.push([object, name, object[name]]);
      object[name] = () => { throw new Error(`Unexpected operation: ${name}`); };
    }
    for (const id of CHECK_IDS) {
      assert.deepEqual(commands(id), commands(id));
    }
    assert.equal(validateRegistry(graph(EXPECTED_IDS)), true);
  } finally {
    for (const [object, name, original] of originals) object[name] = original;
  }
  assert.equal(JSON.stringify(baseContext), before);
  const first = commands('cli-smoke');
  first[0].args.push('must not persist');
  first[0].env.CARGO_TARGET_DIR = 'must not persist';
  assert.ok(!commands('cli-smoke')[0].args.includes('must not persist'));
  assert.equal(commands('cli-smoke')[0].env.CARGO_TARGET_DIR, targetDir);
});

test('impact graph command-looking data cannot supply executable argv', () => {
  const injected = graph(EXPECTED_IDS);
  injected.checks.forEach(check => {
    check.program = 'untrusted-provider';
    check.args = ['run-this'];
  });
  assert.equal(validateRegistry(injected), true);
  const planned = commands('cli-smoke', {plan: {...plan, graph: injected}});
  assert.ok(planned.every(command => command.program === 'cargo'));
  assert.deepEqual(planned.map(command => command.args.at(-1)), ['--help', '--version']);
});

test('paths with whitespace and shell metacharacters remain individual argv values', () => {
  const prepare = commands('oracle-tools').at(-1);
  assert.equal(valueAfter(prepare, '--candidate-root'), root);
  assert.equal(valueAfter(prepare, '--output'), pinnedOutput);
  assert.ok(prepare.args.includes(root));
  assert.ok(!prepare.args.includes(`"${root}"`));
  assert.equal(commands('parameters-oracle')[0].args[1], parameterRoot);
  const comparison = commands('read-parity').at(-1);
  assert.equal(valueAfter(comparison, '--read-probe'),
    path.join(targetDir, 'debug', 'examples', 'read_probe'));
  assert.ok(comparison.args.includes(path.join(artifacts, 'read-parity')));
});

test('only tooling tests, REPL source tests and integrity helpers use candidate Python cwd', () => {
  let comparisons = 0;
  for (const id of CHECK_IDS) for (const command of commands(id)) {
    if (command.program !== 'python') continue;
    if (id === 'oracle-tools') {
      assert.equal(command.cwd, command.kind === 'prepare-oracle' ? rust : artifacts);
    } else if (['command-inventory', 'repl', 'frozen-verify'].includes(id)) {
      assert.equal(command.cwd, rust);
    } else {
      comparisons++;
      assert.equal(command.cwd, pinnedRust, `${id}: ${command.args.join(' ')}`);
      assert.notEqual(command.cwd, rust);
      assert.ok(!command.args.includes(root));
    }
  }
  assert.ok(comparisons > 35);
  const customPython = path.join(artifacts, 'python runtime', 'python executable');
  for (const command of commands('jobs-oracle', {python: customPython}).slice(0, -1)) {
    assert.equal(command.program, customPython);
    assert.equal(command.cwd, pinnedRust);
  }
});

test('default frozen paths are deterministic and unsafe context paths are rejected', () => {
  const prepare = commands('oracle-tools', {
    pinnedOutput: undefined, pinnedRust: undefined, parameterRoot: undefined,
  }).at(-1);
  assert.equal(valueAfter(prepare, '--output'), pinnedOutput);
  assert.equal(commands('batch-oracle', {pinnedRust: undefined})[0].cwd, pinnedRust);
  assert.throws(() => commands('batch-oracle', {pinnedRust: rust}), /candidate Rust tree/);
  assert.throws(() => commands('cli-smoke', {root: 'relative'}), /root must be an absolute path/);
  assert.throws(() => commands('cli-smoke', {artifacts: 'relative'}), /artifacts must be an absolute path/);
  assert.throws(() => commands('cli-smoke', {pinnedRust: 'relative'}), /pinnedRust must be an absolute path/);
  assert.throws(() => commands('cli-smoke', {root: `${root}\0invalid`}), /root must be an absolute path/);
  assert.throws(() => commands('cli-smoke', {pinnedOutput: artifacts}), /inside artifacts/);
  assert.throws(() => commands('cli-smoke', {pinnedOutput: path.dirname(artifacts)}), /inside artifacts/);
  assert.throws(() => commands('parameters-oracle', {
    parameterRoot: path.join(path.dirname(artifacts), 'outside artifact root'),
  }), /inside artifacts/);
  assert.throws(() => commandsFor('cli-smoke'), /context is required/);
});

test('selector tests include coverage tooling with absolute source paths and artifact-only scratch', () => {
  const [command] = commands('selector-tests');
  assert.equal(command.program, 'node');
  assert.deepEqual(command.args, ['--test',
    path.join(rust, 'scripts', 'impact-plan.test.cjs'),
    path.join(rust, 'scripts', 'validate-impact.test.cjs'),
    path.join(rust, 'scripts', 'execution-evidence.test.cjs'),
    path.join(rust, 'scripts', 'impact-shards.test.cjs'),
    path.join(rust, 'scripts', 'impact-ci.test.cjs'),
    path.join(rust, 'scripts', 'impact-cache.test.cjs'),
    path.join(rust, 'scripts', 'validation-checks.test.cjs'),
    path.join(rust, 'scripts', 'rust-fast-check.test.cjs'),
    path.join(rust, 'scripts', 'rust-coverage.test.cjs'),
    path.join(rust, 'scripts', 'managed-bpa-acceptance.test.cjs')]);
  assert.equal(command.cwd, artifacts);
  assert.deepEqual(command.env, {IMPACT_TEST_ROOT: artifacts,
    TMPDIR: artifacts, TEMP: artifacts, TMP: artifacts});
});

test('real process lifecycle tests run only as their selected native contract, not every selector dependency', () => {
  const [selector] = commands('selector-tests');
  assert.ok(!selector.args.some(arg => arg.endsWith('validation-process.test.cjs')));
  const [supervision] = commands('process-supervision');
  assert.equal(supervision.program, 'node');
  assert.deepEqual(supervision.args, ['--test',
    path.join(rust, 'scripts', 'validation-process.test.cjs')]);
  assert.equal(supervision.cwd, artifacts);
  assert.deepEqual(supervision.env, selector.env);
});

test('command inventory captures actual Cargo metadata before its candidate checker consumes that file', () => {
  const [metadata, checker, ...extra] = commands('command-inventory');
  assert.deepEqual(extra, []);
  assert.equal(metadata.program, 'cargo');
  assert.deepEqual(metadata.args, ['metadata', '--offline', '--locked',
    '--no-deps', '--format-version', '1']);
  assert.equal(metadata.stdout, path.join(artifacts, 'command-inventory-metadata.json'));
  inside(artifacts, metadata.stdout);
  assert.equal(checker.program, 'python');
  assert.deepEqual(checker.args, ['-B', path.join('scripts', 'check_command_acceptance.py'),
    '--root', root, '--metadata', metadata.stdout]);
  assert.ok([metadata, checker].every(command => command.cwd === rust && command.kind === 'command'));
  assert.equal(checker.test, undefined, 'Inventory validation does not execute product tests');
  assert.deepEqual(checker.env, {PYTHONUTF8: '1', PYTHONDONTWRITEBYTECODE: '1'});
  assert.ok(!checker.args.includes(pinnedRust));
  assert.deepEqual(commands('command-inventory', {
    pinnedRust: path.join(artifacts, 'unused frozen source'), platform: 'windows',
  }), [metadata, checker]);
});

test('oracle preparation runs all seven tooling suites before its captured marker', () => {
  const result = commands('oracle-tools');
  assert.equal(result.length, 8);
  assert.deepEqual(result.slice(0, -1).map(command => valueAfter(command, '-p')).sort(), [
    'test_assemble_native_release.py', 'test_check_command_acceptance.py', 'test_check_report.py',
    'test_native_acceptance*.py', 'test_pinned_python_source.py', 'test_release_verify.py',
    'test_v2_upstream.py',
  ]);
  const identity = result.find(command => command.args.includes('test_check_report.py'));
  assert.equal(valueAfter(identity, '-s'), path.join(rust, 'tests', 'acceptance', 'identity'));
  for (const command of result.slice(0, -1)) {
    assert.equal(command.cwd, artifacts);
    assert.equal(command.test, 'unittest');
    assert.equal(valueAfter(command, '-s'), path.join(rust,
      ...(command === identity ? ['tests', 'acceptance', 'identity'] : ['scripts'])));
    assert.deepEqual(command.env, {PYTHONUTF8: '1', PYTHONDONTWRITEBYTECODE: '1',
      TMPDIR: artifacts, TEMP: artifacts, TMP: artifacts});
  }
  const marker = result.at(-1);
  assert.equal(marker.kind, 'prepare-oracle');
  assert.equal(marker.pinnedOutput, pinnedOutput);
  assert.equal(marker.stdout, path.join(artifacts, 'pinned-python-receipt.json'));
  assert.deepEqual(marker.args, ['-B', path.join('scripts', 'pinned_python_source.py'),
    'prepare', '--candidate-root', root, '--output', pinnedOutput]);
  const [verify] = commands('frozen-verify');
  assert.equal(verify.kind, 'verify-oracle');
  assert.equal(verify.cwd, rust);
  assert.equal(verify.program, 'python');
  assert.deepEqual(verify.args, ['-B', path.join('scripts', 'pinned_python_source.py'),
    'verify', '--output', pinnedOutput]);
  assert.equal(verify.stdout, undefined);
});

test('release assembly validation honors controller isolation without changing selected tests', () => {
  const result = commands('oracle-tools');
  const controller = result.filter(command => command.args.includes('test_assemble_native_release.py'));
  assert.equal(controller.length, 1);
  assert.deepEqual(controller[0].args, ['-B', '-m', 'unittest', 'discover',
    '-s', path.join(rust, 'scripts'), '-p', 'test_assemble_native_release.py', '-v']);
  assert.ok(path.isAbsolute(valueAfter(controller[0], '-s')));
  assert.equal(result.filter(command => command.args.includes('test_assemble_native_release.py')).length, 1);
  assert.equal(result.length, 8);
});

test('native CI front-loads exact engineering suites without replacing the full gate', () => {
  const workflow = ['rust-ci.yml', 'rust-native-shard.yml'].map(name =>
    fs.readFileSync(path.join(__dirname, '..', '..', '.github', 'workflows', name), 'utf8')).join('\n');
  const engineering = fs.readFileSync(path.join(__dirname, 'engineering_preflight.py'), 'utf8');
  const executor = fs.readFileSync(path.join(__dirname, 'validate-impact.cjs'), 'utf8');
  assert.ok(executor.indexOf('if (shard?.engineering)') <
    executor.indexOf('const cargoCommands = commands.filter('));
  assert.match(executor, /if \(shard\?\.engineering\)/);
  assert.ok(executor.includes("'engineering_preflight.py'"));
  assert.ok(engineering.includes('engineering-inventory.json'));
  assert.ok(engineering.includes('acceptance.os is os'));
  assert.match(engineering, /not result\.expectedFailures[\s\S]*skips == expected/);
  assert.match(engineering, /"discovered": discovered,[\s\S]*"run": 0/);
  assert.match(engineering, /"passed": False,[\s\S]*"reason": "inventory_mismatch"/);
  assert.match(engineering, /persist\(\)\r?\n\s+return 1/);
  assert.ok(engineering.includes('report["pty_receipts"].append(captured[0])'));
  assert.ok(engineering.includes('return captured'));
  assert.ok(workflow.includes('Execute the shared local and CI validation plan'));
  assert.ok(workflow.includes('--restore-oracle-deps --execute'));
  assert.ok(workflow.includes('Require successful selection and every selected native check'));
  assert.ok(workflow.includes('contents: read'));
});

test('upstream tracking inputs select the candidate-only offline tooling tests', () => {
  const actual = JSON.parse(fs.readFileSync(
    path.join(__dirname, '..', 'validation', 'impact-graph.json'), 'utf8'));
  const tooling = actual.checks.find(check => check.id === 'frozen-verify');
  for (const file of ['rust/scripts/v2_upstream.py', 'rust/scripts/test_v2_upstream.py']) {
    assert.ok(tooling.inputs.includes(file), `${file} must select tooling validation and integrity`);
  }
  const unit = commands('oracle-tools').find(command => command.args.includes('test_v2_upstream.py'));
  assert.equal(unit.program, 'python');
  assert.equal(unit.cwd, artifacts);
  assert.deepEqual(unit.args, ['-B', '-m', 'unittest', 'discover',
    '-s', path.join(rust, 'scripts'), '-p', 'test_v2_upstream.py', '-v']);
});

test('all six JSON observations feed their exact pinned assertions and unit selectors', () => {
  const result = commands('json-parity');
  assert.equal(result.length, 15);
  const observations = [
    ['fabric-core-json', 'core_definition_json', 'core-definition-json.json', 'definition_reference.py'],
    ['fabric-cli-output', 'definition_json', 'cli-definition-json.json', 'definition_reference.py'],
    ['fabric-cli-output', 'compatibility_probe', 'output-json.json', 'reference.py'],
    ['fabric-core-json', 'unicode_json', 'unicode-json.json', 'unicode_reference.py'],
    ['fabric-core-json', 'native_json', 'native-json.json', 'native_reference.py'],
    ['fabric-cli-output', 'text_probe', 'core-text-observations.json', 'text_reference.py'],
  ];
  observations.forEach(([pkg, example, output, reference], index) => {
    assert.deepEqual(result[index].args, ['run', '--quiet', '--offline', '--locked',
      '-p', pkg, '--example', example]);
    assert.equal(result[index].stdout, path.join(artifacts, output));
    assert.deepEqual(result[index + 6].args, ['-B',
      path.join('tests', 'compatibility', 'output', reference),
      ...(example === 'native_json' ? ['--encoding-only'] : []),
      '--observed', path.join(artifacts, output)]);
    assert.equal(result[index + 6].cwd, pinnedRust);
  });
  assert.deepEqual(result.slice(12).map(command => valueAfter(command, '-p')),
    ['test_reference.py', 'test_unicode_reference.py', 'test_native_reference.py']);
});

test('read and broker read preserve native builds, unit selectors and all three probes', () => {
  const read = commands('read-parity');
  assert.equal(read.length, 4);
  assert.deepEqual(read[0].args, ['build', '--offline', '--locked', '-p',
    'fabric-cli', '-p', 'fabric-cli-query', '--examples']);
  assert.deepEqual(read.slice(1, 3).map(command => valueAfter(command, '-p')),
    ['test_run_read_parity.py', 'test_inventory.py']);
  const broker = commands('broker-read-parity');
  assert.equal(broker.length, 2);
  assert.deepEqual(broker[0].args, [...read[0].args, '--features', 'fabric-cli/windows-broker']);
  for (const [command, output] of [[read.at(-1), 'read-parity'], [broker.at(-1), 'read-parity-windows-broker']]) {
    assert.equal(script(command), path.join('tests', 'compatibility', 'run_read_parity.py'));
    for (const [flag, name] of [
      ['--read-probe', 'read_probe'], ['--query-probe', 'projection_output'],
      ['--folder-probe', 'folder_read_probe'],
    ]) assert.equal(valueAfter(command, flag), path.join(targetDir, 'debug', 'examples', name));
    assert.equal(valueAfter(command, '--artifacts'), path.join(artifacts, output));
  }
});

test('native read preserves both session consumers and the native example build', () => {
  const result = commands('native-read');
  assert.equal(result.length, 3);
  assert.deepEqual(result[0].args, ['build', '--offline', '--locked', '-p', 'fabric-cli',
    '--examples', '--features', 'native-read']);
  assert.equal(script(result[1]), path.join('tests', 'compatibility', 'read_session', 'consumer.py'));
  assert.equal(valueAfter(result[1], '--rust-probe'),
    path.join(targetDir, 'debug', 'examples', 'read_session_probe'));
  assert.equal(script(result[2]), path.join('tests', 'compatibility', 'job_reads', 'compare.py'));
  assert.equal(valueAfter(result[2], '--probe'),
    path.join(targetDir, 'debug', 'examples', 'job_read_session_probe'));
});

test('probe paths follow explicit, ambient and relative Cargo targets with platform suffixes', () => {
  const ambient = path.join(artifacts, 'ambient cargo target');
  withTargetEnvironment(ambient, () => {
    const explicit = commands('native-read', {platform: 'windows'});
    assert.equal(valueAfter(explicit[1], '--rust-probe'),
      path.join(targetDir, 'debug', 'examples', 'read_session_probe.exe'));
    const inherited = commands('native-read', {targetDir: undefined, platform: 'win32'});
    assert.equal(valueAfter(inherited[1], '--rust-probe'),
      path.join(ambient, 'debug', 'examples', 'read_session_probe.exe'));
    assert.equal(inherited[0].env.CARGO_TARGET_DIR, ambient);
    const contextual = commands('resource-oracle', {
      targetDir: undefined, env: {CARGO_TARGET_DIR: 'context target'},
    });
    assert.equal(valueAfter(contextual.at(-1), '--probes'),
      path.join(rust, 'context target', 'debug', 'examples'));
  });
  withTargetEnvironment(undefined, () => {
    const result = commands('read-parity', {targetDir: undefined, platform: 'macos'});
    assert.equal(valueAfter(result.at(-1), '--read-probe'),
      path.join(rust, 'target', 'debug', 'examples', 'read_probe'));
    assert.equal(result[0].env, undefined);
  });
  const relative = commands('native-read', {targetDir: 'relative target', platform: 'Windows-2025'});
  assert.equal(relative[0].env.CARGO_TARGET_DIR, path.join(rust, 'relative target'));
  assert.ok(valueAfter(relative[1], '--rust-probe').endsWith('read_session_probe.exe'));
  assert.throws(() => commands('native-read', {targetDir: ''}), /nonempty path/);
});

test('resource contracts preserve both example packages, unit selector and native probe directory', () => {
  const result = commands('resource-oracle');
  assert.equal(result.length, 3);
  assert.deepEqual(result[0].args, ['build', '--offline', '--locked', '-p',
    'fabric-cli-mutations', '-p', 'fabric-sdk-operations', '--examples']);
  assert.equal(valueAfter(result[1], '-p'), 'test_run_resource_parity.py');
  assert.equal(script(result[2]), path.join('tests', 'compatibility', 'run_resource_parity.py'));
  assert.equal(valueAfter(result[2], '--probes'), path.join(targetDir, 'debug', 'examples'));
  assert.equal(valueAfter(result[2], '--artifacts'), path.join(artifacts, 'resource-parity'));
});

test('all eight job source contracts run before the actual offline consumer', () => {
  const result = commands('jobs-oracle');
  assert.equal(result.length, 9);
  assert.deepEqual(result.slice(0, -1).map(script), [
    'reference', 'branches_reference', 'operation_output_reference', 'skill_headers_reference',
    'command_source', 'poll_source', 'numeric_source', 'endpoint_reference',
  ].map(name => path.join('tests', 'compatibility', 'jobs', `${name}.py`)));
  assert.deepEqual(result.at(-1).args, ['run', '--offline', '--locked', '-p',
    'fabric-cli-jobs', '--example', 'offline_job']);
});

test('parameter source has finite immutable Git argv before all ten frozen comparisons', () => {
  const result = commands('parameters-oracle');
  assert.equal(result.length, 14);
  const pin = '5ca096c00a9a878c646c59fe537b883eba951329';
  assert.match(pin, /^[a-f0-9]{40}$/);
  assert.deepEqual(result.slice(0, 4).map(command => [command.program, command.args]), [
    ['git', ['init', parameterRoot]],
    ['git', ['-C', parameterRoot, 'config', '--local', 'core.autocrlf', 'true']],
    ['git', ['-C', parameterRoot, 'fetch', '--depth=1',
      'https://github.com/microsoft/fabric-cicd.git', pin]],
    ['git', ['-C', parameterRoot, 'checkout', '--detach', pin]],
  ]);
  assert.ok(result.slice(0, 4).every(command => command.cwd === rust));
  const references = ['configured_flags', 'flag_protocol', 'config_value_edges',
    'parameter_context', 'parameter_pipeline', 'path_filter', 'selected_constants',
    'parameter_resolution', 'parameter_schema_edges', 'config_extensions_execution'];
  result.slice(4).forEach((command, index) => {
    assert.equal(command.cwd, pinnedRust);
    assert.deepEqual(command.args, ['-B', path.join('engine', 'control-plane', 'deployment', 'tests', `${references[index]}_reference.py`),
    '--reference-root', parameterRoot]);
  });
});

test('remaining frozen source gates preserve batch, definition, import and export selectors', () => {
  assert.deepEqual(commands('batch-oracle')[0].args, ['-B',
    path.join('clients', 'cli', 'app', 'tests', 'batch_reference.py'),
    '--check', path.join('clients', 'cli', 'app', 'tests', 'batch_source.json')]);
  const definitions = commands('definitions-oracle');
  assert.equal(definitions.length, 3);
  assert.equal(valueAfter(definitions[0], '-p'), 'test_run_definition_parity.py');
  assert.equal(script(definitions[1]), path.join('tests', 'compatibility', 'run_definition_parity.py'));
  assert.equal(valueAfter(definitions[1], '--artifacts'), path.join(artifacts, 'definition-parity'));
  assert.deepEqual(definitions[2].args, ['test', '--offline', '--locked', '-p',
    'fabric-deploy', '--test', 'definitions']);
  assert.equal(script(commands('import-oracle')[0]), path.join('engine', 'control-plane', 'deployment', 'tests', 'definition_import_reference_checks.py'));
  const [exportCommand] = commands('export-oracle');
  assert.equal(script(exportCommand), path.join('tests', 'compatibility', 'run_export_source_parity.py'));
  assert.equal(valueAfter(exportCommand, '--artifacts'),
    path.join(pinnedRust, '..', '.oracle-artifacts', 'export-source-receipts'));
  inside(path.dirname(pinnedRust), valueAfter(exportCommand, '--artifacts'));
});

test('definitions consumer is retained unless core selects its complete default-mode test', () => {
  for (const target of ['all', 'test:definitions']) {
    const result = commands('definitions-oracle', {plan: {
      ...plan, tests: [{package: 'fabric-deploy', target, default_available: true}],
    }});
    assert.equal(result.length, 2);
    assert.ok(result.every(command => command.program === 'python'));
  }
  for (const tests of [
    [],
    [{package: 'fabric-cli', target: 'all'}],
    [{package: 'fabric-deploy', target: 'lib'}],
    [{package: 'fabric-deploy', target: 'test:other'}],
    [{package: 'fabric-deploy', target: 'test:definitions', filter: 'one_branch'}],
    [{package: 'fabric-deploy', target: 'all', filter: 'one_branch'}],
    [{package: 'fabric-deploy', target: 'test:definitions', default_available: false}],
    [{package: 'fabric-deploy', target: 'all', default_skipped_targets: ['test:definitions']}],
  ]) {
    const result = commands('definitions-oracle', {plan: {...plan, tests}});
    assert.equal(result.length, 3, JSON.stringify(tests));
    assert.deepEqual(result.at(-1).args, ['test', '--offline', '--locked', '-p',
      'fabric-deploy', '--test', 'definitions']);
    assert.equal(result.at(-1).cwd, rust);
  }
});

test('CLI-only feature profiles never expand into unrelated full identity or updater suites', () => {
  const cases = [
    {id: 'headless', pkg: 'fabric-identity', feature: 'fabric-identity/headless'},
    {id: 'updater', pkg: 'fabric-cli-update', feature: 'fabric-cli-update/fixture-harness'},
  ];
  for (const {id, pkg, feature} of cases) {
    assert.deepEqual(commands(id), []);
    const profile = {
      id, packages: ['fabric-cli', pkg], platforms: ['linux'], features: [feature],
      tests: [{package: pkg, target: 'all'}],
    };
    assert.deepEqual(commands(id, {plan: {...plan, profiles: [profile]}}), []);
    for (const scoped of [
      {...profile, tests: []},
      {...profile, tests: [{package: 'fabric-cli', target: 'all'}]},
      {...profile, tests: [{package: pkg, target: 'lib'}]},
      {...profile, tests: [{package: pkg, target: 'all', filter: 'one_branch'}]},
    ]) {
      assert.deepEqual(commands(id, {plan: {
        ...plan, profiles: [scoped], tests: [{package: 'fabric-cli', target: 'test:notebook'}],
      }}), []);
    }
  }
});

test('standalone transport, broker, REPL, context, SDK and CLI consumers remain explicit', () => {
  const transport = commands('transport');
  assert.deepEqual(transport[0].args, ['check', '--offline', '--locked', '-p',
    'fabric-sdk-transport', '--no-default-features', '--lib']);
  assert.deepEqual(transport[1].args, ['run', '--offline', '--locked', '-p',
    'fabric-sdk-transport', '--features', 'hyper-rustls', '--example', 'sdk_consumer']);
  const broker = commands('broker');
  assert.equal(broker.length, 2);
  assert.deepEqual(broker.map(command => valueAfter(command, '--example')), ['safe_consumer', 'safe_consumer']);
  assert.ok(broker[0].args.includes('--no-default-features'));
  assert.equal(valueAfter(broker[1], '--features'), 'windows-broker');
  assert.equal(valueAfter(commands('repl')[0], '-p'), 'test_source_oracle.py');
  const context = commands('context-output');
  assert.equal(context.length, 4);
  assert.equal(valueAfter(context[0], '--example'), 'extended_resolve');
  assert.equal(script(context[1]), path.join('tests', 'compatibility', 'context', 'extended_reference.py'));
  assert.equal(valueAfter(context[2], '--example'), 'text_probe');
  assert.equal(context[2].stdout, path.join(artifacts, 'text-observations.json'));
  assert.equal(valueAfter(context[3], '--observed'), context[2].stdout);
  assert.equal(valueAfter(commands('sdk-example')[0], '--example'), 'injected_credentials');
  assert.deepEqual(commands('cli-smoke').map(command => command.args.at(-1)), ['--help', '--version']);
});

test('dependency trees select exact package argv and reject command-like names', () => {
  assert.deepEqual(commands('sdk-graph')[0].args, ['tree', '--locked', '-p', 'fabric-sdk',
    '--edges', 'normal']);
  assert.deepEqual(commands('dependency-graph').map(command => command.args), [
    ['tree', '--locked', '--package', 'fabric-cli', '--package', 'fabric-sdk-transport',
      '--edges', 'features'],
    ['tree', '--locked', '--package', 'fabric-cli', '--package', 'fabric-sdk-transport',
      '--duplicates'],
  ]);
  for (const packages of [[], ['fabric-cli', 'fabric-cli'], ['--all'], ['fabric-cli;echo'], ['a b'], [null]]) {
    assert.throws(() => commands('dependency-graph', {plan: {packages}}), /selected package names/);
  }
});

test('transport descriptors capture all three trees before a data-only assertion marker', () => {
  const result = commands('transport-graphs');
  assert.equal(result.length, 4);
  const marker = result.at(-1);
  assert.equal(marker.kind, 'transport-graphs');
  assert.equal(marker.program, 'node');
  assert.deepEqual(marker.args, []);
  assert.equal(marker.cwd, rust);
  assert.deepEqual(marker.graphs.defaults.map(item => item.package),
    ['fabric-sdk', 'fabric-sdk-transport']);
  marker.graphs.defaults.forEach((item, index) => {
    assert.equal(item.path, result[index].stdout);
    assert.deepEqual(result[index].args, ['tree', '--offline', '--locked', '-p',
      item.package, '--no-default-features', '--edges', 'normal', '--prefix', 'none',
      '--format', '{p}']);
  });
  assert.equal(marker.graphs.optin, result[2].stdout);
  assert.deepEqual(result[2].args, ['tree', '--offline', '--locked', '-p',
    'fabric-sdk-transport', '--features', 'hyper-rustls', '--edges', 'normal',
    '--prefix', 'none', '--format', '{p}']);
});

const safeDefaults = Object.freeze({
  'fabric-sdk': 'fabric-sdk v0.1.0\nfabric-core v0.1.0\n',
  'fabric-sdk-transport': 'fabric-sdk-transport v0.1.0\nfabric-sdk-http v0.1.0\n',
});
const backendNames = ['hyper', 'hyper-util', 'hyper-rustls', 'rustls'];
const optinGraph = [...backendNames.map(name => `${name} v1.0.0`),
  'rustls-platform-verifier v0.1.0'].join('\n');

test('transport assertion accepts exact allowed graphs, CRLF and normal repeated Cargo nodes', () => {
  assert.equal(assertTransportGraphs(safeDefaults, optinGraph), true);
  assert.equal(assertTransportGraphs(safeDefaults, optinGraph.replaceAll('\n', '\r\n')), true);
  assert.equal(assertTransportGraphs(safeDefaults, `${optinGraph}\nhyper v1.0.0 (*)`), true);
  assert.equal(assertTransportGraphs({
    ...safeDefaults, 'fabric-sdk': `${safeDefaults['fabric-sdk']}hyper-extra v1.0.0`,
  }, `${optinGraph}\nreqwest-extra v1.0.0`), true);
});

test('every forbidden default backend or higher layer fails in either captured graph', () => {
  const forbidden = ['hyper', 'hyper-util', 'hyper-rustls', 'rustls',
    'rustls-platform-verifier', 'tokio-tungstenite', 'tungstenite', 'reqwest', 'native-tls', 'openssl',
    'fabric-identity', 'fabric-identity-windows', 'fabric-cli', 'fabric-deploy'];
  for (const pkg of Object.keys(safeDefaults)) for (const name of forbidden) {
    assert.throws(() => assertTransportGraphs({
      ...safeDefaults, [pkg]: `${safeDefaults[pkg]}\n${name} v1.0.0\n${name} v1.0.0 (*)`,
    }, optinGraph), /selected a backend or higher layer/, `${pkg}: ${name}`);
  }
});

test('opt-in rejects unrelated backends and every missing or ambiguously named required backend', () => {
  for (const name of ['tokio-tungstenite', 'tungstenite', 'reqwest', 'native-tls', 'openssl', 'fabric-identity',
    'fabric-identity-windows', 'fabric-cli', 'fabric-deploy']) {
    assert.throws(() => assertTransportGraphs(safeDefaults,
      `${optinGraph}\n${name} v1.0.0\n${name} v1.0.0 (*)`), /unrelated backend or higher layer/);
  }
  for (const missing of backendNames) {
    const without = optinGraph.split('\n').filter(line => !line.startsWith(`${missing} v`)).join('\n');
    for (const ambiguous of [`${missing}-lookalike v1.0.0`, `${missing}x v1.0.0`,
      `not-${missing} v1.0.0`, `${missing}\tv1.0.0`]) {
      assert.throws(() => assertTransportGraphs(safeDefaults,
        `${without}\n${ambiguous}\n${ambiguous}`), /is missing/, `${missing}: ${ambiguous}`);
    }
  }
  const duplicatesNotCoverage = 'hyper v1.0.0\nhyper v1.0.0 (*)\nhyper-util v1.0.0';
  assert.throws(() => assertTransportGraphs(safeDefaults, duplicatesNotCoverage), /missing hyper-rustls/);
});

test('transport assertions reject absent captures without executing any graph producer', () => {
  for (const defaults of [undefined, [], {}, {'fabric-sdk': 'fabric-sdk v0.1.0'}]) {
    assert.throws(() => assertTransportGraphs(defaults, optinGraph), /graphs must be keyed|Missing default/);
  }
  assert.throws(() => assertTransportGraphs(safeDefaults, undefined), /Missing opt-in/);
  assert.throws(() => assertTransportGraphs(safeDefaults, ''), /is missing hyper/);
  for (const pkg of Object.keys(safeDefaults)) for (const capture of [
    '', '\n \r\n', 'unrelated-package v1.0.0', `${pkg}-lookalike v1.0.0`,
  ]) {
    assert.throws(() => assertTransportGraphs({...safeDefaults, [pkg]: capture}, optinGraph),
      /Missing default .* package in captured graph/);
  }
});
