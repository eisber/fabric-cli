// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const {spawnSync} = require('node:child_process');
const {sourceDigest} = require('./validate-impact.cjs');

const VERSION = '0.9.1';
const METRICS = ['lines', 'functions', 'regions'];
const IGNORE = String.raw`[/\\](tests|examples|target)[/\\]`;
const SHA = value => crypto.createHash('sha256').update(value).digest('hex');
function requireThat(value, message) {
  if (!value) throw new Error(message);
}
function normalized(file) {
  return file.replace(/^\\\\\?\\/, '').replaceAll('\\', '/');
}
function contained(root, file) {
  const relative = path.relative(root, file);
  return !!relative && relative !== '..' && !relative.startsWith(`..${path.sep}`) &&
    !path.isAbsolute(relative);
}
function options(argv) {
  const opts = {scope: 'full', timeout: 7200, execute: false};
  const seen = new Set();
  for (let index = 0; index < argv.length; index++) {
    const key = argv[index];
    requireThat(!seen.has(key), `Duplicate option: ${key}`);
    seen.add(key);
    if (key === '--execute') opts.execute = true;
    else {
      const property = {'--root': 'root', '--output': 'output', '--tool': 'tool',
        '--scope': 'scope', '--packages': 'packages', '--timeout-seconds': 'timeout'}[key];
      requireThat(property && argv[index + 1] && !argv[index + 1].startsWith('--'),
        `Invalid option: ${key}`);
      opts[property] = argv[++index];
    }
  }
  opts.root = fs.realpathSync(opts.root || path.resolve(__dirname, '..', '..'));
  requireThat(opts.output && path.isAbsolute(opts.output), '--output must be an absolute artifact directory');
  opts.output = path.resolve(opts.output);
  requireThat(!contained(opts.root, opts.output) && opts.output !== opts.root,
    'Coverage artifacts must be outside the checkout');
  requireThat(['full', 'packages'].includes(opts.scope), '--scope must be full or packages');
  opts.packages = opts.packages ? opts.packages.split(',').sort() : [];
  requireThat(opts.packages.every(name => /^[a-z0-9][a-z0-9_-]*$/.test(name)) &&
    new Set(opts.packages).size === opts.packages.length, 'Invalid or duplicate package names');
  requireThat((opts.scope === 'packages') === (opts.packages.length > 0),
    'Package scope requires --packages; full scope forbids a narrowed package list');
  opts.timeout = Number(opts.timeout);
  requireThat(Number.isSafeInteger(opts.timeout) && opts.timeout >= 1 && opts.timeout <= 14400,
    'Coverage timeout must be 1-14400 seconds');
  if (opts.tool) {
    requireThat(path.isAbsolute(opts.tool), '--tool must name an absolute cargo-llvm-cov executable');
    opts.tool = fs.realpathSync(opts.tool);
  } else opts.tool = 'cargo';
  return opts;
}

function commands(opts) {
  const invoke = args => opts.tool === 'cargo'
    ? {program: 'cargo', args: ['llvm-cov', ...args]}
    : {program: opts.tool, args: ['llvm-cov', ...args]};
  const packages = opts.packages.flatMap(name => ['--package', name]);
  const selected = opts.scope === 'full' ? ['--workspace'] : packages;
  const report = ['report', '--offline', '--locked', ...packages, '--ignore-filename-regex', IGNORE];
  return [
    {id: 'tests', ...invoke(['--offline', '--locked', ...selected, '--all-features',
      '--all-targets', '--no-fail-fast', '--no-clean', '--no-report', '--',
      '--test-threads=1'])},
    {id: 'json', ...invoke([...report, '--json', '--summary-only', '--output-path',
      path.join(opts.output, 'llvm-summary.json')])},
    {id: 'lcov', ...invoke([...report, '--lcov', '--output-path', path.join(opts.output, 'lcov.info')])},
    {id: 'html', ...invoke([...report, '--html', '--output-dir', path.join(opts.output, 'html')])},
  ];
}

