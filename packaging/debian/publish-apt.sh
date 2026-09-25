#!/usr/bin/env bash
# Publish (or update) the GitHub Pages APT repository after a release.
#
# Called by CI with these env vars:
#   GITHUB_TOKEN         — standard GitHub Actions token with write permissions
#   GITHUB_REPOSITORY    — e.g. mohamadkhani/llm-leaders
#   VERSION              — bare version, e.g. 0.8.0
#   DEB_PACKAGE          — path to built .deb package
#   APT_GPG_PRIVATE_KEY  — (optional) ASCII-armored private GPG key for signing Release
#
set -euo pipefail

: "${GITHUB_TOKEN:?GITHUB_TOKEN env var is required}"
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY env var is required}"
: "${VERSION:?VERSION env var is required}"
: "${DEB_PACKAGE:?DEB_PACKAGE env var is required}"

if [ ! -f "$DEB_PACKAGE" ]; then
  echo "Error: Deb package not found at '$DEB_PACKAGE'" >&2
  exit 1
fi

OWNER="${GITHUB_REPOSITORY%%/*}"
REPO="${GITHUB_REPOSITORY#*/}"
PAGES_BRANCH="gh-pages"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

REPO_DIR="${WORK}/apt-repo"
REMOTE_URL="https://x-access-token:${GITHUB_TOKEN}@github.com/${GITHUB_REPOSITORY}.git"

echo ":: Fetching ${PAGES_BRANCH} branch..."
if git clone --depth 1 --branch "$PAGES_BRANCH" "$REMOTE_URL" "$REPO_DIR" 2>/dev/null; then
  echo ":: Existing ${PAGES_BRANCH} branch found."
else
  echo ":: ${PAGES_BRANCH} branch does not exist yet. Initializing orphan branch..."
  mkdir -p "$REPO_DIR"
  cd "$REPO_DIR"
  git init
  git checkout --orphan "$PAGES_BRANCH"
  git remote add origin "$REMOTE_URL"
fi

cd "$REPO_DIR"

# Copy the newly built .deb file into the repo root
cp "$DEB_PACKAGE" .

# Re-index all .deb packages with dpkg-scanpackages
echo ":: Generating Packages index..."
dpkg-scanpackages --multiversion . /dev/null > Packages
gzip -9c Packages > Packages.gz

# Compute SHA256 hashes and file sizes for the Release file
PKG_SHA256="$(sha256sum Packages | awk '{print $1}')"
PKG_SIZE="$(stat -c%s Packages)"
GZ_SHA256="$(sha256sum Packages.gz | awk '{print $1}')"
GZ_SIZE="$(stat -c%s Packages.gz)"
DATE_UTC="$(date -Ru)"

cat <<EOF > Release
Origin: ${OWNER}
Label: ${REPO}
Suite: stable
Codename: stable
Architectures: amd64
Components: main
Description: APT repository for ${REPO}
Date: ${DATE_UTC}
SHA256:
 ${PKG_SHA256} ${PKG_SIZE} Packages
 ${GZ_SHA256} ${GZ_SIZE} Packages.gz
EOF

# GPG signing if private key secret is provided
if [ -n "${APT_GPG_PRIVATE_KEY:-}" ]; then
  echo ":: Signing Release with GPG..."
  export GNUPGHOME="${WORK}/gnupg"
  mkdir -p "$GNUPGHOME"
  chmod 700 "$GNUPGHOME"

  echo "$APT_GPG_PRIVATE_KEY" | gpg --batch --import

  # Determine key ID
  KEY_ID="$(gpg --list-secret-keys --with-colons | grep '^sec' | cut -d: -f5 | head -n1)"

  # Sign Release to InRelease and Release.gpg
  gpg --batch --yes --default-key "$KEY_ID" --clearsign -o InRelease Release
  gpg --batch --yes --default-key "$KEY_ID" -abs -o Release.gpg Release

  # Export public key for apt clients
  gpg --batch --yes --armor --export "$KEY_ID" > KEY.gpg
  echo ":: GPG signing complete (KEY.gpg created)."
fi

# Create or update index.html landing page
cat <<EOF > index.html
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>${REPO} APT Repository</title>
  <style>
    body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif; max-width: 800px; margin: 40px auto; padding: 0 20px; line-height: 1.6; color: #24292f; }
    pre { background: #f6f8fa; padding: 16px; border-radius: 6px; overflow-x: auto; font-size: 14px; }
    code { font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace; }
    h1 { border-bottom: 1px solid #d0d7de; padding-bottom: 8px; }
    a { color: #0969da; text-decoration: none; }
    a:hover { text-decoration: underline; }
  </style>
</head>
<body>
  <h1>${REPO} APT Repository</h1>
  <p>Official Debian &amp; Ubuntu APT repository for <a href="https://github.com/${GITHUB_REPOSITORY}">${REPO}</a>.</p>

  <h2>Installation on Ubuntu / Debian</h2>
  <p>Run the following commands to add the repository and install <code>${REPO}</code>:</p>

  <pre><code># 1. Add repository GPG key
sudo mkdir -p /etc/apt/keyrings
curl -fsSL https://${OWNER}.github.io/${REPO}/KEY.gpg | sudo gpg --dearmor --yes -o /etc/apt/keyrings/${REPO}.gpg

# 2. Add repository source
echo "deb [signed-by=/etc/apt/keyrings/${REPO}.gpg] https://${OWNER}.github.io/${REPO}/ ./" | sudo tee /etc/apt/sources.list.d/${REPO}.list

# 3. Update and install
sudo apt update
sudo apt install ${REPO}</code></pre>

  <h2>Upgrade</h2>
  <p>Once installed, updates are automatically delivered through standard package management:</p>
  <pre><code>sudo apt update &amp;&amp; sudo apt upgrade</code></pre>
</body>
</html>
EOF

# Commit and push to gh-pages branch
echo ":: Committing and pushing to ${PAGES_BRANCH}..."
git config user.name "llm-leaders-ci"
git config user.email "ci@noreply.llm-leaders"

git add -A
if git diff --cached --quiet; then
  echo ":: APT repository already up to date, nothing to push."
else
  git commit -m "Deploy v${VERSION} to APT repository"
  git push origin "$PAGES_BRANCH"
  echo ":: Successfully published v${VERSION} to ${PAGES_BRANCH} APT repository!"
fi
