#!/usr/bin/env node

import { createHash } from 'node:crypto';
import { createReadStream, readFileSync } from 'node:fs';
import {
  chmod,
  copyFile,
  lstat,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  readlink,
  realpath,
  rm,
  stat,
  writeFile,
} from 'node:fs/promises';
import { createRequire } from 'node:module';
import os from 'node:os';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';

import {
  stageMacosRuntimeResources,
  verifyMacosRuntimeResourceManifest,
} from './lib/macos-package-resources.mjs';

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(SCRIPT_DIR, '..');
const DESKTOP_DIR = path.join(REPO_ROOT, 'apps', 'desktop');
const PRODUCT_NAME = 'Computer Use';
const BUNDLE_ID = 'com.lingxi.computer-use';
const MINIMUM_MACOS_VERSION = '14.2';
const TARGET_ARCH = 'arm64';
const REQUIRED_APP_FILES = [
  'out/main/index.js',
  'out/main/imageWorker.js',
  'out/main/localDecisionWorker.js',
  'out/preload/index.cjs',
  'out/renderer/index.html',
];
const REQUIRED_OUT_FILES = REQUIRED_APP_FILES.map((relativePath) => relativePath.slice('out/'.length));
const BUNDLED_WORKSPACE_DEPENDENCIES = new Set([
  '@computer-use/decision-worker',
  '@computer-use/desktop-api',
]);
const PRODUCTION_DEPENDENCY_ROOTS = ['ajv', 'onnxruntime-node'];
const RESOURCE_TOP_LEVEL_PATHS = [
  'bin',
  'fixtures',
  'models',
  'LICENSES',
  'resource-manifest.json',
];

function usage() {
  return [
    'Usage:',
    '  node scripts/package-macos-local-app.mjs --output /absolute/new/output-dir --native-bin-dir /absolute/bin-dir',
    '',
    'Packages the existing apps/desktop/out tree as a local unsigned arm64 .app.',
    'Build the Electron output and native executables before running this command.',
    'The output directory must not already exist.',
  ].join('\n');
}

function parseArgs(argv) {
  const values = {};
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--help' || arg === '-h') return { help: true };
    if (arg !== '--output' && arg !== '--native-bin-dir') {
      throw new Error(`Unknown argument: ${arg}`);
    }
    const value = argv[index + 1];
    if (!value || value.startsWith('--')) {
      throw new Error(`Expected a value after ${arg}`);
    }
    const key = arg === '--output' ? 'output' : 'nativeBinDir';
    if (values[key]) throw new Error(`${arg} may only be provided once`);
    values[key] = value;
    index += 1;
  }

  for (const key of ['output', 'nativeBinDir']) {
    if (!values[key]) throw new Error(`Missing required --${key === 'output' ? 'output' : 'native-bin-dir'}`);
    if (!path.isAbsolute(values[key])) {
      throw new Error(`--${key === 'output' ? 'output' : 'native-bin-dir'} must be an absolute path`);
    }
    values[key] = path.resolve(values[key]);
  }
  return values;
}

function run(command, args, options = {}) {
  try {
    return execFileSync(command, args, {
      cwd: options.cwd ?? REPO_ROOT,
      encoding: options.encoding ?? 'utf8',
      stdio: options.stdio ?? ['ignore', 'pipe', 'pipe'],
      maxBuffer: 32 * 1024 * 1024,
    });
  } catch (error) {
    const stderr = error.stderr?.toString().trim();
    const stdout = error.stdout?.toString().trim();
    const details = [stderr, stdout].filter(Boolean).join('\n');
    throw new Error(`${command} ${args.join(' ')} failed${details ? `:\n${details}` : ''}`, {
      cause: error,
    });
  }
}

async function readJson(filePath) {
  return JSON.parse(await readFile(filePath, 'utf8'));
}

async function pathExists(filePath) {
  try {
    await lstat(filePath);
    return true;
  } catch (error) {
    if (error.code === 'ENOENT') return false;
    throw error;
  }
}

