# Debian / Ubuntu Packaging for llm-leaders

This directory contains the packaging and release scripts for Debian and Ubuntu distributions.

## Files

- **`control`**: Debian control template containing package metadata, description, and shared library dependencies (`libc6`, `libssl3 | libssl1.1`, `zlib1g`).
- **`build-deb.sh`**: Builds `llm-leaders_<version>_<arch>.deb`. Uses `dpkg-deb` when installed, with an automated fallback to standard `ar`/`tar` for environments without `dpkg`.
- **`publish-apt.sh`**: Updates and pushes the GitHub Pages APT repository on the `gh-pages` branch. It indexes all `.deb` files with `dpkg-scanpackages`, generates `Release` / `InRelease`, and updates `index.html`.

## Local Build

```bash
# Build the binary in release mode
cargo build --release

# Create the .deb package
packaging/debian/build-deb.sh 0.8.0 target/release/llm-leaders dist amd64

# Inspect the built package
dpkg-deb -I dist/llm-leaders_0.8.0_amd64.deb
dpkg-deb -c dist/llm-leaders_0.8.0_amd64.deb
```

## GitHub Pages APT Repository Setup

GitHub Actions automatically builds and publishes the APT repository to the `gh-pages` branch on every tagged release.

### (Optional) GPG Repository Signing

To enable signed APT repository metadata (`InRelease` and `KEY.gpg`):

1. Generate an Ed25519 signing key:
   ```bash
   gpg --batch --gen-key <<EOF
   Key-Type: EDDSA
   Key-Curve: ed25519
   Key-Usage: sign
   Name-Real: llm-leaders-apt
   Expire-Date: 0
   %no-protection
   %commit
   EOF
   ```

2. Export the private key:
   ```bash
   gpg --armor --export-secret-keys llm-leaders-apt
   ```

3. Add it to GitHub repository secrets:
   - Name: `APT_GPG_PRIVATE_KEY`
   - Value: paste the exported ASCII-armored private key.