function summarize(report, metadata, root, scope) {
  requireThat(report.type === 'llvm.coverage.json.export' && Array.isArray(report.data) &&
    report.data.length === 1 && Array.isArray(report.data[0].files), 'Unsupported LLVM coverage JSON');
  const members = new Set(metadata.workspace_members);
  const packages = metadata.packages.filter(pkg => members.has(pkg.id)).map(pkg => ({
    name: pkg.name, directory: normalized(path.dirname(pkg.manifest_path)) + '/',
  })).sort((a, b) => b.directory.length - a.directory.length);
  requireThat(packages.length > 0, 'No workspace packages in coverage metadata');
  const aggregate = () => Object.fromEntries(METRICS.map(metric => [metric, {count: 0, covered: 0}]));
  const totals = aggregate(), byPackage = new Map(), files = [];
  const ignore = new RegExp(IGNORE);
  const rustRoot = normalized(path.join(root, 'rust')) + '/';
  const seen = new Set();
  for (const file of report.data[0].files) {
    const filename = normalized(file.filename);
    requireThat(filename.startsWith(rustRoot) && !ignore.test(filename),
      `Unexpected file in filtered coverage: ${filename}`);
    requireThat(!seen.has(filename), `Duplicate coverage file: ${filename}`);
    seen.add(filename);
    const pkg = packages.find(pkg => filename.startsWith(pkg.directory));
    requireThat(pkg, `Coverage file has no Cargo owner: ${filename}`);
    const metrics = aggregate();
    for (const metric of METRICS) {
      const data = file.summary?.[metric];
      requireThat(data && Number.isSafeInteger(data.count) && Number.isSafeInteger(data.covered) &&
        data.count >= 0 && data.covered >= 0 && data.covered <= data.count,
      `Invalid ${metric} coverage counts: ${filename}`);
      metrics[metric] = {count: data.count, covered: data.covered};
      totals[metric].count += data.count;
      totals[metric].covered += data.covered;
    }
    if (!byPackage.has(pkg.name)) byPackage.set(pkg.name, aggregate());
    const current = byPackage.get(pkg.name);
    for (const metric of METRICS) {
      current[metric].count += metrics[metric].count;
      current[metric].covered += metrics[metric].covered;
    }
    files.push({path: filename.slice(normalized(root).length + 1), package: pkg.name, ...metrics});
  }
  requireThat(files.length > 0 && totals.lines.count > 0, 'Coverage contains no measured production-path lines');
  const percentage = metrics => Object.fromEntries(METRICS.map(metric => [metric, {
    ...metrics[metric], percent: metrics[metric].count
      ? Number((100 * metrics[metric].covered / metrics[metric].count).toFixed(2)) : null,
  }]));
  return {
    schema: 1, scope,
    denominator: 'Compiled workspace files under production paths; inline unit-test code is included',
    excluded_paths: ['tests', 'examples', 'target', 'external dependencies'],
    platform_exclusions: 'Code eliminated by target cfg and other uncompiled sources is not in this denominator',
    branch_coverage: {measured: false, reason: 'The pinned stable toolchain does not support nightly-only branch instrumentation'},
    doctest_coverage: {measured: false, reason: 'Doctest coverage requires nightly; ordinary documentation tests remain a separate validation gate'},
    totals: percentage(totals),
    packages: [...byPackage].sort(([a], [b]) => a.localeCompare(b))
      .map(([name, metrics]) => ({name, ...percentage(metrics)})),
    files: files.sort((a, b) => a.path.localeCompare(b.path)),
    unmeasured_packages: packages.filter(pkg => !byPackage.has(pkg.name)).map(pkg => pkg.name).sort(),
  };
}

