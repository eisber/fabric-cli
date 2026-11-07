// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const {spawnSync} = require('node:child_process');

const PLATFORMS = {win32: 'windows', linux: 'linux', darwin: 'macos'};
function requireThat(condition, message) {
  if (!condition) throw new Error(message);
}

function packageArgs(packages) {
  return [...new Set(packages)].sort().flatMap(name => ['--package', name]);
}

function ownFeatures(profile, name) {
  return (profile.features || []).filter(feature => feature.startsWith(`${name}/`)).sort();
}

function defaultTests(plan) {
  return plan.tests.filter(test => test.default_available !== false &&
    test.default !== false && !test.default_skipped && !test.profile_only);
}

function profileTests(plan, profile) {
  return profile.tests || plan.tests.filter(test => profile.packages.includes(test.package));
}

function testCommands(tests, features = [], label = 'default') {
  const grouped = new Map();
  for (const test of tests) {
    requireThat(!test.filter, 'Impact tests must select complete Cargo targets, not name filters');
    const targets = grouped.get(test.package) || new Set();
    targets.add(test.target);
    grouped.set(test.package, targets);
  }
  const commands = [];
  const all = [...grouped].filter(([, targets]) => targets.has('all')).map(([name]) => name);
  const command = (packages, selectors) => ({
    id: `${label}:test:${packages.join(',')}`,
    program: 'cargo',
    args: ['test', '--offline', '--locked', ...packageArgs(packages),
      ...(features.length ? ['--features', features.join(',')] : []), ...selectors],
    test: true,
  });
  if (all.length) commands.push(command(all.sort(), ['--all-targets']));
  for (const [name, targets] of [...grouped].sort(([a], [b]) => a.localeCompare(b))) {
    if (targets.has('all')) continue;
    const selectors = [...targets].sort().flatMap(target => {
      if (target === 'lib') return ['--lib'];
      const match = /^(bin|test|example):([a-zA-Z0-9_-]+)$/.exec(target);
      requireThat(match, `Unsupported Cargo test target: ${target}`);
      return [`--${match[1]}`, match[2]];
    });
    requireThat(selectors.length, `No test targets selected for ${name}`);
    commands.push(command([name], selectors));
  }
  return commands;
}

function coreCommands(plan, platform) {
  requireThat(Object.values(PLATFORMS).includes(platform), `Unsupported host platform: ${platform}`);
  if (!plan.packages.length) return [];
  const args = packageArgs(plan.packages);
  const cargo = (id, command, extra) => ({
    id, program: 'cargo', args: [command, '--offline', '--locked', ...args, ...extra],
  });
  // cargo-fmt expands targets into rustfmt argv, exceeding Windows limits for a full workspace.
  const formatting = platform === 'windows' ?
    [...new Set(plan.packages)].sort().map(name => ({
      id: `default:fmt:${name}`, program: 'cargo',
      args: ['fmt', '--package', name, '--', '--check'],
    })) :
    [{id: 'default:fmt', program: 'cargo', args: ['fmt', ...args, '--', '--check']}];
  const commands = [
    ...formatting,
    cargo('default:check', 'check', ['--all-targets']),
    cargo('default:clippy', 'clippy', ['--all-targets', '--', '-D', 'warnings']),
    ...testCommands(defaultTests(plan)),
    {...cargo('default:doc', 'test', ['--doc']), test: true},
  ];
  for (const profile of plan.profiles) {
    if (!profile.platforms.includes(platform)) continue;
    const selected = profileTests(plan, profile);
    const packages = [...new Set(profile.packages)].sort();
    requireThat(packages.length && profile.features.length, `Empty feature profile: ${profile.id}`);
    const featureArgs = ['--features', [...profile.features].sort().join(',')];
    for (const kind of ['check', 'clippy']) {
      commands.push({
        id: `${profile.id}:${kind}`,
        program: 'cargo',
        args: [kind, '--offline', '--locked', ...packageArgs(packages), ...featureArgs,
          '--all-targets', ...(kind === 'clippy' ? ['--', '-D', 'warnings'] : [])],
      });
    }
    // Test a consumer with its own features; forwarded dependency features come from Cargo.
    for (const name of [...new Set(selected.map(test => test.package))].sort()) {
      commands.push(...testCommands(selected.filter(test => test.package === name),
        ownFeatures(profile, name), profile.id));
    }
  }
  return commands;
}

