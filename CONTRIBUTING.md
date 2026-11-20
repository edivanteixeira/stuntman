# Contributing to Stuntman

Thank you for helping. The most valuable contribution is a **plugin for a provider you know well**: you describe its
behaviour once, and everyone who calls that provider can test against it.

## Ways to help

- **Write or improve a plugin.** See [Plugins](docs/plugins.md). Plugins live in [`engine/plugins/`](engine/plugins).
- **Report a provider behaviour Stuntman gets wrong.** Include the provider's documented request and response.
- **Fix a bug or improve a line** (SMS, storage, identity…). Each line is a crate in `engine/crates/`.
- **Improve the docs.** Plain, short, correct English beats long English.

## Before you open a pull request

```sh
cd engine
cargo build -p stuntman
cargo test --workspace --exclude stuntman-web
```

If you touched a line, run its end-to-end check against a running server (see [`engine/README.md`](engine/README.md#check)).
If you touched how Stuntman writes (SQLite, the broker, call recording), measure it on a slow disk too: a Mac SSD
flushes almost for free, a cloud disk takes about 6 ms. `engine/tests/perf/run_slow_disk.sh <image>` runs an image on a
Docker volume with every flush delayed 6 ms and prints req/s for 1 and 16 clients; one client without a route should
stay above 1,000 req/s (with `STUNTMAN_DURABILITY=full` it falls to about 55).

If you added or changed a tool, regenerate the reference:

```sh
python3 tools/gen_tool_reference.py
```

## Rules of the house

1. **Synthetic data only.** People, companies and addresses are made up (`@example.com`, Acme, Globex). Never paste
   real customer data, real tokens or production logs, not even in tests.
2. **The provider's words.** Sentences on the timeline read like the provider would say them.
3. **Text is the source of truth.** Anything the web UI can change must round-trip through the YAML.
4. **Safe by default.** Nothing reaches the network or the disk unless it is declared.
5. **One line, one crate.** A new line implements the module interface and does not touch the others.

## License

By contributing, you agree that your contributions are licensed under the [Apache License 2.0](LICENSE).
