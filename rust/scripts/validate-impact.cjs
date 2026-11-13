// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const {spawnSync} = require('node:child_process');
const {executeOwned} = require('./validation-process.cjs');

const PLATFORMS = {win32: 'windows', linux: 'linux', darwin: 'macos'};
function requireThat(condition, message) {
  if (!condition) throw new Error(message);
}

function packageArgs(packages) {
  return [...new Set(packages)].sort().flatMap(name => ['--package', name]);
}

function ownFeatures(profile, name) {
  if (profile.test_features) {
    const features = profile.test_features[name];
    requireThat(Array.isArray(features) &&
      features.every(feature => feature.startsWith(`${name}/`)),
    `Missing or invalid package test features: ${profile.id}/${name}`);
    return [...new Set(features)].sort();
  }
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
  if (plan.mode === 'full' && platform === 'windows') {
    return commands.flatMap(command => {
      const testsWholeCli = command.program === 'cargo' && command.args[0] === 'test' &&
        command.args.includes('--all-targets') && command.args.some((arg, index) =>
          arg === '--package' && command.args[index + 1] === 'fabric-cli');
      return testsWholeCli ? [{
        id: `${command.id}:compile`, program: command.program,
        args: [...command.args, '--no-run'],
      }, command] : [command];
    });
  }
  return commands;
}

function sourceDigest(root) {
  const result = spawnSync('git', ['ls-files', '-z', '--cached', '--others', '--exclude-standard'], {
    cwd: root, encoding: 'utf8', timeout: 30000, maxBuffer: 32 * 1024 * 1024,
  });
  requireThat(!result.error && result.status === 0, 'Unable to inventory source with Git');
  const hash = crypto.createHash('sha256');
  for (const name of [...new Set(result.stdout.split('\0').filter(Boolean))].sort()) {
    const file = path.resolve(root, name);
    requireThat(file.startsWith(`${root}${path.sep}`), `Source path escaped checkout: ${name}`);
    hash.update(name);
    if (!fs.existsSync(file)) {
      hash.update('\0deleted\0');
    } else {
      requireThat(!fs.lstatSync(file).isSymbolicLink(), `Source symlink requires review: ${name}`);
      hash.update('\0');
      hash.update(fs.readFileSync(file));
    }
    hash.update('\0');
  }
  return hash.digest('hex');
}

function sourceInventory(root) {
  const list = args => {
    const result = spawnSync('git', ['ls-files', '-z', ...args], {
      cwd: root, encoding: 'utf8', timeout: 30000, maxBuffer: 32 * 1024 * 1024,
    });
    requireThat(!result.error && result.status === 0, 'Unable to inventory source with Git');
    return result.stdout.split('\0').filter(Boolean);
  };
  const tracked = new Set(list(['--cached']));
  const names = [...new Set([...tracked, ...list(['--others', '--exclude-standard'])])].sort();
  const hash = crypto.createHash('sha256');
  const files = names.map(name => {
    const file = path.resolve(root, name);
    requireThat(file.startsWith(`${root}${path.sep}`), `Source path escaped checkout: ${name}`);
    hash.update(name);
    if (!fs.existsSync(file)) {
      hash.update('\0deleted\0');
      hash.update('\0');
      return {path: name, tracked: tracked.has(name), state: 'deleted'};
    }
    const stat = fs.lstatSync(file);
    requireThat(stat.isFile() && !stat.isSymbolicLink(), `Source must be a regular file: ${name}`);
    const bytes = fs.readFileSync(file);
    hash.update('\0');
    hash.update(bytes);
    hash.update('\0');
    return {path: name, tracked: tracked.has(name), state: 'file', mode: stat.mode & 0o777,
      bytes: bytes.length, sha256: crypto.createHash('sha256').update(bytes).digest('hex')};
  });
  const digest = hash.digest('hex');
  requireThat(digest === sourceDigest(root), 'Source changed while recording the per-file inventory');
  return {schema: 1, digest, files};
}

function sourceDifferences(before, after) {
  const previous = new Map(before.files.map(file => [file.path, file]));
  const current = new Map(after.files.map(file => [file.path, file]));
  return [...new Set([...previous.keys(), ...current.keys()])].sort()
    .filter(name => JSON.stringify(previous.get(name)) !== JSON.stringify(current.get(name)))
    .map(name => ({path: name, before: previous.get(name) || null, after: current.get(name) || null}));
}