async function requireDirectory(filePath, label) {
  const info = await stat(filePath).catch((error) => {
    throw new Error(`${label} does not exist: ${filePath}`, { cause: error });
  });
  if (!info.isDirectory()) throw new Error(`${label} is not a directory: ${filePath}`);
}

function assertMacHost() {
  if (process.platform !== 'darwin') {
    throw new Error('Local .app assembly must run on macOS so the installed Electron runtime can be reused.');
  }
  const [major, minor] = process.versions.node.split('.').map(Number);
  if (major < 22 || (major === 22 && minor < 12)) {
    throw new Error(`@electron/packager requires Node.js 22.12 or newer; found ${process.versions.node}.`);
  }
}

async function verifyOutputPath(outputPath) {
  if (await pathExists(outputPath)) {
    throw new Error(`Refusing to overwrite an existing output path: ${outputPath}`);
  }
  await requireDirectory(path.dirname(outputPath), 'Output parent');
  const repoPath = await realpath(REPO_ROOT);
  const outputParent = await realpath(path.dirname(outputPath));
  const relative = path.relative(repoPath, outputParent);
  if (relative === '' || relative === '..' || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative)) {
    throw new Error('The output directory must be below the repository root and must be new.');
  }
}

async function verifyDebugNativeBinDir(nativeBinDir) {
  const suppliedPath = await realpath(nativeBinDir);
  const expectedPath = await realpath(path.join(REPO_ROOT, 'target', 'debug'));
  if (path.resolve(nativeBinDir) !== suppliedPath || suppliedPath !== expectedPath) {
    throw new Error(`This local package accepts only the canonical debug binary directory: ${expectedPath}`);
  }
  return suppliedPath;
}

function getSourceRevision() {
  const revision = run('git', ['rev-parse', 'HEAD']).trim();
  const status = run('git', ['status', '--porcelain', '--untracked-files=normal']).trim();
  return {
    revision,
    workingTreeModified: status.length > 0,
  };
}

async function assertNoSymlinks(rootPath, label) {
  const entries = await readdir(rootPath, { withFileTypes: true });
  for (const entry of entries) {
    const entryPath = path.join(rootPath, entry.name);
    const info = await lstat(entryPath);
    if (info.isSymbolicLink()) {
      throw new Error(`${label} contains a symbolic link: ${path.relative(rootPath, entryPath)}`);
    }
    if (info.isDirectory()) await assertNoSymlinks(entryPath, label);
    else if (!info.isFile()) throw new Error(`${label} contains an unsupported file type: ${entryPath}`);
  }
}

async function copyTreeWithoutSymlinks(sourcePath, destinationPath, { skipNodeModules = false } = {}) {
  const sourceInfo = await lstat(sourcePath);
  if (sourceInfo.isSymbolicLink()) throw new Error(`Refusing to copy a symbolic link: ${sourcePath}`);
  if (sourceInfo.isDirectory()) {
    await mkdir(destinationPath, { recursive: true });
    const children = await readdir(sourcePath, { withFileTypes: true });
    for (const child of children) {
      if (skipNodeModules && child.name === 'node_modules') continue;
      await copyTreeWithoutSymlinks(
        path.join(sourcePath, child.name),
        path.join(destinationPath, child.name),
        { skipNodeModules },
      );
    }
    await chmod(destinationPath, sourceInfo.mode & 0o777).catch(() => {});
    return;
  }
  if (!sourceInfo.isFile()) throw new Error(`Refusing to copy an unsupported file type: ${sourcePath}`);
  await mkdir(path.dirname(destinationPath), { recursive: true });
  await copyFile(sourcePath, destinationPath);
  await chmod(destinationPath, sourceInfo.mode & 0o777);
}

function findPackageRoot(entryPath, packageName) {
  let current = path.dirname(entryPath);
  while (true) {
    const manifestPath = path.join(current, 'package.json');
    try {
      const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
      if (manifest.name === packageName) return current;
    } catch (error) {
      if (error.code !== 'ENOENT' && !(error instanceof SyntaxError)) throw error;
    }
    const parent = path.dirname(current);
    if (parent === current) break;
    current = parent;
  }
  throw new Error(`Could not find package.json for resolved dependency ${packageName} at ${entryPath}`);
}

