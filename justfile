# Update the cargoHash in flake.nix after dependency changes
update-hash:
    #!/usr/bin/env bash
    set -euo pipefail
    fake="sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
    # Replace current hash with fake hash
    sed -i "s|cargoHash = \"sha256-[^\"]*\"|cargoHash = \"$fake\"|" flake.nix
    # Build and capture the correct hash from the error
    hash=$(nix build 2>&1 | grep -oP 'got:\s+sha256-\K[^\s]+' || true)
    if [ -z "$hash" ]; then
        echo "ERROR: Could not extract hash from nix build output"
        git checkout flake.nix
        exit 1
    fi
    # Patch in the correct hash
    sed -i "s|$fake|sha256-$hash|" flake.nix
    echo "Updated cargoHash to sha256-$hash"

# Run the in-tree fixture diagnostic ladder. Bridge must be up beforehand
# (sudo nix run ./tests/fixture#cluster-fixture-net-up).
diag-fixture *ARGS:
    nix develop -c bash tests/fixture/diagnose/run.sh {{ARGS}}

# Same as diag-fixture but leaves the VM running for follow-up SSH inspection.
diag-fixture-keep *ARGS:
    nix develop -c bash tests/fixture/diagnose/run.sh --keep-up {{ARGS}}