function recordSourceAfter(root, artifacts, before) {
  const after = sourceInventory(root);
  fs.writeFileSync(path.join(artifacts, 'source-after.json'), `${JSON.stringify(after, null, 2)}\n`);
  fs.writeFileSync(path.join(artifacts, 'source-differences.json'),
    `${JSON.stringify(sourceDifferences(before, after), null, 2)}\n`);
  return after.digest;
}

function buildEnvironment(environment = process.env) {
  const names = ['CARGO_BUILD_JOBS', 'RUST_TEST_THREADS', 'CARGO_PROFILE_DEV_DEBUG',
    'CARGO_PROFILE_TEST_DEBUG', 'CARGO_INCREMENTAL'];
  return Object.fromEntries(names.filter(name => environment[name] !== undefined)
    .map(name => [name, environment[name]]));
}

function commandEnvironment(command, context) {
  const {targetDir} = require('./execution-evidence.cjs').executionContext(context);
  requireThat(command.env?.CARGO_TARGET_DIR === undefined ||
    path.resolve(context.root, 'rust', command.env.CARGO_TARGET_DIR) === targetDir,
  'Command CARGO_TARGET_DIR differs from the execution metadata context');
  return {...process.env, PYTHONUTF8: '1', PYTHONDONTWRITEBYTECODE: '1',
    FAB_UPDATE_TEST_ROOT: context.artifacts, FAB_CLI_V2_TEST_ROOT: context.artifacts,
    FAB_IDENTITY_TEST_ROOT: context.artifacts, CARGO_TARGET_DIR: targetDir, ...command.env};
}

function commandIdentity(command, context) {
  const environment = commandEnvironment(command, context);
  return crypto.createHash('sha256').update(JSON.stringify({
    cwd: path.resolve(command.cwd || path.join(context.root, 'rust')),
    program: command.program, args: command.args,
    env: Object.keys(environment).sort().map(key => [key, environment[key]]),
    platform: context.platform, toolchain: context.toolchain, source: context.sourceBefore,
  })).digest('hex');
}

function commandClass(command) {
  if (command.program === 'cargo' &&
    (['build', 'check', 'clippy', 'run', 'rustc'].includes(command.args[0]) ||
      (command.args[0] === 'test' && command.args.includes('--no-run')))) return 'compilation';
  if (command.test || (command.program === 'cargo' && command.args[0] === 'test') ||
    command.args.includes('--test') || command.args.includes('unittest')) return 'test';
  return 'tool';
}

function engineeringReceipt(expected) {
  let report = expected.report;
  if (report === undefined) {
    requireThat(fs.statSync(expected.receipt).size <= 1024 * 1024,
      'Engineering preflight receipt exceeded 1 MiB');
    report = JSON.parse(fs.readFileSync(expected.receipt, 'utf8'));
  }
  const inventory = require('./engineering-inventory.json');
  const suites = report.suites;
  const skips = expected.platform === 'windows' ? inventory.windows_skips : [];
  requireThat(report.kind === 'engineering-unit-preflight' && report.head === expected.head &&
    report.platform === expected.platform && report.shard === expected.shard &&
    report.passed === true && Array.isArray(suites) && suites.length === 1 &&
    suites[0].pattern === inventory.pattern && suites[0].expected === inventory.count &&
    suites[0].discovered === inventory.count && suites[0].run === inventory.count &&
    suites[0].passed === true &&
    suites[0].failures === 0 && suites[0].errors === 0 && suites[0].expected_failures === 0 &&
    JSON.stringify(suites[0].skipped) === JSON.stringify(skips),
  'Engineering preflight did not pass its exact native inventory');
  return {tests_passed: inventory.count - skips.length, tests_ignored: skips.length};
}

function summarizeLogs(stdout, stderr) {
  const totals = {tests_passed: 0, tests_ignored: 0, summaries: 0, pythonCount: 0, skipped: 0};
  const inspect = (line, isStderr) => {
    for (const match of line.matchAll(/test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored/g)) {
      totals.tests_passed += Number(match[1]);
      totals.tests_ignored += Number(match[3]);
    }
    const summary = /^Ran (\d+) tests? in [^\r\n]+\r?$/.exec(line);
    if (summary) { totals.summaries++; totals.pythonCount += Number(summary[1]); }
    if (isStderr) totals.skipped += Number(/(?:^|\W)skipped=(\d+)/.exec(line)?.[1] || 0);
  };
  const scan = file => {
    const hash = crypto.createHash('sha256');
    const fd = fs.openSync(file, 'r');
    const buffer = Buffer.alloc(65536);
    let tail = '';
    try {
      let size;
      while ((size = fs.readSync(fd, buffer, 0, buffer.length, null)) > 0) {
        hash.update(buffer.subarray(0, size));
        const lines = (tail + buffer.toString('utf8', 0, size)).split('\n');
        tail = lines.pop().slice(-8192);
        for (const line of lines) inspect(line, file === stderr);
      }
      if (tail) inspect(tail, file === stderr);
    } finally { fs.closeSync(fd); }
    return hash.digest('hex');
  };
  const stdout_sha256 = scan(stdout);
  const stderr_sha256 = scan(stderr);
  return {...totals, stdout_sha256, stderr_sha256};
}