function packagePathForNodeModules(nodeModulesPath, packageName) {
  return path.join(nodeModulesPath, ...packageName.split('/'));
}

function isWorkspacePackagePath(packageRoot) {
  const relative = path.relative(REPO_ROOT, packageRoot);
  return relative === 'apps' || relative.startsWith(`apps${path.sep}`)
    || relative === 'packages' || relative.startsWith(`packages${path.sep}`);
}

async function resolveInstalledPackage(packageName, requesterSourceDir, optional) {
  try {
    const requesterRequire = createRequire(path.join(requesterSourceDir, '__computer_use_package_resolver__.cjs'));
    let entryPath;
    try {
      entryPath = requesterRequire.resolve(packageName);
    } catch (error) {
      if (error.code !== 'MODULE_NOT_FOUND' && error.code !== 'ERR_PACKAGE_PATH_NOT_EXPORTED') throw error;
      entryPath = requesterRequire.resolve(`${packageName}/package.json`);
    }
    const sourceRoot = findPackageRoot(entryPath, packageName);
    if (isWorkspacePackagePath(sourceRoot)) {
      if (BUNDLED_WORKSPACE_DEPENDENCIES.has(packageName)) return null;
      throw new Error(`Workspace package ${packageName} would be copied as an external dependency.`);
    }
    return { sourceRoot, manifest: await readJson(path.join(sourceRoot, 'package.json')) };
  } catch (error) {
    if (optional && (error.code === 'MODULE_NOT_FOUND' || error.code === 'ERR_PACKAGE_PATH_NOT_EXPORTED')) return null;
    throw new Error(`Could not resolve production dependency ${packageName} from ${requesterSourceDir}.`, {
      cause: error,
    });
  }
}

async function readInstalledManifest(packageRoot) {
  try {
    return await readJson(path.join(packageRoot, 'package.json'));
  } catch (error) {
    if (error.code === 'ENOENT') return null;
    throw error;
  }
}

async function installProductionDependency({
  packageName,
  requesterSourceDir,
  requesterDestinationDir,
  stagedAppDir,
  optional = false,
}) {
  const resolved = await resolveInstalledPackage(packageName, requesterSourceDir, optional);
  if (!resolved) return null;
  const { sourceRoot, manifest } = resolved;
  const globalTarget = packagePathForNodeModules(path.join(stagedAppDir, 'node_modules'), packageName);
  const globalManifest = await readInstalledManifest(globalTarget);
  let destinationRoot = globalTarget;
  if (globalManifest && globalManifest.version !== manifest.version) {
    destinationRoot = packagePathForNodeModules(
      path.join(requesterDestinationDir, 'node_modules'),
      packageName,
    );
  }

  const existingManifest = await readInstalledManifest(destinationRoot);
  if (existingManifest) {
    if (existingManifest.name !== packageName || existingManifest.version !== manifest.version) {
      throw new Error(`Conflicting production dependency ${packageName} at ${destinationRoot}`);
    }
    return { destinationRoot, manifest };
  }

  await copyTreeWithoutSymlinks(sourceRoot, destinationRoot, { skipNodeModules: true });

  const required = Object.entries(manifest.dependencies ?? {}).map(([name]) => [name, false]);
  const optionalDependencies = Object.entries(manifest.optionalDependencies ?? {}).map(([name]) => [name, true]);
  const optionalPeerNames = new Set(
    Object.entries(manifest.peerDependenciesMeta ?? {})
      .filter(([, metadata]) => metadata.optional)
      .map(([name]) => name),
  );
  const peerDependencies = Object.keys(manifest.peerDependencies ?? {}).map((name) => [name, optionalPeerNames.has(name)]);
  const dependencies = new Map([...required, ...optionalDependencies, ...peerDependencies]);

  for (const [dependencyName, isOptional] of dependencies) {
    await installProductionDependency({
      packageName: dependencyName,
      requesterSourceDir: sourceRoot,
      requesterDestinationDir: destinationRoot,
      stagedAppDir,
      optional: isOptional,
    });
  }

  return { destinationRoot, manifest };
}

