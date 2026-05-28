const REPO = "beefiker/labradorcli";
const RELEASES_URL = `https://github.com/${REPO}/releases`;
const LATEST_RELEASE_URL = `${RELEASES_URL}/latest`;
const LATEST_RELEASE_API = `https://api.github.com/repos/${REPO}/releases/latest`;

const platformRules = {
  mac: ["darwin", "mac", "osx", "apple", "universal"],
  windows: ["windows", "win", "setup", ".exe", ".msi"],
  linux: ["linux", "appimage", ".deb", ".rpm", "x86_64", "aarch64"],
};

const preferredFormats = {
  mac: [".dmg"],
  windows: [".exe", ".msi", ".zip"],
  linux: [".appimage", ".deb", ".rpm", ".tar.gz", ".tgz"],
};

function getPlatform() {
  const platform =
    navigator.userAgentData?.platform || navigator.platform || navigator.userAgent || "";
  const value = platform.toLowerCase();

  if (value.includes("mac")) {
    return { id: "mac", label: "macOS" };
  }

  if (value.includes("win")) {
    return { id: "windows", label: "Windows" };
  }

  if (value.includes("linux")) {
    return { id: "linux", label: "Linux" };
  }

  return { id: "unknown", label: "your platform" };
}

function scoreAsset(asset, platformId) {
  const name = asset.name.toLowerCase();
  let score = 0;

  if (name.includes("debug") || name.includes("dsym") || name.includes("pdb")) {
    return -100;
  }

  if (name.includes("cli") || name.includes("labrador") || name.includes("oz")) {
    score += 4;
  }

  for (const token of platformRules[platformId] || []) {
    if (name.includes(token)) {
      score += 8;
    }
  }

  for (const extension of preferredFormats[platformId] || []) {
    if (name.endsWith(extension)) {
      score += 10;
      break;
    }
  }

  if (
    name.endsWith(".dmg") ||
    name.endsWith(".zip") ||
    name.endsWith(".tar.gz") ||
    name.endsWith(".tgz")
  ) {
    score += 2;
  }

  return score;
}

function selectAsset(assets, platformId) {
  return assets
    .map((asset) => ({ asset, score: scoreAsset(asset, platformId) }))
    .filter((entry) => entry.score > 0)
    .sort((a, b) => b.score - a.score)[0]?.asset;
}

function formatBytes(bytes) {
  if (!Number.isFinite(bytes) || bytes <= 0) {
    return "";
  }

  const units = ["B", "KB", "MB", "GB"];
  let size = bytes;
  let unitIndex = 0;

  while (size >= 1024 && unitIndex < units.length - 1) {
    size /= 1024;
    unitIndex += 1;
  }

  return `${size.toFixed(size >= 10 || unitIndex === 0 ? 0 : 1)} ${units[unitIndex]}`;
}

function setDownloadTarget(url, label) {
  for (const id of ["primaryDownload", "downloadMirror"]) {
    const link = document.getElementById(id);
    if (link) {
      link.href = url;
      link.textContent = label;
    }
  }
}

function renderAssets(assets) {
  const list = document.getElementById("releaseAssets");
  if (!list || assets.length === 0) {
    return;
  }

  list.replaceChildren(
    ...assets.slice(0, 6).map((asset) => {
      const item = document.createElement("li");
      const link = document.createElement("a");
      const meta = document.createElement("span");

      link.href = asset.browser_download_url;
      link.rel = "noreferrer";
      link.textContent = asset.name;
      meta.textContent = formatBytes(asset.size);

      item.append(link, meta);
      return item;
    }),
  );
}

async function hydrateRelease() {
  const platform = getPlatform();
  const releaseStatus = document.getElementById("releaseStatus");
  const releaseMeta = document.getElementById("releaseMeta");

  setDownloadTarget(LATEST_RELEASE_URL, `Download for ${platform.label}`);

  try {
    const response = await fetch(LATEST_RELEASE_API, {
      headers: { Accept: "application/vnd.github+json" },
    });

    if (!response.ok) {
      throw new Error(`GitHub returned ${response.status}`);
    }

    const release = await response.json();
    const assets = Array.isArray(release.assets) ? release.assets : [];
    const asset = selectAsset(assets, platform.id);

    if (asset) {
      setDownloadTarget(asset.browser_download_url, `Download for ${platform.label}`);
      releaseStatus.textContent = `Latest release: ${release.tag_name}`;
      releaseMeta.textContent = `${release.name || release.tag_name} includes ${assets.length} downloadable asset${assets.length === 1 ? "" : "s"}.`;
    } else {
      setDownloadTarget(release.html_url || LATEST_RELEASE_URL, "Open latest release");
      releaseStatus.textContent = "Latest release found. Choose an asset on GitHub.";
      releaseMeta.textContent = `${release.name || release.tag_name} is available on GitHub Releases.`;
    }

    renderAssets(assets);
  } catch (_error) {
    setDownloadTarget(LATEST_RELEASE_URL, "Open GitHub Releases");
    if (releaseStatus) {
      releaseStatus.textContent = "Could not load release metadata. Opening GitHub Releases.";
    }
    if (releaseMeta) {
      releaseMeta.textContent = "Release metadata could not be loaded in this browser session.";
    }
  }
}

hydrateRelease();
