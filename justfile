# Recipes for working on the plugin. `just` lists them.

mkdocs := "uvx --with-requirements .github/mkdocs/requirements.txt mkdocs"

# List the recipes
default:
    @just --list

# Serve the documentation with live reload on http://127.0.0.1:8000/ (host=0.0.0.0 to reach it from another machine)
docs port="8000" host="127.0.0.1":
    DOCS_SITE_URL=http://{{host}}:{{port}}/ {{mkdocs}} serve --dev-addr {{host}}:{{port}} --watch README.md --watch CHANGELOG.md --watch .github/mkdocs

# Build the documentation into site/ the way CI does, failing on warnings
docs-build:
    {{mkdocs}} build --strict

# Format check, lint and test the crate, then run the black-box tests against the debug build
check:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test
    npm run check

# Run the black-box tests (builds the debug binary first)
test:
    npm test

# Build the release binary the way a source install does
build:
    cargo build --release --locked

# Build this checkout and install it as the nono.sandbox plugin, replacing a GitHub install or an older link
install:
    cargo build --release --locked
    install -D -m 755 target/release/herdr-nono bin/herdr-nono
    -herdr plugin unlink nono.sandbox
    -herdr plugin uninstall nono.sandbox
    herdr plugin link "{{justfile_directory()}}"
