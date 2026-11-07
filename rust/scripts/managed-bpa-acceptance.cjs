// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const {spawnSync} = require('node:child_process');

function xml(value) {
  return value.replaceAll('&', '&amp;').replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;').replaceAll('"', '&quot;').replaceAll("'", '&apos;');
}

function prepare(root, artifacts) {
  root = fs.realpathSync(root);
  if (!path.isAbsolute(artifacts)) throw new Error('Managed artifacts must be absolute');
  artifacts = path.resolve(artifacts);
  if (artifacts === root || artifacts.startsWith(`${root}${path.sep}`)) {
    throw new Error('Managed artifacts must be outside the checkout');
  }
  if (fs.existsSync(artifacts)) throw new Error('Managed acceptance needs a fresh artifact directory');
  const managed = path.join(root, 'rust/sdk/semantic-model/dotnet/managed');
  const projects = [
    ['Bpa/Tests/Fabric.SemanticModel.Bpa.Tests.csproj', 'Fabric.SemanticModel.Bpa.Tests.dll'],
    ['BoundaryContractTests.csproj', 'BoundaryContractTests.dll'],
  ];
  for (const [project] of projects) {
    if (!fs.statSync(path.join(managed, project)).isFile()) throw new Error('Managed test project missing');
  }
  const sdkPin = fs.readFileSync(path.join(managed, 'global.json'));
  fs.mkdirSync(artifacts, {recursive: true});
  fs.writeFileSync(path.join(artifacts, 'global.json'), sdkPin);
  const props = path.join(artifacts, 'Directory.Build.props');
  const artifactPath = xml(artifacts.replaceAll('\\', '/'));
  fs.writeFileSync(props, `<Project>
  <PropertyGroup>
    <BaseIntermediateOutputPath>${artifactPath}/obj/$(MSBuildProjectName)/</BaseIntermediateOutputPath>
    <BaseOutputPath>${artifactPath}/bin/$(MSBuildProjectName)/</BaseOutputPath>
    <RestorePackagesPath>${artifactPath}/packages/</RestorePackagesPath>
  </PropertyGroup>
</Project>
`);
  return projects.flatMap(([project, assembly], index) => {
    const output = path.join(artifacts, `published-${index}`);
    return [
      {id: `publish-${index}`, program: 'dotnet', cwd: artifacts,
        args: ['publish', path.join(managed, project), '--configuration', 'Release',
          '--output', output, '--self-contained', 'false', '--nologo',
          `-p:DirectoryBuildPropsPath=${props}`,
          `-p:RestoreConfigFile=${path.join(root, 'rust/NuGet.Config')}`,
          '-p:RestoreLockedMode=true', '-p:RestorePackagesWithLockFile=true',
          '-p:NuGetInteractive=false', '-nodeReuse:false']},
      {id: `acceptance-${index}`, program: 'dotnet', cwd: artifacts,
        args: [path.join(output, assembly)]},
    ];
  });
}

function run(root, artifacts, execute = spawnSync) {
  const commands = prepare(root, artifacts);
  const deadline = Date.now() + 30 * 60 * 1000;
  const results = [];
  for (const command of commands) {
    const remaining = deadline - Date.now();
    if (remaining <= 0) throw new Error('Managed acceptance budget exhausted');
    const result = execute(command.program, command.args, {
      cwd: command.cwd, timeout: Math.min(20 * 60 * 1000, remaining),
      encoding: 'utf8', windowsHide: true, maxBuffer: 8 * 1024 * 1024,
    });
    fs.writeFileSync(path.join(artifacts, `${command.id}.stdout.log`), result.stdout || '');
    fs.writeFileSync(path.join(artifacts, `${command.id}.stderr.log`), result.stderr || '');
    results.push({...command, exit: result.status, signal: result.signal || null,
      error: result.error?.message || null});
    fs.writeFileSync(path.join(artifacts, 'commands.json'), `${JSON.stringify(results, null, 2)}\n`);
    if (result.error || result.status !== 0) {
      throw new Error(`Managed acceptance failed: ${command.id}; inspect retained logs`);
    }
    console.log(`${command.id}: passed`);
  }
}

module.exports = {prepare, run};
if (require.main === module) {
  try {
    if (process.argv.length !== 3) throw new Error('Usage: managed-bpa-acceptance.cjs <absolute-artifacts>');
    run(path.resolve(__dirname, '..', '..'), process.argv[2]);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
