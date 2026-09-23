<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue)
[![Discord](https://img.shields.io/discord/1124083992740761730)](https://discord.moq.dev)
[![Crates.io](https://img.shields.io/crates/v/moq-net)](https://crates.io/crates/moq-net)
[![npm](https://img.shields.io/npm/v/@moq/net)](https://www.npmjs.com/package/@moq/net)

# Media over QUIC

[Media over QUIC](https://moq.dev) (MoQ) is a next-generation live media protocol that provides **real-time latency** at **massive scale**.
Built using modern web technologies, MoQ delivers WebRTC-like latency without the constraints of WebRTC.
The core networking is delegated to a QUIC library but the rest is in application-space, giving you full control over your media pipeline.

**Key Features:**

- 🚀 **Real-time latency** using QUIC for prioritization and partial reliability.
- 📈 **Massive scale** designed for fan-out and supports cross-region clustering.
- 🌐 **Modern Web** using [WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API), [WebCodecs](https://developer.mozilla.org/en-US/docs/Web/API/WebCodecs_API), and [WebAudio](https://developer.mozilla.org/en-US/docs/Web/API/Web_Audio_API).
- 🎯 **Multi-language** with both Rust (native) and TypeScript (web) libraries.
- 🔧 **Generic** for any live data, not just media. Includes text chat as both an example and a core feature.

> **Note:** This project implements [moq-lite](https://doc.moq.dev/concept/moq-lite), a forwards-compatible subset of the IETF [moq-transport](https://datatracker.ietf.org/doc/draft-ietf-moq-transport/) draft. moq-lite works with any moq-transport CDN (ex. [Cloudflare](https://moq.dev/blog/first-cdn/)). The focus is narrower, prioritizing simplicity and deployability.

## Getting Started

Full documentation lives at **[doc.moq.dev](https://doc.moq.dev)**.

- **[Run the demo](https://doc.moq.dev/setup/)** - try MoQ locally with a relay, demo media, and the web UI.
- **[Agent setup](https://doc.moq.dev/setup/agent)** - teach your AI coding agent (Claude Code, Cursor, etc.) how to build with MoQ. The docs are also served as markdown via [llms.txt](https://doc.moq.dev/llms.txt).
- **[Linux packages](https://doc.moq.dev/setup/install)** - install the relay and GStreamer plugin from `apt.moq.dev` / `rpm.moq.dev`.
- **[Production setup](https://doc.moq.dev/setup/prod)** - deploy a relay with a real domain and TLS.

The quickest way to see it in action (requires [Nix](https://nixos.org/download.html) with [flakes](https://nixos.wiki/wiki/Flakes)):

```sh
# Runs a relay, demo media, and the web server
nix develop -c just
```

Then visit <https://localhost:8080>. Don't have Nix? See the [demo guide](https://doc.moq.dev/setup/) for manual setup.

## Architecture

MoQ is designed as a layered protocol stack.

**Rule 1**: The CDN MUST NOT know anything about your application, media codecs, or even the available tracks.
Everything could be fully E2EE and the CDN wouldn't care. **No business logic allowed**.

Instead, [`moq-relay`](rs/moq-relay) operates on rules encoded in the [`moq-net`](https://docs.rs/moq-net) header.
These rules are based on video encoding but are generic enough to be used for any live data.
The goal is to keep the server as dumb as possible while supporting a wide range of use-cases.

The media logic is split into another protocol called [`hang`](https://docs.rs/hang).
It's pretty simple and only intended to be used by clients or media servers.
If you want to do something more custom, then you can always extend it or replace it entirely.

Think of `hang` as like HLS/DASH, while `moq-lite` is like HTTP.

```
┌─────────────────┐
│   Application   │   🏢 Your business logic
│                 │    - authentication, non-media tracks, etc.
├─────────────────┤
│      hang       │   🎬 Media-specific encoding/streaming
│                 │     - codecs, containers, catalog
├─────────────────├
│    moq-lite     │  🚌 Generic pub/sub transport
│                 │     - broadcasts, tracks, groups, frames
├─────────────────┤
│  WebTransport   │  🌐 Browser-compatible QUIC
│      QUIC       │     - HTTP/3 handshake, multiplexing, etc.
└─────────────────┘
```

## Libraries

This repository provides both [Rust](rs) and [TypeScript](js) libraries with similar APIs but language-specific optimizations.

### Rust

| Crate                       | Description                                                                                                                           | Docs                                                                           |
|-----------------------------|---------------------------------------------------------------------------------------------------------------------------------------|--------------------------------------------------------------------------------|
| [moq-net](rs/moq-net)            | The networking layer: real-time pub/sub with built-in caching, fan-out, and prioritization. Negotiates either the `moq-lite` or `moq-transport` wire protocol. | [![docs.rs](https://docs.rs/moq-net/badge.svg)](https://docs.rs/moq-net)       |
| [moq-relay](rs/moq-relay)   | A clusterable relay server. This relay performs fan-out connecting multiple clients and servers together.                             |                                                                                |
| [moq-auth](rs/moq-auth)     | The authorization contract `moq-relay` speaks: requests, grants, leases, and the JWT that rides them. `moq auth` is the CLI.          | [![docs.rs](https://docs.rs/moq-auth/badge.svg)](https://docs.rs/moq-auth)     |
| [moq-tokio](rs/moq-tokio) | Opinionated helpers to configure a Quinn QUIC endpoint. It's harder than it should be.                                                | [![docs.rs](https://docs.rs/moq-tokio/badge.svg)](https://docs.rs/moq-tokio) |
| [libmoq](rs/libmoq)         | C bindings for `moq-net`.                                                                                                             | [![docs.rs](https://docs.rs/libmoq/badge.svg)](https://docs.rs/libmoq)         |
| [hang](rs/hang)             | Media-specific encoding/streaming layered on top of `moq-net`. Can be used as a library.                      | [![docs.rs](https://docs.rs/hang/badge.svg)](https://docs.rs/hang)             |
| [moq-cli](rs/moq-cli)       | A CLI for publishing media to MoQ relays.                                                                                             |                                                                                |
| [moq-mux](rs/moq-mux)       | Media muxers and demuxers (fMP4/CMAF, HLS) for importing content into MoQ broadcasts.                                                 | [![docs.rs](https://docs.rs/moq-mux/badge.svg)](https://docs.rs/moq-mux)       |
| [moq-gst](rs/moq-gst)       | A GStreamer plugin for publishing or consuming MoQ broadcasts. Not built by default; requires GStreamer dev libraries.                         |                                                                                |

### TypeScript

| Package                                  | Description                                                                                                        | NPM                                                                                                   |
|------------------------------------------|--------------------------------------------------------------------------------------------------------------------|-------------------------------------------------------------------------------------------------------|
| **[@moq/net](js/net)**             | The networking layer: real-time pub/sub with built-in caching, fan-out, and prioritization. Negotiates either the `moq-lite` or `moq-transport` wire protocol. Intended for browsers, runs server-side with a WebTransport polyfill. | [![npm](https://img.shields.io/npm/v/@moq/net)](https://www.npmjs.com/package/@moq/net)     |
| **[@moq/auth](js/auth)**             |  The authorization contract and JWT tooling for JS/TS environments (see [Authentication](https://doc.moq.dev/bin/relay/auth))                               | [![npm](https://img.shields.io/npm/v/@moq/auth)](https://www.npmjs.com/package/@moq/auth)   |
| **[@moq/hang](js/hang)**           | Core media library: catalog, container, and support. Shared by `@moq/watch` and `@moq/publish`. | [![npm](https://img.shields.io/npm/v/@moq/hang)](https://www.npmjs.com/package/@moq/hang) |
| **[@moq/demo](demo/web)** | Examples using `@moq/hang`.                                                                                  |                                                                                                       |
| **[@moq/watch](js/watch)**         | Subscribe to and render MoQ broadcasts (Web Component + JS API).                                                        | [![npm](https://img.shields.io/npm/v/@moq/watch)](https://www.npmjs.com/package/@moq/watch)     |
| **[@moq/publish](js/publish)**     | Publish media to MoQ broadcasts (Web Component + JS API).                                                               | [![npm](https://img.shields.io/npm/v/@moq/publish)](https://www.npmjs.com/package/@moq/publish) |

## Protocol

Read the specifications:

- [moq-lite](https://moq-dev.github.io/drafts/draft-lcurley-moq-lite.html)
- [hang](https://moq-dev.github.io/drafts/draft-lcurley-moq-hang.html)
- [use-cases](https://moq-dev.github.io/drafts/draft-lcurley-moq-use-cases.html)

## Development

Contributions are welcome, including AI-assisted issues, pull requests, reviews, and comments. See [CONTRIBUTING.md](CONTRIBUTING.md#ai-contributions) for the attribution policy and guidance on when to open an issue before writing code.

```sh
# See all available commands
just

# Build everything
just build

# Lint and compile what your branch changed
just check

# Test what your branch changed, same scope
just test

# Automatically fix some linting errors, same scope
just fix

# Same as the above, over every package
just check --all
just test all
just fix --all
```

CI runs these same two recipes, so they cover the same ground locally. It sets two things you don't: `MOQ_STRICT=1`, which turns a missing tool into an error instead of a skipped check, and `NEXTEST_PROFILE=ci`, which allows a longer hang timeout.

See the [development guide](https://doc.moq.dev/setup/dev) and the [justfile](justfile) for more.

## Fork maintenance

This is a fork of [moq-dev/moq](https://github.com/moq-dev/moq). Everything above is upstream's; this section is the only thing the fork adds to the docs.

### Branches

| Branch | What it is |
| --- | --- |
| `main` | A pristine mirror of `moq-dev/moq@main`. Fast-forward only. Nothing is ever committed here. |
| `spaceghost` | The default branch: upstream's tip plus the local patch series, rebased (not merged) onto each new upstream tip. |

Keeping `main` byte-identical to upstream is what makes the rest cheap:

- `git diff main..spaceghost` is always exactly the local delta, with no merge noise in the way.
- A change meant for upstream is a topic branch cut from `main`, so its PR contains only that change.
- Dropping the fork is `git branch -D spaceghost`; see [Going back to upstream](#going-back-to-upstream).

Rebase rather than merge because the point of the patch branch is to stay a short, readable stack of commits that can be read, reordered, and sent upstream one at a time. The cost is that `spaceghost` is force-pushed on every sync, which is why consumers pin a tag or a commit rather than the branch name.

### How the sync works

[`.github/workflows/fork-sync.yml`](.github/workflows/fork-sync.yml) runs [`.github/scripts/fork-sync.sh`](.github/scripts/fork-sync.sh) daily at 05:17 UTC, and on demand from the Actions tab ("Fork sync" → "Run workflow", with a `dry_run` checkbox that does the whole rebase and pushes nothing). It runs on a GitHub-hosted runner; it never touches a developer machine.

Each run:

1. Fetches `moq-dev/moq@main`.
2. Fast-forwards `main` to it. A non-fast-forward means someone committed to the mirror, and the run fails instead of forcing it.
3. Rebases the `spaceghost` patch series onto the new upstream tip, in a scratch branch.
4. Tags the pre-rebase tip as `fork-sync/backup-<timestamp>-<sha>` and pushes it, then force-pushes `spaceghost` with `--force-with-lease`.

It is built to fail loudly rather than lose a patch. Any of these stops the run, leaves both remote branches exactly as they were, and files (or comments on) a single issue labelled `fork-sync`:

- a conflict between a local patch and upstream;
- a local patch that has become **empty**, which normally means it landed upstream. `git rebase` would drop such a commit silently, so the script passes `--empty=stop` and makes a human confirm it;
- a patch-commit count that changed across a rebase that otherwise claimed success.

Those three safety properties are the whole point of the script, so they are tested rather than asserted. [`.github/scripts/fork-sync.test.sh`](.github/scripts/fork-sync.test.sh) builds a synthetic upstream, mirror, and patch branch in a temp directory and checks that a clean series survives, that a conflicting patch and an emptied patch each stop the run, and that a mirror with local commits is never force-updated. It is offline, takes a second, and needs no fixture updates as upstream moves:

```sh
.github/scripts/fork-sync.test.sh
```

The pre-rebase tip is always recoverable from the backup tag, so even a bad force-push is undoable. Prune old backup tags whenever they get tedious:

```sh
git ls-remote --tags origin 'refs/tags/fork-sync/backup-*'
git push origin --delete fork-sync/backup-<timestamp>-<sha>
```

**One known limitation.** `GITHUB_TOKEN` is not allowed to push commits that add or change files under `.github/workflows/`. Rebasing does not change those files' contents, so the default token covers the ordinary case. It is not enough the day upstream edits a workflow file, or a local patch touches one, the push is rejected and the run fails. The fix is a fine-grained PAT scoped to this repository with **Contents: read/write**, **Workflows: read/write** and **Issues: read/write**, stored as the `FORK_SYNC_TOKEN` secret; the workflow prefers it and falls back to `GITHUB_TOKEN`.

Two other operational notes:

- GitHub disables scheduled workflows after 60 days without repository activity. If the sync goes quiet, one manual dispatch restarts the schedule.

- Some of upstream's own workflows are guarded by `if: github.repository_owner == 'moq-dev'` and correctly skip here; several are **not**. Observed on the first mirror push to `main`: `Release RS`, `Release JS` and `Release Go` skipped as intended, while `Swift` and `Cache` both started real nix builds on this fork. Every sync push will start them again. They publish nothing (the registry credentials are upstream's), so this is wasted runner time and red checkmarks rather than a danger, and Actions minutes are free on a public repository, but it is worth switching off. Disable each unwanted workflow once, under Actions → the workflow → "Disable workflow"; the setting is per workflow and survives pushes:

  ```sh
  for wf in swift cache cachix nightly smoke alert apt-repo rpm-repo docker \
            libmoq moq-cli moq-gst moq-relay moq-token-cli release-brew \
            release-winget release-dart release-dart-ffi release-go-ffi \
            release-kt-ffi release-kt-lib release-py-ffi release-swift-ffi; do
    id=$(gh api repos/Spaceghost/moq/actions/workflows \
           --jq ".workflows[] | select(.path == \".github/workflows/${wf}.yml\") | .id")
    [ -n "$id" ] && gh api -X PUT "repos/Spaceghost/moq/actions/workflows/${id}/disable"
  done
  ```

  Worth keeping enabled: `check.yml`, `obs.yml`, `wasm.yml` and `dependabot.yml` only run on pull requests, so they cost nothing until a patch is proposed here and are useful when one is.

- `fork-sync.yml` and `fork-release.yml` are guarded with `if: github.repository_owner == 'Spaceghost'`, so forking this fork does not inherit the automation.

### Self-hosted check

[`.github/workflows/fork-check.yml`](.github/workflows/fork-check.yml) runs `just check` and `just test` on every push to a branch of this fork other than `main`, on the owner's fedora build host (x86_64). For each job, `ci-dispatchd` there boots a one-shot container from the `ci-runner-rust` image, which already has Nix, this repository's devshell and a warm sccache. It diffs against `origin/main`, so a push to `spaceghost` checks the fork's patch series. It never runs for pull requests; upstream's `check.yml` still covers those on GitHub-hosted runners, forks included. Set the repository variable `CI_SELF_HOSTED` to `false` to switch it off.

### Adding a local change

```sh
git clone https://github.com/Spaceghost/moq && cd moq
git remote add upstream https://github.com/moq-dev/moq.git
git fetch upstream

git checkout spaceghost
git checkout -b my-change            # optional; small patches can go straight on
# ... edit, then:
just check && just test              # the same recipes CI runs
git commit -s
git checkout spaceghost && git merge --ff-only my-change
git push origin spaceghost
```

Keep each patch one self-contained commit with a message that says *why it is not upstream yet*: "upstream PR #1234 pending", "local-only, never upstreamable", or "implements quest/m2/cs early". That note is what makes the next conflict cheap to resolve and tells you when a patch can be deleted.

If the change is upstreamable, and most are, send it from `main`, not from `spaceghost`:

```sh
git checkout -b upstream-fix main
git cherry-pick <commit-from-spaceghost>
git push origin upstream-fix         # then open a PR against moq-dev/moq
```

Leave the commit on `spaceghost` until the PR merges. On the sync run after it lands, the rebase will report it as empty and file an issue telling you to drop it.

### Releasing a package

[`.github/workflows/fork-release.yml`](.github/workflows/fork-release.yml) builds the artifacts that cannot be consumed straight from git and attaches them to a GitHub Release:

- `moq-relay` for `linux-x64` and `linux-arm64`, built on `ubuntu-22.04`. The x86-64 binary's highest required symbol is `GLIBC_2.34`, so it runs on Ubuntu 22.04 and RHEL/Rocky 9 and newer;
- the `moq_ffi` cdylib for `linux-x64` and `win-x64`, the native library a .NET host `DllImport`s. `moq-ffi` is already `crate-type = ["staticlib", "cdylib"]` upstream, so this needs no patch.

Two ways to run it:

```sh
# Build and publish a dated release from the current spaceghost tip
gh workflow run fork-release.yml --ref spaceghost

# Build only, publish nothing (a pre-tag check)
gh workflow run fork-release.yml --ref spaceghost -f publish=false

# Or cut the tag yourself; the tag push triggers the same workflow
git tag spaceghost-v2026.09.19.1 && git push origin spaceghost-v2026.09.19.1
```

Releases are tagged `spaceghost-v<YYYY.MM.DD>.<n>` and marked pre-release. The prefix is deliberately unlike every upstream tag (`moq-relay-v*`, `libmoq-v*`, `cs-v*`, `v*`), so a fork build can never be mistaken for an upstream one, and nothing here publishes to crates.io, npm, or ghcr, which remain upstream's.

### Consuming the fork

**Rust: pin by git.** This needs no release and no registry. Cargo resolves the workspace's path dependencies inside the git checkout for you.

```toml
[dependencies]
moq-net = { git = "https://github.com/Spaceghost/moq", tag = "spaceghost-v2026.09.19.1" }
moq-native = { git = "https://github.com/Spaceghost/moq", tag = "spaceghost-v2026.09.19.1" }
```

Pin a `tag` or a `rev`, never `branch = "spaceghost"`: the branch is force-pushed on every sync, so a branch pin silently changes under you and cannot be reproduced later.

To switch a project between upstream and the fork in one place, put the choice in the workspace root and let members inherit it:

```toml
# Cargo.toml (workspace root): one of these two blocks
[workspace.dependencies]
moq-net = "0.x"                                                            # upstream, from crates.io
# moq-net = { git = "https://github.com/Spaceghost/moq", tag = "..." }     # the fork
```

```toml
# member Cargo.toml, unchanged either way
[dependencies]
moq-net = { workspace = true }
```

For a temporary local experiment, `[patch]` overrides a dependency without touching any member:

```toml
# upstream crates.io versions, redirected wholesale
[patch.crates-io]
moq-net = { git = "https://github.com/Spaceghost/moq", tag = "..." }

# or, offline, against a working copy
[patch.crates-io]
moq-net = { path = "../moq/rs/moq-net" }
```

**Binaries.** Download `moq-relay` from a fork release and verify it before running it:

```sh
gh release download spaceghost-v2026.09.19.1 -R Spaceghost/moq -p 'moq-relay-linux-x64*'
sha256sum -c moq-relay-linux-x64-moq-relay.sha256
```

**.NET / C#.** There is **no** C# binding in this repository yet, upstream or forked. Upstream has it planned; see [`quest/m2/cs/`](quest/m2/cs/README.md), which specifies generating it from `rs/moq-ffi` with NordSecurity's `uniffi-bindgen-cs` and shipping a NuGet package (working name `Moq.Net`, because `Moq` on NuGet is the unrelated .NET mocking library) carrying `runtimes/<rid>/native` libraries. Until that exists, a .NET consumer has two honest options, both of which need the `moq_ffi` cdylib from a fork release:

1. Implement `quest/m2/cs` early as a fork patch, and publish the resulting package to GitHub Packages from this repository. That is the version worth pinning, and it is the recommended route.
2. Hand-write a narrow `DllImport` layer over just the calls a project needs. Fine for a spike, a liability as a dependency, and upstream's quest says explicitly never to hand-roll a mirror of the FFI surface.

Once such a package exists, consuming it from GitHub Packages looks like this, with `nuget.config` beside the solution:

```xml
<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <packageSources>
    <clear />
    <add key="nuget.org" value="https://api.nuget.org/v3/index.json" />
    <add key="spaceghost" value="https://nuget.pkg.github.com/Spaceghost/index.json" />
  </packageSources>
  <!-- Keep the fork feed authoritative only for its own packages, so a
       compromised or stale feed cannot shadow a nuget.org package. -->
  <packageSourceMapping>
    <packageSource key="nuget.org">
      <package pattern="*" />
    </packageSource>
    <packageSource key="spaceghost">
      <package pattern="Moq.Net*" />
    </packageSource>
  </packageSourceMapping>
</configuration>
```

GitHub Packages requires authentication even for public packages. Store a PAT with `read:packages` outside the repository:

```sh
dotnet nuget update source spaceghost \
  --username Spaceghost --password "$GITHUB_PACKAGES_TOKEN" --store-password-in-clear-text
```

and switch the project between upstream and fork with one property:

```xml
<PropertyGroup>
  <!-- dotnet build -p:UseMoqFork=true -->
  <UseMoqFork Condition="'$(UseMoqFork)' == ''">false</UseMoqFork>
</PropertyGroup>

<ItemGroup Condition="'$(UseMoqFork)' != 'true'">
  <PackageReference Include="Moq.Net" Version="0.1.0" />
</ItemGroup>
<ItemGroup Condition="'$(UseMoqFork)' == 'true'">
  <PackageReference Include="Moq.Net" Version="0.1.0-spaceghost.1" />
</ItemGroup>
```

**Local fallback, no network.** A project reference beats a feed for offline work and for editing both sides at once:

```xml
<ItemGroup Condition="'$(UseMoqFork)' == 'local'">
  <ProjectReference Include="$(MoqForkPath)/cs/moq/Moq.Net.csproj" />
</ItemGroup>
```

Or point a local directory feed at packed `.nupkg` files, which keeps the `PackageReference` shape identical to the published one:

```sh
dotnet pack -o /path/to/local-feed
dotnet nuget add source /path/to/local-feed --name local
```

### Going back to upstream

When the local patches have all landed upstream, or are no longer wanted, there is nothing to unwind:

1. Change consumers to depend on upstream (crates.io versions for Rust, upstream's package for .NET) and drop the `[patch]`/git pins and the `spaceghost` `nuget.config` source.
2. Set the default branch back to `main` (Settings → Branches).
3. `git push origin --delete spaceghost`, and delete the `fork-sync/backup-*` tags and the `spaceghost-v*` releases.
4. Delete `.github/workflows/fork-sync.yml`, `.github/workflows/fork-release.yml`, `.github/scripts/fork-sync.sh` and this section; they only ever lived on `spaceghost`, so deleting that branch already removes them.

`main` is upstream, unmodified, so at that point the fork is indistinguishable from a fresh one and can be deleted outright.

## License

Licensed under either:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or https://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or https://opensource.org/licenses/MIT)

**Exception:** the OBS plugin under [`cpp/obs/`](cpp/obs) is licensed under **GPL-2.0-or-later** (see [`cpp/obs/LICENSE`](cpp/obs/LICENSE)), because it links OBS Studio's `libobs`, which is GPL-2.0. This is a separately-distributable work; per GPLv2 its presence in this repository is mere aggregation and does not affect the MIT/Apache licensing of the rest of the project. `libmoq` and the other moq crates remain MIT/Apache.
