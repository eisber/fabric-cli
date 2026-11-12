// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {test} = require('node:test');
const {setTimeout: delay} = require('node:timers/promises');
const {once} = require('node:events');
const {commandResult, commandIdentity, deduplicateCoreCommands, commandClass,
  runContracts, options, main} = require('./validate-impact.cjs');
const engineeringInventory = require('./engineering-inventory.json');

const STARTUP_WAIT_MS = 15000;
const PROCESS_BUDGET_MS = 60000;
const TIMEOUT_BUDGET_MS = 20000;
const CLEANUP_WAIT_MS = 7000;

function fixture(t, ceiling = PROCESS_BUDGET_MS) {
  const base = process.env.IMPACT_TEST_ROOT || path.join(__dirname, '..', 'target', 'validation-tests');
  fs.mkdirSync(base, {recursive: true});
  const root = fs.mkdtempSync(path.join(base, 'process-'));
  t.after(() => fs.rmSync(root, {recursive: true, force: true}));
  return {root, artifacts: root, commandTimeout: ceiling, deadline: Date.now() + 120000, results: []};
}

async function waitFor(predicate, message, timeout = STARTUP_WAIT_MS) {
  const end = Date.now() + timeout;
  while (!predicate()) {
    assert.ok(Date.now() < end, message);
    await delay(25);
  }
}

function alive(pid) {
  if (process.platform === 'linux') {
    // kill(pid, 0) also succeeds for zombies; termination is not proof of reaping.
    try {
      const stat = fs.readFileSync(`/proc/${pid}/stat`, 'utf8');
      const state = stat.slice(stat.lastIndexOf(')') + 2).split(' ')[0];
      if (state === 'Z' || state === 'X') return false;
    } catch (error) {
      if (error.code === 'ENOENT' || error.code === 'ESRCH') return false;
      throw error;
    }
  }
  try { process.kill(pid, 0); return true; }
  catch (error) { if (error.code === 'ESRCH') return false; throw error; }
}

function command(context, script, id = 'process-fixture') {
  return {id, program: process.execPath, args: ['-e', script], cwd: context.root};
}

test('only exact core Cargo executions deduplicate; aliases retain every obligation and count tests once', t => {
  const context = fixture(t);
  const base = {id: 'one:test', program: 'cargo', args: ['test', '--lib']};
  const same = {...base, id: 'two:test'};
  const unique = deduplicateCoreCommands([base, same, {...base, id: 'three:test',
    env: {FABRIC_DEDUP_TEST_VARIANT: 'distinct'}}], context);
  assert.equal(unique.length, 2);
  assert.deepEqual(unique[0].aliases, ['one:test', 'two:test']);
  assert.equal(commandIdentity(base, context), commandIdentity(same, context));
  for (const changed of [
    {...base, cwd: path.join(context.root, 'different')}, {...base, program: 'other-cargo'},
    {...base, args: ['test', '--lib', '--ignored']}, {...base, env: {OTHER: 'different'}},
    {...base, args: ['--lib', 'test']},
  ]) assert.notEqual(commandIdentity(base, context), commandIdentity(changed, context));
  commandResult(unique[0], context, () => ({status: 0,
    stdout: 'test result: ok. 7 passed; 0 failed; 1 ignored;\n'}));
  assert.equal(context.results.length, 1);
  assert.equal(context.results[0].tests_passed, 7);
  assert.deepEqual(context.results[0].aliases, ['one:test', 'two:test']);
  assert.throws(() => deduplicateCoreCommands([{program: 'python', args: []}], context), /Only core Cargo/);
  // commandResult itself must never coalesce repeated contracts.
  for (let index = 0; index < 2; index++) commandResult(base, context, () => ({status: 0}));
  assert.equal(context.results.length, 3);
});

test('scheduler-owned aliases survive further exact deduplication and receipt recording', t => {
  const context = fixture(t);
  const command = {id: 'profile-a:test', program: 'cargo', args: ['test', '--lib'],
    aliases: ['default:test', 'profile-a:test']};
  const singleton = deduplicateCoreCommands([command], context);
  assert.equal(singleton.length, 1);
  assert.deepEqual(singleton[0].aliases, ['profile-a:test', 'default:test']);
  const duplicate = {...command, id: 'profile-b:test', aliases: ['legacy:test', 'default:test']};
  const commands = deduplicateCoreCommands([command, duplicate], context);
  const expected = ['profile-a:test', 'default:test', 'profile-b:test', 'legacy:test'];
  assert.equal(commands.length, 1);
  assert.deepEqual(commands[0].aliases, expected);
  commandResult(commands[0], context, () => ({status: 0,
    stdout: 'test result: ok. 1 passed; 0 failed; 0 ignored;\n'}));
  assert.deepEqual(context.results[0].aliases, expected);
  assert.equal(context.results[0].tests_passed, 1);
  assert.deepEqual(JSON.parse(fs.readFileSync(path.join(context.artifacts, 'commands.json')))[0].aliases,
    expected);
});

test('tools, tests and compilation have separate budgets under a configurable ceiling', t => {
  const context = fixture(t, 900000);
  context.deadline = Date.now() + 3600000;
  const cases = [
    [{program: 'cargo', args: ['fmt']}, 'tool', 120000],
    [{program: 'cargo', args: ['metadata']}, 'tool', 120000],
    [{program: 'cargo', args: ['test', '--doc']}, 'test', 300000],
    [{program: 'node', args: ['--test', 'a.cjs']}, 'test', 300000],
    [{program: 'python', args: [], test: 'unittest'}, 'test', 300000],
    [{program: 'cargo', args: ['clippy']}, 'compilation', 600000],
    [{program: 'cargo', args: ['build']}, 'compilation', 600000],
    [{program: 'cargo', args: ['test', '--all-targets', '--no-run']}, 'compilation', 600000],
  ];
  for (const [descriptor, expected, budget] of cases) {
    assert.equal(commandClass(descriptor), expected);
    commandResult({...descriptor, id: expected}, context, (_, __, launch) => {
      assert.equal(launch.timeout, budget);
      return {status: 0, stderr: descriptor.test ? 'Ran 1 test in 0.1s\n' : ''};
    });
    assert.equal(context.results.at(-1).budget_class, expected);
  }
  context.commandTimeout = 12000;
  commandResult({id: 'ceiling', program: 'cargo', args: ['check']}, context,
    (_, __, launch) => { assert.equal(launch.timeout, 12000); return {status: 0}; });
});