function sourceInventory(root) {
  const result = spawnSync('git', ['ls-files', '-z', '--cached', '--others', '--exclude-standard'], {
    cwd: root, encoding: 'utf8', timeout: 30000, maxBuffer: 32 * 1024 * 1024,
  });
  requireThat(!result.error && result.status === 0, 'Unable to inventory source with Git');
  const hash = crypto.createHash('sha256');
  const files = [];
  for (const name of [...new Set(result.stdout.split('\0').filter(Boolean))].sort()) {
    const file = path.resolve(root, name);
    requireThat(file.startsWith(`${root}${path.sep}`), `Source path escaped checkout: ${name}`);
    hash.update(name);
    if (!fs.existsSync(file)) {
      hash.update('\0deleted\0');
      files.push({path: name, state: 'deleted'});
    } else {
      requireThat(!fs.lstatSync(file).isSymbolicLink(), `Source symlink requires review: ${name}`);
      const bytes = fs.readFileSync(file);
      hash.update('\0');
      hash.update(bytes);
      files.push({path: name, state: 'file', size: bytes.length,
        sha256: crypto.createHash('sha256').update(bytes).digest('hex')});
    }
    hash.update('\0');
  }
  return {schema: 1, digest: hash.digest('hex'), files};
}

function sourceDigest(root) {
  return sourceInventory(root).digest;
}

function buildEnvironment(environment = process.env) {
  const names = ['CARGO_BUILD_JOBS', 'RUST_TEST_THREADS', 'CARGO_PROFILE_DEV_DEBUG',
    'CARGO_PROFILE_TEST_DEBUG', 'CARGO_INCREMENTAL'];
  return Object.fromEntries(names.filter(name => environment[name] !== undefined)
    .map(name => [name, environment[name]]));
}

function commandResult(command, context, execute = spawnSync) {
  const started = Date.now();
  const environment = {...process.env, PYTHONUTF8: '1', PYTHONDONTWRITEBYTECODE: '1',
    FAB_UPDATE_TEST_ROOT: context.artifacts, FAB_CLI_V2_TEST_ROOT: context.artifacts,
    FAB_IDENTITY_TEST_ROOT: context.artifacts, ...command.env};
  const result = execute(command.program, command.args, {
    cwd: command.cwd || path.join(context.root, 'rust'),
    env: environment,
    encoding: 'utf8',
    timeout: Math.min(context.commandTimeout,
      Math.max(1, Math.min(context.deadline, command.deadline || context.deadline) - Date.now())),
    maxBuffer: 64 * 1024 * 1024,
    windowsHide: true,
    shell: false,
  });
  const commandId = String(command.id || 'contract');
  const safeId = commandId.replace(/[^a-zA-Z0-9._-]/g, '_');
  const label = safeId.length > 64 ?
    `${safeId.slice(0, 40)}-${crypto.createHash('sha256').update(commandId).digest('hex').slice(0, 16)}` :
    safeId;
  const prefix = `${String(context.results.length).padStart(3, '0')}-${label}`;
  const stdout = result.stdout || '';
  const stderr = result.stderr || '';
  const stdoutFile = path.join(context.artifacts, `${prefix}.stdout.log`);
  const stderrFile = path.join(context.artifacts, `${prefix}.stderr.log`);
  fs.writeFileSync(stdoutFile, stdout);
  fs.writeFileSync(stderrFile, stderr);
  if (command.stdout) fs.writeFileSync(command.stdout, stdout);
  const digest = value => crypto.createHash('sha256').update(value).digest('hex');
  const counts = [...stdout.matchAll(/test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored/g)];
  const record = {
    id: command.id, program: command.program, args: command.args,
    cwd: command.cwd || path.join(context.root, 'rust'), elapsed_ms: Date.now() - started,
    build_environment: buildEnvironment(environment),
    exit: result.status, signal: result.signal || null, error: result.error?.message || null,
    stdout: stdoutFile, stderr: stderrFile,
    stdout_sha256: digest(stdout), stderr_sha256: digest(stderr),
    tests_passed: counts.reduce((total, match) => total + Number(match[1]), 0),
    tests_ignored: counts.reduce((total, match) => total + Number(match[3]), 0),
  };
  context.results.push(record);
  fs.writeFileSync(path.join(context.artifacts, 'commands.json'), `${JSON.stringify(context.results, null, 2)}\n`);
  requireThat(!result.error && result.status === 0 && !result.signal,
    `Validation failed: ${command.id || command.program} (exit ${result.status}; ` +
    `${result.error?.message || result.signal || 'see command logs'})`);
  return stdout;
}