async function buildProductionDependencyTree(appSourceDir) {
  const resolvedVersions = {};
  for (const packageName of PRODUCTION_DEPENDENCY_ROOTS) {
    const dependency = await installProductionDependency({
      packageName,
      requesterSourceDir: DESKTOP_DIR,
      requesterDestinationDir: appSourceDir,
      stagedAppDir: appSourceDir,
    });
    if (!dependency) throw new Error(`Required production dependency was skipped: ${packageName}`);
    resolvedVersions[packageName] = dependency.manifest.version;
  }
  return resolvedVersions;
}

async function keepOnlyDarwinArm64OnnxRuntime(appSourceDir) {
  const packageRoot = path.join(appSourceDir, 'node_modules', 'onnxruntime-node');
  const napiRoot = path.join(packageRoot, 'bin', 'napi-v6');
  const arm64Runtime = path.join(napiRoot, 'darwin', TARGET_ARCH);
  const requiredRuntimeFiles = [
    'onnxruntime_binding.node',
    'libonnxruntime.1.dylib',
    'libonnxruntime.1.30.0.dylib',
  ];
  await requireDirectory(arm64Runtime, 'ONNX Runtime arm64 native directory');
  for (const fileName of requiredRuntimeFiles) {
    if (!(await pathExists(path.join(arm64Runtime, fileName)))) {
      throw new Error(`ONNX Runtime package is missing ${path.join(arm64Runtime, fileName)}`);
    }
  }
  const selectedRuntime = path.join(path.dirname(packageRoot), '.onnxruntime-darwin-arm64');
  await rm(selectedRuntime, { recursive: true, force: true });
  await copyTreeWithoutSymlinks(arm64Runtime, selectedRuntime);
  await rm(napiRoot, { recursive: true, force: true });
  await mkdir(arm64Runtime, { recursive: true });
  await copyTreeWithoutSymlinks(selectedRuntime, arm64Runtime);
  await rm(selectedRuntime, { recursive: true, force: true });
}

async function prepareAppSource(appSourceDir, desktopPackage, sourceRevision) {
  const outDir = path.join(DESKTOP_DIR, 'out');
  await requireDirectory(outDir, 'Electron build output');
  for (const relativePath of REQUIRED_OUT_FILES) {
    if (!(await pathExists(path.join(outDir, relativePath)))) {
      throw new Error(`Electron build output is incomplete; missing ${path.join(outDir, relativePath)}`);
    }
  }

  await mkdir(appSourceDir, { recursive: true });
  await copyTreeWithoutSymlinks(outDir, path.join(appSourceDir, 'out'));
  const dependencies = await buildProductionDependencyTree(appSourceDir);
  if (dependencies['onnxruntime-node'] !== '1.30.0') {
    throw new Error(`The packaged Laya worker requires onnxruntime-node@1.30.0; found ${dependencies['onnxruntime-node']}.`);
  }
  await keepOnlyDarwinArm64OnnxRuntime(appSourceDir);

  const packageManifest = {
    name: 'computer-use',
    productName: PRODUCT_NAME,
    version: desktopPackage.version,
    private: true,
    type: 'module',
    main: desktopPackage.main,
    dependencies,
    buildSourceRevision: sourceRevision.revision,
  };
  await writeFile(path.join(appSourceDir, 'package.json'), `${JSON.stringify(packageManifest, null, 2)}\n`);
  await assertNoSymlinks(appSourceDir, 'Packaged app source');
}

function getElectronPackage() {
  const desktopRequire = createRequire(path.join(DESKTOP_DIR, 'package.json'));
  const electronPackagePath = desktopRequire.resolve('electron/package.json');
  return {
    desktopRequire,
    electronPackagePath,
  };
}