test('deadline exhaustion and cancellation never launch a process but retain typed failure receipts', t => {
  const context = fixture(t);
  context.deadline = Date.now() - 1;
  let spawned = false;
  assert.throws(() => commandResult(command(context, ''), context,
    () => { spawned = true; return {status: 0}; }), /deadline exhausted/);
  assert.equal(spawned, false);
  assert.equal(context.results[0].status, 'timed_out');
  assert.equal(context.results[0].cleanup_confirmed, true);
  context.deadline = Date.now() + 10000;
  context.signal = AbortSignal.abort();
  assert.throws(() => commandResult(command(context, ''), context,
    () => { spawned = true; return {status: 0}; }), /ECANCELLED/);
  assert.equal(spawned, false);
  assert.equal(context.results[1].status, 'cancelled');
});

test('a zero exit cannot pass when descendant cleanup is unconfirmed', t => {
  const context = fixture(t);
  assert.throws(() => commandResult(command(context, '', 'unconfirmed-tree'), context, () => ({
    status: 0, cleanup_confirmed: false,
    cleanup_method: 'darwin-inherited-marker-starttime',
    cleanup_errors: ['darwin_candidate_environment_unavailable'],
  })), /Validation failed: unconfirmed-tree/);
  assert.equal(context.results[0].status, 'failed');
  assert.equal(context.results[0].cleanup_confirmed, false);
  assert.equal(context.results[0].cleanup_method, 'darwin-inherited-marker-starttime');
});

test('owned execution preserves null inherited environments and does not mutate explicit environments', async t => {
  const context = fixture(t);
  const {executeOwned} = require('./validation-process.cjs');
  const environment = Object.freeze({...process.env, FABRIC_VALIDATION_TEST_INPUT: 'preserved'});
  const before = JSON.stringify(environment);
  for (const [index, env] of [null, environment].entries()) {
    const stdout = path.join(context.root, `environment-${index}.stdout.log`);
    const result = await executeOwned(process.execPath, ['-e',
      `if (!process.env.PATH && !process.env.Path) process.exit(2);
       process.stdout.write(${env ? 'process.env.FABRIC_VALIDATION_TEST_INPUT' : "'inherited'"});`],
    {cwd: context.root, env, timeout: PROCESS_BUDGET_MS, stdout,
      stderr: path.join(context.root, `environment-${index}.stderr.log`)});
    assert.equal(result.status, 0, result.error?.message);
    assert.equal(result.cleanup_confirmed, true);
    assert.equal(fs.readFileSync(stdout, 'utf8'), env ? 'preserved' : 'inherited');
  }
  assert.equal(JSON.stringify(environment), before);
});

test('stdout and stderr reach files before exit, retaining streamed bytes and exact hashes', async t => {
  const context = fixture(t);
  const release = path.join(context.root, 'release-streaming-child');
  const script = `const fs=require('node:fs');
    process.stdout.write('live out\\n'); process.stderr.write('live err\\n');
    const timer=setInterval(() => {
      if (fs.existsSync(${JSON.stringify(release)})) {
        process.stdout.write('test result: ok. 3 passed; 0 failed; 2 ignored;\\n');
        clearInterval(timer);
      }
    },25);`;
  const pending = commandResult(command(context, script), context);
  const stdout = path.join(context.root, '000-process-fixture.stdout.log');
  const stderr = path.join(context.root, '000-process-fixture.stderr.log');
  await waitFor(() => fs.readFileSync(stdout, 'utf8').includes('live out') &&
    fs.readFileSync(stderr, 'utf8').includes('live err'), 'live logs were not streamed');
  assert.equal(context.results.length, 0, 'command has not exited when logs become visible');
  fs.writeFileSync(release, 'ready');
  await pending;
  const record = context.results[0];
  assert.equal(record.status, 'passed');
  assert.equal(record.cleanup_confirmed, true);
  assert.equal(record.tests_passed, 3);
  assert.equal(record.tests_ignored, 2);
  for (const channel of ['stdout', 'stderr']) {
    const hash = require('node:crypto').createHash('sha256').update(fs.readFileSync(record[channel])).digest('hex');
    assert.equal(record[`${channel}_sha256`], hash);
  }
  assert.ok(record.started_at && record.finished_at);
  assert.ok(!fs.readdirSync(context.root).some(name => name.includes('process-result')));
});

function treeScript(context, detached = false) {
  const pidfile = path.join(context.root, 'owned-pids.json');
  const grandchild = 'setInterval(() => {}, 60000)';
  return {pidfile, script: `
    const fs = require('node:fs');
    const {spawn} = require('node:child_process');
    const grandchild = spawn(process.execPath, ['-e', ${JSON.stringify(grandchild)}],
      {stdio: 'inherit', shell: false, detached: ${JSON.stringify(detached)}, windowsHide: true});
    process.stdout.write('partial stdout\\n'); process.stderr.write('partial stderr\\n');
    fs.writeFileSync(${JSON.stringify(`${pidfile}.partial`)}, JSON.stringify([process.pid, grandchild.pid]));
    fs.renameSync(${JSON.stringify(`${pidfile}.partial`)}, ${JSON.stringify(pidfile)});
    setInterval(() => {}, 60000);`};
}