function oracleVersions(rust, requirement, seen = new Set()) {
  const file = path.resolve(rust, requirement);
  requireThat(file.startsWith(`${rust}${path.sep}`), 'Oracle requirements escaped the Rust workspace');
  requireThat(!seen.has(file), 'Oracle requirements contain an inclusion cycle');
  const next = new Set([...seen, file]);
  const versions = {};
  for (const raw of fs.readFileSync(file, 'utf8').split(/\r?\n/)) {
    const line = raw.trim();
    if (!line || line.startsWith('#')) continue;
    if (line.startsWith('-r ')) {
      const included = oracleVersions(rust,
        path.relative(rust, path.resolve(path.dirname(file), line.slice(3).trim())), next);
      for (const [name, version] of Object.entries(included)) {
        requireThat(!versions[name] || versions[name] === version, `Conflicting oracle dependency pin: ${name}`);
        versions[name] = version;
      }
    } else {
      const match = /^([a-zA-Z0-9_.-]+)==([a-zA-Z0-9.+-]+)$/.exec(line);
      requireThat(match, `Oracle dependency must be exactly pinned: ${line}`);
      requireThat(!versions[match[1]] || versions[match[1]] === match[2],
        `Conflicting oracle dependency pin: ${match[1]}`);
      versions[match[1]] = match[2];
    }
  }
  return versions;
}

function restoreOracleDependencies(context, run, probeCommand = spawnSync) {
  const probes = [
    {module: 'jmespath, cachetools', requirement: path.join('tests', 'compatibility', 'requirements.txt')},
  ];
  if (context.plan.checks.some(id =>
    ['definitions-oracle', 'parameters-oracle', 'import-oracle', 'export-oracle'].includes(id))) {
    probes.push({module: 'requests, urllib3, yaml, jsonpath_ng',
      requirement: path.join('scripts', 'rust-fast-requirements.txt')});
  }
  for (const probe of probes) {
    const versions = oracleVersions(path.join(context.root, 'rust'), probe.requirement);
    const script = `import ${probe.module}; import importlib.metadata as m, json; ` +
      `expected = json.loads(${JSON.stringify(JSON.stringify(versions))}); ` +
      'bad = {name: m.version(name) for name, version in expected.items() if m.version(name) != version}; ' +
      'print(json.dumps(bad)); raise SystemExit(42 if bad else 0)';
    const result = probeCommand(context.python, ['-c', script], {
      cwd: path.join(context.root, 'rust'), encoding: 'utf8', timeout: 30000,
    });
    if (!result.error && result.status === 0) continue;
    requireThat(result.status === 42 || (result.status !== null && result.stderr?.includes('ModuleNotFoundError')),
      `Oracle dependency probe failed: ${result.error?.message || result.stderr}`);
    requireThat(context.restoreOracleDeps,
      `Missing oracle dependencies or unsupported versions (${result.stdout?.trim() || 'module unavailable'}); ` +
      `restore ${probe.requirement} or use --restore-oracle-deps`);
    run({id: 'oracle:restore', program: context.python,
      args: ['-m', 'pip', 'install', '--disable-pip-version-check', '-r', probe.requirement]});
  }
}

