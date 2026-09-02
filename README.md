# rbx-inject

[![CI](https://github.com/rbx-dev-tools/rbx-inject/actions/workflows/ci.yml/badge.svg)](https://github.com/rbx-dev-tools/rbx-inject/actions/workflows/ci.yml)
[![License: MPL 2.0](https://img.shields.io/badge/license-MPL--2.0-blue.svg)](./LICENSE)

Write asset ids and config values into a Roblox place file, just before you
upload it.

```sh
rbx-inject apply --place build/game.rbxl \
                 --config injections.toml \
                 --assets output/Assets.luau
```

No network, no credentials, no Roblox API. It reads a `.rbxl`, changes it, and
writes it back. Uploading is somebody else's job.

## Install

With [Rokit](https://github.com/rojo-rbx/rokit), in your project's
`rokit.toml`:

```toml
[tools]
rbx-inject = "rbx-dev-tools/rbx-inject@0.1.0"
```

then `rokit install`. Or take a binary from the
[releases page](https://github.com/rbx-dev-tools/rbx-inject/releases), which
ships one zip per platform with a `SHA256SUMS` beside them, or build from
source:

```sh
cargo install --git https://github.com/rbx-dev-tools/rbx-inject
```

> The published Linux binary is compiled against the release runner's glibc and
> will not start on an older distribution. There is no musl build yet, for the
> reason given in `.github/workflows/release.yml`; build from source there.

## Where this sits

Every tool in a Roblox pipeline owns one boundary. This one owns the binary
format, and nothing else does.

| tool | talks to | needs credentials |
| --- | --- | --- |
| **rbx-inject** | **the bytes of a `.rbxl`, on disk** | **no** |
| [asphalt](https://github.com/jacktabscode/asphalt) | uploads images and audio, writes a Luau module of ids | yes |
| [rbx-cli](https://github.com/rbx-forge/rbx-cli) | Open Cloud and the Roblox web API: metadata, shop, configs, place upload, live ops | yes |
| [rbx-observe](https://github.com/rbx-forge/rbx-observe) | public Roblox pages, read-only, other people's games | no |
| [rbx-switch](https://github.com/rbx-dev-tools/rbx-switch) | the Studio account store on this machine | no |
| [Rojo](https://rojo.space) | a filesystem tree, building a place from source | no |

The split is not tidiness. rbx-inject is the only one of them that parses
Roblox's binary format, and that format moves: on 2026-06-30 Roblox started
storing `Instance.Tags` in the SharedString index, and every parser older than
that day began refusing any place saved since, with

```
Type mismatch: Property Actor.Tags should be SharedString, but it was Tags
```

Fixing that means bumping rbx-dom, which means releasing every tool with rbx-dom
compiled into it. So there is exactly one of those. A deployment CLI that only
speaks Open Cloud should not carry that risk, and this binary is small enough to
re-release the same afternoon. It is the same reason `rbx switch` left rbx-cli
to become `rbx-switch`.

Embedding Luau (`mlua`, vendored) rather than shelling out to Lune follows from
it: shelling out would put a *second* Roblox-format parser in the pipeline, on a
release cadence nobody here controls, and that second parser is exactly what
broke.

### Which tool do I want

| I want to | use |
| --- | --- |
| upload an image or a sound and get its id | `asphalt sync` |
| put that id onto an instance in my place | **`rbx-inject apply`** |
| create passes, badges, products and get their ids | `rbx shop sync` |
| set the game's name, description, icon, thumbnails, devices | `rbx meta sync` |
| upload the finished place | `rbx place upload` |
| see what a competitor sells and for how much | `rbx-observe storefront` |
| switch which Studio account the next command uses | `rbx-switch` |
| read code out of a place file, or build a place from source | Rojo |

`rbxsync`, `rbxplace` and `rbxapikey` were the ancestors of `rbx shop`,
`rbx place` and `rbx apikey`. If a project still calls them directly, that is a
migration waiting, not a fourth tool.

### In a pipeline

```just
deploy env:
    asphalt sync
    rbx shop sync --env {{env}} --apply
    rbx-inject apply --place build/game.rbxl \
                     --config injections.toml \
                     --assets output/Assets.luau \
                     --module ids=src/shared/GameIds.luau \
                     --strict
    rbx place upload --env {{env}} --file build/game.rbxl
    rbx meta sync --env {{env}}
```

`--strict` matters there. Without it a rule that resolves to nothing is a
warning and the deploy carries on, which ships a game with an empty
`AnimationId`. With it, the deploy stops.

## What it does

Two kinds of rule, both addressing an instance by a dot-separated path from the
DataModel.

### `properties` - set a property on an instance

```toml
[[injections]]
roblox_path = "StarterGui.Shop.Icon"
properties.Image = "ui.ShopIcon"
```

| value | meaning |
| --- | --- |
| `ui.ShopIcon` | look the key up in the asset map |
| `$module:<name>` | the whole source of a named module (see below) |
| `$require:models.main` | `return require(<id>)`, for a stub pointing at an uploaded model |

`$module` and `$rbxsync_module` still work; they are the names `assets` and
`ids` under their old spellings.

There are only two kinds of input, and the difference is what happens to the
file rather than where it came from:

```sh
--assets output/Assets.luau              # evaluated: becomes the lookup map
                                         # and the module named `assets`
--module ids=src/shared/GameIds.luau     # copied verbatim into a Source
--module env=src/shared/Env.luau         # repeatable, any name you like
```

Asphalt's output has a flag of its own only because it is the one file that is
*both*: read as data for the map, and injectable as a module. Every other
generated Luau file is the same kind of object and goes through `--module`,
rather than earning a new flag each time you generate one.

A config declares what it needs, so a forgotten flag fails immediately and by
name:

```
Error: this config injects the module 'ids', which was not given;
       pass --module ids=<path> (given: assets)
```

That check exists because the alternative is worse than it looks: without it, a
missing input resolves nothing, reads exactly like a place that has drifted away
from its rules, and exits zero.

The type comes from the rbx-dom reflection database, not from the property name.
That matters more than it sounds: Roblox has two content types, the modern
`Content` and the legacy string-shaped `ContentId`, and no naming rule separates
them. `ImageLabel.Image` is a `ContentId`, `ImageLabel.ImageContent` is a
`Content`. Writing the wrong one makes the *serializer* fail, far from the line
that chose it.

### What happens to the target

A rule that writes a module source handles all three states it can find:

| the path | what happens |
| --- | --- |
| exists, and can hold the property | its `Source` is replaced |
| does not exist | created: `Folder` for the intermediate segments, `ModuleScript` for the leaf |
| exists as something that cannot hold the property | refused, with a warning |

Creating is the point: the file is generated, so requiring it to already exist
in the place would mean checking a generated stub into the `.rbxl`. But the
first segment is never created, because `ReplicatedStorge` should be an error
rather than a new Folder sitting beside the real service. And nothing is created
until the value that would fill it has resolved, or a missing input would leave
an empty ModuleScript behind and call it a change.

The third row used to be silent, which is the worst of the three: writing
`Source` onto a `Folder` succeeds, Roblox drops the property on load, and
nothing anywhere says so. The reflection database is what makes it visible.
The refusal only happens when the database knows the class and says the property
is not on it; an unknown class is a Roblox release the database has not caught
up with, and refusing there would break a working setup.

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

## `check`

```sh
rbx-inject check --place build/game.rbxl --config injections.toml
```

Injection targets instances by path, and a path is a string nobody in Studio
knows they are breaking. Rename `ReplicatedStorage.Assets.Characters.Player` and
every rule underneath it quietly resolves to nothing.

`check` is that failure moved earlier. It needs no asset map and no credentials,
so it runs in a pre-commit hook or in CI, the day the rename happens rather than
at the next deploy. It reports:

- **error** - the rule cannot apply: no such instance, a `keys` target that is
  not a ModuleScript, a module whose source does not evaluate to a table, a
  first segment that is not a service.
- **warning** - the rule will apply but probably not as intended: a property
  name the reflection database does not know for that class. Roblox writes it,
  then drops it on load, and nothing anywhere says so. A warning rather than an
  error because the database lags each Roblox release by a few weeks.

Pass `--assets` to also check that every lookup resolves. Exit is non-zero on
any error, or on any warning with `--strict`.

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

In `apply`, a rule that resolves to nothing is a warning, not an error: a place
often lags the config while a feature is being built, and failing every deploy
for it would be wrong. `--strict` turns warnings into a non-zero exit, which is
what a pipeline wants. `--dry-run` reports what would change and writes nothing.

The place is written to a temporary file and renamed, so an interrupted run
cannot leave a truncated `.rbxl` where a working one used to be.

## A note on property migrations

Roblox is migrating legacy properties to the `Content` type, and rbx-dom applies
those migrations *on read*. A place written with `Image` comes back with
`ImageContent`; one written with `SoundId` comes back with `AudioContent`, not
the `SoundContent` the pattern would suggest. Write the property your Studio
build uses and let the migration happen. There is no list of names worth
hardcoding, which is the whole reason the reflection database decides.

## Do I still need this?

Less than you might, and the reason is not Rojo.

Injection exists because asset ids are *baked into instance properties* rather
than *read from a module by code*. `anim.AnimationId = Assets.animations.Eating`
needs no rule at all. That is a code-architecture choice, and it is available
without moving anything into a filesystem tree.

Going fully Rojo-managed does remove the need, because generated modules become
source files the build includes. But it also means GUIs, terrain and world
building leave Studio, which is a real cost that a hybrid setup does not pay and
does not avoid either: instances that stay authored in Studio still have
properties to fill.

So: convert ids from baked properties to a module read whenever you touch a
system anyway, and keep this for the rest. It is a small binary with tests.

## Building

```sh
cargo build --release
cargo test
```

The Luau runtime is vendored, so a C++ toolchain is needed to build it and
nothing is needed to run it. That is also why CI builds on both Linux and
Windows: nothing here is platform-specific Rust, but the C++ compiler
underneath is, and that is where a build breaks.

## License

[MPL-2.0](./LICENSE).
