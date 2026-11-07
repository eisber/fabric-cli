// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {test} = require('node:test');
const {prepare, run} = require('./managed-bpa-acceptance.cjs');

function fixture(t) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'fabric-managed-acceptance-'));
  t.after(() => fs.rmSync(directory, {recursive: true, force: true}));
  const root = path.join(directory, 'source');
  const managed = path.join(root, 'rust/sdk/semantic-model/dotnet/managed');
  fs.mkdirSync(path.join(managed, 'Bpa/Tests'), {recursive: true});
  for (const project of ['BoundaryContractTests.csproj', 'Bpa/Tests/Fabric.SemanticModel.Bpa.Tests.csproj']) {
    fs.writeFileSync(path.join(managed, project), '<Project/>');
  }
  const pin = Buffer.from('{"sdk":{"version":"8.0.425"}}\r\n');
  fs.writeFileSync(path.join(managed, 'global.json'), pin);
  return {root, artifacts: path.join(directory, 'artifacts'), pin};
}

test('managed acceptance isolates every referenced project output and preserves the SDK pin and locks', t => {
  const {root, artifacts, pin} = fixture(t);
  const commands = prepare(root, artifacts);
  assert.deepEqual(fs.readFileSync(path.join(artifacts, 'global.json')), pin);
  const props = fs.readFileSync(path.join(artifacts, 'Directory.Build.props'), 'utf8');
  assert.ok(props.includes('/obj/$(MSBuildProjectName)/'));
  assert.ok(props.includes('/bin/$(MSBuildProjectName)/'));
  assert.ok(props.includes('/packages/'));
  assert.equal(commands.length, 4);
  for (const command of commands) assert.equal(command.cwd, artifacts);
  for (const command of commands.filter(command => command.id.startsWith('publish'))) {
    assert.ok(path.isAbsolute(command.args[1]));
    assert.ok(command.args.includes('-p:RestoreLockedMode=true'));
    assert.ok(command.args.includes('-p:RestorePackagesWithLockFile=true'));
    assert.ok(command.args.includes(`-p:RestoreConfigFile=${path.join(root, 'rust/NuGet.Config')}`));
    assert.ok(!command.args.some(argument => /Audit|ignore-failed|force/i.test(argument)));
  }
});

test('managed acceptance refuses checkout outputs or an existing artifact directory', t => {
  const {root, artifacts} = fixture(t);
  assert.throws(() => prepare(root, path.join(root, 'out')), /outside/);
  assert.throws(() => prepare(root, 'relative'), /absolute/);
  fs.mkdirSync(artifacts);
  assert.throws(() => prepare(root, artifacts), /fresh/);
});

test('managed acceptance runs both original suites and stops explicitly on a failed publish', t => {
  const {root, artifacts} = fixture(t);
  const calls = [];
  run(root, artifacts, (program, args, options) => {
    calls.push({program, args, options});
    return {status: 0, stdout: 'passed', stderr: ''};
  });
  assert.equal(calls.length, 4);
  assert.ok(calls[1].args[0].endsWith('Fabric.SemanticModel.Bpa.Tests.dll'));
  assert.ok(calls[3].args[0].endsWith('BoundaryContractTests.dll'));
  assert.ok(calls.every(call => call.options.timeout > 0 && call.options.timeout <= 1200000));
  const second = path.join(path.dirname(artifacts), 'failed');
  let count = 0;
  assert.throws(() => run(root, second, () => {
    count++;
    return {status: 1, stdout: '', stderr: 'publish refused'};
  }), /publish-0/);
  assert.equal(count, 1);
  assert.equal(JSON.parse(fs.readFileSync(path.join(second, 'commands.json')))[0].exit, 1);
});