function markdown(summary) {
  const format = data => data.percent === null ? 'not measured' :
    `${data.covered}/${data.count} (${data.percent.toFixed(2)}%)`;
  return [
    '# Rust code coverage', '',
    `Scope: **${summary.scope}**. ${summary.denominator}.`,
    `Tests: **${summary.tests_status}**. Platform: \`${summary.host}\`. Git base: \`${summary.head}\`.`,
    `Exact source inventory: \`${summary.source_inventory_sha256 || 'not supplied'}\`. Dirty checkout: **${summary.dirty === true ? 'yes' : summary.dirty === false ? 'no' : 'unknown'}**.`, '',
    '| Package | Lines | Functions | Regions |',
    '| --- | ---: | ---: | ---: |',
    ...[{name: '**Total**', ...summary.totals}, ...summary.packages].map(pkg =>
      `| ${pkg.name} | ${format(pkg.lines)} | ${format(pkg.functions)} | ${format(pkg.regions)} |`),
    '', 'Branch coverage and instrumented doctests are **not measured** on the pinned stable toolchain.',
    'Inline unit-test code in production source files is included; standalone tests/examples and external dependencies are excluded.',
    'Target-excluded code is unmeasured. Test counts are not percentages; an affected/package report is not full-workspace coverage.',
    'A report from failing tests is partial diagnostic evidence and does not satisfy the validation gate.',
    ...(summary.unmeasured_packages.length ? ['', `Unmeasured packages: ${summary.unmeasured_packages.join(', ')}.`] : []),
    '',
  ].join('\n');
}