for (const detached of [false, true]) test(`real stalled child and grandchild are killed and reaped on timeout, with partial logs${detached ? ' in separate sessions' : ''}`, async t => {
  const context = fixture(t, TIMEOUT_BUDGET_MS);
  const tree = treeScript(context, detached);
  const pending = commandResult(command(context, tree.script, 'stalled-tree'), context);
  // Attach rejection handling immediately, including slow-host startup failures.
  const rejected = assert.rejects(pending, /Validation failed: stalled-tree/);
  await waitFor(() => fs.existsSync(tree.pidfile), 'owned tree failed to start');
  const pids = JSON.parse(fs.readFileSync(tree.pidfile));
  assert.ok(pids.every(alive));
  await rejected;
  assert.ok(pids.every(pid => !alive(pid)), 'owned child/grandchild survived completed timeout');
  const [record] = context.results;
  assert.equal(record.status, 'timed_out');
  assert.equal(record.timed_out, true);
  assert.equal(record.cancelled, false);
  assert.equal(record.timeout_ms, TIMEOUT_BUDGET_MS);
  assert.equal(record.cleanup_confirmed, true);
  assert.deepEqual(record.cleanup_errors, []);
  assert.match(fs.readFileSync(record.stdout, 'utf8'), /partial stdout/);
  assert.match(fs.readFileSync(record.stderr, 'utf8'), /partial stderr/);
  assert.ok(record.elapsed_ms < TIMEOUT_BUDGET_MS + CLEANUP_WAIT_MS);
});

for (const detached of [false, true]) test(`cancellation kills the owned tree without waiting for its command budget${detached ? ' in separate sessions' : ''}`, async t => {
  const context = fixture(t);
  const controller = new AbortController();
  context.signal = controller.signal;
  const tree = treeScript(context, detached);
  const pending = commandResult(command(context, tree.script, 'cancelled-tree'), context);
  const rejected = assert.rejects(pending, /Validation failed: cancelled-tree/);
  await waitFor(() => fs.existsSync(tree.pidfile), 'owned tree failed to start');
  const pids = JSON.parse(fs.readFileSync(tree.pidfile));
  const cancelledAt = Date.now();
  controller.abort();
  await rejected;
  assert.ok(pids.every(pid => !alive(pid)));
  assert.equal(context.results[0].status, 'cancelled');
  assert.equal(context.results[0].timed_out, false);
  assert.equal(context.results[0].cleanup_confirmed, true);
  assert.ok(Date.now() - cancelledAt < CLEANUP_WAIT_MS);
  assert.ok(context.results[0].elapsed_ms < context.commandTimeout);
});

test('POSIX cleanup kills a separate-session orphan after its intermediate parent exits',
  {skip: process.platform === 'win32' && 'requires POSIX sessions'}, async t => {
    const context = fixture(t);
    const controller = new AbortController();
    context.signal = controller.signal;
    const pidfile = path.join(context.root, 'orphan-pids.json');
    const intermediate = `const fs=require('node:fs'); const {spawn}=require('node:child_process');
      const child=spawn(process.execPath,['-e','setInterval(()=>{},60000)'],
        {detached:true,stdio:'inherit'});
      fs.writeFileSync(${JSON.stringify(`${pidfile}.partial`)},
        JSON.stringify([Number(process.argv[1]),process.pid,child.pid]));
      fs.renameSync(${JSON.stringify(`${pidfile}.partial`)},${JSON.stringify(pidfile)});
      process.exit(0);`;
    const script = `const {spawn}=require('node:child_process');
      spawn(process.execPath,['-e',${JSON.stringify(intermediate)},String(process.pid)],{stdio:'inherit'});
      setInterval(()=>{},60000);`;
    const pending = commandResult(command(context, script, 'orphan-tree'), context);
    const rejected = assert.rejects(pending, /Validation failed: orphan-tree/);
    await waitFor(() => fs.existsSync(pidfile), 'separate-session orphan did not start');
    const pids = JSON.parse(fs.readFileSync(pidfile));
    assert.ok(alive(pids[0]) && alive(pids[2]));
    const cancelledAt = Date.now();
    controller.abort();
    await rejected;
    assert.ok(pids.every(pid => !alive(pid)), 'owned orphan survived cancellation/reaping');
    assert.equal(context.results[0].status, 'cancelled');
    assert.equal(context.results[0].cleanup_confirmed, true);
    assert.ok(Date.now() - cancelledAt < CLEANUP_WAIT_MS);
    assert.equal(context.results[0].cleanup_method, process.platform === 'linux' ?
      'linux-subreaper-pidfd' : 'darwin-inherited-marker-starttime');
  });

function orphanScript(context, rootExits = false) {
  const pidfile = path.join(context.root, 'double-fork-pids.json');
  const leaf = `const fs=require('node:fs');
    process.stdout.write('orphan partial stdout\\n');
    process.stderr.write('orphan partial stderr\\n');
    fs.writeFileSync(${JSON.stringify(`${pidfile}.partial`)},
      JSON.stringify([...JSON.parse(process.argv[1]),process.pid]));
    fs.renameSync(${JSON.stringify(`${pidfile}.partial`)},${JSON.stringify(pidfile)});
    setInterval(()=>{},60000);`;
  const fork = code => `const {spawn}=require('node:child_process');
    spawn(process.execPath,['-e',${JSON.stringify(code)},
      JSON.stringify([...JSON.parse(process.argv[1]),process.pid])],
      {detached:true,stdio:'inherit'}).unref(); process.exit(0);`;
  const intermediate = fork(fork(leaf));
  return {pidfile, script: `const fs=require('node:fs'); const {spawn}=require('node:child_process');
    spawn(process.execPath,['-e',${JSON.stringify(intermediate)},JSON.stringify([process.pid])],
      {detached:true,stdio:'inherit'}).unref();
    setInterval(()=>{if(${JSON.stringify(rootExits)} && fs.existsSync(${JSON.stringify(pidfile)}))
      process.exit(0);},20);`};
}