async function prepareLocalElectronZip(tempRoot, electronPackagePath, electronPackage) {
  const electronDist = path.resolve(path.dirname(electronPackagePath), 'dist');
  const electronApp = path.join(electronDist, 'Electron.app');
  await requireDirectory(electronApp, 'Installed Electron runtime app');
  const electronExecutable = path.join(electronApp, 'Contents', 'MacOS', 'Electron');
  const archOutput = run('lipo', ['-archs', electronExecutable]).trim().split(/\s+/);
  if (!archOutput.includes(TARGET_ARCH)) {
    throw new Error(`Installed Electron runtime does not contain ${TARGET_ARCH}: ${archOutput.join(', ')}`);
  }

  const runtimeZipDir = path.join(tempRoot, 'electron-runtime-zips');
  await mkdir(runtimeZipDir, { recursive: true });
  const zipName = `electron-v${electronPackage.version}-darwin-${TARGET_ARCH}.zip`;
  const zipPath = path.join(runtimeZipDir, zipName);
  run('ditto', ['-c', '-k', '--norsrc', '--noextattr', '--keepParent', electronApp, zipPath], {
    stdio: 'inherit',
  });
  if (!(await pathExists(zipPath))) throw new Error(`Local Electron runtime archive was not created: ${zipPath}`);
  return { electronApp, runtimeZipDir, zipPath };
}

async function loadPackager(desktopRequire) {
  const modulePath = desktopRequire.resolve('@electron/packager');
  const packagerModule = await import(pathToFileURL(modulePath).href);
  if (typeof packagerModule.packager !== 'function') {
    throw new Error('@electron/packager does not expose its documented packager() API.');
  }
  return packagerModule.packager;
}

async function copyRuntimeResources(resourceStage, root, sourceRevision, nativeBinDir) {
  const staged = await stageMacosRuntimeResources({
    root,
    destination: resourceStage,
    architecture: TARGET_ARCH,
    buildKind: 'debug',
    sourceRevision: sourceRevision.revision,
    nativeBinDir,
  });
  await verifyMacosRuntimeResourceManifest({
    root,
    destination: staged.destination,
  });

  const extras = RESOURCE_TOP_LEVEL_PATHS.map((relativePath) => path.join(staged.destination, relativePath));
  for (const extraPath of extras) {
    if (!(await pathExists(extraPath))) throw new Error(`Staged runtime resource is missing: ${extraPath}`);
  }
  return { staged, extras };
}

async function appBundlePathFromPackagerOutput(packagerOutputPath, outputPath) {
  const resolvedOutputPath = path.resolve(outputPath);
  const resolvedPackagerPath = path.resolve(packagerOutputPath);
  const relativePackagerPath = path.relative(resolvedOutputPath, resolvedPackagerPath);
  if (relativePackagerPath.startsWith('..') || path.isAbsolute(relativePackagerPath)) {
    throw new Error(`Packager returned a path outside the output directory: ${packagerOutputPath}`);
  }

  const appPath = path.basename(resolvedPackagerPath) === `${PRODUCT_NAME}.app`
    ? resolvedPackagerPath
    : path.join(resolvedPackagerPath, `${PRODUCT_NAME}.app`);
  const appInfo = await lstat(appPath).catch((error) => {
    throw new Error(`Packager output is missing the expected ${PRODUCT_NAME}.app bundle at ${appPath}`, {
      cause: error,
    });
  });
  if (appInfo.isSymbolicLink() || !appInfo.isDirectory()) {
    throw new Error(`Packager output is not a regular app bundle directory: ${appPath}`);
  }
  return appPath;
}

function parsePlist(plistPath) {
  const encoded = run('plutil', ['-convert', 'json', '-o', '-', plistPath]);
  return JSON.parse(encoded);
}

function assertArchitecture(binaryPath, architecture) {
  const archs = run('lipo', ['-archs', binaryPath]).trim().split(/\s+/);
  if (!archs.includes(architecture)) {
    throw new Error(`Expected ${architecture} architecture in ${binaryPath}; found ${archs.join(', ')}`);
  }
  if (archs.some((arch) => arch !== architecture)) {
    throw new Error(`Expected a single ${architecture} slice in ${binaryPath}; found ${archs.join(', ')}`);
  }
}

async function assertRegularFile(filePath, label) {
  const info = await lstat(filePath);
  if (info.isSymbolicLink() || !info.isFile()) throw new Error(`${label} must be a regular file: ${filePath}`);
  return info;
}

