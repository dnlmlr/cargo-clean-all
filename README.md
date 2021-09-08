# cargo-clean-all

A custom cargo comand that analyses all cargo `target` directories under a given parent directory 
and allows for cleaning them, following certain criteria. The cleaning-criteria include 
`keep target dirs last modified X days ago` and `keep target dirs with size less than X`.

## Installation

Install using cargo:
```
cargo install cargo-clean-all
```

## Usage

Clean all target directories under the current working directory.
```
cargo-clean-all
```

Clean all target directories under the directory `[dir]`.
```
cargo-clean-all --dir [dir]
```

Keep target directories that have a size of less than `[filesize]`.
```
cargo-clean-all --keep-size [filesize]
```

Keep target directories younger than `[days]` days.
```
cargo-clean-all --keep-days [days]
```

# Alternatives

## [cargo-clean-recursive](https://github.com/IgaguriMK/cargo-clean-recursive)

| Feature      | `cargo-clean-all` | `cargo-clean-recursive` |
|------------------------------------------------|:---:|:---:|
| Clean projects under current dir               | yes | yes |
| Clean projects under any dir                   | yes | no  |
| Display freed up disk space                    | yes | no  |
| Keep target dirs below a size threshold        | yes | no  |
| Keep target dirs with a last modified treshold | yes | no  |
| Clean only `release`, `debug` or `docs`        | no (not yet)  | yes |