for (const mode of ['timeout', 'cancelled', 'deadline', 'normal']) {
  test(`POSIX ${mode} cleans double-fork orphans without touching an unrelated same-user process`,
    {skip: process.platform === 'win32' && 'requires POSIX sessions'}, async t => {
      const {spawn} = require('node:child_process');
      const context = fixture(t, mode === 'timeout' ? TIMEOUT_BUDGET_MS : PROCESS_BUDGET_MS);
      if (mode === 'deadline') context.deadline = Date.now() + TIMEOUT_BUDGET_MS;
      const controller = new AbortController();
      context.signal = controller.signal;
      const tree = orphanScript(context, mode === 'normal');
      const pending = commandResult(command(context, tree.script, `orphan-${mode}`), context);
      const finished = mode === 'normal' ? pending :
        assert.rejects(pending, new RegExp(`Validation failed: orphan-${mode}`));
      await waitFor(() => fs.existsSync(tree.pidfile), 'double-fork orphan failed to start');
      const pids = JSON.parse(fs.readFileSync(tree.pidfile));
      if (mode !== 'normal') assert.ok(alive(pids.at(-1)), 'orphan was not alive before cleanup');
      const unrelated = spawn(process.execPath, ['-e', 'setInterval(()=>{},60000)'],
        {stdio: 'ignore'});
      const unrelatedExit = once(unrelated, 'exit');
      t.after(async () => { unrelated.kill('SIGKILL'); await unrelatedExit; });
      const cancelledAt = Date.now();
      if (mode === 'cancelled') controller.abort();
      await finished;
      assert.ok(alive(unrelated.pid), 'cleanup touched an unrelated process');
      assert.ok(pids.every(pid => !alive(pid)), 'double-fork descendant survived cleanup');
      const [record] = context.results;
      assert.equal(record.status, {normal: 'passed', cancelled: 'cancelled'}[mode] || 'timed_out');
      assert.equal(record.cleanup_confirmed, true);
      assert.deepEqual(record.cleanup_errors, []);
      assert.match(fs.readFileSync(record.stdout, 'utf8'), /orphan partial stdout/);
      assert.match(fs.readFileSync(record.stderr, 'utf8'), /orphan partial stderr/);
      if (mode === 'cancelled') assert.ok(Date.now() - cancelledAt < CLEANUP_WAIT_MS);
      const budget = mode === 'timeout' || mode === 'deadline' ? TIMEOUT_BUDGET_MS : PROCESS_BUDGET_MS;
      assert.ok(record.elapsed_ms < budget + CLEANUP_WAIT_MS);
    });
}

test('cancellation has a bounded fallback even if the POSIX supervisor itself is stopped',
  {skip: process.platform === 'win32' && 'requires POSIX SIGSTOP'}, async t => {
    const context = fixture(t, 60000);
    const controller = new AbortController();
    context.signal = controller.signal;
    const tree = treeScript(context);
    const stopped = path.join(context.root, 'supervisor-stopped');
    const pending = commandResult(command(context,
      `${tree.script}\nsetTimeout(() => {
        process.kill(process.ppid, 'SIGSTOP');
        fs.writeFileSync(${JSON.stringify(stopped)}, 'ready');
      }, 100);`, 'stopped-supervisor'), context);
    const rejected = assert.rejects(pending, /Validation failed: stopped-supervisor/);
    await waitFor(() => fs.existsSync(stopped), 'owned supervisor failed to stop');
    const pids = JSON.parse(fs.readFileSync(tree.pidfile));
    const cancelledAt = Date.now();
    controller.abort();
    await rejected;
    await waitFor(() => pids.every(pid => !alive(pid)),
      'stopped supervisor left an owned process alive', CLEANUP_WAIT_MS);
    assert.equal(context.results[0].status, 'cancelled');
    assert.equal(context.results[0].cleanup_confirmed, false);
    assert.ok(Date.now() - cancelledAt < 12000);
  });

for (const detached of [false, true]) test(`owner death closes the control pipe and cleans up its otherwise stalled process tree${detached ? ' in separate sessions' : ''}`, async t => {
  const context = fixture(t);
  const tree = treeScript(context, detached);
  const {spawn} = require('node:child_process');
  const helper = path.join(__dirname, 'validation-process.cjs');
  const owner = spawn(process.execPath, ['-e', `
    const {executeOwned} = require(${JSON.stringify(helper)});
    executeOwned(process.execPath, ['-e', ${JSON.stringify(tree.script)}], {
      cwd: ${JSON.stringify(context.root)}, env: process.env, timeout: ${PROCESS_BUDGET_MS},
      stdout: ${JSON.stringify(path.join(context.root, 'owner.stdout.log'))},
      stderr: ${JSON.stringify(path.join(context.root, 'owner.stderr.log'))}
    }).then(() => {}).catch(() => {process.exitCode = 1;});`],
  {cwd: context.root, stdio: 'ignore', windowsHide: true});
  t.after(() => { if (owner.exitCode === null) owner.kill('SIGKILL'); });
  const exit = once(owner, 'exit');
  await waitFor(() => fs.existsSync(tree.pidfile), 'owned tree failed to start');
  const pids = JSON.parse(fs.readFileSync(tree.pidfile));
  owner.kill('SIGKILL');
  await exit;
  await waitFor(() => pids.every(pid => !alive(pid)),
    'owner death left a descendant alive', CLEANUP_WAIT_MS);
  // libuv's enclosing Windows Job can close before the supervisor writes an outcome.
  if (process.platform === 'win32') return;
  const reportFile = path.join(context.root, 'owner.stdout.log.process-result.json');
  await waitFor(() => fs.existsSync(reportFile),
    'owner-death cleanup outcome was not retained', CLEANUP_WAIT_MS);
  const report = JSON.parse(fs.readFileSync(reportFile));
  assert.equal(report.cleanup_confirmed, true);
  if (process.platform === 'linux') assert.equal(report.cleanup_method, 'linux-subreaper-pidfd');
});