function commandResult(command, context, execute) {
  const started = Date.now();
  const environment = commandEnvironment(command, context);
  const descriptorDigest = require('./execution-evidence.cjs').descriptorDigest(command, context, environment);
  const budgetClass = commandClass(command);
  const classBudget = {tool: 120000, test: 300000, compilation: 600000}[budgetClass];
  const timeout = Math.max(0, Math.min(context.commandTimeout || 600000, classBudget,
    (context.deadline || Infinity) - started, (command.deadline || Infinity) - started));
  const commandId = String(command.id || 'contract');
  const safeId = commandId.replace(/[^a-zA-Z0-9._-]/g, '_');
  const label = safeId.length > 64 ?
    `${safeId.slice(0, 40)}-${crypto.createHash('sha256').update(commandId).digest('hex').slice(0, 16)}` :
    safeId;
  const prefix = `${String(context.results.length).padStart(3, '0')}-${label}`;
  const stdoutFile = path.join(context.artifacts, `${prefix}.stdout.log`);
  const stderrFile = path.join(context.artifacts, `${prefix}.stderr.log`);
  fs.writeFileSync(stdoutFile, '');
  fs.writeFileSync(stderrFile, '');
  console.log(`${new Date(started).toISOString()} START ${commandId}: ` +
    `${command.program} ${JSON.stringify(command.args)}; ${budgetClass}; ${timeout}ms budget`);
  const finish = result => {
    if (result.stdout !== undefined) fs.writeFileSync(stdoutFile, result.stdout || '');
    if (result.stderr !== undefined) fs.writeFileSync(stderrFile, result.stderr || '');
    if (command.stdout) fs.copyFileSync(stdoutFile, command.stdout);
    const totals = summarizeLogs(stdoutFile, stderrFile);
    const unittest = command.test === 'unittest';
    const timedOut = Boolean(result.timed_out || result.error?.code === 'ETIMEDOUT');
    const cancelled = Boolean(result.cancelled || result.error?.code === 'ECANCELLED');
    const noTests = unittest && result.status === 0 && !result.error && !result.signal &&
      !(totals.summaries === 1 && totals.pythonCount > totals.skipped);
    let engineering = {tests_passed: 0, tests_ignored: 0}, engineeringError;
    if (command.engineering && result.status === 0 && !result.error && !result.signal) {
      try { engineering = engineeringReceipt(command.engineering); }
      catch (error) { engineeringError = error.message; }
    }
    const oversized = !execute && (command.kind === 'prepare-oracle' || command.capture) &&
      fs.statSync(stdoutFile).size > 1024 * 1024;
    const error = result.error?.message || engineeringError || (oversized ?
      `Captured command output exceeded 1 MiB: ${commandId}` : null) ||
      (noTests ? 'No executed unittest cases' : null);
    const passed = !error && result.status === 0 && !result.signal && !timedOut && !cancelled &&
      result.cleanup_confirmed !== false;
    const record = {
      schema: 1, id: command.id, aliases: command.aliases || [command.id],
      execution_identity: command.execution_identity || commandIdentity(command, context),
      descriptor_sha256: descriptorDigest,
      program: command.program, args: command.args,
      cwd: command.cwd || path.join(context.root, 'rust'),
      started_at: new Date(started).toISOString(), finished_at: new Date().toISOString(),
      elapsed_ms: Date.now() - started, budget_class: budgetClass,
      timeout_ms: timeout, timed_out: timedOut, cancelled,
      status: passed ? 'passed' : timedOut ? 'timed_out' : cancelled ? 'cancelled' : 'failed',
      cleanup_confirmed: result.cleanup_confirmed ?? null,
      cleanup_method: result.cleanup_method || null,
      cleanup_errors: result.cleanup_errors || [], pid: result.pid || null,
      build_environment: buildEnvironment(environment),
      exit: result.status ?? null, signal: result.signal || null, error,
      stdout: stdoutFile, stderr: stderrFile,
      stdout_sha256: totals.stdout_sha256, stderr_sha256: totals.stderr_sha256,
      tests_passed: (command.engineering ? 0 : totals.tests_passed) +
        (unittest && result.status === 0 ? Math.max(0, totals.pythonCount - totals.skipped) : 0) +
        engineering.tests_passed,
      tests_ignored: (command.engineering ? 0 : totals.tests_ignored) +
        (unittest ? totals.skipped : 0) + engineering.tests_ignored,
      ...(command.engineering ? {engineering_receipt: command.engineering.receipt} : {}),
    };
    context.results.push(record);
    fs.writeFileSync(path.join(context.artifacts, 'commands.json'), `${JSON.stringify(context.results, null, 2)}\n`);
    console.log(`${record.finished_at} END ${commandId}: ${record.status}; exit ${record.exit}; ` +
      `${(record.elapsed_ms / 1000).toFixed(1)}s elapsed; logs: ${stdoutFile}, ${stderrFile}`);
    requireThat(!noTests, `Validation discovered no executed unittest cases: ${commandId}`);
    requireThat(passed, `Validation failed: ${commandId} (exit ${record.exit}; ` +
      `${error || record.signal || 'see command logs'})`);
    if (execute) return result.stdout || '';
    if (command.kind === 'prepare-oracle' || command.capture) {
      return fs.readFileSync(stdoutFile, 'utf8');
    }
    return '';
  };
  const launch = {cwd: command.cwd || path.join(context.root, 'rust'), env: environment,
    encoding: 'utf8', timeout, maxBuffer: 64 * 1024 * 1024, windowsHide: true, shell: false,
    deadline: started + timeout, stdout: stdoutFile, stderr: stderrFile,
    python: context.python, signal: context.signal};
  const exhausted = timeout <= 0 || Date.now() >= started + timeout;
  if (exhausted || context.signal?.aborted) {
    return finish({status: null, timed_out: exhausted, cancelled: Boolean(context.signal?.aborted),
      cleanup_confirmed: true, error: Object.assign(new Error(
        exhausted ? 'ETIMEDOUT: validation deadline exhausted before launch' : 'ECANCELLED'),
      {code: exhausted ? 'ETIMEDOUT' : 'ECANCELLED'})});
  }
  if (execute) {
    try { return finish(execute(command.program, command.args, launch)); }
    catch (error) {
      if (context.results.at(-1)?.stdout === stdoutFile) throw error;
      return finish({status: null, error});
    }
  }
  return executeOwned(command.program, command.args, launch).then(finish,
    error => finish({status: null, cleanup_confirmed: false, error}));
}