async function verifyPackagedApp(appPath, resourceManifestPath, installedElectronResourceNames) {
  const contentsPath = path.join(appPath, 'Contents');
  const resourcesPath = path.join(contentsPath, 'Resources');
  const infoPlistPath = path.join(contentsPath, 'Info.plist');
  const info = parsePlist(infoPlistPath);
  if (info.CFBundleIdentifier !== BUNDLE_ID) {
    throw new Error(`Unexpected app bundle identifier: ${info.CFBundleIdentifier}`);
  }
  if (info.CFBundleName !== PRODUCT_NAME) {
    throw new Error(`Unexpected app bundle name: ${info.CFBundleName}`);
  }
  if (info.LSMinimumSystemVersion !== MINIMUM_MACOS_VERSION) {
    throw new Error(`Expected LSMinimumSystemVersion=${MINIMUM_MACOS_VERSION}, found ${info.LSMinimumSystemVersion}`);
  }

  const appSource = path.join(resourcesPath, 'app');
  for (const relativePath of REQUIRED_APP_FILES) {
    await assertRegularFile(path.join(appSource, relativePath), `Packaged app entry ${relativePath}`);
  }
  await assertRegularFile(path.join(appSource, 'package.json'), 'Packaged app package.json');
  await assertRegularFile(path.join(appSource, 'node_modules', 'ajv', 'package.json'), 'Packaged ajv package');
  await assertRegularFile(path.join(appSource, 'node_modules', 'onnxruntime-node', 'package.json'), 'Packaged ONNX package');

  const onnxRuntimeDir = path.join(appSource, 'node_modules', 'onnxruntime-node', 'bin', 'napi-v6', 'darwin', TARGET_ARCH);
  for (const fileName of [
    'onnxruntime_binding.node',
    'libonnxruntime.1.dylib',
    'libonnxruntime.1.30.0.dylib',
  ]) {
    const binaryPath = path.join(onnxRuntimeDir, fileName);
    await assertRegularFile(binaryPath, `Packaged ONNX native file ${fileName}`);
    assertArchitecture(binaryPath, TARGET_ARCH);
  }

  const nativeBinsPath = path.join(resourcesPath, 'bin');
  const nativeBinNames = (await readdir(nativeBinsPath)).sort();
  const expectedNativeBins = [
    'cu-auth',
    'cu-device',
    'cu-host',
    'cu-macos-device-guard',
    'cu-macos-keychain',
    'cu-macos-owned-document-launcher',
    'cu-macos-source',
    'cu-source',
  ].sort();
  if (JSON.stringify(nativeBinNames) !== JSON.stringify(expectedNativeBins)) {
    throw new Error(`Unexpected packaged native binaries: ${nativeBinNames.join(', ')}`);
  }
  for (const binaryName of expectedNativeBins) {
    const binaryPath = path.join(nativeBinsPath, binaryName);
    const info = await assertRegularFile(binaryPath, `Packaged native binary ${binaryName}`);
    if ((info.mode & 0o111) === 0) throw new Error(`Packaged native binary is not executable: ${binaryPath}`);
    assertArchitecture(binaryPath, TARGET_ARCH);
  }

  const readonlyFixture = path.join(resourcesPath, 'fixtures', 'headless', 'readonly-page.html');
  await assertRegularFile(readonlyFixture, 'Packaged read-only browser fixture');
  await assertRegularFile(resourceManifestPath, 'Packaged resource manifest');
  const installedResourceRoots = await readdir(resourcesPath, { withFileTypes: true });
  const allowedAdditionalTopLevelPaths = installedResourceRoots
    .map((entry) => entry.name)
    .filter((name) => !RESOURCE_TOP_LEVEL_PATHS.includes(name));
  for (const name of allowedAdditionalTopLevelPaths) {
    if (!installedElectronResourceNames.has(name) && name !== 'app') {
      throw new Error(`Unexpected app-owned Contents/Resources entry: ${name}`);
    }
  }
  await verifyMacosRuntimeResourceManifest({
    root: REPO_ROOT,
    destination: resourcesPath,
    allowedAdditionalTopLevelPaths,
  });

  const appExecutable = path.join(contentsPath, 'MacOS', info.CFBundleExecutable);
  assertArchitecture(appExecutable, TARGET_ARCH);
  return {
    info,
    appSource,
    resourcesPath,
    appExecutable,
    allowedAdditionalTopLevelPaths,
  };
}

