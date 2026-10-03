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

# Syntax check every module, then run the tests
check:
    npm run check

# Run the tests
test:
    npm test
