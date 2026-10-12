# Dodo

[![Discord badge][]][Discord link]

A community fork of [Deno](https://github.com/denoland/deno).

> [!IMPORTANT]
> On October 9, 2026, Deno [announced](https://deno.com/blog/cloudflare) that its team is [joining Cloudflare](https://blog.cloudflare.com/deno-joins-cloudflare/). The Deno team will keep shipping monthly bug fix and security releases of the runtime for one more year, and then stop developing it. Deno stays open source, and the announcement invites others to continue it. Dodo is one of those efforts.

## Why fork

Deno's core pieces live in separate `denoland` repos (`deno_graph`, `deno_ast`, `deno_doc`, `deno_lint`, `deno_media_type`, and more) and reach this repo as pinned crates.io releases. A change that spans them has to be merged and released in each repo before Deno can use it, which makes outside contributions very hard.

The first goal is to bring those repos into this one, so any change can be made and reviewed in a single PR.

## Status

Early. There are no Dodo releases yet, so the install commands below install upstream Deno. To run the fork, [build it from source](#build-and-install-from-source).

## About Deno

<img align="right" src="https://deno.land/logo.svg" height="150px" alt="the deno mascot dinosaur standing in the rain">

[Deno](https://deno.com)
([/ˈdiːnoʊ/](https://ipa-reader.com/?text=%CB%88di%CB%90no%CA%8A), pronounced
`dee-no`) is a JavaScript, TypeScript, and WebAssembly runtime with secure
defaults and a great developer experience. It's built on [V8](https://v8.dev/),
[Rust](https://www.rust-lang.org/), and [Tokio](https://tokio.rs/).

Learn more about the Deno runtime
[in the documentation](https://docs.deno.com/runtime/manual).

## Installation

Install the Deno runtime on your system using one of the commands below. Note
that there are a number of ways to install Deno - a comprehensive list of
installation options can be found
[here](https://docs.deno.com/runtime/manual/getting_started/installation).

Shell (Mac, Linux):

```sh
curl -fsSL https://deno.land/install.sh | sh
```

PowerShell (Windows):

```powershell
irm https://deno.land/install.ps1 | iex
```

[Homebrew](https://formulae.brew.sh/formula/deno) (Mac):

```sh
brew install deno
```

[Chocolatey](https://community.chocolatey.org/packages/deno) (Windows):

```powershell
choco install deno
```

[WinGet](https://winstall.app/apps/DenoLand.Deno) (Windows):

```powershell
winget install --id=DenoLand.Deno
```

[Scoop](https://scoop.sh/#/apps?q=deno&id=678d8fb557b611df996989c675b1099630a5bbee)
(Windows):

```powershell
scoop install main/deno
```

### Build and install from source

Complete instructions for building Dodo from source can be found [here](.github/CONTRIBUTING.md#building-from-source).

## Your first Deno program

Deno can be used for many different applications, but is most commonly used to
build web servers. Create a file called `server.ts` and include the following
TypeScript code:

```ts
Deno.serve((_req: Request) => {
  return new Response("Hello, world!");
});
```

Run your server with the following command:

```sh
deno run --allow-net server.ts
```

This should start a local web server on
[http://localhost:8000](http://localhost:8000).

Learn more about writing and running Deno programs
[in the docs](https://docs.deno.com/runtime/manual).

## Additional resources

- **[Deno Docs](https://docs.deno.com)**: official guides and reference docs for
  the Deno runtime, [Deno Deploy](https://deno.com/deploy), and beyond.
- **[Deno Standard Library](https://jsr.io/@std)**: officially supported common
  utilities for Deno programs.
- **[JSR](https://jsr.io/)**: The open-source package registry for modern
  JavaScript and TypeScript
- **[Developer Blog](https://deno.com/blog)**: Product updates, tutorials, and
  more from the Deno team.

## Contributing

We appreciate your help! Open pull requests against the `trunk` branch of [are-we-dodo-yet/dodo](https://github.com/are-we-dodo-yet/dodo), and please read the [contributing instructions](.github/CONTRIBUTING.md).

[Discord badge]: https://img.shields.io/discord/1558888938251223050?logo=discord&style=social
[Discord link]: https://discord.gg/sX7R8Pqnp