function after(value, continuation) {
  return value && typeof value.then === 'function' ? value.then(continuation) : continuation(value);
}

function series(items, action, index = 0) {
  for (; index < items.length; index++) {
    const result = action(items[index]);
    if (result && typeof result.then === 'function') {
      return result.then(() => series(items, action, index + 1));
    }
  }
}

// Only these audited offline suites may share runtime test coverage.
const REUSABLE_CORE_PACKAGES = new Set([
  'fabric-sdk-transport', 'fabric-identity', 'fabric-identity-windows',
  'fabric-cli-repl', 'fabric-cli-update',
]);

function reusableCoreCommand(command) {
  const args = command.args;
  if (command.program !== 'cargo' || !Array.isArray(args) ||
      Object.keys(command).some(key =>
        !['id', 'aliases', 'program', 'args', 'test', 'cwd', 'env'].includes(key))) return false;
  if (['fmt', 'check', 'clippy'].includes(args[0])) return !command.test;
  return command.test === true &&
    ['test', '--offline', '--locked', '--package'].every((arg, index) => args[index] === arg) &&
    REUSABLE_CORE_PACKAGES.has(args[4]) &&
    (args.length === 6 && args[5] === '--all-targets' ||
      args.length === 8 && args[5] === '--features' &&
      typeof args[6] === 'string' && args[7] === '--all-targets');
}

function coreReuseKey(command, context) {
  return reusableCoreCommand(command) ? commandIdentity(command, context) : null;
}