test('POSIX owner death cleans an environment-preserving double-fork orphan',
  {skip: process.platform === 'win32' && 'requires POSIX sessions'}, async t => {
    const {spawn} = require('node:child_process');
    const context = fixture(t, 60000);
    const tree = orphanScript(context);
    const helper = path.join(__dirname, 'validation-process.cjs');
    const output = path.join(context.root, 'owner.stdout.log');
    const owner = spawn(process.execPath, ['-e', `
      require(${JSON.stringify(helper)}).executeOwned(process.execPath,
        ['-e',${JSON.stringify(tree.script)}], {
          cwd:${JSON.stringify(context.root)},env:process.env,timeout:60000,
          stdout:${JSON.stringify(output)},
          stderr:${JSON.stringify(path.join(context.root, 'owner.stderr.log'))}
        }).catch(()=>{process.exitCode=1;});`], {stdio: 'ignore'});
    const ownerExit = once(owner, 'exit');
    t.after(() => { if (owner.exitCode === null) owner.kill('SIGKILL'); });
    await waitFor(() => fs.existsSync(tree.pidfile), 'double-fork orphan failed to start');
    const pids = JSON.parse(fs.readFileSync(tree.pidfile));
    owner.kill('SIGKILL');
    await ownerExit;
    await waitFor(() => pids.every(pid => !alive(pid)),
      'owner death left a double-fork orphan', CLEANUP_WAIT_MS);
    const result = `${output}.process-result.json`;
    await waitFor(() => fs.existsSync(result), 'owner-death cleanup report missing', CLEANUP_WAIT_MS);
    const outcome = JSON.parse(fs.readFileSync(result));
    assert.equal(outcome.cancelled, true);
    assert.equal(outcome.cleanup_confirmed, true);
    assert.deepEqual(outcome.cleanup_errors, []);
  });

test('Darwin observed marker loss fails closed while still cleaning the known child identity',
  {skip: process.platform !== 'darwin' && 'requires Darwin process environment API'}, async t => {
    const context = fixture(t);
    const controller = new AbortController();
    context.signal = controller.signal;
    const pidfile = path.join(context.root, 'markerless-pids.json');
    const script = `const fs=require('node:fs'); const {spawn}=require('node:child_process');
      const env={...process.env};
      for (const key of Object.keys(env)) {
        if (key.startsWith('FABRIC_VALIDATION_PROCESS_')) delete env[key];
      }
      const child=spawn(process.execPath,['-e','setInterval(()=>{},60000)'],
        {env,detached:true,stdio:'inherit'});
      fs.writeFileSync(${JSON.stringify(`${pidfile}.partial`)},JSON.stringify([process.pid,child.pid]));
      fs.renameSync(${JSON.stringify(`${pidfile}.partial`)},${JSON.stringify(pidfile)});
      setInterval(()=>{},60000);`;
    const pending = commandResult(command(context, script, 'markerless-tree'), context);
    const rejected = assert.rejects(pending, /Validation failed: markerless-tree/);
    await waitFor(() => fs.existsSync(pidfile), 'markerless child failed to start');
    const pids = JSON.parse(fs.readFileSync(pidfile));
    controller.abort();
    await rejected;
    assert.ok(pids.every(pid => !alive(pid)), 'observed markerless child survived cleanup');
    assert.equal(context.results[0].cleanup_confirmed, false);
    assert.ok(context.results[0].cleanup_errors.includes('darwin_owned_tracking_marker_missing'));
  });

