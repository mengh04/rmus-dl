# rmus-dl

Music downloader written in Rust. Sources: kuwo (酷我) + kugou (酷狗) —
search, resolve download URLs, write lyrics & cover art into tags.

Two ways to run:

## Interactive CLI (standalone)

```
rmus-dl <keyword>        # search, pick in the terminal, download to CWD
```

Files land in the current directory with tags written via a **metadata gate**:
a download that fails to obtain (or verify) lyrics/cover art is deleted, so no
half-tagged files ever survive.

## rmus plugin mode

As a child process of [rmus](../rmus) (terminal music player):

```
rmus-dl --rmus-manifest   # print capability manifest, exit
rmus-dl --rmus-plugin     # serve the plugin protocol on stdin/stdout
```

Install into rmus's plugin directory and restart `rmusd`:

```
cargo install --path .
mkdir -p ~/.local/share/rmus/plugins
ln -s ~/.cargo/bin/rmus-dl ~/.local/share/rmus/plugins/
```

Then search/download from the rmus TUI (在线 tab) or `rmus dl <keyword> -g 1`.
Products are written to `~/Music` (`RMUS_LIBRARY_DIR`, injected by rmusd) with
full tags, and enter the player's library automatically.

Protocol details: see rmus's `docs/plugins.md`.

## Layout

```
src/
  main.rs        mode dispatch (CLI / manifest / plugin)
  app.rs         interactive CLI flow
  plugin.rs      plugin session: protocol loop, download jobs, cancel
  download.rs    streaming download (temp file, magic-number ext, tag rename)
  sources/       MusicSource trait + kuwo / kugou implementations
```
