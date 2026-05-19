/**
 * ast-bro binary installer — downloads from GitHub releases on postinstall.
 */

const { execSync } = require("child_process");
const fs = require("fs");
const os = require("os");
const path = require("path");

const VERSION = "2.1.0";
const GITHUB_REPO = "aeroxy/ast-bro";
const BINARY_NAME = "ast-bro";

function getCacheDir() {
  if (os.platform() === "darwin") {
    const dir = path.join(os.homedir(), "Library", "Caches", "ast-bro");
    fs.mkdirSync(dir, { recursive: true });
    return dir;
  }
  const dir = path.join(os.homedir(), ".cache", "ast-bro");
  fs.mkdirSync(dir, { recursive: true });
  return dir;
}

function getPlatform() {
  const platform = os.platform();
  const arch = os.arch();

  const osMap = { darwin: "macos", linux: "linux", win32: "windows" };
  const archMap = { arm64: "arm64", x64: "x86_64" };

  const osName = osMap[platform];
  const archName = archMap[arch];

  if (!osName || !archName) {
    throw new Error(
      `No pre-built binary for ${platform}/${arch}. ` +
      `Build from source: cargo install ast-bro`
    );
  }

  // Check if pre-built binary exists (only macos-arm64 for now)
  const available = new Set(["macos-arm64"]);
  if (!available.has(`${osName}-${archName}`)) {
    throw new Error(
      `No pre-built binary for ${osName}-${archName} yet (available: macos-arm64). ` +
      `Build from source: cargo install ast-bro`
    );
  }

  return { osName, archName };
}

function getBinaryPath() {
  const cacheDir = getCacheDir();
  const ext = os.platform() === "win32" ? ".exe" : "";
  return path.join(cacheDir, `${BINARY_NAME}${ext}`);
}

function downloadBinary() {
  const cacheDir = getCacheDir();
  const binaryPath = getBinaryPath();

  if (fs.existsSync(binaryPath)) {
    return binaryPath;
  }

  const { osName, archName } = getPlatform();
  const ext = osName === "windows" ? ".zip" : ".tar.gz";
  const url = `https://github.com/${GITHUB_REPO}/releases/download/${VERSION}/${BINARY_NAME}-${osName}-${archName}${ext}`;

  console.log(`Downloading ast-bro ${VERSION} for ${osName}-${archName}...`);
  console.log(`  ${url}`);

  const archivePath = path.join(cacheDir, `archive${ext}`);

  // Download
  const data = execSync(`curl -fsSL "${url}"`, { maxBuffer: 100 * 1024 * 1024 });
  fs.writeFileSync(archivePath, data);

  // Extract
  if (ext === ".tar.gz") {
    execSync(`tar xzf "${archivePath}" -C "${cacheDir}"`);
  } else {
    execSync(`cd "${cacheDir}" && unzip -o "${archivePath}"`);
  }

  fs.unlinkSync(archivePath);

  // Make executable
  if (os.platform() !== "win32") {
    fs.chmodSync(binaryPath, 0o755);
  }

  console.log(`Installed ast-bro to ${binaryPath}`);
  return binaryPath;
}

// Run on postinstall
if (require.main === module) {
  try {
    downloadBinary();
  } catch (err) {
    console.error("Failed to install ast-bro binary:", err.message);
    console.error("You can manually install by running: cargo install ast-bro");
    // Don't fail install — user can still use the CLI after manual install
  }
}

module.exports = { getBinaryPath, downloadBinary };
