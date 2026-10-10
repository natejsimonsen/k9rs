# k9rs

A Rust port of [k9s](https://github.com/derailed/k9s), the terminal UI for
Kubernetes. It aims to look and behave exactly like k9s v0.50.18: same
screens, keys, flags and config files.

It does not need k9s installed. It reads the same config folder as k9s
(`$K9S_CONFIG_DIR`, else `$XDG_CONFIG_HOME/k9s`), so your skins, aliases,
hotkeys and plugins carry over.

For that compatibility k9rs keeps k9s's config names: the `K9S_*` env vars,
the `k9s` config folders and file names, the `k9s:` key in `config.yaml`
and the plugin env vars. It logs to its own `k9rs.log`.

## Install

### Homebrew

```sh
brew tap natejsimonsen/k9rs https://github.com/natejsimonsen/k9rs
brew install natejsimonsen/k9rs/k9rs
```

### Flox

```sh
flox install 'github:natejsimonsen/k9rs'
```

### Nix

```sh
nix run github:natejsimonsen/k9rs              # run once
nix profile install github:natejsimonsen/k9rs  # install
```

### Cargo

Needs a Rust toolchain ([rustup.rs](https://rustup.rs)).

```sh
cargo install k9rs
```

Or the latest code from git:

```sh
cargo install --git https://github.com/natejsimonsen/k9rs
```

### Prebuilt binaries

Download a tarball for macOS or Linux from the
[releases page](https://github.com/natejsimonsen/k9rs/releases) and put
`k9rs` on your `PATH`.

## Use

```sh
k9rs                    # current kubeconfig context
k9rs -n kube-system     # start in a namespace
k9rs --readonly         # block delete, edit, scale and other changes
k9rs -c deploy          # start on deployments
k9rs info               # show config and log file locations
```

All k9s flags work. Run `k9rs --help` for the list. Press `?` inside the app
for keys.

If the cluster can't be reached at startup (for example, expired login),
k9rs opens on the contexts screen like k9s does. Fix your login, then pick
a context and press Enter.

## Build from source

```sh
git clone https://github.com/natejsimonsen/k9rs
cd k9rs
cargo build --release
./target/release/k9rs
```

Run the tests with `cargo test`.

## License

Apache 2.0, same as k9s. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