function runCoreCommands(commands, context, run) {
  const completed = new Map();
  requireThat(context.sourceBefore && context.toolchain && context.platform,
    'Core reuse requires source, toolchain and platform identity');
  return after(series(commands, command => {
    requireThat(!context.signal?.aborted, 'Validation cancelled');
    requireThat(Date.now() < context.deadline, 'Validation budget exhausted');
    const key = coreReuseKey(command, context);
    const aliases = [...new Set([command.id, ...(command.aliases || [])])];
    requireThat(aliases.length === 1 || key !== null, `Unsafe core aliases: ${command.id}`);
    const source = key === null ? undefined : completed.get(key);
    if (source) {
      source.aliases = [...new Set([...source.aliases, ...aliases])];
      fs.writeFileSync(path.join(context.artifacts, 'commands.json'),
        `${JSON.stringify(context.results, null, 2)}\n`);
      return;
    }
    const index = context.results.length;
    return after(run({...command, aliases}), () => {
      const result = context.results[index];
      requireThat(context.results.length === index + 1 && result?.status === 'passed' &&
        result.exit === 0 && !result.error && !result.signal && !result.timed_out &&
        !result.cancelled && result.cleanup_confirmed !== false,
      `No successful execution receipt: ${command.id}`);
      if (key !== null) completed.set(key, result);
    });
  }), () => {
    // The final manifest also verifies source after contracts; aliases never
    // survive a changed checkout as successful evidence.
    requireThat(sourceDigest(context.root) === context.sourceBefore, 'Source changed during core validation');
  });
}

function executionSummary(results) {
  return {
    commands: results.length, commands_executed: results.length,
    commands_reused: results.reduce((total, item) => total + (item.aliases?.length || 1) - 1, 0),
    tests_passed: results.reduce((total, item) => total + item.tests_passed, 0),
    tests_ignored: results.reduce((total, item) => total + item.tests_ignored, 0),
  };
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
  return series(probes, probe => {
    const versions = oracleVersions(path.join(context.root, 'rust'), probe.requirement);
    const script = `import ${probe.module}; import importlib.metadata as m, json; ` +
      `expected = json.loads(${JSON.stringify(JSON.stringify(versions))}); ` +
      'bad = {name: m.version(name) for name, version in expected.items() if m.version(name) != version}; ' +
      'print(json.dumps(bad)); raise SystemExit(42 if bad else 0)';
    const result = probeCommand(context.python, ['-c', script], {
      cwd: path.join(context.root, 'rust'), encoding: 'utf8', timeout: 30000,
    });
    if (!result.error && result.status === 0) return;
    requireThat(result.status === 42 || (result.status !== null && result.stderr?.includes('ModuleNotFoundError')),
      `Oracle dependency probe failed: ${result.error?.message || result.stderr}`);
    requireThat(context.restoreOracleDeps,
      `Missing oracle dependencies or unsupported versions (${result.stdout?.trim() || 'module unavailable'}); ` +
      `restore ${probe.requirement} or use --restore-oracle-deps`);
    return run({id: 'oracle:restore', program: context.python,
      args: ['-m', 'pip', 'install', '--disable-pip-version-check', '-r', probe.requirement]});
  });
}