test('Darwin marker parsing and identity-bound cleanup source contracts run on every host', () => {
  const {spawnSync} = require('node:child_process');
  const script = String.raw`
import ctypes
import errno
import os
import struct
import sys
import time
from types import SimpleNamespace
from unittest.mock import Mock, patch
sys.path.insert(0, sys.argv[1])
from validation_posix import DarwinTree, _darwin_has_marker

marker = b"FABRIC_VALIDATION_PROCESS_MARKER=synthetic"
def arguments(args, env):
    return (struct.pack("=i", len(args)) + b"/executable\0\0\0" +
            b"\0".join(args) + b"\0" + b"\0".join(env) + b"\0\0")

assert _darwin_has_marker(arguments([b"node", b"", b"-e"], [b"OTHER=x", marker]), marker)
assert not _darwin_has_marker(arguments([b"node", marker], [b"OTHER=x"]), marker)
assert not _darwin_has_marker(arguments([b"node"], [b"PREFIX=" + marker]), marker)
assert not _darwin_has_marker(arguments([b"node"], [marker + b"suffix"]), marker)
assert not _darwin_has_marker(arguments([b"node"], []), marker)
for raw in [b"", struct.pack("=i", -1), struct.pack("=i", 2) + b"x\0\0arg\0"]:
    try:
        _darwin_has_marker(raw, marker)
        raise AssertionError("malformed process arguments accepted")
    except ValueError:
        pass

def tree():
    result = object.__new__(DarwinTree)
    result.owned, result.errors, result.started = {}, [], (1, 0)
    return result

def info(seconds=2, status=2, ppid=1):
    return SimpleNamespace(seconds=seconds, microseconds=0, status=status, ppid=ppid)

def sysctl(mib, count, buffer, length, new, newlength):
    assert list(mib) == [1, 8]
    ctypes.cast(buffer, ctypes.POINTER(ctypes.c_int))[0] = 262144
    return 0
api = SimpleNamespace(proc_pidinfo=Mock(), proc_listpids=Mock())
libc = SimpleNamespace(sysctl=Mock(side_effect=sysctl))
with patch("validation_posix.ctypes.CDLL", side_effect=[api, libc, api, libc]), \
     patch.object(DarwinTree, "_info", return_value=info(1)):
    first, second = DarwinTree(), DarwinTree()
assert ctypes.sizeof(first.info_type) == 136
assert first.info_type.seconds.offset == 120
assert first.info_type.microseconds.offset == 128
assert first.environment.keys().isdisjoint(second.environment)
assert len(first.environment) == len(second.environment) == 1
assert first.marker.endswith(b"=1")
def process_args(mib, count, buffer, length, new, newlength):
    assert list(mib) == [1, 49, 101]
    raw = arguments([b"node"], [first.marker])
    ctypes.memmove(buffer, raw, len(raw))
    ctypes.cast(length, ctypes.POINTER(ctypes.c_size_t))[0] = len(raw)
    return 0
libc.sysctl.side_effect = process_args
assert first._has_marker(101)

# Darwin proc_listpids returns BYTES, unlike proc_listchildpids' PID count.
value = tree()
def listpids(kind, uid, buffer, size):
    if buffer is None:
        return 12
    buffer[0], buffer[1], buffer[2] = 101, 102, 103
    return 12
value.api = SimpleNamespace(proc_listpids=listpids)
assert value._pids(4, 1, time.monotonic() + 1) == [101, 102, 103]
value.api.proc_listpids = lambda *args: 0
assert value._pids(6, 1, time.monotonic() + 1) == []
def denied(*args):
    ctypes.set_errno(errno.EPERM)
    return 0
value.api.proc_listpids = denied
try:
    value._pids(4, 1, time.monotonic() + 1)
    raise AssertionError("inaccessible inventory accepted")
except OSError:
    pass

with patch("validation_posix.os.getuid", return_value=501, create=True):
    value = tree()
    value._pids = lambda *args: [101, 102, 103]
    value._info = lambda pid: info()
    value._has_marker = lambda pid: pid == 102
    value._discover(time.monotonic() + 1)
    assert value.owned == {102: (2, 0)}
    assert value.errors == []
    value._has_marker = lambda pid: False
    value._discover(time.monotonic() + 1)
    assert value.errors == ["darwin_owned_tracking_marker_missing"]

    value = tree()
    value._pids = lambda *args: [101]
    snapshots = iter([info(2), info(3)])
    value._info = lambda pid: next(snapshots)
    value._has_marker = lambda pid: True
    value._discover(time.monotonic() + 1)
    assert value.owned == {}, "reused PID claimed with stale environment"

    value = tree()
    value.owned[101] = (2, 0)
    value._info = lambda pid: info(3)
    assert value._same(101) is None

    value = tree()
    value._pids = lambda *args: [101]
    value._info = lambda pid: info()
    def unreadable(pid):
        raise OSError("private diagnostic must not escape")
    value._has_marker = unreadable
    value._discover(time.monotonic() + 1)
    assert value.errors == ["darwin_candidate_environment_unavailable"]

    # A late orphan appears only after the first cohort was frozen.
    value = tree()
    processes = {101: info(), 102: info(), 103: info()}
    scans = []
    def discover(deadline):
        scans.append(1)
        value.owned[101] = (2, 0)
        if len(scans) > 1:
            value.owned[102] = (2, 0)
    value._discover = discover
    value._info = lambda pid: processes.get(pid)
    value._pids = lambda *args: []
    signals = []
    def kill(pid, sig):
        signals.append((pid, sig))
        if sig == signal.SIGSTOP:
            processes[pid].status = 4
        else:
            del processes[pid]
    import signal
    with patch("validation_posix.signal.SIGSTOP", 19, create=True), \
         patch("validation_posix.signal.SIGKILL", 9, create=True), \
         patch("validation_posix.os.kill", side_effect=kill):
        assert value.terminate(101, time.monotonic() + 1) == []
    assert len(scans) == 3
    assert set(pid for pid, sig in signals) == {101, 102}
    assert processes.keys() == {103}, "unrelated identity was signalled"
    value._discover = lambda deadline: None
    assert value.confirm(time.monotonic() + 1) == []

print("Darwin marker, inventory, PID reuse, fail-closed and late-orphan contracts passed")
`;
  const result = spawnSync(process.env.PYTHON || 'python', ['-B', '-c', script, __dirname],
    {encoding: 'utf8', timeout: PROCESS_BUDGET_MS, windowsHide: true});
  assert.equal(result.status, 0, `${result.error || ''}\n${result.stdout}\n${result.stderr}`);
});

test('multi-megabyte logs stay file-backed and explicit stdout artifacts copy exact streamed bytes', async t => {
  const context = fixture(t);
  const output = path.join(context.root, 'observed.bin');
  const result = await commandResult({...command(context,
    'process.stdout.write(Buffer.alloc(2 * 1024 * 1024, 120))', 'large-output'), stdout: output}, context);
  assert.equal(result, '', 'ordinary commands do not materialize their stdout as a returned string');
  assert.equal(fs.statSync(output).size, 2 * 1024 * 1024);
  assert.equal(context.results[0].status, 'passed');
  assert.equal(require('node:crypto').createHash('sha256').update(fs.readFileSync(output)).digest('hex'),
    context.results[0].stdout_sha256);
});

test('bounded metadata capture rejects oversized output without losing its logs or failure status', async t => {
  const context = fixture(t);
  await assert.rejects(commandResult({...command(context,
    'process.stdout.write(Buffer.alloc(2 * 1024 * 1024, 120))', 'oversized-metadata'),
  capture: true}, context), /exceeded 1 MiB/);
  assert.equal(context.results[0].status, 'failed');
  assert.equal(fs.statSync(context.results[0].stdout).size, 2 * 1024 * 1024);
});

test('unexpected supervisor death fails closed and terminates the owned tree', async t => {
  const context = fixture(t);
  const tree = treeScript(context);
  await assert.rejects(commandResult(command(context,
    `${tree.script}\nsetTimeout(() => process.kill(process.ppid, 'SIGKILL'), 100);`,
    'lost-supervisor'), context), /Process supervisor failed/);
  const pids = JSON.parse(fs.readFileSync(tree.pidfile));
  await waitFor(() => pids.every(pid => !alive(pid)),
    'lost supervisor left its owned descendants alive', CLEANUP_WAIT_MS);
  assert.equal(context.results[0].status, 'failed');
  assert.equal(context.results[0].cleanup_confirmed, false);
  assert.deepEqual(context.results[0].cleanup_errors, ['supervisor_result_missing']);
  assert.match(fs.readFileSync(context.results[0].stdout, 'utf8'), /partial stdout/);
});

