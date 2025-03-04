# google-fonts-sources

Rust utility to help find the sources of Google Fonts fonts.

This is currently bare-bones; it inspects (or checks out) the repository at
github.com/google/fonts, and for each font parses its [metadata file], looking
for a repository.

For each repository we find, we then look for a `config.yaml` file in that
repository's `/source` directory, which is present by convention on sources
intended to be built by Google Fonts.

# use

To use this tool from the command line, in order to generate a JSON dictionary
containing information about source repositories:

```sh
RUST_LOG=INFO cargo run -- -o repo_list.json
```

To use this tool from another Rust crate, see [the docs].

[metadata file]: https://github.com/googlefonts/gftools/blob/main/Lib/gftools/fonts_public.proto
[the docs]: https://docs.rs/google-fonts-sources/