function runContracts(context, registry, run, probeCommand = spawnSync) {
  const graph = JSON.parse(fs.readFileSync(path.join(context.root, 'rust', 'validation', 'impact-graph.json'), 'utf8'));
  registry.validateRegistry(graph);
  const checks = context.checks || context.plan.checks;
  const selected = new Set(checks);
  const visited = new Set();
  context.completedChecks ||= [];
  let prepared = false;
  const execute = id => {
    if (visited.has(id) || id === 'frozen-verify') return;
    const check = graph.checks.find(item => item.id === id);
    requireThat(check, `Unknown selected contract: ${id}`);
    if (check.platforms && !check.platforms.includes(context.platform)) return;
    return after(series(check.depends_on || [], execute), () => {
      visited.add(id);
      const deadline = Math.min(context.deadline || Infinity,
        Date.now() + (registry.CHECK_BUDGETS?.[id] || Infinity));
      const checkRun = command => {
        requireThat(Date.now() < deadline, `Validation contract budget exhausted: ${id}`);
        return run({...command, deadline});
      };
      const commands = registry.commandsFor(id, context);
      const completed = {id, descriptors: [], profiles: []};
      if (!commands.length) {
        const profiles = context.plan.profiles.filter(profile => profile.checks?.includes(id) &&
          profile.platforms.includes(context.platform) &&
          profileTests(context.plan, profile).length > 0);
        requireThat(profiles.length > 0,
        `No execution profile with test targets satisfies ${id}`);
        completed.profiles = profiles.map(profile => profile.id);
      }
      const recordCommand = command => {
        const descriptor = require('./execution-evidence.cjs').descriptorDigest(command, context);
        return after(checkRun(command), output => {
          completed.descriptors.push({kind: command.kind || 'command',
            command: context.results?.length - 1, descriptor_sha256: descriptor});
          return output;
        });
      };
      return after(series(commands, original => {
        const command = {...original, id: original.id || id};
        if (command.kind === 'prepare-oracle') {
          const descriptor = require('./execution-evidence.cjs').descriptorDigest(command, context);
          return after(restoreOracleDependencies(context, checkRun, probeCommand), () =>
            after(checkRun({...command, id: 'oracle:prepare'}), output => {
              completed.descriptors.push({kind: command.kind, command: context.results?.length - 1,
                descriptor_sha256: descriptor});
              context.pinnedRust = JSON.parse(output).rust_root;
              requireThat(context.pinnedRust && fs.existsSync(context.pinnedRust),
                'Oracle preparation returned no source');
              prepared = true;
            }));
        } else if (command.kind === 'transport-graphs') {
          const descriptor = require('./execution-evidence.cjs').descriptorDigest(command, context);
          const graphs = Object.fromEntries(command.graphs.defaults.map(graph =>
            [graph.package, fs.readFileSync(graph.path, 'utf8')]));
          const optin = fs.readFileSync(command.graphs.optin, 'utf8');
          registry.assertTransportGraphs(graphs, optin);
          completed.descriptors.push({kind: command.kind, passed: true,
            inputs: completed.descriptors.map(descriptor => descriptor.command),
            descriptor_sha256: descriptor});
        } else {
          return recordCommand(command);
        }
      }), () => { context.completedChecks.push(completed); });
    });
  };
  const frozen = () => {
    if (prepared) {
      requireThat(selected.has('frozen-verify'), 'Oracle execution did not select its integrity gate');
      const completed = {id: 'frozen-verify', descriptors: [], profiles: []};
      return after(series(registry.commandsFor('frozen-verify', context), command => {
        const descriptor = require('./execution-evidence.cjs').descriptorDigest(command, context);
        return after(run({...command, id: 'frozen-verify'}), () => {
          completed.descriptors.push({kind: command.kind || 'command',
            command: context.results?.length - 1, descriptor_sha256: descriptor});
        });
      }), () => { context.completedChecks.push(completed); });
    }
  };
  const fail = error => {
    const rethrow = () => { throw error; };
    try {
      const cleanup = frozen();
      return cleanup?.then ? cleanup.then(rethrow, cleanupError => {
        throw new Error(`${error.message}; frozen integrity gate also failed: ${cleanupError.message}`);
      }) : rethrow();
    } catch (cleanupError) {
      if (cleanupError === error) throw error;
      throw new Error(`${error.message}; frozen integrity gate also failed: ${cleanupError.message}`);
    }
  };
  let result;
  try { result = series(checks, execute); }
  catch (error) { return fail(error); }
  if (result?.then) {
    return result.then(frozen, fail);
  }
  return frozen();
}