test('failed launches retain diagnostics and cannot look like clean validation', async t => {
  const context = fixture(t);
  await assert.rejects(commandResult({id: 'missing', program: path.join(context.root, 'fabric-nonexistent-executable'),
    args: [], cwd: context.root}, context), /FileNotFoundError/);
  assert.equal(context.results[0].status, 'failed');
  assert.equal(context.results[0].cleanup_confirmed, true);
  assert.ok(fs.existsSync(context.results[0].stderr));
});

test('async selected contracts keep dependencies, full marker profiles and frozen cleanup on failure', async t => {
  const context = fixture(t);
  fs.mkdirSync(path.join(context.root, 'rust', 'validation'), {recursive: true});
  fs.mkdirSync(path.join(context.root, 'rust', 'tests', 'compatibility'), {recursive: true});
  fs.writeFileSync(path.join(context.root, 'rust', 'tests', 'compatibility', 'requirements.txt'), '');
  fs.writeFileSync(path.join(context.root, 'rust', 'validation', 'impact-graph.json'),
    JSON.stringify({checks: [{id: 'prepare'}, {id: 'oracle', depends_on: ['prepare']},
      {id: 'marker'}, {id: 'unselected'}, {id: 'frozen-verify'}]}));
  context.platform = 'linux';
  context.checks = ['marker', 'oracle', 'frozen-verify'];
  context.plan = {checks: ['unselected', ...context.checks], profiles: [
    {checks: ['marker'], platforms: ['linux'], tests: [{package: 'a', target: 'lib'}]},
  ]};
  const calls = [];
  const registry = {validateRegistry: () => {}, commandsFor: id =>
    id === 'marker' ? [] : [{id, program: 'fixture', args: [], kind:
      id === 'prepare' ? 'prepare-oracle' : 'command'}]};
  await assert.rejects(runContracts(context, registry, async command => {
    calls.push(command.id);
    await delay(1);
    if (command.id === 'oracle:prepare') return JSON.stringify({rust_root: context.root});
    if (command.id === 'oracle') throw new Error('original asynchronous failure');
    return '';
  }, () => ({status: 0})), /original asynchronous failure/);
  assert.deepEqual(calls, ['oracle:prepare', 'oracle', 'frozen-verify']);
});

test('shard execution cannot silently request a partial stage', t => {
  const context = fixture(t);
  const args = ['--root', context.root, '--plan', path.join(context.root, 'plan.json'),
    '--artifacts', path.join(context.root, '..', 'artifacts'), '--shard', 'core'];
  assert.equal(options(args).shard, 'core');
  assert.throws(() => options([...args, '--stage', 'core']), /partial stage/);
});

function executionFixture(t) {
  const context = fixture(t);
  const root = path.join(context.root, 'checkout');
  const source = path.join(root, 'rust', 'engine', 'sample', 'src', 'lib.rs');
  fs.mkdirSync(path.dirname(source), {recursive: true});
  fs.mkdirSync(path.join(root, 'rust', 'validation'), {recursive: true});
  fs.writeFileSync(source, 'pub fn sample() {}\n');
  const graph = {schema: 1, components: [], profiles: [],
    checks: require('./validation-checks.cjs').CHECK_IDS.map(id => ({id}))};
  fs.writeFileSync(path.join(root, 'rust', 'validation', 'impact-graph.json'), JSON.stringify(graph));
  require('node:child_process').execFileSync('git', ['init', '--quiet'], {cwd: root});
  const packageId = 'path+fabric-core-sample#0.1.0';
  const metadata = {version: 1, workspace_members: [packageId],
    workspace_default_members: [packageId], packages: [{
      id: packageId, name: 'fabric-core-sample',
      manifest_path: path.join(root, 'rust', 'engine', 'sample', 'Cargo.toml'),
      dependencies: [], features: {default: []}, targets: [{
        name: 'fabric_core_sample', kind: ['lib'], crate_types: ['lib'],
        src_path: source, test: true, doctest: true, required_features: [],
      }],
    }]};
  const planFile = path.join(context.root, 'plan.json');
  const git = args => require('node:child_process').execFileSync('git', args, {cwd: root, encoding: 'utf8'});
  git(['add', '.']);
  git(['-c', 'user.name=Validation Fixture', '-c', 'user.email=validation@example.invalid',
    'commit', '--quiet', '-m', 'fixture']);
  fs.appendFileSync(source, '// affected fixture source\n');
  const metadataFile = path.join(context.root, 'metadata.json');
  fs.writeFileSync(metadataFile, JSON.stringify(metadata));
  const selected = require('./impact-plan.cjs').main(['--root', root,
    '--metadata', metadataFile, '--graph', path.join(root, 'rust', 'validation', 'impact-graph.json'),
    '--output', planFile], {env: {}});
  const artifacts = path.join(context.root, 'artifacts');
  return {root, source, artifacts, plan: selected, args: ['--root', root, '--plan', planFile,
    '--artifacts', artifacts, '--execute']};
}

function fakeToolchain(program, args) {
  const success = stdout => ({status: 0, stdout, cleanup_confirmed: true, cleanup_errors: []});
  const architecture = process.arch === 'arm64' ? 'aarch64' : 'x86_64';
  const host = {win32: `${architecture}-pc-windows-msvc`, linux: `${architecture}-unknown-linux-gnu`,
    darwin: `${architecture}-apple-darwin`}[process.platform];
  if (args.some(arg => arg.endsWith('engineering_preflight.py'))) {
    const get = name => args[args.indexOf(name) + 1];
    const platform = get('--platform');
    const directory = get('--artifacts');
    fs.mkdirSync(directory);
    fs.writeFileSync(path.join(directory, 'engineering-receipt.json'), JSON.stringify({
      kind: 'engineering-unit-preflight', head: get('--head'), platform,
      shard: get('--shard'), passed: true, suites: [{
        pattern: engineeringInventory.pattern, expected: engineeringInventory.count,
        discovered: engineeringInventory.count, run: engineeringInventory.count,
        passed: true, failures: 0, errors: 0, expected_failures: 0,
        skipped: platform === 'windows' ? engineeringInventory.windows_skips : [],
      }],
    }));
    return success('engineering preflight fixture\n');
  }
  if (program === 'cargo' && args[0] === '--version') return success('cargo 1.90.0 (fixture)\n');
  if (program === 'rustc') return success(`rustc 1.90.0 (fixture)\nhost: ${host}\n`);
  if (program === 'rustup') return success(`1.90.0-${host} (default)\n`);
  return success(program === 'cargo' && args[0] === 'test' ?
    'test result: ok. 1 passed; 0 failed; 0 ignored;\n' : 'fixture\n');
}