async function hashFile(filePath) {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(filePath)) hash.update(chunk);
  return hash.digest('hex');
}

function isAllowedFrameworkSymlink(relativePath) {
  return relativePath.startsWith(`Contents${path.sep}Frameworks${path.sep}`);
}

async function createAppFileManifest(appPath) {
  const entries = [];
  async function visit(currentPath, relativePath) {
    const children = await readdir(currentPath, { withFileTypes: true });
    children.sort((left, right) => left.name.localeCompare(right.name));
    for (const child of children) {
      const childPath = path.join(currentPath, child.name);
      const childRelative = relativePath ? path.join(relativePath, child.name) : child.name;
      const info = await lstat(childPath);
      if (info.isSymbolicLink()) {
        if (!isAllowedFrameworkSymlink(childRelative)) {
          throw new Error(`Unexpected symbolic link in app bundle: ${childRelative}`);
        }
        const target = await readlink(childPath);
        const resolvedTarget = path.resolve(path.dirname(childPath), target);
        const relativeTarget = path.relative(appPath, resolvedTarget);
        if (relativeTarget.startsWith('..') || path.isAbsolute(relativeTarget)) {
          throw new Error(`Framework symlink escapes the app bundle: ${childRelative} -> ${target}`);
        }
        entries.push({
          path: childRelative,
          type: 'symlink',
          linkTarget: target,
          byteLength: 0,
          executable: false,
        });
        continue;
      }
      if (info.isDirectory()) {
        await visit(childPath, childRelative);
        continue;
      }
      if (!info.isFile()) throw new Error(`Unexpected file type in app bundle: ${childRelative}`);
      entries.push({
        path: childRelative,
        type: 'file',
        byteLength: info.size,
        sha256: await hashFile(childPath),
        executable: (info.mode & 0o111) !== 0,
      });
    }
  }
  await visit(appPath, '');
  return entries;
}

async function writeAssemblyManifest({
  outputPath,
  appPath,
  electronVersion,
  sourceRevision,
  nativeBinDir,
  resourceManifest,
}) {
  const appFiles = await createAppFileManifest(appPath);
  const manifest = {
    schemaVersion: 1,
    product: PRODUCT_NAME,
    bundleIdentifier: BUNDLE_ID,
    version: JSON.parse(await readFile(path.join(DESKTOP_DIR, 'package.json'), 'utf8')).version,
    signed: false,
    buildKind: 'debug-development',
    sourceRevision: sourceRevision.revision,
    sourceRevisionScope: 'HEAD-only; working-tree changes are not represented by this field',
    workingTreeModified: sourceRevision.workingTreeModified,
    target: {
      platform: 'darwin',
      architecture: TARGET_ARCH,
      minimumMacOSVersion: MINIMUM_MACOS_VERSION,
      electronVersion,
    },
    appPath: path.relative(outputPath, appPath),
    nativeBinDir,
    resourceManifestPath: 'Contents/Resources/resource-manifest.json',
    icon: {
      source: 'Electron runtime default icon',
      productIconBundled: false,
      defaultElectronIconLocal: true,
    },
    resourceManifestSha256: await hashFile(path.join(appPath, 'Contents', 'Resources', 'resource-manifest.json')),
    compiledBuildKindAttested: false,
    nativeBuildDirectoryKind: 'debug',
    runtimeResources: resourceManifest,
    files: appFiles,
  };
  const manifestPath = path.join(outputPath, 'assembly-manifest.json');
  await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
  return manifestPath;
}

async function removeNewOutput(outputPath) {
  await rm(outputPath, { recursive: true, force: true });
}

