// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const {spawn} = require('node:child_process');

/** Run one owned tree, preserving partial logs and awaiting confirmed cleanup. */
async function executeOwned(program, args, options) {
  const resultFile = `${options.stdout}.process-result.json`;
  const deadline = Math.min(Date.now() + options.timeout, options.deadline || Infinity);
  fs.writeFileSync(options.stdout, '');
  fs.writeFileSync(options.stderr, '');
  const config = JSON.stringify({
    program, args, cwd: options.cwd, env: options.env, timeout_ms: options.timeout,
    deadline_ms: deadline,
    stdout: options.stdout, stderr: options.stderr, result: resultFile,
  });
  let supervisorError = '';
  let cancellation;
  let watchdog;
  let emergency;
  let result;
  let ownedPid;
  try {
    result = await new Promise(resolve => {
      const child = spawn(options.python || process.env.PYTHON || 'python',
        ['-B', path.join(__dirname, 'validation-process.py')], {
          stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true, shell: false,
          env: {...process.env, PYTHONDONTWRITEBYTECODE: '1'},
        });
      let metadata = '';
      child.stdout.on('data', chunk => {
        metadata = (metadata + chunk.toString('utf8')).slice(-1024);
        if (metadata.includes('\n')) {
          try { ownedPid = JSON.parse(metadata.trim()).pid; } catch {}
        }
      });
      // Do not write credentials/effective environment into uploaded artifacts.
      child.stdin.write(`${config}\n`);
      const force = () => {
        if (process.platform !== 'win32' && Number.isSafeInteger(ownedPid) && ownedPid > 0) {
          try { process.kill(-ownedPid, 'SIGKILL'); } catch {}
        }
        child.kill('SIGKILL'); // Only our supervisor; Windows closes its kill-on-close Job.
      };
      const stop = (reason = 'x') => {
        cancellation ||= reason;
        if (!child.stdin.destroyed && !child.stdin.writableEnded) child.stdin.end(reason);
        clearTimeout(watchdog);
        clearTimeout(emergency);
        // Cancellation must not wait for a long command's original deadline.
        emergency = setTimeout(force, 7000);
      };
      const abort = () => stop();
      const signal = options.signal;
      child.stdin.on('error', () => {});
      child.stderr.on('data', chunk => {
        // Only supervisor diagnostics travel through a bounded pipe.
        supervisorError = (supervisorError + chunk.toString('utf8')).slice(-16384);
      });
      // The Python deadline owns normal shutdown; this catches startup/IPC failure.
      const remaining = Math.max(0, deadline - Date.now());
      watchdog = setTimeout(() => stop('t'), remaining + 1000);
      emergency = setTimeout(force, remaining + 8000);
      const finish = (code, error) => {
        clearTimeout(watchdog);
        clearTimeout(emergency);
        signal?.removeEventListener('abort', abort);
        resolve({code, error});
      };
      child.once('error', error => finish(null, error));
      child.once('close', code => finish(code));
      signal?.addEventListener('abort', abort, {once: true});
      if (signal?.aborted) abort();
    });
    if (fs.existsSync(resultFile)) {
      const outcome = JSON.parse(fs.readFileSync(resultFile, 'utf8'));
      if (result.code !== 0 && !outcome.error) {
        outcome.error = `Process supervisor exited ${result.code}: ${supervisorError}`;
        outcome.cleanup_confirmed = false;
      }
      return {...outcome, error: outcome.error ? Object.assign(new Error(outcome.error), {
        code: outcome.timed_out ? 'ETIMEDOUT' : outcome.cancelled ? 'ECANCELLED' : 'EPROCESS',
      }) : null};
    }
    if (Number.isSafeInteger(ownedPid) && ownedPid > 0) {
      try {
        process.kill(process.platform === 'win32' ? ownedPid : -ownedPid, 'SIGKILL');
      } catch (error) {
        if (error.code !== 'ESRCH') supervisorError += `; owned cleanup: ${error.message}`;
      }
    }
    return {status: null, signal: null, timed_out: cancellation === 't', cancelled: cancellation === 'x',
      cleanup_confirmed: false, cleanup_errors: ['supervisor_result_missing'],
      error: result.error || new Error(`Process supervisor failed (${result.code}): ${supervisorError}`)};
  } finally {
    clearTimeout(watchdog);
    clearTimeout(emergency);
    fs.rmSync(resultFile, {force: true});
    fs.rmSync(`${resultFile}.partial`, {force: true});
  }
}

module.exports = {executeOwned};
