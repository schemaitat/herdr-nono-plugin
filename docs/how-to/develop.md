# Work on the plugin

## Set up

[Link a checkout](install.md#link-a-checkout-instead). There is nothing to
install: the plugin has no dependencies and no build step beyond recording
the node path.

## Run the tests

```bash
npm run check   # syntax check every module, then run the tests (just check)
npm test        # just test
node --test test/actions.test.mjs                                # one file
node --test --test-name-pattern="stop" "test/*.test.mjs"         # by name
```

The tests run the real scripts as child processes against fake `nono`,
`herdr` and `opencode` executables in `test/fakes/`, and the verification
against fake `/proc` trees, so they need no nono and no Herdr, only `git`.
For what only a real host shows, follow
[Test on a real host](test-on-a-real-host.md).

## Preview the documentation

The site is built with MkDocs Material from `docs/`, `README.md` and
`CHANGELOG.md`. With [just](https://just.systems) and
[uv](https://docs.astral.sh/uv/):

```bash
just docs        # live preview on http://127.0.0.1:8000/ (just docs 8001 for another port)
just docs-build  # strict build into site/, what CI runs
```

On a remote machine, forward the port (`ssh -L 8000:127.0.0.1:8000 <host>`)
and open `http://127.0.0.1:8000/` locally, or listen on every interface with
`just docs 8000 0.0.0.0`, which exposes the preview to the network.

Without them, `pip install -r .github/mkdocs/requirements.txt`, then
`mkdocs serve` or `mkdocs build --strict`. The strict build fails on broken
links and anchors and on pages missing from `nav`.

The pages follow [Diátaxis](https://diataxis.fr/start-here/). Put a new page
in the folder for what the reader is doing: learning by doing (`tutorials/`),
getting a task done (`how-to/`), looking something up (`reference/`), or
understanding why (`explanation/`). Add it to `nav` in `mkdocs.yml`. Facts go
in reference and are linked from the other kinds, not repeated.

## Find your way around

[Source layout](../reference/source-layout.md) lists what each file does, and
[Design](../explanation/design.md) why it is built that way.
