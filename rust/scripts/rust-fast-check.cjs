// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const {execFileSync, spawnSync} = require('node:child_process');

const REPO = 'eisber/fabric-cli';
const LANES = Object.freeze({
  baseline: [],
  'cli-jobs': ['fabric-cli', 'fabric-cli-jobs', 'fabric-sdk-jobs', 'fabric-sdk-paging'],
  endpoint: ['fabric-core', 'fabric-sdk-http', 'fabric-sdk-items'],
  'native-values': ['fabric-core-json', 'fabric-cli-output', 'fabric-deploy'],
  'sdk-values': ['fabric-core', 'fabric-sdk', 'fabric-sdk-http', 'fabric-core-json', 'fabric-cli-output'],
  'cli-auth': ['fabric-cli', 'fabric-identity'],
});
const CI_FILES = [
  '.github/workflows/rust-fast-check.yml', '.github/rust-fast-candidate.json',
  'rust/scripts/rust-fast-check.cjs', 'rust/scripts/rust-fast-check.test.cjs',
  'rust/scripts/rust-fast-requirements.txt',
  'rust/docs/fast-check.md', 'rust/docs/build-validation.md',
  'rust/tests/performance/startup.py', 'rust/tests/performance/test_startup.py',
  'rust/tests/performance/measurement-request.json', 'rust/docs/performance-baseline.md',
];
const root = path.resolve(__dirname, '../..');
const rust = path.join(root, 'rust');
const receipts = process.env.FAST_RECEIPTS;
const sha = value => /^[a-f0-9]{40}$/.test(value || '') && !/^0+$/.test(value);
const digest = value => crypto.createHash('sha256').update(value).digest('hex');
function requireThat(condition, message) {
  if (!condition) throw new Error(message);
}
function lane(value) {
  requireThat(Object.hasOwn(LANES, value), `Unknown lane: ${value}`);
  return value;
}
function command(program, args, cwd = root) {
  return execFileSync(program, args, {cwd, encoding: 'utf8', timeout: 120000}).trim();
}
const git = args => command('git', args);
function write(file, value) {
  fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`, {flag: 'w'});
}
function ciDelta(files) {
  requireThat(files.every(file => CI_FILES.includes(file)),
    `Non-CI delta from original product source: ${files.filter(file => !CI_FILES.includes(file)).join(', ')}`);
}
function select(metadata, selectedLane) {
  lane(selectedLane);
  const defaults = new Set(metadata.workspace_default_members);
  const packages = metadata.packages.filter(pkg => defaults.has(pkg.id) &&
    (selectedLane !== 'sdk-values' || pkg.name === 'fabric-cli-output' ||
      ['core', 'sdk'].includes(pkg.metadata?.fabric?.layer)));
  requireThat(packages.length > 0, 'No default workspace members');
  const names = new Set(packages.map(pkg => pkg.name));
  const selected = new Set(selectedLane === 'baseline' ? names : LANES[selectedLane]);
  for (const name of selected) requireThat(names.has(name), `Missing lane package: ${name}`);
  let changed;
  do {
    changed = false;
    for (const pkg of packages) {
      if (!selected.has(pkg.name) && pkg.dependencies.some(dep => selected.has(dep.name))) {
        selected.add(pkg.name);
        changed = true;
      }
    }
  } while (changed);
  return [...selected].sort();
}
function prepare() {
  requireThat(receipts && path.isAbsolute(receipts), 'FAST_RECEIPTS must be an absolute private directory');
  fs.mkdirSync(receipts, {recursive: true});
  const candidate = JSON.parse(fs.readFileSync(path.join(root, '.github/rust-fast-candidate.json')));
  requireThat(sha(candidate.source), 'Candidate must name a full immutable product SHA');
  const head = git(['rev-parse', 'HEAD']);
  requireThat(!process.env.GITHUB_SHA || head === process.env.GITHUB_SHA, 'Checkout differs from github.sha');
  git(['merge-base', '--is-ancestor', candidate.source, head]);
  ciDelta(git(['diff', '--name-only', candidate.source, head]).split('\n').filter(Boolean));
  const metadata = JSON.parse(command('cargo',
    ['metadata', '--offline', '--locked', '--no-deps', '--format-version', '1'], rust));
  const selectedLane = lane(process.env.INPUT_LANE || candidate.lane);
  const packages = select(metadata, selectedLane);
  const rustc = command('rustc', ['--version', '--verbose'], rust);
  const toolchain = command('rustup', ['show', 'active-toolchain'], rust);
  const target = rustc.match(/^host: (.+)$/m)?.[1];
  requireThat(target === 'x86_64-unknown-linux-gnu', 'Fast preflight requires the native Linux runner');
  const lockHash = digest(fs.readFileSync(path.join(rust, 'Cargo.lock')));
  const features = {default: true, packages, family: selectedLane,
    nativeRead: packages.includes('fabric-cli') && selectedLane !== 'cli-auth',
    windowsBroker: selectedLane === 'cli-auth',
    transport: selectedLane === 'sdk-values' && packages.includes('fabric-sdk-transport')
      ? ['hyper-rustls'] : []};
  const fingerprint = digest(JSON.stringify({rustc, target, features, lockHash}));
  const coverage = selectedLane === 'cli-auth'
    ? 'Linux default CLI and opt-in auth parser/synthetic command boundary only; real broker, native Windows adapter/STA tests, root mounts and live acceptance deferred'
    : selectedLane === 'sdk-values'
      ? 'Core/SDK transitive consumers and CLI output only; central CLI, identity, deployment and native platform gates deferred'
      : 'Selected default members and transitive consumers; complete native platform gates deferred';
  const manifest = {source: candidate.source, head, lane: selectedLane, packages, coverage,
    target, rustc, toolchain, features, lockHash, fingerprint, startedAt: new Date().toISOString()};
  write(path.join(receipts, 'manifest.json'), manifest);
  if (process.env.GITHUB_OUTPUT) fs.appendFileSync(process.env.GITHUB_OUTPUT, `fingerprint=${fingerprint}\n`);
  console.log(JSON.stringify(manifest, null, 2));
}
function plan(stage, manifest, target, output, exists = file => fs.existsSync(path.join(rust, file))) {
  const packages = manifest.packages.flatMap(name => ['--package', name]);
  const cargo = (...args) => ({program: 'cargo', args});
  const python = (...args) => ({program: 'python', args: ['-B', ...args]});
  const commands = [];
  const has = name => manifest.packages.includes(name);
  const sdkValues = manifest.lane === 'sdk-values';
  const cliAuth = manifest.lane === 'cli-auth';
  const nativeTransport = sdkValues && has('fabric-sdk-transport');
  const observation = (pkg, example, reference, extra = []) => {
    const file = path.join(output, `${example}-${pkg}.json`);
    commands.push({...cargo('run', '--quiet', '--offline', '--locked', '-p', pkg, '--example', example), stdout: file},
      python(`tests/compatibility/output/${reference}`, ...extra, '--observed', file));
  };
  if (stage === 'fmt') return [cargo('fmt', '--all', '--', '--check')];
  if (stage === 'fetch') return [cargo('fetch', '--locked', '--target', manifest.target)];
  if (stage === 'check') {
    commands.push(cargo('check', '--offline', '--locked', ...packages, '--all-targets'));
    if (has('fabric-cli')) {
      commands.push(cargo('check', '--offline', '--locked', '-p', 'fabric-cli',
        '--features', cliAuth ? 'windows-broker' : 'native-read', '--all-targets'));
    }
    if (nativeTransport) commands.push(cargo('check', '--offline', '--locked',
      '-p', 'fabric-sdk-transport', '--no-default-features', '--lib'));
    return commands;
  }
  if (stage === 'clippy') {
    commands.push(cargo('clippy', '--offline', '--locked', ...packages, '--all-targets', '--', '-D', 'warnings'));
    if (cliAuth) commands.push(cargo('clippy', '--offline', '--locked',
      '-p', 'fabric-cli', '--features', 'windows-broker', '--all-targets', '--', '-D', 'warnings'));
    if (nativeTransport) commands.push(cargo('clippy', '--offline', '--locked',
      '-p', 'fabric-sdk-transport', '--features', 'hyper-rustls', '--all-targets', '--', '-D', 'warnings'));
    return commands;
  }
  if (stage === 'test') {
    if (cliAuth) return [
      cargo('test', '--offline', '--locked', '-p', 'fabric-cli', '--features', 'windows-broker',
        '--test', 'auth_commands', '--test', 'auth_arguments', '--test', 'cli_contract',
        '--test', 'workspace_contract', '--', '--test-threads=1'),
      cargo('test', '--offline', '--locked', '-p', 'fabric-cli', '--features', 'windows-broker',
        '--bin', 'fab-rs', '--', '--test-threads=1'),
      cargo('test', '--offline', '--locked', '-p', 'fabric-cli', '--no-default-features',
        '--', '--test-threads=1'),
    ];
    if (sdkValues) {
      requireThat(has('fabric-sdk-http') && has('fabric-sdk-operations'),
        'SDK/value lane requires HTTP and operation endpoint consumers');
      commands.push(cargo('test', '--offline', '--locked', '-p', 'fabric-sdk-http',
        '-p', 'fabric-sdk-operations', '--test', 'endpoint_context', '--', '--test-threads=1'));
    }
    commands.push(cargo('test', '--offline', '--locked', ...packages, '--', '--test-threads=1'),
      cargo('test', '--offline', '--locked', ...packages, '--doc', '--', '--test-threads=1'));
    if (nativeTransport) commands.push(cargo('test', '--offline', '--locked',
      '-p', 'fabric-sdk-transport', '--features', 'hyper-rustls', '--', '--test-threads=1'));
    return commands;
  }
  requireThat(stage === 'family', `Unknown stage: ${stage}`);
  if (cliAuth) {
    requireThat(exists('crates/fabric-cli/tests/auth_commands.rs'),
      'Released offline auth command consumer is missing');
    return [cargo('test', '--offline', '--locked', '-p', 'fabric-cli',
      '--features', 'windows-broker', '--doc', '--', '--test-threads=1')];
  }
  if (has('fabric-cli-jobs') || has('fabric-sdk-jobs') || has('fabric-sdk-paging')) {
    for (const reference of ['reference', 'branches_reference', 'operation_output_reference', 'skill_headers_reference']) {
      commands.push(python(`tests/compatibility/jobs/${reference}.py`));
    }
    if (has('fabric-cli-jobs')) {
      commands.push(cargo('run', '--offline', '--locked', '-p', 'fabric-cli-jobs', '--example', 'offline_job'));
    }
  }
  if (has('fabric-core-json') || has('fabric-cli-output')) {
    observation('fabric-core-json', 'definition_json', 'definition_reference.py');
    observation('fabric-cli-output', 'definition_json', 'definition_reference.py');
    observation('fabric-cli-output', 'compatibility_probe', 'reference.py');
    observation('fabric-core-json', 'unicode_json', 'unicode_reference.py');
    observation('fabric-cli-output', 'text_probe', 'text_reference.py');
    for (const test of ['test_reference.py', 'test_unicode_reference.py', 'test_text_reference.py']) {
      commands.push(python('-m', 'unittest', 'discover', '-s', 'tests/compatibility/output', '-p', test, '-v'));
    }
    if (exists('tests/compatibility/output/native_reference.py')) {
      observation('fabric-core-json', 'native_json', 'native_reference.py', ['--encoding-only']);
      commands.push(python('-m', 'unittest', 'discover', '-s', 'tests/compatibility/output',
        '-p', 'test_native_reference.py', '-v'));
    } else {
      requireThat(!['native-values', 'sdk-values'].includes(manifest.lane),
        'Released native-values comparator is missing');
    }
  }
  if (sdkValues) {
    commands.push(python('tests/compatibility/items/reference.py', '--check', '--include-python-only'),
      python('-m', 'unittest', 'discover', '-s', 'tests/compatibility/items',
        '-p', 'test_reference.py', '-v'),
      python('tests/compatibility/http/reference.py'));
  }
  if (nativeTransport) commands.push(cargo('run', '--offline', '--locked',
    '-p', 'fabric-sdk-transport', '--features', 'hyper-rustls', '--example', 'sdk_consumer'));
  if (has('fabric-cli')) {
    const probes = path.join(target, 'debug', 'examples');
    commands.push(cargo('build', '--offline', '--locked', '-p', 'fabric-cli', '-p', 'fabric-cli-query', '--examples'),
      python('tests/compatibility/run_read_parity.py',
        '--read-probe', path.join(probes, 'read_probe'),
        '--query-probe', path.join(probes, 'projection_output'),
        '--folder-probe', path.join(probes, 'folder_read_probe'), '--artifacts', path.join(output, 'read')),
      cargo('clippy', '--offline', '--locked', '-p', 'fabric-cli', '--features', 'native-read',
        '--all-targets', '--', '-D', 'warnings'),
      cargo('test', '--offline', '--locked', '-p', 'fabric-cli', '-p', 'fabric-identity',
        '--features', 'fabric-cli/native-read', '--', '--test-threads=1'),
      cargo('build', '--offline', '--locked', '-p', 'fabric-cli', '--examples', '--features', 'native-read'),
      python('tests/compatibility/read_session/consumer.py',
        '--rust-probe', path.join(probes, 'read_session_probe')));
    if (exists('tests/compatibility/job_reads/compare.py')) {
      commands.push(python('tests/compatibility/job_reads/compare.py',
        '--probe', path.join(probes, 'job_read_session_probe')));
    } else {
      requireThat(manifest.lane !== 'cli-jobs', 'Released CLI job comparator is missing');
    }
  }
  if (has('fabric-deploy')) {
    commands.push(python('tests/compatibility/run_definition_parity.py', '--artifacts', path.join(output, 'definition')));
  }
  if (manifest.lane !== 'sdk-values' &&
    (has('fabric-cli-mutations') || has('fabric-sdk-operations') || has('fabric-sdk-onelake') || has('fabric-sdk-onelake-http'))) {
    commands.push(cargo('build', '--offline', '--locked', '-p', 'fabric-cli-mutations',
      '-p', 'fabric-sdk-operations', '--examples'),
    python('tests/compatibility/run_resource_parity.py', '--probes', path.join(target, 'debug', 'examples'),
      '--artifacts', path.join(output, 'resources')));
  }
  if (has('fabric-sdk')) commands.push(cargo('run', '--offline', '--locked', '-p', 'fabric-sdk', '--example', 'injected_credentials'));
  requireThat(commands.length > 0, 'Selected family has no executable consumer');
  return commands;
}
function execute(commands, cwd, run = spawnSync) {
  const results = [];
  for (const item of commands) {
    const startedAt = new Date().toISOString();
    console.log(`> ${item.program} ${item.args.join(' ')}`);
    const fd = item.stdout ? fs.openSync(item.stdout, 'w') : undefined;
    let result;
    try {
      result = run(item.program, item.args, {cwd, shell: false, timeout: 300000,
        stdio: ['ignore', fd === undefined ? 'inherit' : fd, 'inherit']});
    } finally {
      if (fd !== undefined) fs.closeSync(fd);
    }
    results.push({program: item.program, args: item.args, startedAt,
      completedAt: new Date().toISOString(), exit: result.status, signal: result.signal,
      error: result.error?.message});
    if (result.error || result.signal || result.status !== 0) break;
  }
  return results;
}
function runStage(stage) {
  const manifest = JSON.parse(fs.readFileSync(path.join(receipts, 'manifest.json')));
  requireThat(process.env.CARGO_BUILD_JOBS === '1' && process.env.RUST_TEST_THREADS === '1',
    'Hosted preflight must use one Cargo job and one test thread');
  requireThat(process.env.CARGO_TARGET_DIR && path.isAbsolute(process.env.CARGO_TARGET_DIR),
    'An absolute private CARGO_TARGET_DIR is required');
  const commands = plan(stage, manifest, process.env.CARGO_TARGET_DIR, receipts);
  const results = execute(commands, rust);
  write(path.join(receipts, `${stage}.json`), {head: manifest.head, source: manifest.source, results});
  requireThat(results.length === commands.length && results.every(result =>
    result.exit === 0 && !result.signal && !result.error), `${stage} failed; product owner fix required, no automatic retry`);
}
function finish() {
  const manifest = JSON.parse(fs.readFileSync(path.join(receipts, 'manifest.json')));
  for (const stage of ['fmt', 'check', 'clippy', 'test', 'family']) {
    const receipt = JSON.parse(fs.readFileSync(path.join(receipts, `${stage}.json`)));
    requireThat(receipt.head === manifest.head && receipt.source === manifest.source &&
      receipt.results.length > 0 && receipt.results.every(result =>
        result.exit === 0 && !result.signal && !result.error), `No actual passing ${stage} exit`);
  }
  requireThat(digest(fs.readFileSync(path.join(rust, 'Cargo.lock'))) === manifest.lockHash, 'Lockfile changed');
  requireThat(git(['status', '--porcelain', '--untracked-files=all']) === '', 'Validation mutated the source checkout');
  write(path.join(receipts, 'passed.json'), {source: manifest.source, head: manifest.head,
    completedAt: new Date().toISOString(), scope: manifest.coverage});
}

function options(args, allowed) {
  requireThat(args.length % 2 === 0, 'Options must be explicit name/value pairs');
  const result = {};
  for (let i = 0; i < args.length; i += 2) {
    requireThat(allowed.includes(args[i]) && !Object.hasOwn(result, args[i]), `Invalid option: ${args[i]}`);
    result[args[i]] = args[i + 1];
  }
  for (const name of allowed) requireThat(result[name], `Missing ${name}`);
  return result;
}
function publish(args) {
  const opts = options(args, ['--source', '--automation', '--lane', '--branch', '--worktree', '--receipt']);
  const source = opts['--source'], automation = opts['--automation'], branch = opts['--branch'];
  requireThat(sha(source) && sha(automation), 'Source and automation must be full immutable SHAs');
  lane(opts['--lane']);
  requireThat(/^validation\/rust-fast-[a-z0-9][a-z0-9-]*$/.test(branch), 'Only a new validation/rust-fast-* branch is permitted');
  for (const name of ['--worktree', '--receipt']) requireThat(path.isAbsolute(opts[name]), `${name} must be absolute`);
  requireThat(!fs.existsSync(opts['--worktree']) && !fs.existsSync(opts['--receipt']), 'Never overwrite a worktree or receipt');
  requireThat(git(['remote', 'get-url', '--push', 'eisber']) === 'https://github.com/eisber/fabric-cli.git',
    'Existing eisber push remote must be exactly the owner fork');
  requireThat(git(['ls-remote', '--heads', 'eisber', `refs/heads/${branch}`]) === '', 'Validation branch already exists; inspect existing receipt');
  const automationSource = JSON.parse(git(['show', `${automation}:.github/rust-fast-candidate.json`])).source;
  requireThat(sha(automationSource), 'Automation source must be a full immutable SHA');
  git(['merge-base', '--is-ancestor', automationSource, automation]);
  requireThat(git(['rev-list', '--merges', `${automationSource}..${automation}`]) === '',
    'Automation must be a non-merge CI-only commit series');
  const commits = git(['rev-list', '--reverse', `${automationSource}..${automation}`]).split('\n').filter(Boolean);
  requireThat(commits.length > 0, 'Automation has no CI commits');
  for (const commit of commits) {
    ciDelta(git(['diff-tree', '--no-commit-id', '--name-only', '-r', commit]).split('\n').filter(Boolean));
  }
  const active = JSON.parse(command('gh', ['api', '--method', 'GET',
    `repos/${REPO}/actions/runs`, '-f', 'per_page=100'])).workflow_runs;
  requireThat(!active.some(run => run.path === '.github/workflows/rust-fast-check.yml' &&
    run.status !== 'completed' && run.head_branch?.startsWith('validation/rust-fast-')),
    'Another fast candidate is ongoing; finish it before publishing another');
  git(['worktree', 'add', '-b', branch, opts['--worktree'], source]);
  const at = argv => command('git', argv, opts['--worktree']);
  // Replay only the CI patch: merging its older base could import product fixes.
  const contained = at(['merge-base', source, automation]) === automation;
  if (!contained) at(['cherry-pick', '--no-commit', ...commits]);
  write(path.join(opts['--worktree'], '.github/rust-fast-candidate.json'), {source, lane: opts['--lane']});
  at(['add', '.github/rust-fast-candidate.json']);
  ciDelta(at(['diff', '--cached', '--name-only', source]).split('\n').filter(Boolean));
  at(['commit', '-m', `Validate ${opts['--lane']} at ${source}\n\nCo-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>`]);
  const head = at(['rev-parse', 'HEAD']);
  const receipt = {repository: REPO, source, automation, head, lane: opts['--lane'], branch,
    state: 'prepared', createdAt: new Date().toISOString()};
  write(opts['--receipt'], receipt);
  at(['push', 'eisber', `HEAD:refs/heads/${branch}`]);
  requireThat(at(['ls-remote', '--heads', 'eisber', `refs/heads/${branch}`]).split(/\s+/)[0] === head,
    'Provider branch verification failed; do not resend uncertain push');
  write(opts['--receipt'], {...receipt, state: 'pushed'});
  console.log(JSON.stringify({...receipt, state: 'pushed'}, null, 2));
}
function full(args) {
  const opts = options(args, ['--receipt', '--fast-run']);
  const receipt = JSON.parse(fs.readFileSync(opts['--receipt']));
  requireThat(receipt.repository === REPO && sha(receipt.head) && sha(receipt.source), 'Invalid candidate receipt');
  requireThat(/^[1-9][0-9]*$/.test(opts['--fast-run']), 'A real provider fast run ID is required');
  const gh = argv => command('gh', argv);
  const run = JSON.parse(gh(['api', `repos/${REPO}/actions/runs/${opts['--fast-run']}`]));
  requireFastPass(receipt, run);
  requireThat(git(['ls-remote', '--heads', 'eisber', `refs/heads/${receipt.branch}`]).split(/\s+/)[0] === receipt.head,
    'Stale candidate branch: full dispatch forbidden');
  const runs = JSON.parse(gh(['api', '--method', 'GET',
    `repos/${REPO}/actions/workflows/rust-ci.yml/runs`, '-f', `head_sha=${receipt.head}`, '-f', 'per_page=100'])).workflow_runs;
  const sourceRuns = receipt.source === receipt.head ? [] : JSON.parse(gh(['api', '--method', 'GET',
    `repos/${REPO}/actions/workflows/rust-ci.yml/runs`, '-f', `head_sha=${receipt.source}`, '-f', 'per_page=100'])).workflow_runs;
  if (runs.length || sourceRuns.length) {
    write(opts['--receipt'], {...receipt, full: {state: 'existing', runs: [...runs, ...sourceRuns].map(item =>
      ({id: item.id, head: item.head_sha, status: item.status, conclusion: item.conclusion, url: item.html_url}))}});
    console.log('Existing native batch recorded; no duplicate dispatch and no new full-coverage claim');
    return;
  }
  requireThat(!receipt.full, 'Full dispatch already requested or uncertain; inspect provider, do not replay');
  const active = JSON.parse(gh(['api', '--method', 'GET',
    `repos/${REPO}/actions/workflows/rust-ci.yml/runs`, '-f', 'per_page=100'])).workflow_runs;
  requireThat(!active.some(item => item.status !== 'completed' &&
    item.head_branch?.startsWith('validation/rust-fast-')),
  'Another fast-lane native batch is ongoing; no second full dispatch');
  write(opts['--receipt'], {...receipt, fastRun: run.id, full: {state: 'dispatch-requested', head: receipt.head}});
  gh(['workflow', 'run', 'rust-ci.yml', '--repo', REPO, '--ref', receipt.head]);
  console.log('Full batch requested at exact immutable CI head; read provider jobs before claiming coverage');
}
function requireFastPass(receipt, run) {
  requireThat(run.head_sha === receipt.head && run.path === '.github/workflows/rust-fast-check.yml' &&
    run.status === 'completed' && run.conclusion === 'success', 'Exact combined head has not passed fast preflight');
}
function main(args) {
  const [action, ...rest] = args;
  if (action === 'prepare' && !rest.length) return prepare();
  if (action === 'run' && rest.length === 1) return runStage(rest[0]);
  if (action === 'finish' && !rest.length) return finish();
  if (action === 'publish') return publish(rest);
  if (action === 'full') return full(rest);
  throw new Error('Use prepare, run <fmt|fetch|check|clippy|test|family>, finish, publish or full; see docs/fast-check.md');
}
module.exports = {LANES, CI_FILES, sha, lane, ciDelta, select, plan, execute, options, requireFastPass};
if (require.main === module) {
  try { main(process.argv.slice(2)); }
  catch (error) { console.error(`Rust fast check: ${error.message}`); process.exitCode = 1; }
}