async function main(argv) {
  const args = parseArgs(argv);
  if (args.help) {
    process.stdout.write(`${usage()}\n`);
    return;
  }
  assertMacHost();
  await verifyOutputPath(args.output);
  await requireDirectory(args.nativeBinDir, 'Native binary directory');
  args.nativeBinDir = await verifyDebugNativeBinDir(args.nativeBinDir);

  const desktopPackage = await readJson(path.join(DESKTOP_DIR, 'package.json'));
  const { desktopRequire, electronPackagePath } = getElectronPackage();
  const electronPackage = await readJson(electronPackagePath);
  const packager = await loadPackager(desktopRequire);
  const sourceRevision = getSourceRevision();
  const tempRoot = await mkdtemp(path.join(os.tmpdir(), 'computer-use-macos-package-'));
  const appSourceDir = path.join(tempRoot, 'app-source');
  const resourceStageDir = path.join(tempRoot, 'runtime-resources');
  let outputCreated = false;

  try {
    await prepareAppSource(appSourceDir, desktopPackage, sourceRevision);
    const { staged, extras } = await copyRuntimeResources(
      resourceStageDir,
      REPO_ROOT,
      sourceRevision,
      args.nativeBinDir,
    );
    const { electronApp, runtimeZipDir } = await prepareLocalElectronZip(
      tempRoot,
      electronPackagePath,
      electronPackage,
    );
    const electronResourceNames = new Set(await readdir(path.join(electronApp, 'Contents', 'Resources')));

    const outputParent = path.dirname(args.output);
    await mkdir(outputParent, { recursive: true });
    try {
      await mkdir(args.output);
    } catch (error) {
      if (error.code === 'EEXIST') {
        throw new Error(`Refusing to overwrite an existing output path: ${args.output}`, { cause: error });
      }
      throw error;
    }
    outputCreated = true;
    process.stdout.write(`Packaging ${PRODUCT_NAME} for darwin/${TARGET_ARCH} with the installed Electron ${electronPackage.version} runtime.\n`);

    const packagedPaths = await packager({
      dir: appSourceDir,
      name: PRODUCT_NAME,
      platform: 'darwin',
      arch: TARGET_ARCH,
      electronVersion: electronPackage.version,
      electronZipDir: runtimeZipDir,
      out: args.output,
      overwrite: false,
      prune: false,
      asar: false,
      appBundleId: BUNDLE_ID,
      helperBundleId: `${BUNDLE_ID}.helper`,
      appVersion: desktopPackage.version,
      buildVersion: desktopPackage.version,
      extendInfo: { LSMinimumSystemVersion: MINIMUM_MACOS_VERSION },
      extraResource: extras,
      quiet: false,
    });
    if (!Array.isArray(packagedPaths) || packagedPaths.length !== 1) {
      throw new Error(`Packager returned an unexpected app path list: ${JSON.stringify(packagedPaths)}`);
    }
    const appPath = await appBundlePathFromPackagerOutput(packagedPaths[0], args.output);
    const resourcesPath = path.join(appPath, 'Contents', 'Resources');
    const installedResourceManifest = path.join(resourcesPath, 'resource-manifest.json');
    const verified = await verifyPackagedApp(
      appPath,
      installedResourceManifest,
      electronResourceNames,
    );
    const outputManifestPath = await writeAssemblyManifest({
      outputPath: args.output,
      appPath,
      electronVersion: electronPackage.version,
      sourceRevision,
      nativeBinDir: args.nativeBinDir,
      resourceManifest: staged.manifest,
    });

    process.stdout.write(`App bundle: ${appPath}\n`);
    process.stdout.write(`Assembly manifest: ${outputManifestPath}\n`);
    process.stdout.write(`Packaged app runtime path: ${verified.appSource}\n`);
  } catch (error) {
    if (outputCreated) await removeNewOutput(args.output).catch(() => {});
    throw error;
  } finally {
    await rm(tempRoot, { recursive: true, force: true });
  }
}

main(process.argv.slice(2)).catch((error) => {
  process.stderr.write(`macOS local app packaging failed: ${error.message}\n`);
  if (error.cause) process.stderr.write(`${error.cause.stack ?? error.cause}\n`);
  process.exitCode = 1;
});