function main(argv) {
  const opts = options(argv);
  if (!opts.execute) {
    console.log(JSON.stringify({scope: opts.scope, commands: commands(opts),
      note: 'Plan only; no coverage measured'}, null, 2));
    return;
  }
  requireThat(!fs.existsSync(opts.output), 'Coverage requires a fresh artifact directory');
  fs.mkdirSync(opts.output, {recursive: true});
  const cwd = path.join(opts.root, 'rust');
  const deadline = Date.now() + opts.timeout * 1000;
  const target = path.join(opts.output, 'target');
  const env = {...process.env, CARGO_LLVM_COV_TARGET_DIR: target, CARGO_TARGET_DIR: target,
    CARGO_BUILD_JOBS: '2', RUST_TEST_THREADS: '1', CARGO_INCREMENTAL: '0',
    FAB_IDENTITY_TEST_ROOT: opts.output, FAB_UPDATE_TEST_ROOT: opts.output};
  const result = {
    schema: 1, scope: opts.scope, packages: opts.packages, status: 'running',
    started_at: new Date().toISOString(), source_before: sourceDigest(opts.root),
    commands: [], tool_version: VERSION, tests_status: 'not_run',
  };
  const save = () => fs.writeFileSync(path.join(opts.output, 'receipt.json'),
    JSON.stringify(result, null, 2) + '\n');
  const run = (id, program, args) => {
    requireThat(Date.now() < deadline, 'Coverage budget exhausted');
    const child = spawnSync(program, args, {cwd, env, shell: false, windowsHide: true,
      encoding: 'utf8', timeout: Math.max(1, deadline - Date.now()), maxBuffer: 64 * 1024 * 1024});
    const out = child.stdout || '', err = child.stderr || '';
    const stdout = path.join(opts.output, `${id}.stdout.log`);
    const stderr = path.join(opts.output, `${id}.stderr.log`);
    fs.writeFileSync(stdout, out);
    fs.writeFileSync(stderr, err);
    result.commands.push({id, program, args, exit: child.status, signal: child.signal,
      error: child.error?.message, stdout, stderr, stdout_sha256: SHA(out), stderr_sha256: SHA(err)});
    save();
    requireThat(!child.error && !child.signal, `Coverage command could not complete: ${id}`);
    return {exit: child.status, stdout: out, stderr: err};
  };
  save();
  try {
    const cargo = run('cargo', 'cargo', ['--version']);
    const rustc = run('rustc', 'rustc', ['--version', '--verbose']);
    const meta = run('metadata', 'cargo', ['metadata', '--offline', '--locked', '--no-deps', '--format-version', '1']);
    requireThat(cargo.exit === 0 && rustc.exit === 0 && meta.exit === 0, 'Cannot record coverage toolchain/metadata');
    result.host = /^host: (.+)$/m.exec(rustc.stdout)?.[1];
    requireThat(result.host, 'No native rustc host recorded');
    result.cargo = cargo.stdout.trim();
    result.rustc = rustc.stdout.trim();
    const metadata = JSON.parse(meta.stdout);
    const names = new Set(metadata.packages.filter(pkg => metadata.workspace_members.includes(pkg.id)).map(pkg => pkg.name));
    requireThat(opts.packages.every(name => names.has(name)), 'Unknown coverage package');
    const identity = run('source', 'git', ['rev-parse', 'HEAD']);
    requireThat(identity.exit === 0, 'Cannot record coverage source');
    result.head = identity.stdout.trim();
    const status = run('working-tree', 'git', ['status', '--porcelain=v1', '--untracked-files=all']);
    requireThat(status.exit === 0, 'Cannot record coverage working-tree identity');
    result.dirty = status.stdout.length > 0;
    const versionCommand = opts.tool === 'cargo'
      ? ['cargo', ['llvm-cov', '--version']] : [opts.tool, ['llvm-cov', '--version']];
    const version = run('version', ...versionCommand);
    requireThat(version.exit === 0 && version.stdout.trim() === `cargo-llvm-cov ${VERSION}`,
      `Install the pinned cargo-llvm-cov ${VERSION} and the pinned toolchain's llvm-tools-preview component`);
    for (const command of commands(opts)) {
      console.log(`${command.id}: ${command.program} ${JSON.stringify(command.args)}`);
      const executed = run(command.id, command.program, command.args);
      if (command.id === 'tests') {
        result.tests_status = executed.exit === 0 ? 'passed' : 'failed';
        const matches = [...executed.stdout.matchAll(/test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored/g)];
        result.tests_passed = matches.reduce((total, match) => total + Number(match[1]), 0);
        result.tests_ignored = matches.reduce((total, match) => total + Number(match[3]), 0);
        requireThat(result.tests_status !== 'passed' || result.tests_passed > 0,
          'Coverage test execution produced no passing registered tests');
      } else requireThat(executed.exit === 0, `Coverage report failed: ${command.id}`);
    }
    const summary = {...summarize(
      JSON.parse(fs.readFileSync(path.join(opts.output, 'llvm-summary.json'), 'utf8')),
      metadata, opts.root, opts.scope,
    ), head: result.head, host: result.host, tests_status: result.tests_status,
    source_inventory_sha256: result.source_before, dirty: result.dirty};
    fs.writeFileSync(path.join(opts.output, 'summary.json'), JSON.stringify(summary, null, 2) + '\n');
    fs.writeFileSync(path.join(opts.output, 'summary.md'), markdown(summary));
    result.source_after = sourceDigest(opts.root);
    requireThat(result.source_before === result.source_after, 'Source changed during coverage measurement');
    const finalHead = run('final-source', 'git', ['rev-parse', 'HEAD']);
    requireThat(finalHead.exit === 0 && finalHead.stdout.trim() === result.head,
      'Git base changed during coverage measurement');
    result.summary_sha256 = SHA(fs.readFileSync(path.join(opts.output, 'summary.json')));
    result.status = result.tests_status === 'passed' ? 'passed' : 'failed_tests_partial_report';
    requireThat(result.tests_status === 'passed', 'Coverage retained a partial report, but tests failed');
  } catch (error) {
    result.status = result.status === 'failed_tests_partial_report' ? result.status : 'failed';
    result.error = error.message;
    throw error;
  } finally {
    result.finished_at = new Date().toISOString();
    save();
  }
}

module.exports = {VERSION, IGNORE, options, commands, summarize, markdown};
if (require.main === module) {
  try { main(process.argv.slice(2)); }
  catch (error) { console.error(error.message); process.exitCode = 1; }
}
