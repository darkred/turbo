/* tslint:disable */
/* eslint-disable */
/* prettier-ignore */

// This is an adapted version of the binding file created by napi
// If you want to update this file, change the napi build command
// argument '--js false' to '--js index-gen.js' and merge the
// updates that are needed into this file.

const { existsSync, readFileSync } = require('fs')
const { join } = require("path");

const { platform, arch } = process;

let nativeBinding = null;
let localFileExisted = false;
let loadError = null;

function isMusl() {
  // For Node 10
  if (!process.report || typeof process.report.getReport !== "function") {
    try {
      const lddPath = require("child_process")
        .execSync("which ldd")
        .toString()
        .trim();
      return readFileSync(lddPath, "utf8").includes("musl");
    } catch (e) {
      return true;
    }
  } else {
    const { glibcVersionRuntime } = process.report.getReport().header;
    return !glibcVersionRuntime;
  }
}

function isUnsupportedGlibc() {
  const { glibcVersionRuntime } = process.report.getReport().header;
  if (typeof glibcVersionRuntime === "string") {
    try {
      // We support glibc v2.26+
      let [major, minor] = glibcVersionRuntime.split(".", 2);
      if (parseInt(major, 10) !== 2) {
        return true;
      }
      if (parseInt(minor, 10) < 26) {
        return true;
      }
      return false;
    } catch (e) {
      return true;
    }
  }
  return !glibcVersionRuntime;
}

// TODO: find-up to turbo-repository? This currently only works from turbo-repository/js/dist
const localPath = join(__dirname, "..", "..", "native", "@turbo");
const pkgRoot = "@turbo/repository";

function loadViaSuffix(suffix) {
  const localNativePath = join(localPath, `repository.${suffix}.node`);
  if (existsSync(localNativePath)) {
    return require(localNativePath);
  }
  return require(`${pkgRoot}-${suffix}`);
}

let suffix;
switch (platform) {
  case "win32":
    switch (arch) {
      case "x64":
        suffix = "win32-x64-msvc";
        break;
      case "arm64":
        suffix = "win32-arm64-msvc";
        break;
      default:
        throw new Error(`Unsupported architecture on Windows: ${arch}`);
    }
    break;
  case "darwin":
    switch (arch) {
      case "x64":
        suffix = "darwin-x64";
        break;
      case "arm64":
        suffix = "darwin-arm64";
        break;
      default:
        throw new Error(`Unsupported architecture on macOS: ${arch}`);
    }
    break;
  case "linux":
    if (isMusl()) {
      switch (arch) {
        case "x64":
          suffix = "linux-x64-musl";
          break;
        case "arm64":
          suffix = "linux-arm64-musl";
          break;
        default:
          throw new Error(`Unsupported architecture on Linux: ${arch}`);
      }
    } else {
      if (isUnsupportedGlibc()) {
        throw new Error("unsuported glibc version, need version 2.26 or newer");
      }
      switch (arch) {
        case "x64":
          suffix = "linux-x64-gnu";
          break;
        case "arm64":
          suffix = "linux-arm64-gnu";
          break;
        default:
          throw new Error(`Unsupported architecture on Linux: ${arch}`);
      }
    }
    break;
  default:
    throw new Error(`Unsupported OS: ${platform}, architecture: ${arch}`);
}

nativeBinding = loadViaSuffix(suffix);

const { PackageManagerRoot, PackageManager, Workspace } = nativeBinding;

module.exports.PackageManagerRoot = PackageManagerRoot;
module.exports.PackageManager = PackageManager;
module.exports.Workspace = Workspace;