function runContracts(context, registry, run, probeCommand = spawnSync) {
  const graph = JSON.parse(fs.readFileSync(path.join(context.root, 'rust', 'validation', 'impact-graph.json'), 'utf8'));
  registry.validateRegistry(graph);
  const selected = new Set(context.plan.checks);
  const visited = new Set();
  let prepared = false;
  const execute = id => {
    if (visited.has(id) || id === 'frozen-verify') return;
    const check = graph.checks.find(item => item.id === id);
    requireThat(check, `Unknown selected contract: ${id}`);
    if (check.platforms && !check.platforms.includes(context.platform)) return;
    for (const dependency of check.depends_on || []) execute(dependency);
    visited.add(id);
    const deadline = Math.min(context.deadline || Infinity,
      Date.now() + (registry.CHECK_BUDGETS?.[id] || Infinity));
    const checkRun = command => {
      requireThat(Date.now() < deadline, `Validation contract budget exhausted: ${id}`);
      return run({...command, deadline});
    };
    const commands = registry.commandsFor(id, context);
    if (!commands.length) {
      requireThat(context.plan.profiles.some(profile => profile.checks?.includes(id) &&
        profile.platforms.includes(context.platform) &&
        profileTests(context.plan, profile).length > 0),
      `No execution profile with test targets satisfies ${id}`);
    }
    for (const command of commands) {
      command.id ||= id;
      if (command.kind === 'prepare-oracle') {
        restoreOracleDependencies(context, checkRun, probeCommand);
        const output = checkRun({...command, id: 'oracle:prepare', kind: 'command'});
        context.pinnedRust = JSON.parse(output).rust_root;
        requireThat(context.pinnedRust && fs.existsSync(context.pinnedRust), 'Oracle preparation returned no source');
        prepared = true;
      } else if (command.kind === 'transport-graphs') {
        const graphs = Object.fromEntries(command.graphs.defaults.map(graph =>
          [graph.package, fs.readFileSync(graph.path, 'utf8')]));
        const optin = fs.readFileSync(command.graphs.optin, 'utf8');
        registry.assertTransportGraphs(graphs, optin);
      } else {
        checkRun(command);
      }
    }
  };
  try {
    for (const id of context.plan.checks) execute(id);
  } finally {
    if (prepared) {
      requireThat(selected.has('frozen-verify'), 'Oracle execution did not select its integrity gate');
      for (const command of registry.commandsFor('frozen-verify', context)) {
        run({...command, id: 'frozen-verify'});
      }
    }
  }
}

function options(argv) {
  const options = {execute: false, stage: 'all', commandTimeout: 1800000, budget: 7200000};
  const seen = new Set();
  for (let index = 0; index < argv.length; index++) {
    const key = argv[index];
    requireThat(!seen.has(key), `Duplicate option: ${key}`);
    seen.add(key);
    if (key === '--execute') options.execute = true;
    else if (key === '--restore-oracle-deps') options.restoreOracleDeps = true;
    else {
      const name = {'--root': 'root', '--plan': 'planFile', '--artifacts': 'artifacts',
        '--platform': 'platform', '--stage': 'stage', '--python': 'python',
        '--command-timeout-seconds': 'commandTimeout', '--budget-seconds': 'budget'}[key];
      requireThat(name && argv[index + 1] && !argv[index + 1].startsWith('--'), `Invalid option: ${key}`);
      options[name] = argv[++index];
      if (['commandTimeout', 'budget'].includes(name)) {
        options[name] = Number(options[name]) * 1000;
        requireThat(Number.isSafeInteger(options[name]) && options[name] > 0 &&
          options[name] <= 7200000, `Invalid timeout: ${key}`);
      }
    }
  }
  requireThat(['all', 'core', 'contracts'].includes(options.stage), 'Stage must be all, core or contracts');
  requireThat(options.planFile && path.isAbsolute(options.planFile), '--plan must name an absolute plan file');
  requireThat(options.artifacts && path.isAbsolute(options.artifacts), '--artifacts must be absolute');
  options.root = fs.realpathSync(options.root || path.resolve(__dirname, '..', '..'));
  options.artifacts = path.resolve(options.artifacts);
  requireThat(options.artifacts !== options.root &&
    !options.artifacts.startsWith(`${options.root}${path.sep}`), 'Artifacts must be outside the checkout');
  options.platform ||= PLATFORMS[process.platform];
  options.python ||= process.env.PYTHON || 'python';
  requireThat(options.platform === PLATFORMS[process.platform] || !options.execute,
    'Execution platform must match the actual native host');
  return options;
}

