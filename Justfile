tap := "../homebrew-spoon"

# Bump the version, test, then commit, tag and push; the tag starts .github/workflows/release.yml
release version:
    #!/usr/bin/env bash
    set -euo pipefail
    [[ "{{version}}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "version must look like 1.2.3"; exit 1; }
    [[ -z "$(git status --porcelain)" ]] || { echo "commit or stash your changes first"; exit 1; }
    git rev-parse -q --verify "refs/tags/v{{version}}" >/dev/null && { echo "v{{version}} already exists"; exit 1; }
    perl -pi -e 's/^version = ".*"/version = "{{version}}"/' Cargo.toml
    cargo check -q # updates Cargo.lock
    cargo test -q
    git diff --quiet || git commit -am "Release v{{version}}"
    git tag "v{{version}}"
    git push origin HEAD "v{{version}}"

# Point the Homebrew formula at a pushed tag; review and open a PR in the tap yourself
tap version:
    #!/usr/bin/env bash
    set -euo pipefail
    url="https://github.com/DanWlker/spoon/archive/refs/tags/v{{version}}.tar.gz"
    sha=$(curl -fsSL "$url" | shasum -a 256 | cut -d ' ' -f 1)
    perl -pi -e "s{^  url \".*\"}{  url \"$url\"}; s{^  sha256 \".*\"}{  sha256 \"$sha\"}" "{{tap}}/Formula/spoon.rb"
    git -C "{{tap}}" diff
