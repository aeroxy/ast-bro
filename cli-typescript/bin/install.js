/**
 * ast-bro binary installer — downloads from GitHub releases on postinstall.
 */

const { execFileSync } = require("child_process");
const fs = require("fs");
const os = require("os");
const path = require("path");

const VERSION = "4.2.0";
const GITHUB_REPO = "aeroxy/ast-bro";
const BINARY_NAME = "ast-bro";

/** Return the version-specific binary cache for this user. */
function getCacheDir() {
  if (os.platform() === "win32") {
    const local = process.env.LOCALAPPDATA || path.join(os.homedir(), "AppData", "Local");
    const dir = path.join(local, "ast-bro", "Cache", VERSION);
    fs.mkdirSync(dir, { recursive: true });
    return dir;
  }
  const versioned = `ast-bro-${VERSION}`;
  if (os.platform() === "darwin") {
    const dir = path.join(os.homedir(), "Library", "Caches", versioned);
    fs.mkdirSync(dir, { recursive: true });
    return dir;
  }
  const dir = path.join(os.homedir(), ".cache", versioned);
  fs.mkdirSync(dir, { recursive: true });
  return dir;
}

/** Resolve only platforms for which the release workflow builds an archive. */
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

  const available = new Set(["macos-arm64", "windows-x86_64"]);
  if (!available.has(`${osName}-${archName}`)) {
    throw new Error(
      `No pre-built binary for ${osName}-${archName} yet (available: ${[...available].join(", ")}). ` +
      `Build from source: cargo install ast-bro`
    );
  }

  return { osName, archName };
}

/** Return the executable path, including the Windows suffix. */
function getBinaryPath() {
  const cacheDir = getCacheDir();
  const ext = os.platform() === "win32" ? ".exe" : "";
  return path.join(cacheDir, `${BINARY_NAME}${ext}`);
}

/** Avoid resolving Windows utilities from the current checkout or PATH. */
function windowsSystemPath(...parts) {
  const root = process.env.SystemRoot || "C:\\Windows";
  if (!path.win32.isAbsolute(root)) {
    throw new Error("SystemRoot must be an absolute Windows path");
  }
  return path.win32.join(root, "System32", ...parts);
}

/** Download and extract a release without requiring a Unix shell on Windows. */
function downloadBinary() {
  const { osName, archName } = getPlatform();
  const cacheDir = getCacheDir();
  const binaryPath = getBinaryPath();

  if (fs.existsSync(binaryPath)) {
    return binaryPath;
  }

  const url = `https://github.com/${GITHUB_REPO}/releases/download/${VERSION}/${BINARY_NAME}-${osName}-${archName}.zip`;
  console.error(`Downloading ast-bro ${VERSION} for ${osName}-${archName}...`);

  // Stage downloads so a failed extraction cannot leave a cached partial binary.
  const staging = fs.mkdtempSync(path.join(cacheDir, "install-"));
  const archivePath = path.join(staging, "archive.zip");
  try {
    execFileSync(osName === "windows" ? windowsSystemPath("curl.exe") : "curl", [
      "-fsSL", "--connect-timeout", "30", "--max-time", "120", "--output", archivePath, url,
    ]);
    if (osName === "windows") {
      execFileSync(windowsSystemPath("WindowsPowerShell", "v1.0", "powershell.exe"), [
        "-NoProfile", "-NonInteractive", "-Command",
        "$ErrorActionPreference = 'Stop'; Add-Type -AssemblyName System.IO.Compression.FileSystem; [System.IO.Compression.ZipFile]::ExtractToDirectory($env:AST_BRO_ARCHIVE, $env:AST_BRO_DESTINATION)",
      ], {
        env: { ...process.env, AST_BRO_ARCHIVE: archivePath, AST_BRO_DESTINATION: staging },
        windowsHide: true,
      });
    } else {
      execFileSync("unzip", ["-o", archivePath, "-d", staging]);
    }

    // Publish the entrypoint last: its presence marks a complete installation.
    const names = ["ast-outline", "sb", BINARY_NAME].map(
      (name) => `${name}${osName === "windows" ? ".exe" : ""}`
    );
    for (const name of names) {
      if (!fs.statSync(path.join(staging, name)).isFile()) {
        throw new Error(`Release archive is missing ${name}`);
      }
    }
    for (const name of names) {
      const source = path.join(staging, name);
      const destination = path.join(cacheDir, name);
      if (osName !== "windows") fs.chmodSync(source, 0o755);
      if (fs.existsSync(destination) && fs.statSync(destination).isFile()) continue;
      try {
        fs.renameSync(source, destination);
      } catch (error) {
        // A concurrent npm/Python install may have published and launched it.
        // Windows refuses to replace a running executable; reuse that winner.
        if (!fs.existsSync(destination) || !fs.statSync(destination).isFile()) throw error;
      }
    }
  } finally {
    fs.rmSync(staging, { recursive: true, force: true });
  }

  console.error(`Installed ast-bro to ${binaryPath}`);
  return binaryPath;
}

// Run on postinstall
if (require.main === module) {
  try {
    downloadBinary();
  } catch (err) {
    console.error("Failed to install ast-bro binary:", err.message);
    console.error("You can manually install by running: cargo install ast-bro");
    process.exitCode = 1;
  }
}

module.exports = { getPlatform, getBinaryPath, downloadBinary };