function main(argv) {
  const opts = options(argv);
  const plan = JSON.parse(fs.readFileSync(opts.planFile, 'utf8'));
  requireThat(plan.schema === 1 && Array.isArray(plan.packages) && Array.isArray(plan.tests) &&
    Array.isArray(plan.profiles) && Array.isArray(plan.checks), 'Invalid impact plan');
  const commands = coreCommands(plan, opts.platform);
  if (!opts.execute) {
    console.log(JSON.stringify({core: commands, checks: plan.checks, profiles: plan.profiles,
      scope: 'Planned commands only; no validation executed'}, null, 2));
    return;
  }
  const planner = require('./impact-plan.cjs');
  const registry = require('./validation-checks.cjs');
  requireThat(typeof planner.verifyPlan === 'function', 'Impact planner must support source-bound verification');
  planner.verifyPlan(plan, {root: opts.root});
  requireThat(!fs.existsSync(opts.artifacts), 'Execution requires a new artifact directory');
  fs.mkdirSync(opts.artifacts, {recursive: true});
  const context = {...opts, plan, results: [], deadline: Date.now() + opts.budget,
    pinnedOutput: path.join(opts.artifacts, 'pinned-python'),
    parameterRoot: path.join(opts.artifacts, 'fabric-cicd-parameter-source')};
  const beforeInventory = sourceInventory(opts.root);
  fs.writeFileSync(path.join(opts.artifacts, 'source-before.json'),
    `${JSON.stringify(beforeInventory, null, 2)}\n`);
  const before = beforeInventory.digest;
  const toolchain = {};
  for (const [name, program, args] of [
    ['cargo', 'cargo', ['--version']], ['rustc', 'rustc', ['--version', '--verbose']],
    ['rustup', 'rustup', ['show', 'active-toolchain']],
  ]) {
    const result = spawnSync(program, args, {cwd: path.join(opts.root, 'rust'),
      encoding: 'utf8', timeout: 30000, windowsHide: true});
    requireThat(!result.error && result.status === 0, `Cannot record ${name} toolchain identity`);
    toolchain[name] = result.stdout.trim();
  }
  const expectedHost = {windows: /host: .+-pc-windows-/, linux: /host: .+-unknown-linux-/,
    macos: /host: .+-apple-darwin/}[opts.platform];
  requireThat(expectedHost.test(toolchain.rustc), 'Rust compiler host does not match the execution platform');
  const receipt = {
    schema: 1, plan_fingerprint: plan.fingerprint, platform: opts.platform, stage: opts.stage,
    complete_plan: false,
    toolchain,
    build_environment: buildEnvironment(),
    source_before: before, started_at: new Date().toISOString(), status: 'running',
    scope: opts.stage !== 'all' ? `Partial ${opts.stage} stage only on this host` :
      plan.mode === 'full' ? 'Full configured offline checks on this host' :
      'Affected configured offline checks on this host; not full workspace or GA acceptance',
  };
  const receiptFile = path.join(opts.artifacts, 'receipt.json');
  const run = command => {
    requireThat(Date.now() < context.deadline, 'Validation budget exhausted');
    console.log(`${command.id || 'contract'}: ${command.program} ${JSON.stringify(command.args)}`);
    return commandResult(command, context);
  };
  fs.writeFileSync(receiptFile, `${JSON.stringify(receipt, null, 2)}\n`);
  try {
    if (opts.stage !== 'contracts') for (const command of commands) run(command);
    if (opts.stage !== 'core') runContracts(context, registry, run);
    const afterInventory = sourceInventory(opts.root);
    fs.writeFileSync(path.join(opts.artifacts, 'source-after.json'),
      `${JSON.stringify(afterInventory, null, 2)}\n`);
    receipt.source_after = afterInventory.digest;
    requireThat(before === receipt.source_after, 'Source changed during validation');
    planner.verifyPlan(plan, {root: opts.root});
    receipt.complete_plan = opts.stage === 'all';
    receipt.status = receipt.complete_plan ? 'passed' : 'partial_passed';
  } catch (error) {
    receipt.status = 'failed';
    receipt.error = error.message;
    throw error;
  } finally {
    receipt.finished_at = new Date().toISOString();
    receipt.commands = context.results.length;
    receipt.tests_passed = context.results.reduce((total, item) => total + item.tests_passed, 0);
    receipt.tests_ignored = context.results.reduce((total, item) => total + item.tests_ignored, 0);
    fs.writeFileSync(receiptFile, `${JSON.stringify(receipt, null, 2)}\n`);
  }
}

module.exports = {packageArgs, ownFeatures, defaultTests, profileTests, testCommands, coreCommands, commandResult,
  sourceDigest, sourceInventory, oracleVersions, restoreOracleDependencies, runContracts, options, buildEnvironment};
if (require.main === module) {
  try { main(process.argv.slice(2)); }
  catch (error) { console.error(error.message); process.exitCode = 1; }
}