test('complete and shard receipts bind canonical selection and pass only after source verification', async t => {
  for (const sharded of [false, true]) {
    const fixture = executionFixture(t);
    const platform = {win32: 'windows', linux: 'linux', darwin: 'macos'}[process.platform];
    const shard = sharded ? require('./impact-shards.cjs').shardsForPlan(fixture.plan, platform)[0] : null;
    await main([...fixture.args, ...(shard ? ['--shard', shard.id] : [])], fakeToolchain);
    const receipt = JSON.parse(fs.readFileSync(path.join(fixture.artifacts, 'receipt.json')));
    assert.equal(receipt.status, 'passed');
    assert.equal(receipt.complete_plan, !sharded);
    assert.equal(receipt.shard, shard?.id || null);
    assert.equal(receipt.head, fixture.plan.head ?? null);
    assert.equal(receipt.selection_digest, require('./impact-ci.cjs').selectionDigest(fixture.plan));
    const records = JSON.parse(fs.readFileSync(path.join(fixture.artifacts, 'commands.json')));
    assert.ok(records.every(record => record.cleanup_confirmed === true));
    assert.ok(records.every(record => Array.isArray(record.cleanup_errors) && record.cleanup_errors.length === 0));
    assert.match(receipt.toolchain.cargo, /^cargo \d+\.\d+\.\d+\b/);
    assert.match(receipt.toolchain.rustc, /^rustc \d+\.\d+\.\d+\b/);
    const host = /(?:^|\n)host: ([a-z0-9_-]+)(?:\r?\n|$)/.exec(receipt.toolchain.rustc)?.[1];
    assert.ok(host && receipt.toolchain.rustup.includes(host));
    if (shard) assert.deepEqual(receipt.obligations, shard.obligations);
    assert.equal(receipt.source_before, receipt.source_after);
    assert.ok(fs.existsSync(path.join(fixture.artifacts, 'source-differences.json')));
    const engineeringPassed = engineeringInventory.count -
      (process.platform === 'win32' ? engineeringInventory.windows_skips.length : 0);
    assert.equal(receipt.tests_passed, 2 + (sharded ? engineeringPassed : 0));
  }
});

test('a zero-exit engineering preflight cannot satisfy its shard obligation with a false inventory', async t => {
  const fixture = executionFixture(t);
  const platform = {win32: 'windows', linux: 'linux', darwin: 'macos'}[process.platform];
  const shard = require('./impact-shards.cjs').shardsForPlan(fixture.plan, platform)[0];
  let compilationStarted = false;
  await assert.rejects(main([...fixture.args, '--shard', shard.id], (program, args) => {
    const result = fakeToolchain(program, args);
    if (args.some(arg => arg.endsWith('engineering_preflight.py'))) {
      const reportFile = path.join(args[args.indexOf('--artifacts') + 1], 'engineering-receipt.json');
      const report = JSON.parse(fs.readFileSync(reportFile));
      report.suites[0].run = engineeringInventory.count - 1;
      fs.writeFileSync(reportFile, JSON.stringify(report));
    }
    if (program === 'cargo' && args[0] === 'check') compilationStarted = true;
    return result;
  }), /Engineering preflight did not pass/);
  assert.equal(compilationStarted, false);
  const receipt = JSON.parse(fs.readFileSync(path.join(fixture.artifacts, 'receipt.json')));
  const records = JSON.parse(fs.readFileSync(path.join(fixture.artifacts, 'commands.json')));
  assert.equal(receipt.status, 'failed');
  assert.equal(receipt.complete_plan, false);
  assert.equal(records.at(-1).id, 'preflight:engineering-unit');
  assert.equal(records.at(-1).status, 'failed');
  assert.equal(records.at(-1).tests_passed, 0);
});

test('command failures and source writes cannot produce a passed shard receipt', async t => {
  for (const changeSource of [false, true]) {
    const fixture = executionFixture(t);
    const platform = {win32: 'windows', linux: 'linux', darwin: 'macos'}[process.platform];
    const shard = require('./impact-shards.cjs').shardsForPlan(fixture.plan, platform)[0];
    await assert.rejects(main([...fixture.args, '--shard', shard.id], (program, args) => {
      if (program === 'cargo' && args[0] === 'check') {
        if (changeSource) fs.appendFileSync(fixture.source, '// forbidden source write\n');
        else return {status: 19, stderr: 'original cargo failure'};
      }
      return fakeToolchain(program, args);
    }), changeSource ? /Source changed during validation/ : /Validation failed/);
    const receipt = JSON.parse(fs.readFileSync(path.join(fixture.artifacts, 'receipt.json')));
    assert.equal(receipt.status, 'failed');
    assert.equal(receipt.complete_plan, false);
    assert.ok(receipt.source_after);
    assert.ok(receipt.error);
    if (changeSource) {
      assert.notEqual(receipt.source_after, receipt.source_before);
      assert.match(receipt.source_integrity_error, /Source changed/);
    } else {
      const records = JSON.parse(fs.readFileSync(path.join(fixture.artifacts, 'commands.json')));
      assert.equal(records.at(-1).exit, 19);
      assert.equal(fs.readFileSync(records.at(-1).stderr, 'utf8'), 'original cargo failure');
    }
  }
});