function options(argv) {
  const options = {execute: false, stage: 'all', commandTimeout: 600000, budget: 7200000};
  const limits = {commandTimeout: 7200000, budget: 8100000};
  const seen = new Set();
  for (let index = 0; index < argv.length; index++) {
    const key = argv[index];
    requireThat(!seen.has(key), `Duplicate option: ${key}`);
    seen.add(key);
    if (key === '--execute') options.execute = true;
    else if (key === '--restore-oracle-deps') options.restoreOracleDeps = true;
    else {
      const name = {'--root': 'root', '--plan': 'planFile', '--artifacts': 'artifacts',
        '--platform': 'platform', '--stage': 'stage', '--python': 'python', '--shard': 'shard',
        '--command-timeout-seconds': 'commandTimeout', '--budget-seconds': 'budget'}[key];
      requireThat(name && argv[index + 1] && !argv[index + 1].startsWith('--'), `Invalid option: ${key}`);
      options[name] = argv[++index];
      if (['commandTimeout', 'budget'].includes(name)) {
        options[name] = Number(options[name]) * 1000;
        requireThat(Number.isSafeInteger(options[name]) && options[name] > 0 &&
          options[name] <= limits[name], `Invalid timeout: ${key}`);
      }
    }
  }
  requireThat(['all', 'core', 'contracts'].includes(options.stage), 'Stage must be all, core or contracts');
  requireThat(!options.shard || options.stage === 'all', '--shard cannot be combined with a partial stage');
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

async function main(argv, execute) {
  const opts = options(argv);
  const plan = JSON.parse(fs.readFileSync(opts.planFile, 'utf8'));
  requireThat(plan.schema === 1 && Array.isArray(plan.packages) && Array.isArray(plan.tests) &&
    Array.isArray(plan.profiles) && Array.isArray(plan.checks), 'Invalid impact plan');
  let commands = coreCommands(plan, opts.platform);
  const shard = opts.shard ?
    require('./impact-shards.cjs').selectShard(plan, opts.platform, opts.shard, commands) : null;
  if (shard) commands = shard.commands;
  const checks = shard?.checks || plan.checks;
  if (!opts.execute) {
    console.log(JSON.stringify({core: commands, checks, profiles: plan.profiles,
      ...(shard ? {shard: shard.id, obligations: shard.obligations,
        selection_digest: shard.selection_digest} : {}),
      scope: 'Planned commands only; no validation executed'}, null, 2));
    return;
  }
  const planner = require('./impact-plan.cjs');
  const registry = require('./validation-checks.cjs');
  requireThat(typeof planner.verifyPlan === 'function', 'Impact planner must support source-bound verification');
  const verifiedBefore = planner.verifyPlan(plan, {root: opts.root});
  requireThat(!fs.existsSync(opts.artifacts), 'Execution requires a new artifact directory');
  fs.mkdirSync(opts.artifacts, {recursive: true});
  const controller = new AbortController();
  const cancel = () => controller.abort();
  const started = Date.now();
  const context = {...opts, plan, checks, results: [], deadline: started + opts.budget,
    signal: controller.signal,
    pinnedOutput: path.join(opts.artifacts, 'pinned-python'),
    parameterRoot: path.join(opts.artifacts, 'fabric-cicd-parameter-source')};
  const nativeContext = require('./execution-evidence.cjs').executionContext(context);
  context.targetDir = nativeContext.targetDir;
  context.targetSource = nativeContext.targetSource;
  const before = sourceDigest(opts.root);
  const inventoryBefore = sourceInventory(opts.root);
  requireThat(before === inventoryBefore.digest, 'Source changed before validation');
  fs.writeFileSync(path.join(opts.artifacts, 'source-before.json'), `${JSON.stringify(inventoryBefore, null, 2)}\n`);
  const toolchain = {};
  const receipt = {
    schema: 1, plan_fingerprint: plan.fingerprint, head: plan.head ?? null,
    platform: opts.platform, stage: opts.stage,
    shard: shard?.id || null,
    selection_digest: shard?.selection_digest || require('./impact-ci.cjs').selectionDigest(plan),
    obligations: shard?.obligations || [
      ...(opts.stage === 'contracts' ? [] : commands.map(command => `${opts.platform}:core:${command.id}`)),
      ...(opts.stage === 'core' ? [] : checks.map(id => `${opts.platform}:check:${id}`)),
    ].sort(),
    complete_plan: false,
    budget_ms: opts.budget, command_timeout_ms: opts.commandTimeout,
    toolchain,
    build_environment: buildEnvironment(),
    source_before: before, started_at: new Date(started).toISOString(), status: 'running',
    scope: opts.stage !== 'all' ? `Partial ${opts.stage} stage only on this host` :
      plan.mode === 'full' ? 'Full configured offline checks on this host' :
      'Affected configured offline checks on this host; not full workspace or GA acceptance',
  };
  context.sourceBefore = before;
  context.toolchain = toolchain;
  const receiptFile = path.join(opts.artifacts, 'receipt.json');
  const run = command => commandResult(command, context, execute);
  fs.writeFileSync(receiptFile, `${JSON.stringify(receipt, null, 2)}\n`);
  process.on('SIGINT', cancel);
  process.on('SIGTERM', cancel);
  let failure;
  try {
    for (const [name, program, args] of [
      ['cargo', 'cargo', ['--version']], ['rustc', 'rustc', ['--version', '--verbose']],
      ['rustup', 'rustup', ['show', 'active-toolchain']],
    ]) {
      toolchain[name] = (await run({id: `toolchain:${name}`, program, args, capture: true})).trim();
    }
    const expectedHost = {windows: /host: .+-pc-windows-/, linux: /host: .+-unknown-linux-/,
      macos: /host: .+-apple-darwin/}[opts.platform];
    requireThat(expectedHost.test(toolchain.rustc), 'Rust compiler host does not match the execution platform');
    if (shard?.engineering) {
      requireThat(/^[a-f0-9]{40}$/.test(plan.head), 'Engineering preflight requires the exact source head');
      const artifacts = path.join(path.dirname(opts.artifacts), 'rust-engineering-unit');
      await run({id: 'preflight:engineering-unit', program: opts.python, test: 'engineering',
        cwd: opts.artifacts,
        env: {TMPDIR: opts.artifacts, TEMP: opts.artifacts, TMP: opts.artifacts},
        args: ['-I', '-B', path.join(opts.root, 'rust', 'scripts', 'engineering_preflight.py'),
          '--source', path.join(opts.root, 'rust', 'scripts'), '--artifacts', artifacts,
          '--head', plan.head, '--platform', opts.platform, '--shard', shard.id],
        deadline: Math.min(context.deadline, Date.now() + 120000),
        engineering: {head: plan.head, platform: opts.platform, shard: shard.id,
          receipt: path.join(artifacts, 'engineering-receipt.json')}});
    }
    if (opts.stage !== 'contracts') await runCoreCommands(commands, context, run);
    if (opts.stage !== 'core') await runContracts(context, registry, run);
    requireThat(!controller.signal.aborted, 'Validation cancelled');
    requireThat(Date.now() < context.deadline, 'Validation budget exhausted');
  } catch (error) {
    receipt.status = 'failed';
    receipt.error = error.message;
    failure = error;
  } finally {
    try {
      receipt.source_after = recordSourceAfter(opts.root, opts.artifacts, inventoryBefore);
      requireThat(before === receipt.source_after, 'Source changed during validation');
      if (!failure) {
        const verifiedAfter = planner.verifyPlan(plan, {root: opts.root});
        const inventoryAfter = JSON.parse(fs.readFileSync(
          path.join(opts.artifacts, 'source-after.json'), 'utf8'));
        receipt.provenance = require('./execution-evidence.cjs').provenance(
          plan, inventoryBefore, inventoryAfter, verifiedBefore, verifiedAfter);
        await new Promise(resolve => setImmediate(resolve));
        requireThat(!controller.signal.aborted, 'Validation cancelled');
        requireThat(Date.now() < context.deadline, 'Validation budget exhausted');
        receipt.complete_plan = !shard && opts.stage === 'all';
        receipt.status = opts.stage === 'all' ? 'passed' : 'partial_passed';
      }
    } catch (error) {
      receipt.source_integrity_error = error.message;
      receipt.status = 'failed';
      if (!failure) {
        receipt.error = error.message;
        failure = error;
      }
    }
    receipt.finished_at = new Date().toISOString();
    receipt.elapsed_ms = Date.now() - started;
    receipt.budget_ms = opts.budget;
    receipt.timed_out = context.results.some(item => item.timed_out) || Date.now() >= context.deadline;
    receipt.cancelled = controller.signal.aborted || context.results.some(item => item.cancelled);
    Object.assign(receipt, executionSummary(context.results));
    try {
      const evidence = require('./execution-evidence.cjs');
      const manifest = evidence.createManifest(context, receipt);
      receipt.execution_manifest = {sha256: evidence.digest(manifest), data: manifest};
      requireThat(Buffer.byteLength(JSON.stringify(receipt)) + 1 <= evidence.MAX_RECEIPT_BYTES,
        'Execution receipt exceeded 512 KiB');
    } catch (error) {
      delete receipt.execution_manifest;
      receipt.status = 'failed';
      receipt.error ||= error.message;
      failure ||= error;
    }
    try { fs.writeFileSync(receiptFile, `${JSON.stringify(receipt)}\n`); }
    finally {
      process.removeListener('SIGINT', cancel);
      process.removeListener('SIGTERM', cancel);
    }
  }
  if (failure) throw failure;
}

module.exports = {packageArgs, ownFeatures, defaultTests, profileTests, testCommands, coreCommands, commandResult,
  sourceDigest, sourceInventory, sourceDifferences, recordSourceAfter, oracleVersions,
  restoreOracleDependencies, runContracts, options, buildEnvironment,
  commandIdentity, reusableCoreCommand, coreReuseKey, runCoreCommands, executionSummary,
  commandClass, engineeringReceipt, summarizeLogs, main};
if (require.main === module) {
  main(process.argv.slice(2)).catch(error => { console.error(error.message); process.exitCode = 1; });
}
