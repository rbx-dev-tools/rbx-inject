# rbx-inject

Write asset ids and config values into a Roblox place file, just before you
upload it.

```sh
rbx-inject apply \
  --place build/game.rbxl \
  --config injections.toml \
  --assets output/Assets.luau
```

No network, no credentials, no Roblox API. It reads a `.rbxl`, changes it, and
writes it back. Uploading is somebody else's job.

## Why it is its own binary

Because it is the only tool in the chain that parses Roblox's binary format,
and that format moves.

On 2026-06-30 Roblox started storing `Instance.Tags` in the SharedString index.
Every parser older than that day began refusing any place saved since, with
`Type mismatch: Property Actor.Tags should be SharedString, but it was Tags`.
Fixing it meant bumping rbx-dom, which meant a release of every tool that had
rbx-dom compiled into it.

So there is exactly one of those. A deployment CLI that only talks to Open
Cloud does not need to carry that risk, and this binary is small enough to
re-release the same afternoon.

Embedding Luau (`mlua`, vendored) rather than shelling out to Lune is the same
decision: shelling out would put a second Roblox-format parser in the pipeline,
on a release cadence nobody here controls.

## What it does

Two kinds of rule, both addressing an instance by a dot-separated path from the
DataModel.

### `properties` - set a property on an instance

```toml
[[injections]]
roblox_path = "StarterGui.Shop.Icon"
properties.Image = "ui.ShopIcon"          # looked up in the asset map
```

| value | meaning |
| --- | --- |
| `ui.ShopIcon` | look the key up in the asphalt asset map |
| `$module` | the whole asphalt module source (needs `--assets`) |
| `$rbxsync_module` | the whole generated ids module (needs `--ids-module`) |
| `$require:models.main` | `return require(<id>)`, for a stub pointing at an uploaded model |

The type comes from the rbx-dom reflection database, not from the property
name. That matters more than it sounds: Roblox has two content types, the
modern `Content` and the legacy string-shaped `ContentId`, and no naming rule
separates them. `ImageLabel.Image` is a `ContentId`, `ImageLabel.ImageContent`
is a `Content`. Writing the wrong one makes the *serializer* fail, far from the
line that chose it.

A rule that writes a module source may create what it targets, intermediate
`Folder`s included: the file is generated, so requiring it to already exist in
the place would mean checking a generated stub into the `.rbxl`. The first
segment is never created, because `ReplicatedStorge` should be an error rather
than a new Folder next to the real service.

### `keys` - set a value inside a ModuleScript's table

```toml
[[injections]]
roblox_path = "ReplicatedStorage.GameConfig"
keys."Sounds.Bang" = "audio.Bang"
keys."Settings.Volume" = "$0.5"
keys."Settings.Debug" = "$true"
keys.GameName = "$$My Game"
```

The module's source is evaluated, the table is edited, and it is printed back
out as source. Intermediate tables are created as needed.

| key syntax | addresses |
| --- | --- |
| `Name` | the string key `Name` |
| `$4` | the integer key `4` |
| `Config.Sound` | nested, `Config = { Sound = ... }` |

| value syntax | result |
| --- | --- |
| `audio.Bang` | asset map lookup |
| `$1234`, `$0.5` | a number |
| `$true`, `$false` | a boolean |
| `$nil` | removes the key |
| `$$Hello` | the literal string `Hello` |

A bare value is always a lookup, so the common case stays short. `$$` is the
escape hatch for a literal that happens to look like a key.

Keys are printed sorted, so re-running with unchanged inputs produces a
byte-identical file and the place diff stays readable.

## Config format

TOML or JSON, picked by extension. JSON is supported because that is what
existing setups have, not because it is the better choice here: an injections
file is full of decisions that want a comment beside them, and JSON has no
comments.

```sh
rbx-inject migrate injections.json    # writes injections.toml, original untouched
```

Unknown top-level fields are ignored, so a file that also carries configuration
for another step of your pipeline stays readable by both.

## Ordering

Every `properties` rule runs before any `keys` rule. A rule can replace a
ModuleScript's whole `Source` with a generated module, and a `keys` rule
elsewhere may then edit a value inside that new source. One pass would make the
result depend on the order rules happen to appear in the file.

## Unresolved rules

A rule that resolves to nothing is a warning, not an error: a place file often
lags the config while a feature is being built, and failing the deploy for it
would be wrong. Pass `--strict` to turn warnings into a non-zero exit, which is
what you want in CI.

`--dry-run` reports what would change and writes nothing.

## A note on property migrations

Roblox is migrating legacy properties to the `Content` type, and rbx-dom applies
those migrations *on read*. A place written with `Image` comes back with
`ImageContent`; one written with `SoundId` comes back with `AudioContent`, not
the `SoundContent` the pattern would suggest. Write the property your Studio
build uses and let the migration happen; there is no list of names worth
hardcoding.

## Building

```sh
cargo build --release
cargo test
```

The Luau runtime is vendored, so a C++ toolchain is needed to build, and nothing
is needed to run.
