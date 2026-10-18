# gray-fsearch

Indexed file and content search for [gray](https://github.com/vstaln/gray).

The plugin keeps a persistent index of the session's project (`~/.gray/fsearch`)
and puts `grep` and `rg` stand-ins on the bash tool's `PATH`. A stand-in asks the
index which files could match, then runs the real tool on just those files; when
the index can't answer, the real tool runs untouched, so output is always the
real tool's output.

## Install

```sh
gray plugin install fsearch
gray plugin capabilities fsearch --all   # grants shell.env (the PATH stand-ins)
```

It ships with gray's default install. For gray without it, use the core-only
installer or `GRAY_PLUGINS` to pick your own set.

## Use

```sh
gray fsearch find '*.rs' src        # files by glob
gray fsearch grep parse_config      # content search
gray fsearch status                 # index, socket and stand-in status
```

`GRAY_SEARCH_INDEX=off` disables the stand-ins. Linux only.

## License

MIT
